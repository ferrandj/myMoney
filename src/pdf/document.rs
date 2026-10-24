//! The document: cross-reference tables (classic, streams, or rebuilt by scanning), indirect objects and the
//! page tree, following pdfminer's `PDFDocument` and `PDFPage`.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::lexer::Token;
use super::objects::{Dict, Obj, Resolver, Stream};
use super::parser::{parse_object_stream, safe_int, FileParser};

#[derive(Debug)]
pub enum PdfError {
    NoRoot,
    Encrypted,
    Syntax(String),
}

impl std::fmt::Display for PdfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PdfError::NoRoot => write!(f, "No /Root object! - Is this really a PDF?"),
            PdfError::Encrypted => write!(f, "PDF protégé par un mot de passe"),
            PdfError::Syntax(s) => write!(f, "{s}"),
        }
    }
}

enum XRef {
    Table { offsets: HashMap<u32, (Option<u32>, usize, u32)>, order: Vec<u32>, trailer: Dict },
    Stream { data: Vec<u8>, w: (usize, usize, usize), ranges: Vec<(i64, i64)>, trailer: Dict },
}

fn nunpack(b: &[u8], default: u64) -> u64 {
    if b.is_empty() {
        return default;
    }
    b.iter().fold(0u64, |acc, &x| (acc << 8) | x as u64)
}

impl XRef {
    fn trailer(&self) -> &Dict {
        match self {
            XRef::Table { trailer, .. } | XRef::Stream { trailer, .. } => trailer,
        }
    }
    fn get_pos(&self, objid: u32) -> Option<(Option<u32>, usize, u32)> {
        match self {
            XRef::Table { offsets, .. } => offsets.get(&objid).copied(),
            XRef::Stream { data, w, ranges, .. } => {
                let mut index: i64 = 0;
                let mut found = false;
                for &(start, n) in ranges {
                    if start <= objid as i64 && (objid as i64) < start + n {
                        index += objid as i64 - start;
                        found = true;
                        break;
                    }
                    index += n;
                }
                if !found {
                    return None;
                }
                let entlen = w.0 + w.1 + w.2;
                let off = entlen * index as usize;
                if off + entlen > data.len() {
                    return None;
                }
                let ent = &data[off..off + entlen];
                let f1 = nunpack(&ent[..w.0], 1);
                let f2 = nunpack(&ent[w.0..w.0 + w.1], 0);
                let f3 = nunpack(&ent[w.0 + w.1..], 0);
                match f1 {
                    1 => Some((None, f2 as usize, f3 as u32)),
                    2 => Some((Some(f2 as u32), f3 as usize, 0)),
                    _ => None,
                }
            }
        }
    }
    fn objids(&self) -> Vec<u32> {
        match self {
            XRef::Table { order, .. } => order.clone(),
            XRef::Stream { data, w, ranges, .. } => {
                let entlen = w.0 + w.1 + w.2;
                let mut v = Vec::new();
                for &(start, n) in ranges {
                    for i in 0..n {
                        let off = entlen * i as usize;
                        if off + entlen > data.len() {
                            break;
                        }
                        let f1 = nunpack(&data[off..off + w.0], 1);
                        if f1 == 1 || f1 == 2 {
                            v.push((start + i) as u32);
                        }
                    }
                }
                v
            }
        }
    }
}

pub struct Document {
    data: Vec<u8>,
    xrefs: Vec<XRef>,
    cache: RefCell<HashMap<u32, Obj>>,
    objstm: RefCell<HashMap<u32, Rc<(Vec<Obj>, i64)>>>,
    pub catalog: Dict,
    resolving: RefCell<HashSet<u32>>,
}

impl Resolver for Document {
    fn resolve(&self, o: &Obj) -> Obj {
        let mut cur = o.clone();
        let mut guard = 0;
        while let Obj::Ref(id) = cur {
            cur = self.getobj(id).unwrap_or(Obj::Null);
            guard += 1;
            if guard > 32 {
                return Obj::Null;
            }
        }
        cur
    }
}

