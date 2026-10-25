//! Fonts as pdfminer models them: how bytes become character codes, codes become text, and how wide each
//! character is. Same encodings, same glyph list, same metrics for the 14 standard fonts, same CMap parsing.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::OnceLock;

use super::lexer::Lexer;
use super::objects::{literal_name, Dict, Obj, Resolver};
use super::parser::{StackCore, Step};
use super::tables;

// ----------------------------------------------------------------------------------------- glyph names

fn glyphlist() -> &'static HashMap<&'static str, &'static str> {
    static G: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    G.get_or_init(|| tables::GLYPHLIST.iter().copied().collect())
}

fn invalid_unicode(d: u32) -> bool {
    55295 < d && d < 57344
}

/// pdfminer's `name2unicode` (including its use of `str.strip("uni")`).
pub fn name2unicode(name: &str) -> Option<String> {
    let name = name.split('.').next().unwrap_or("");
    let components: Vec<&str> = name.split('_').collect();
    if components.len() > 1 {
        let mut s = String::new();
        for c in components {
            s.push_str(&name2unicode(c)?);
        }
        return Some(s);
    }
    if let Some(u) = glyphlist().get(name) {
        return Some(u.to_string());
    }
    let is_hex = |s: &str| !s.is_empty() && s.chars().next().map(|c| c.is_ascii_hexdigit()).unwrap_or(false);
    let hex_prefix_len = |s: &str| s.chars().take_while(|c| c.is_ascii_hexdigit()).count();
    if name.starts_with("uni") {
        let without = name.trim_matches(|c| c == 'u' || c == 'n' || c == 'i');
        // HEXADECIMAL.match: the string starts with hex digits; then every 4-digit group is parsed.
        if is_hex(without) && without.chars().count() % 4 == 0 {
            let chars: Vec<char> = without.chars().collect();
            let mut out = String::new();
            for chunk in chars.chunks(4) {
                let s: String = chunk.iter().collect();
                let d = u32::from_str_radix(&s, 16).ok()?;
                if invalid_unicode(d) {
                    return None;
                }
                out.push(char::from_u32(d)?);
            }
            let _ = hex_prefix_len;
            return Some(out);
        }
    } else if name.starts_with('u') {
        let without = name.trim_matches('u');
        let n = without.chars().count();
        if is_hex(without) && (4..=6).contains(&n) {
            let d = u32::from_str_radix(without, 16).ok()?;
            if invalid_unicode(d) {
                return None;
            }
            return Some(char::from_u32(d)?.to_string());
        }
    }
    None
}

// ----------------------------------------------------------------------------------------- encodings

fn base_encoding(name: &str) -> &'static HashMap<u32, String> {
    static E: OnceLock<HashMap<&'static str, HashMap<u32, String>>> = OnceLock::new();
    let all = E.get_or_init(|| {
        let mk = |t: &[(u32, &str)]| t.iter().map(|(k, v)| (*k, v.to_string())).collect::<HashMap<_, _>>();
        let mut m = HashMap::new();
        m.insert("StandardEncoding", mk(tables::STD_ENCODING));
        m.insert("MacRomanEncoding", mk(tables::MAC_ENCODING));
        m.insert("WinAnsiEncoding", mk(tables::WIN_ENCODING));
        m.insert("PDFDocEncoding", mk(tables::PDF_ENCODING));
        m
    });
    all.get(name).unwrap_or_else(|| all.get("StandardEncoding").unwrap())
}

fn get_encoding(name: &str, diff: &[Obj]) -> HashMap<u32, String> {
    let mut m = base_encoding(name).clone();
    let mut cid: i64 = 0;
    for x in diff {
        match x {
            Obj::Int(i) => cid = *i,
            Obj::Name(n) => {
                if let super::lexer::Name::Text(s) = n {
                    if let Some(u) = name2unicode(s) {
                        if cid >= 0 {
                            m.insert(cid as u32, u);
                        }
                    }
                }
                cid += 1;
            }
            _ => {}
        }
    }
    m
}

// ------------------------------------------------------------------------------------------ cmaps

#[derive(Clone, Debug, Default)]
pub struct UnicodeMap {
    pub cid2unichr: HashMap<u32, String>,
    pub identity: bool,
    pub attrs: HashMap<String, Obj>,
}

impl UnicodeMap {
    pub fn get(&self, cid: u32) -> Option<String> {
        if self.identity {
            return char::from_u32(cid).map(|c| c.to_string());
        }
        self.cid2unichr.get(&cid).cloned()
    }

    fn add(&mut self, cid: u32, code: &Obj) {
        let u = match code {
            Obj::Name(n) => match n {
                super::lexer::Name::Text(s) => match name2unicode(s) {
                    Some(u) => u,
                    None => return, // KeyError: pdfminer stops the parsing of this map here
                },
                _ => return,
            },
            Obj::Str(b) => utf16be_ignore(b),
            Obj::Int(i) => match char::from_u32(*i as u32) {
                Some(c) => c.to_string(),
                None => return,
            },
            _ => return,
        };
        if u == "\u{a0}" && self.cid2unichr.get(&cid).map(|s| s == " ").unwrap_or(false) {
            return;
        }
        self.cid2unichr.insert(cid, u);
    }
}

/// `bytes.decode("UTF-16BE", "ignore")`.
pub fn utf16be_ignore(b: &[u8]) -> String {
    let units: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
    char::decode_utf16(units.into_iter()).filter_map(|r| r.ok()).collect()
}

fn nunpack(b: &[u8]) -> u64 {
    b.iter().fold(0u64, |acc, &x| (acc << 8) | x as u64)
}

/// `CMapParser` run over a ToUnicode stream.
pub fn parse_tounicode(data: &[u8]) -> UnicodeMap {
    let mut map = UnicodeMap::default();
    let mut lexer = Lexer::new(data);
    let mut core = StackCore::default();
    let mut in_cmap = true;
    while let Some((pos, tok)) = lexer.next_token() {
        let Step::Keyword(p, k) = core.feed(pos, tok) else { continue };
        let objs = |core: &mut StackCore| core.popall().into_iter().map(|(_, o)| o).collect::<Vec<_>>();
        match k.as_slice() {
            b"begincmap" => {
                in_cmap = true;
                core.popall();
                continue;
            }
            b"endcmap" => {
                in_cmap = false;
                continue;
            }
            _ => {}
        }
        if !in_cmap {
            continue;
        }
        match k.as_slice() {
            b"def" => {
                let two = core.pop(2);
                if two.len() == 2 {
                    map.attrs.insert(literal_name(&two[0].1), two[1].1.clone());
                }
            }
            b"usecmap" => {
                core.pop(1);
            }
            b"begincodespacerange" | b"endcodespacerange" | b"begincidrange" | b"begincidchar" | b"beginbfrange"
            | b"beginbfchar" | b"beginnotdefrange" | b"endnotdefrange" => {
                core.popall();
            }
            b"endcidrange" => {
                let o = objs(&mut core);
                for c in o.chunks(3) {
                    if c.len() < 3 {
                        break;
                    }
                    let (Obj::Str(s), Obj::Str(e), Obj::Int(cid)) = (&c[0], &c[1], &c[2]) else { continue };
                    if s.len() != e.len() {
                        continue;
                    }
                    let sp = &s[..s.len().saturating_sub(4)];
                    let ep = &e[..e.len().saturating_sub(4)];
                    if sp != ep {
                        continue;
                    }
                    let sv = &s[s.len().saturating_sub(4)..];
                    let ev = &e[e.len().saturating_sub(4)..];
                    let (start, end) = (nunpack(sv), nunpack(ev));
                    let vlen = sv.len();
                    if end < start {
                        continue;
                    }
                    for i in 0..=(end - start) {
                        let v = ((start + i) as u32).to_be_bytes();
                        let mut x = sp.to_vec();
                        x.extend_from_slice(&v[4 - vlen..]);
                        map.add((*cid + i as i64) as u32, &Obj::Str(x));
                    }
                }
            }
            b"endcidchar" => {
                let o = objs(&mut core);
                for c in o.chunks(2) {
                    if c.len() < 2 {
                        break;
                    }
                    if let (Obj::Int(cid), Obj::Str(_)) = (&c[0], &c[1]) {
                        map.add(*cid as u32, &c[1]);
                    }
                }
            }
            b"endbfrange" => {
                let o = objs(&mut core);
                for c in o.chunks(3) {
                    if c.len() < 3 {
                        break;
                    }
                    let (Obj::Str(s), Obj::Str(e)) = (&c[0], &c[1]) else { continue };
                    if s.len() != e.len() {
                        continue;
                    }
                    let (start, end) = (nunpack(s), nunpack(e));
                    match &c[2] {
                        Obj::Array(codes) => {
                            for (i, code) in codes.iter().enumerate() {
                                let cid = start + i as u64;
                                if cid > end {
                                    break;
                                }
                                map.add(cid as u32, code);
                            }
                        }
                        Obj::Str(code) => {
                            let var = &code[code.len().saturating_sub(4)..];
                            let base = nunpack(var);
                            let prefix = &code[..code.len().saturating_sub(4)];
                            let vlen = var.len();
                            if end < start {
                                continue;
                            }
                            for i in 0..=(end - start) {
                                let v = ((base + i) as u32).to_be_bytes();
                                let mut x = prefix.to_vec();
                                x.extend_from_slice(&v[4 - vlen..]);
                                map.add((start + i) as u32, &Obj::Str(x));
                            }
                        }
                        _ => {}
                    }
                }
            }
            b"endbfchar" => {
                let o = objs(&mut core);
                for c in o.chunks(2) {
                    if c.len() < 2 {
                        break;
                    }
                    if let (Obj::Str(cid), Obj::Str(_)) = (&c[0], &c[1]) {
                        map.add(nunpack(cid) as u32, &c[1]);
                    }
                }
            }
            _ => core.push(p, Obj::Keyword(k)),
        }
    }
    map
}