impl Document {
    pub fn open(data: Vec<u8>) -> Result<Document, PdfError> {
        let mut doc = Document {
            data,
            xrefs: vec![],
            cache: Default::default(),
            objstm: Default::default(),
            catalog: Dict::new(),
            resolving: Default::default(),
        };
        let mut fallback = false;
        match doc.find_xref() {
            Some(start) => {
                let mut xrefs = vec![];
                let mut seen = HashSet::new();
                if doc.read_xref_from(start, &mut xrefs, &mut seen).is_err() {
                    fallback = true;
                } else {
                    doc.xrefs = xrefs;
                }
            }
            None => fallback = true,
        }
        if fallback {
            let x = doc.load_fallback();
            doc.xrefs = vec![x];
        }
        let mut catalog = None;
        for x in &doc.xrefs {
            let t = x.trailer();
            if t.is_empty() {
                continue;
            }
            if t.contains_key("Encrypt") {
                return Err(PdfError::Encrypted);
            }
            if let Some(root) = t.get("Root") {
                catalog = Some(root.clone());
                break;
            }
        }
        let Some(root) = catalog else { return Err(PdfError::NoRoot) };
        let root = doc.resolve(&root);
        doc.catalog = match root {
            Obj::Dict(d) => (*d).clone(),
            _ => Dict::new(),
        };
        Ok(doc)
    }

    fn find_xref(&self) -> Option<usize> {
        // revreadlines: lines from the end; the value is the line after "startxref" in reading order.
        let d = &self.data;
        let mut prev: Vec<u8> = Vec::new();
        let mut end = d.len();
        let mut lines: Vec<&[u8]> = Vec::new();
        let mut i = d.len();
        while i > 0 {
            i -= 1;
            if d[i] == b'\r' || d[i] == b'\n' {
                lines.push(&d[i..end]);
                end = i;
            }
        }
        lines.push(&d[0..end]);
        for line in lines {
            let l = trim(line);
            if l == b"startxref" {
                if prev.is_empty() || !prev.iter().all(|c| c.is_ascii_digit()) {
                    return None;
                }
                let start: u64 = std::str::from_utf8(&prev).ok()?.parse().ok()?;
                if start >= 1 << 31 {
                    return None;
                }
                return Some(start as usize);
            }
            if !l.is_empty() {
                prev = l.to_vec();
            }
        }
        None
    }

    fn read_xref_from(&self, start: usize, xrefs: &mut Vec<XRef>, seen: &mut HashSet<usize>) -> Result<(), ()> {
        if !seen.insert(start) {
            return Ok(());
        }
        let resolve = |id: u32| self.getobj(id).unwrap_or(Obj::Null);
        let mut p = FileParser::new(&self.data);
        p.resolve = Some(&resolve);
        p.seek(start);
        let (pos, tok) = p.next_token().ok_or(())?;
        let xref = if let Token::Int(_) = tok {
            p.seek(pos);
            p.next_token().ok_or(())?;
            p.next_token().ok_or(())?;
            p.next_token().ok_or(())?;
            let obj = p.next_object().ok_or(())?;
            let Obj::Stream(s) = obj else { return Err(()) };
            if !s.get("Type").map(|t| t.is_name("XRef")).unwrap_or(false) {
                return Err(());
            }
            let size = s.get("Size").and_then(safe_int).ok_or(())?;
            let index: Vec<i64> = match s.get("Index") {
                Some(Obj::Array(a)) => a.iter().filter_map(safe_int).collect(),
                _ => vec![0, size],
            };
            if index.len() % 2 != 0 {
                return Err(());
            }
            let ranges = index.chunks(2).map(|c| (c[0], c[1])).collect();
            let w: Vec<usize> = match s.get("W") {
                Some(Obj::Array(a)) if a.len() == 3 => a.iter().map(|x| safe_int(x).unwrap_or(0).max(0) as usize).collect(),
                _ => return Err(()),
            };
            let data = s.data(self).to_vec();
            XRef::Stream { data, w: (w[0], w[1], w[2]), ranges, trailer: s.dict.clone() }
        } else {
            if tok == Token::Keyword(b"xref".to_vec()) {
                p.next_line();
            }
            self.load_table(&mut p)?
        };
        let trailer = xref.trailer().clone();
        xrefs.push(xref);
        if let Some(pos) = trailer.get("XRefStm").and_then(|x| safe_int(&self.resolve(x))) {
            self.read_xref_from(pos as usize, xrefs, seen)?;
        }
        if let Some(pos) = trailer.get("Prev").and_then(|x| safe_int(&self.resolve(x))) {
            self.read_xref_from(pos as usize, xrefs, seen)?;
        }
        Ok(())
    }