#[derive(Clone, Debug)]
pub enum CMap {
    Identity { vertical: bool },
    IdentityByte { vertical: bool },
    /// A CMap pdfminer could not find: it decodes nothing.
    Empty,
}

impl CMap {
    fn by_name(name: &str) -> CMap {
        match name {
            "Identity-H" => CMap::Identity { vertical: false },
            "Identity-V" => CMap::Identity { vertical: true },
            "OneByteIdentityH" => CMap::IdentityByte { vertical: false },
            "OneByteIdentityV" => CMap::IdentityByte { vertical: true },
            _ => CMap::Empty,
        }
    }
    fn is_vertical(&self) -> bool {
        matches!(self, CMap::Identity { vertical: true } | CMap::IdentityByte { vertical: true })
    }
    fn decode(&self, b: &[u8]) -> Vec<u32> {
        match self {
            CMap::Identity { .. } => b.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]]) as u32).collect(),
            CMap::IdentityByte { .. } => b.iter().map(|&c| c as u32).collect(),
            CMap::Empty => vec![],
        }
    }
}

// --------------------------------------------------------------------------------------- font files

/// `Type1FontHeaderParser.get_encoding`: the `dup <code> /<name> put` pairs of an embedded Type 1 font.
fn type1_encoding(data: &[u8]) -> HashMap<u32, String> {
    let mut out = HashMap::new();
    let mut lexer = Lexer::new(data);
    let mut core = StackCore::default();
    while let Some((pos, tok)) = lexer.next_token() {
        if let Step::Keyword(_, k) = core.feed(pos, tok) {
            if k.as_slice() == b"put" {
                let two = core.pop(2);
                if two.len() == 2 {
                    if let (Obj::Int(key), Obj::Name(super::lexer::Name::Text(name))) = (&two[0].1, &two[1].1) {
                        if let Some(u) = name2unicode(name) {
                            out.insert(*key as u32, u);
                        }
                    }
                }
            }
            // other keywords are not pushed: PSStackParser.do_keyword does nothing
        }
    }
    out
}

/// `TrueTypeFont.create_unicode_map`: gid -> text from the font's own Unicode cmap.
fn truetype_unicode_map(data: &[u8]) -> Option<UnicodeMap> {
    let u16at = |p: usize| -> Option<u16> { data.get(p..p + 2).map(|b| u16::from_be_bytes([b[0], b[1]])) };
    let i16at = |p: usize| -> Option<i16> { u16at(p).map(|v| v as i16) };
    let u32at = |p: usize| -> Option<u32> { data.get(p..p + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]])) };
    let ntables = u16at(4)? as usize;
    let mut cmap_off = None;
    for t in 0..ntables {
        let base = 12 + t * 16;
        if data.get(base..base + 4)? == b"cmap" {
            cmap_off = Some(u32at(base + 8)? as usize);
        }
    }
    let base = cmap_off?;
    let nsub = u16at(base + 2)? as usize;
    let mut char2gid: indexmap::IndexMap<u32, u32> = indexmap::IndexMap::new();
    for i in 0..nsub {
        let e = base + 4 + i * 8;
        let (pid, eid, off) = (u16at(e)?, u16at(e + 2)?, u32at(e + 4)? as usize);
        if !(pid == 0 || (pid == 3 && (eid == 1 || eid == 10))) {
            continue;
        }
        let p = base + off;
        let fmt = u16at(p)?;
        match fmt {
            0 => {
                for c in 0..256usize {
                    char2gid.insert(c as u32, *data.get(p + 6 + c)? as u32);
                }
            }
            2 => {
                let keys: Vec<u16> = (0..256).map(|k| u16at(p + 6 + k * 2).unwrap_or(0)).collect();
                let mut firstbytes = vec![0usize; 8192];
                for (i, k) in keys.iter().enumerate() {
                    firstbytes[(*k / 8) as usize] = i;
                }
                let nhdrs = (*keys.iter().max().unwrap_or(&0) / 8) as usize + 1;
                let mut q = p + 6 + 512;
                let mut hdrs = vec![];
                for i in 0..nhdrs {
                    let (fc, ec, delta, ro) = (u16at(q)?, u16at(q + 2)?, i16at(q + 4)?, u16at(q + 6)?);
                    hdrs.push((i, fc, ec, delta, q + 8 - 2 + ro as usize));
                    q += 8;
                }
                for (i, fc, ec, delta, pos) in hdrs {
                    if ec == 0 {
                        continue;
                    }
                    let first = fc as u32 + ((firstbytes[i] as u32) << 8);
                    for c in 0..ec as usize {
                        let mut gid = u16at(pos + c * 2)? as i32;
                        if gid != 0 {
                            gid += delta as i32;
                        }
                        char2gid.insert(first + c as u32, gid as u32);
                    }
                }
            }
            4 => {
                let segcount = (u16at(p + 6)? / 2) as usize;
                let ecs = p + 14;
                let scs = ecs + segcount * 2 + 2;
                let idds = scs + segcount * 2;
                let idrs = idds + segcount * 2;
                for s in 0..segcount {
                    let (ec, sc, idd, idr) = (u16at(ecs + s * 2)?, u16at(scs + s * 2)?, i16at(idds + s * 2)?, u16at(idrs + s * 2)?);
                    if sc > ec {
                        continue;
                    }
                    if idr != 0 {
                        let mut q = idrs + idr as usize;
                        for c in sc..=ec {
                            let b = u16at(q)? as i32;
                            q += 2;
                            char2gid.insert(c as u32, ((b + idd as i32) & 0xffff) as u32);
                        }
                    } else {
                        for c in sc..=ec {
                            char2gid.insert(c as u32, ((c as i32 + idd as i32) & 0xffff) as u32);
                        }
                    }
                }
            }
            6 => {
                let (fc, n) = (u16at(p + 6)? as u32, u16at(p + 8)? as usize);
                for i in 0..n {
                    char2gid.insert(fc + i as u32, u16at(p + 10 + i * 2)? as u32);
                }
            }
            10 => {
                let (sc, n) = (u32at(p + 12)?, u32at(p + 16)? as usize);
                for i in 0..n {
                    char2gid.insert(sc + i as u32, u16at(p + 20 + i * 2)? as u32);
                }
            }
            12 => {
                let n = u32at(p + 12)? as usize;
                for g in 0..n {
                    let q = p + 16 + g * 12;
                    let (sc, ec, mut sgid) = (u32at(q)?, u32at(q + 4)?, u32at(q + 8)?);
                    for code in sc..=ec {
                        char2gid.insert(code, sgid);
                        sgid += 1;
                    }
                }
            }
            _ => return None, // pdfminer raises on other formats
        }
    }
    if char2gid.is_empty() {
        return None;
    }
    let mut m = UnicodeMap::default();
    for (ch, gid) in char2gid {
        m.add(gid, &Obj::Int(ch as i64));
    }
    Some(m)
}