    fn load_table(&self, p: &mut FileParser) -> Result<XRef, ()> {
        let mut offsets = HashMap::new();
        let mut order = Vec::new();
        loop {
            let (pos, line) = p.next_line().ok_or(())?;
            let line = trim(&line).to_vec();
            if line.is_empty() {
                continue;
            }
            if line.starts_with(b"trailer") {
                p.seek(pos);
                break;
            }
            let f: Vec<&[u8]> = line.split(|&c| c == b' ').collect();
            if f.len() != 2 {
                return Err(());
            }
            let start: i64 = parse_int(f[0]).ok_or(())?;
            let n: i64 = parse_int(f[1]).ok_or(())?;
            for objid in start..start + n {
                let (_, l) = p.next_line().ok_or(())?;
                let l = trim(&l).to_vec();
                let f: Vec<&[u8]> = l.split(|&c| c == b' ').collect();
                if f.len() != 3 {
                    return Err(());
                }
                if f[2] != b"n" {
                    continue;
                }
                if let (Some(pos), Some(gen)) = (parse_int(f[0]), parse_int(f[1])) {
                    if offsets.insert(objid as u32, (None, pos as usize, gen as u32)).is_none() {
                        order.push(objid as u32);
                    }
                }
            }
        }
        let trailer = self.load_trailer(p);
        Ok(XRef::Table { offsets, order, trailer })
    }

    fn load_trailer(&self, p: &mut FileParser) -> Dict {
        let _kwd = p.next_token();
        match p.next_object() {
            Some(Obj::Dict(d)) => (*d).clone(),
            Some(_) => Dict::new(),
            None => match p.core.pop(1).into_iter().next() {
                Some((_, Obj::Dict(d))) => (*d).clone(),
                _ => Dict::new(),
            },
        }
    }

    fn load_fallback(&self) -> XRef {
        let mut offsets: HashMap<u32, (Option<u32>, usize, u32)> = HashMap::new();
        let mut order = Vec::new();
        let mut trailer = Dict::new();
        let resolve = |id: u32| self.getobj(id).unwrap_or(Obj::Null);
        let mut p = FileParser::new(&self.data);
        p.resolve = Some(&resolve);
        p.fallback = true;
        p.seek(0);
        let re = regex::bytes::Regex::new(r"^(\d+)\s+(\d+)\s+obj\b").unwrap();
        while let Some((pos, line)) = p.next_line() {
            if line.starts_with(b"trailer") {
                p.seek(pos);
                trailer = self.load_trailer(&mut p);
                break;
            }
            let Some(m) = re.captures(&line) else { continue };
            let objid: u32 = std::str::from_utf8(&m[1]).unwrap().parse().unwrap_or(0);
            let genno: u32 = std::str::from_utf8(&m[2]).unwrap().parse().unwrap_or(0);
            if offsets.insert(objid, (None, pos, genno)).is_none() {
                order.push(objid);
            }
            p.seek(pos);
            let after = pos + line.len();
            if let Some(Obj::Stream(s)) = p.next_object() {
                if s.get("Type").map(|t| t.is_name("ObjStm")).unwrap_or(false) {
                    let n = s.get("N").and_then(safe_int).unwrap_or(0);
                    let objs = parse_object_stream(s.data(self));
                    let n = n.min(objs.len() as i64 / 2).max(0);
                    for index in 0..n as usize {
                        if let Some(id) = safe_int(&objs[index * 2]) {
                            if offsets.insert(id as u32, (Some(objid), index, 0)).is_none() {
                                order.push(id as u32);
                            }
                        }
                    }
                }
            }
            // nextline continues from where the object ended; pdfminer reads lines from the parser's position.
            let _ = after;
        }
        XRef::Table { offsets, order, trailer }
    }

    pub fn getobj(&self, objid: u32) -> Option<Obj> {
        if let Some(o) = self.cache.borrow().get(&objid) {
            return Some(o.clone());
        }
        if !self.resolving.borrow_mut().insert(objid) {
            return None; // a reference loop: give up on this object
        }
        let mut result = None;
        for x in &self.xrefs {
            let Some((strmid, index, genno)) = x.get_pos(objid) else { continue };
            let obj = match strmid {
                Some(sid) => {
                    let s = match self.getobj(sid).map(|o| self.resolve(&o)) {
                        Some(Obj::Stream(s)) => s,
                        _ => continue,
                    };
                    self.objstm_get(sid, &s, index)
                }
                None => self.parse_at(index, objid),
            };
            if let Some(mut o) = obj {
                if let Obj::Stream(s) = &o {
                    let st = Stream::new(s.dict.clone(), s.raw.clone()).with_id(objid, genno);
                    o = Obj::Stream(Rc::new(st));
                }
                result = Some(o);
                break;
            }
        }
        self.resolving.borrow_mut().remove(&objid);
        let r = result?;
        self.cache.borrow_mut().insert(objid, r.clone());
        Some(r)
    }