// -------------------------------------------------------------------------------------------- fonts

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum WidthKey {
    Cid(i64),
    Text(String),
}

#[derive(Debug)]
pub enum Kind {
    Simple { cid2unicode: HashMap<u32, String> },
    Cid { cmap: CMap },
}

#[derive(Debug)]
pub struct Font {
    pub fontname: String,
    pub descent: f64,
    pub ascent: f64,
    pub bbox: [f64; 4],
    pub default_width: f64,
    pub widths: HashMap<WidthKey, Option<f64>>,
    pub hscale: f64,
    pub vscale: f64,
    pub unicode_map: Option<UnicodeMap>,
    pub kind: Kind,
    pub vertical: bool,
    pub disps: HashMap<i64, (Option<f64>, f64)>,
    pub default_disp: (Option<f64>, f64),
}

fn num(r: &dyn Resolver, o: Option<&Obj>, default: f64) -> f64 {
    match o {
        None => default,
        Some(o) => r.resolve(o).as_f64().unwrap_or(0.0),
    }
}

fn dict_of(r: &dyn Resolver, o: Option<&Obj>) -> Dict {
    match o.map(|x| r.resolve(x)) {
        Some(Obj::Dict(d)) => (*d).clone(),
        Some(Obj::Stream(s)) => s.dict.clone(),
        _ => Dict::new(),
    }
}

fn list_of(r: &dyn Resolver, o: Option<&Obj>) -> Vec<Obj> {
    match o.map(|x| r.resolve(x)) {
        Some(Obj::Array(a)) => a,
        _ => vec![],
    }
}

fn parse_bbox(r: &dyn Resolver, d: &Dict) -> [f64; 4] {
    let l = list_of(r, d.get("FontBBox"));
    if l.len() == 4 {
        let v: Vec<Option<f64>> = l.iter().map(|x| r.resolve(x).as_f64()).collect();
        if v.iter().all(|x| x.is_some()) {
            return [v[0].unwrap(), v[1].unwrap(), v[2].unwrap(), v[3].unwrap()];
        }
    }
    [0.0; 4]
}

pub struct FontError(pub String);

impl Font {
    fn base(
        r: &dyn Resolver,
        descriptor: &Dict,
        widths: HashMap<WidthKey, Option<f64>>,
        default_width: Option<f64>,
        kind: Kind,
        unicode_map: Option<UnicodeMap>,
    ) -> Font {
        let fontname = match descriptor.get("FontName").map(|x| r.resolve(x)) {
            None => "unknown".to_string(),
            Some(Obj::Name(n)) => n.as_str(),
            Some(Obj::Str(b)) => String::from_utf8_lossy(&b).into_owned(),
            Some(other) => super::objects::python_str(&other),
        };
        let mut descent = num(r, descriptor.get("Descent"), 0.0);
        let ascent = num(r, descriptor.get("Ascent"), 0.0);
        let default_width = match default_width {
            Some(w) => w,
            None => num(r, descriptor.get("MissingWidth"), 0.0),
        };
        if descent > 0.0 {
            descent = -descent;
        }
        Font {
            fontname,
            descent,
            ascent,
            bbox: parse_bbox(r, descriptor),
            default_width,
            widths,
            hscale: 0.001,
            vscale: 0.001,
            unicode_map,
            kind,
            vertical: false,
            disps: HashMap::new(),
            default_disp: (None, 0.0),
        }
    }

    pub fn is_multibyte(&self) -> bool {
        matches!(self.kind, Kind::Cid { .. })
    }

    pub fn decode(&self, b: &[u8]) -> Vec<u32> {
        match &self.kind {
            Kind::Simple { .. } => b.iter().map(|&c| c as u32).collect(),
            Kind::Cid { cmap } => cmap.decode(b),
        }
    }

    pub fn get_descent(&self) -> f64 {
        self.descent * self.vscale
    }

    /// `to_unichr`: None when pdfminer raises PDFUnicodeNotDefined.
    pub fn to_unichr(&self, cid: u32) -> Option<String> {
        match &self.kind {
            Kind::Simple { cid2unicode } => {
                if let Some(m) = &self.unicode_map {
                    if let Some(u) = m.get(cid) {
                        return Some(u);
                    }
                }
                cid2unicode.get(&cid).cloned()
            }
            Kind::Cid { .. } => self.unicode_map.as_ref().and_then(|m| m.get(cid)),
        }
    }

    pub fn char_width(&self, cid: u32) -> f64 {
        if let Some(Some(w)) = self.widths.get(&WidthKey::Cid(cid as i64)) {
            return w * self.hscale;
        }
        if let Some(u) = self.to_unichr(cid) {
            if let Some(Some(w)) = self.widths.get(&WidthKey::Text(u)) {
                return w * self.hscale;
            }
        }
        self.default_width * self.hscale
    }

    pub fn char_disp(&self, cid: u32) -> (Option<f64>, f64) {
        if !self.vertical {
            return (None, 0.0);
        }
        self.disps.get(&(cid as i64)).copied().unwrap_or(self.default_disp)
    }

    fn simple_encoding(r: &dyn Resolver, spec: &Dict) -> (HashMap<u32, String>, Option<UnicodeMap>) {
        let default = if spec.get("Subtype").map(|s| literal_name(&r.resolve(s)) == "TrueType").unwrap_or(false) {
            "WinAnsiEncoding"
        } else {
            "StandardEncoding"
        };
        let enc = spec.get("Encoding").map(|e| r.resolve(e));
        let cid2unicode = match &enc {
            None => get_encoding(default, &[]),
            Some(Obj::Dict(d)) => {
                let name = match d.get("BaseEncoding") {
                    Some(b) => literal_name(b),
                    None => default.to_string(),
                };
                let diff = list_of(r, d.get("Differences"));
                get_encoding(&name, &diff)
            }
            Some(other) => get_encoding(&literal_name(other), &[]),
        };
        let unicode_map = spec.get("ToUnicode").map(|t| match r.resolve(t) {
            Obj::Stream(s) => parse_tounicode(s.data(r)),
            _ => UnicodeMap::default(),
        });
        (cid2unicode, unicode_map)
    }

    fn type1(r: &dyn Resolver, spec: &Dict) -> Result<Font, FontError> {
        let basefont = match spec.get("BaseFont") {
            Some(b) => literal_name(&r.resolve(b)),
            None => "unknown".to_string(),
        };
        let (descriptor, widths) = match tables::STD_FONTS.iter().find(|f| f.name == basefont) {
            Some(m) => {
                let mut d = Dict::new();
                d.insert("FontName".into(), Obj::Name(super::lexer::Name::Text(m.name.into())));
                d.insert("Flags".into(), Obj::Int(m.flags));
                d.insert("Ascent".into(), Obj::Real(m.ascent));
                d.insert("Descent".into(), Obj::Real(m.descent));
                d.insert("ItalicAngle".into(), Obj::Real(m.italic_angle));
                d.insert("FontBBox".into(), Obj::Array(m.bbox.iter().map(|x| Obj::Real(*x)).collect()));
                let w = m.widths.iter().map(|(k, v)| (WidthKey::Text(k.to_string()), Some(*v))).collect();
                (d, w)
            }
            None => {
                let descriptor = dict_of(r, spec.get("FontDescriptor"));
                let firstchar = match spec.get("FirstChar").map(|x| r.resolve(x)) {
                    Some(Obj::Int(i)) => i,
                    _ => 0,
                };
                let list = match spec.get("Widths") {
                    Some(w) => list_of(r, Some(w)),
                    None => vec![Obj::Int(0); 256],
                };
                let w = list.iter().enumerate().map(|(i, w)| (WidthKey::Cid(i as i64 + firstchar), r.resolve(w).as_f64())).collect();
                (descriptor, w)
            }
        };
        let (mut cid2unicode, unicode_map) = Font::simple_encoding(r, spec);
        if !spec.contains_key("Encoding") && descriptor.contains_key("FontFile") {
            if let Obj::Stream(ff) = r.resolve(descriptor.get("FontFile").unwrap()) {
                let length1 = match ff.get("Length1").map(|x| r.resolve(x)) {
                    Some(Obj::Int(i)) => i.max(0) as usize,
                    Some(_) => 0,
                    None => return Err(FontError("Length1".into())),
                };
                let data = ff.data(r);
                cid2unicode = type1_encoding(&data[..length1.min(data.len())]);
            }
        }
        Ok(Font::base(r, &descriptor, widths, None, Kind::Simple { cid2unicode }, unicode_map))
    }