    fn objstm_get(&self, sid: u32, s: &Rc<Stream>, index: usize) -> Option<Obj> {
        let parsed = {
            let cached = self.objstm.borrow().get(&sid).cloned();
            match cached {
                Some(p) => p,
                None => {
                    let n = s.get("N").and_then(safe_int).unwrap_or(0);
                    let objs = parse_object_stream(s.data(self));
                    let p = Rc::new((objs, n));
                    self.objstm.borrow_mut().insert(sid, p.clone());
                    p
                }
            }
        };
        let i = parsed.1 as usize * 2 + index;
        parsed.0.get(i).cloned()
    }

    fn parse_at(&self, pos: usize, objid: u32) -> Option<Obj> {
        let resolve = |id: u32| self.getobj(id).unwrap_or(Obj::Null);
        let mut p = FileParser::new(&self.data);
        p.resolve = Some(&resolve);
        p.seek(pos);
        let (_, t1) = p.next_token()?;
        let (_, _t2) = p.next_token()?;
        let (_, mut kwd) = p.next_token()?;
        let mut id1 = match t1 {
            Token::Int(i) => Some(i),
            _ => None,
        };
        if id1 != Some(objid as i64) {
            let mut x = vec![];
            while kwd != Token::Keyword(b"obj".to_vec()) {
                let (_, k) = p.next_token()?;
                kwd = k.clone();
                x.push(k);
            }
            if x.len() >= 2 {
                id1 = match &x[x.len() - 2] {
                    Token::Int(i) => Some(*i),
                    _ => None,
                };
            }
        }
        if id1 != Some(objid as i64) || kwd != Token::Keyword(b"obj".to_vec()) {
            return None;
        }
        p.next_object()
    }

    /// The pages, depth first through the page tree, with inherited attributes (`PDFPage.create_pages`).
    pub fn pages(&self) -> Vec<(Option<u32>, Dict)> {
        let mut out = Vec::new();
        if let Some(pages) = self.catalog.get("Pages").cloned() {
            let mut visited = HashSet::new();
            self.dfs(&pages, &self.catalog.clone(), &mut visited, &mut out);
        }
        if out.is_empty() {
            for x in &self.xrefs {
                for id in x.objids() {
                    if let Some(Obj::Dict(d)) = self.getobj(id) {
                        if d.get("Type").map(|t| t.is_name("Page")).unwrap_or(false) {
                            out.push((Some(id), (*d).clone()));
                        }
                    }
                }
            }
        }
        out
    }

    fn dfs(&self, obj: &Obj, parent: &Dict, visited: &mut HashSet<Option<u32>>, out: &mut Vec<(Option<u32>, Dict)>) {
        let (id, props) = match obj {
            Obj::Ref(id) => match self.getobj(*id).map(|o| self.resolve(&o)) {
                Some(Obj::Dict(d)) => (Some(*id), (*d).clone()),
                _ => (Some(*id), Dict::new()),
            },
            Obj::Int(i) => match self.getobj(*i as u32).map(|o| self.resolve(&o)) {
                Some(Obj::Dict(d)) => (Some(*i as u32), (*d).clone()),
                _ => (Some(*i as u32), Dict::new()),
            },
            Obj::Dict(d) => (None, (**d).clone()),
            _ => (None, Dict::new()),
        };
        if !visited.insert(id) {
            return;
        }
        let mut props = props;
        for (k, v) in parent {
            if ["Resources", "MediaBox", "CropBox", "Rotate"].contains(&k.as_str()) && !props.contains_key(k) {
                props.insert(k.clone(), v.clone());
            }
        }
        let t = props.get("Type").or_else(|| props.get("type")).cloned();
        if t.as_ref().map(|t| t.is_name("Pages")).unwrap_or(false) && props.contains_key("Kids") {
            let kids = match self.resolve(props.get("Kids").unwrap()) {
                Obj::Array(a) => a,
                _ => vec![],
            };
            for k in kids {
                self.dfs(&k, &props, visited, out);
            }
        } else if t.as_ref().map(|t| t.is_name("Page")).unwrap_or(false) {
            out.push((id, props));
        }
    }
}

fn trim(b: &[u8]) -> &[u8] {
    let mut s = 0;
    let mut e = b.len();
    while s < e && super::lexer::is_space(b[s]) {
        s += 1;
    }
    while e > s && super::lexer::is_space(b[e - 1]) {
        e -= 1;
    }
    &b[s..e]
}

fn parse_int(b: &[u8]) -> Option<i64> {
    std::str::from_utf8(b).ok()?.trim().parse().ok()
}