    fn type3(r: &dyn Resolver, spec: &Dict) -> Font {
        let firstchar = match spec.get("FirstChar").map(|x| r.resolve(x)) {
            Some(Obj::Int(i)) => i,
            _ => 0,
        };
        let list = match spec.get("Widths") {
            Some(w) => list_of(r, Some(w)),
            None => vec![Obj::Int(0); 256],
        };
        let widths = list.iter().enumerate().map(|(i, w)| (WidthKey::Cid(i as i64 + firstchar), r.resolve(w).as_f64())).collect();
        let descriptor = if spec.contains_key("FontDescriptor") {
            dict_of(r, spec.get("FontDescriptor"))
        } else {
            let mut d = Dict::new();
            d.insert("Ascent".into(), Obj::Int(0));
            d.insert("Descent".into(), Obj::Int(0));
            if let Some(b) = spec.get("FontBBox") {
                d.insert("FontBBox".into(), b.clone());
            }
            d
        };
        let (cid2unicode, unicode_map) = Font::simple_encoding(r, spec);
        let mut f = Font::base(r, &descriptor, widths, None, Kind::Simple { cid2unicode }, unicode_map);
        let m: Vec<f64> = list_of(r, spec.get("FontMatrix")).iter().map(|x| r.resolve(x).as_f64().unwrap_or(0.0)).collect();
        f.descent = f.bbox[1];
        f.ascent = f.bbox[3];
        if m.len() >= 4 {
            f.hscale = m[0] * 1.0 + m[2] * 1.0;
            f.vscale = m[1] * 1.0 + m[3] * 1.0;
        }
        f
    }

    fn cid(r: &dyn Resolver, spec: &Dict) -> Font {
        let sysinfo = dict_of(r, spec.get("CIDSystemInfo"));
        let s = |k: &str| match sysinfo.get(k).map(|x| r.resolve(x)) {
            Some(Obj::Str(b)) => b.iter().map(|&c| c as char).collect::<String>(),
            _ => "unknown".to_string(),
        };
        let (registry, ordering) = (s("Registry"), s("Ordering"));
        let cidcoding = format!("{}-{}", registry.trim(), ordering.trim());
        let cmap_name = {
            let mut name = "unknown".to_string();
            if let Some(e) = spec.get("Encoding") {
                match r.resolve(e) {
                    Obj::Name(n) => name = n.as_str(),
                    Obj::Dict(d) => {
                        if let Some(c) = d.get("CMapName") {
                            name = literal_name(&r.resolve(c));
                        }
                    }
                    Obj::Stream(st) => {
                        if let Some(c) = st.get("CMapName") {
                            name = literal_name(&r.resolve(c));
                        }
                    }
                    _ => {}
                }
            }
            match name.as_str() {
                "DLIdent-H" => "Identity-H".to_string(),
                "DLIdent-V" => "Identity-V".to_string(),
                _ => name,
            }
        };
        let cmap = CMap::by_name(&cmap_name);
        let descriptor = dict_of(r, spec.get("FontDescriptor"));
        let ttf: Option<Vec<u8>> = match descriptor.get("FontFile2").map(|x| r.resolve(x)) {
            Some(Obj::Stream(s)) => Some(s.data(r).to_vec()),
            _ => None,
        };
        let mut unicode_map = None;
        if let Some(tu) = spec.get("ToUnicode") {
            match r.resolve(tu) {
                Obj::Stream(s) => unicode_map = Some(parse_tounicode(s.data(r))),
                other => {
                    let name = literal_name(&other);
                    let enc = spec.get("Encoding").map(|e| literal_name(&r.resolve(e))).unwrap_or_default();
                    if ordering.contains("Identity") || name.contains("Identity") || enc.contains("Identity") {
                        unicode_map = Some(UnicodeMap { identity: true, ..Default::default() });
                    }
                }
            }
        } else if cidcoding == "Adobe-Identity" || cidcoding == "Adobe-UCS" {
            if let Some(t) = &ttf {
                unicode_map = truetype_unicode_map(t);
            }
        }
        let vertical = cmap.is_vertical();
        let mut widths: HashMap<WidthKey, Option<f64>> = HashMap::new();
        let mut disps = HashMap::new();
        let default_width;
        let mut default_disp = (None, 0.0);
        if vertical {
            let w2 = list_of(r, spec.get("W2"));
            let mut acc: Vec<f64> = vec![];
            for v in &w2 {
                match r.resolve(v) {
                    Obj::Array(a) => {
                        if let Some(&c1) = acc.last() {
                            let nums: Vec<f64> = a.iter().map(|x| r.resolve(x).as_f64().unwrap_or(0.0)).collect();
                            for (i, t) in nums.chunks(3).enumerate() {
                                if t.len() == 3 {
                                    widths.insert(WidthKey::Cid(c1 as i64 + i as i64), Some(t[0]));
                                    disps.insert(c1 as i64 + i as i64, (Some(t[1]), t[2]));
                                }
                            }
                            acc.clear();
                        }
                    }
                    o => {
                        if let Some(x) = o.as_f64() {
                            acc.push(x);
                            if acc.len() == 5 {
                                for c in (acc[0] as i64)..=(acc[1] as i64) {
                                    widths.insert(WidthKey::Cid(c), Some(acc[2]));
                                    disps.insert(c, (Some(acc[3]), acc[4]));
                                }
                                acc.clear();
                            }
                        }
                    }
                }
            }
            let dw2 = list_of(r, spec.get("DW2"));
            let (vy, w) = if dw2.len() == 2 { (r.resolve(&dw2[0]).as_f64().unwrap_or(880.0), r.resolve(&dw2[1]).as_f64().unwrap_or(-1000.0)) } else { (880.0, -1000.0) };
            default_disp = (None, vy);
            default_width = w;
        } else {
            let w = list_of(r, spec.get("W"));
            let mut acc: Vec<Obj> = vec![];
            for v in &w {
                let v = r.resolve(v);
                match &v {
                    Obj::Array(a) => {
                        if let Some(last) = acc.last() {
                            if let Some(c1) = last.as_f64() {
                                for (i, x) in a.iter().enumerate() {
                                    widths.insert(WidthKey::Cid(c1 as i64 + i as i64), r.resolve(x).as_f64());
                                }
                            }
                            acc.clear();
                        }
                    }
                    Obj::Int(_) | Obj::Real(_) => {
                        acc.push(v.clone());
                        if acc.len() == 3 {
                            if let (Obj::Int(a), Obj::Int(b)) = (&acc[0], &acc[1]) {
                                for c in *a..=*b {
                                    widths.insert(WidthKey::Cid(c), acc[2].as_f64());
                                }
                            }
                            acc.clear();
                        }
                    }
                    _ => {}
                }
            }
            default_width = match spec.get("DW") {
                Some(x) => r.resolve(x).as_f64().unwrap_or(0.0),
                None => 1000.0,
            };
        }
        let mut f = Font::base(r, &descriptor, widths, Some(default_width), Kind::Cid { cmap }, unicode_map);
        f.vertical = vertical;
        f.disps = disps;
        f.default_disp = default_disp;
        f
    }

    /// `PDFResourceManager.get_font`.
    pub fn from_spec(r: &dyn Resolver, spec: &Dict) -> Result<Rc<Font>, FontError> {
        let subtype = match spec.get("Subtype") {
            Some(s) => literal_name(&r.resolve(s)),
            None => "Type1".to_string(),
        };
        let f = match subtype.as_str() {
            "Type1" | "MMType1" | "TrueType" => Font::type1(r, spec)?,
            "Type3" => Font::type3(r, spec),
            "CIDFontType0" | "CIDFontType2" => Font::cid(r, spec),
            "Type0" => {
                let d = list_of(r, spec.get("DescendantFonts"));
                let Some(first) = d.first() else { return Err(FontError("DescendantFonts".into())) };
                let mut sub = dict_of(r, Some(first));
                for k in ["Encoding", "ToUnicode"] {
                    if let Some(v) = spec.get(k) {
                        sub.insert(k.to_string(), r.resolve(v));
                    }
                }
                return Font::from_spec(r, &sub);
            }
            _ => Font::type1(r, spec)?,
        };
        Ok(Rc::new(f))
    }
}
