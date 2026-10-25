//! PDF objects as pdfminer sees them, and the stream filters it applies.

use std::cell::OnceCell;
use std::io::Read;
use std::rc::Rc;

use super::lexer::Name;

pub type Dict = indexmap::IndexMap<String, Obj>;

#[derive(Clone, Debug)]
pub enum Obj {
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    Name(Name),
    Str(Vec<u8>),
    Array(Vec<Obj>),
    Dict(Rc<Dict>),
    Stream(Rc<Stream>),
    Ref(u32),
    /// A keyword left on a stack (content operators are dispatched before they get here).
    Keyword(Vec<u8>),
}

#[derive(Debug)]
pub struct Stream {
    pub dict: Dict,
    pub raw: Vec<u8>,
    pub objid: Option<u32>,
    pub genno: u32,
    /// The document's security handler, for a stream read once the document was found to be encrypted.
    pub crypt: Option<Rc<super::crypt::Security>>,
    data: OnceCell<Vec<u8>>,
}

impl Stream {
    pub fn new(dict: Dict, raw: Vec<u8>) -> Self {
        Stream { dict, raw, objid: None, genno: 0, crypt: None, data: OnceCell::new() }
    }
    pub fn with_id(mut self, objid: u32, genno: u32) -> Self {
        self.objid = Some(objid);
        self.genno = genno;
        self
    }
    pub fn with_crypt(mut self, crypt: Option<Rc<super::crypt::Security>>) -> Self {
        self.crypt = crypt;
        self
    }
    pub fn get(&self, key: &str) -> Option<&Obj> {
        self.dict.get(key)
    }
}

impl Obj {
    pub fn is_name(&self, n: &str) -> bool {
        matches!(self, Obj::Name(Name::Text(s)) if s == n)
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Obj::Int(i) => Some(*i as f64),
            Obj::Real(f) => Some(*f),
            Obj::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            _ => None,
        }
    }
}

/// `literal_name()`: the name of a literal, or Python's `str()` of anything else.
pub fn literal_name(o: &Obj) -> String {
    match o {
        Obj::Name(n) => n.as_str(),
        other => python_str(other),
    }
}

/// Close enough to Python's `str()` for the rare cases where a non-name is used as a name.
pub fn python_str(o: &Obj) -> String {
    match o {
        Obj::Null => "None".into(),
        Obj::Bool(b) => if *b { "True".into() } else { "False".into() },
        Obj::Int(i) => i.to_string(),
        Obj::Real(f) => python_float_repr(*f),
        Obj::Name(n) => format!("/{:?}", n.as_str()),
        Obj::Str(b) => super::lexer::python_bytes_repr(b),
        Obj::Keyword(k) => format!("/{}", super::lexer::python_bytes_repr(k)),
        Obj::Ref(id) => format!("<PDFObjRef:{}>", id),
        _ => String::new(),
    }
}

/// Python's `repr(float)`: shortest round-trip form, with ".0" for integral values.
pub fn python_float_repr(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let abs = f.abs();
    if abs != 0.0 && (abs >= 1e16 || abs < 1e-4) {
        // Exponent form like Python: "1e-05", "1.5e+16".
        let s = format!("{:e}", f);
        let (mant, exp) = s.split_once('e').unwrap();
        let e: i32 = exp.parse().unwrap();
        return format!("{}e{}{:02}", mant, if e < 0 { '-' } else { '+' }, e.abs());
    }
    let s = format!("{}", f);
    if s.contains('.') || s.contains("inf") {
        s
    } else {
        format!("{}.0", s)
    }
}

// ------------------------------------------------------------------------------------------- filters

fn zlib_decompress(data: &[u8]) -> Result<Vec<u8>, ()> {
    let mut out = Vec::new();
    let mut d = flate2::read::ZlibDecoder::new(data);
    match d.read_to_end(&mut out) {
        Ok(_) => Ok(out),
        Err(_) => Err(()),
    }
}

/// pdfminer's `decompress_corrupted`: feed the data byte by byte, keep what came out before the error, unless
/// the error happens before the last 3 bytes (then the whole decoding fails).
fn decompress_corrupted(data: &[u8]) -> Result<Vec<u8>, ()> {
    let mut d = flate2::Decompress::new(true);
    let mut out: Vec<u8> = Vec::new();
    let mut buf = vec![0u8; 65536];
    let mut i = 0usize;
    while i < data.len() {
        let input = &data[i..i + 1];
        loop {
            let before_in = d.total_in();
            let before_out = d.total_out();
            match d.decompress(input, &mut buf, flate2::FlushDecompress::None) {
                Ok(status) => {
                    let produced = (d.total_out() - before_out) as usize;
                    out.extend_from_slice(&buf[..produced]);
                    let consumed = (d.total_in() - before_in) as usize;
                    if status == flate2::Status::StreamEnd {
                        // Python keeps going but anything after the end is "unused data": nothing more comes out.
                        return Ok(out);
                    }
                    if consumed >= 1 || produced == 0 {
                        break;
                    }
                }
                Err(_) => {
                    if (i as isize) < data.len() as isize - 3 {
                        return Err(());
                    }
                    return Ok(out);
                }
            }
        }
        i += 1;
    }
    Ok(out)
}

fn ascii85_decode(data: &[u8]) -> Vec<u8> {
    // pdfminer.ascii85.ascii85decode
    let mut n: u64 = 0;
    let mut b: u32 = 0;
    let mut out = Vec::new();
    for &c in data {
        if (b'!'..=b'u').contains(&c) {
            n += 1;
            b = b.wrapping_mul(85).wrapping_add((c - 33) as u32);
            if n == 5 {
                out.extend_from_slice(&b.to_be_bytes());
                n = 0;
                b = 0;
            }
        } else if c == b'z' {
            assert!(n == 0);
            out.extend_from_slice(&[0, 0, 0, 0]);
        } else if c == b'~' {
            if n > 0 {
                for _ in 0..(5 - n) {
                    b = b.wrapping_mul(85).wrapping_add(84);
                }
                let bytes = b.to_be_bytes();
                out.extend_from_slice(&bytes[..(n as usize - 1)]);
            }
            break;
        }
    }
    out
}

fn asciihex_decode(data: &[u8]) -> Vec<u8> {
    // pdfminer.ascii85.asciihexdecode: pairs of hex digits (spaces allowed), a lone last digit counts as x0.
    let mut out = Vec::new();
    let mut digits: Vec<u8> = Vec::new();
    for &c in data {
        if c == b'>' {
            break;
        }
        if c.is_ascii_hexdigit() {
            digits.push(c);
        } else if super::lexer::is_space(c) {
            continue;
        } else {
            // the regular expression only matches hex pairs: other bytes break pairs
            digits.push(c);
        }
    }
    let mut k = 0;
    let hexval = |c: u8| (c as char).to_digit(16);
    while k + 1 < digits.len() {
        if let (Some(a), Some(b)) = (hexval(digits[k]), hexval(digits[k + 1])) {
            out.push((a * 16 + b) as u8);
            k += 2;
        } else {
            k += 1;
        }
    }
    if k < digits.len() {
        if let Some(a) = hexval(digits[k]) {
            out.push((a * 16) as u8);
        }
    }
    out
}

fn rl_decode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let len = data[i];
        if len == 128 {
            break;
        }
        if len < 128 {
            let run = len as usize + 1;
            let end = (i + 1 + run).min(data.len());
            out.extend_from_slice(&data[i + 1..end]);
            i += run + 1;
        } else {
            let run = 257 - len as usize;
            if i + 1 < data.len() {
                out.extend(std::iter::repeat_n(data[i + 1], run));
            }
            i += 2;
        }
    }
    out
}

fn lzw_decode(data: &[u8]) -> Vec<u8> {
    // pdfminer.lzw.LZWDecoder (early change).
    let mut out = Vec::new();
    let mut table: Vec<Vec<u8>> = Vec::new();
    let mut prevbuf: Option<Vec<u8>> = None;
    let mut nbits = 9u32;
    let mut bitbuf: u64 = 0;
    let mut bitcount = 0u32;
    let mut pos = 0usize;
    let reset = |table: &mut Vec<Vec<u8>>| {
        table.clear();
        for i in 0..256u32 {
            table.push(vec![i as u8]);
        }
        table.push(Vec::new());
        table.push(Vec::new());
    };
    reset(&mut table);
    loop {
        while bitcount < nbits {
            if pos >= data.len() {
                return out;
            }
            bitbuf = (bitbuf << 8) | data[pos] as u64;
            pos += 1;
            bitcount += 8;
        }
        let code = ((bitbuf >> (bitcount - nbits)) & ((1 << nbits) - 1)) as usize;
        bitcount -= nbits;
        if code == 256 {
            reset(&mut table);
            prevbuf = None;
            nbits = 9;
            continue;
        }
        if code == 257 {
            break;
        }
        let x: Vec<u8>;
        match &prevbuf {
            None => {
                x = table.get(code).cloned().unwrap_or_default();
            }
            Some(prev) => {
                if code < table.len() {
                    x = table[code].clone();
                    let mut entry = prev.clone();
                    if let Some(&c) = x.first() {
                        entry.push(c);
                    }
                    table.push(entry);
                } else {
                    let mut e = prev.clone();
                    if let Some(&c) = prev.first() {
                        e.push(c);
                    }
                    table.push(e.clone());
                    x = e;
                }
                let l = table.len();
                if l == 511 {
                    nbits = 10;
                } else if l == 1023 {
                    nbits = 11;
                } else if l == 2047 {
                    nbits = 12;
                }
            }
        }
        out.extend_from_slice(&x);
        prevbuf = Some(x);
    }
    out
}

fn png_predictor(pred: i64, colors: i64, columns: i64, bpc: i64, data: &[u8]) -> Vec<u8> {
    let bpp = ((colors * bpc + 7) / 8).max(1) as usize;
    let nbytes = ((colors * columns * bpc + 7) / 8) as usize;
    let _ = pred;
    let mut out = Vec::new();
    let mut prev = vec![0u8; nbytes];
    let mut i = 0;
    while i + nbytes < data.len() + 1 && i < data.len() {
        let ft = data[i];
        let end = (i + 1 + nbytes).min(data.len());
        let line = &data[i + 1..end];
        let mut cur = vec![0u8; line.len()];
        for j in 0..line.len() {
            let left = if j >= bpp { cur[j - bpp] } else { 0 };
            let up = prev.get(j).copied().unwrap_or(0);
            let ul = if j >= bpp { prev.get(j - bpp).copied().unwrap_or(0) } else { 0 };
            cur[j] = match ft {
                0 => line[j],
                1 => line[j].wrapping_add(left),
                2 => line[j].wrapping_add(up),
                3 => line[j].wrapping_add(((left as u16 + up as u16) / 2) as u8),
                4 => {
                    let p = left as i32 + up as i32 - ul as i32;
                    let pa = (p - left as i32).abs();
                    let pb = (p - up as i32).abs();
                    let pc = (p - ul as i32).abs();
                    let pr = if pa <= pb && pa <= pc { left } else if pb <= pc { up } else { ul };
                    line[j].wrapping_add(pr)
                }
                _ => line[j],
            };
        }
        out.extend_from_slice(&cur);
        prev = cur;
        prev.resize(nbytes, 0);
        i += nbytes + 1;
    }
    out
}

fn tiff_predictor(colors: i64, columns: i64, bpc: i64, data: &[u8]) -> Vec<u8> {
    if bpc != 8 {
        return data.to_vec();
    }
    let bpp = colors as usize;
    let nbytes = (colors * columns) as usize;
    let mut out = Vec::new();
    for line in data.chunks(nbytes.max(1)) {
        let mut cur = line.to_vec();
        for i in bpp..cur.len() {
            cur[i] = cur[i].wrapping_add(cur[i - bpp]);
        }
        out.extend_from_slice(&cur);
    }
    out
}

pub trait Resolver {
    fn resolve(&self, o: &Obj) -> Obj;
}

impl Stream {
    /// The decoded data (`PDFStream.get_data()`), computed once.
    pub fn data(&self, r: &dyn Resolver) -> &[u8] {
        self.data.get_or_init(|| self.decode(r))
    }

    fn decode(&self, r: &dyn Resolver) -> Vec<u8> {
        let raw: std::borrow::Cow<[u8]> = match (&self.crypt, self.objid) {
            (Some(c), Some(id)) => std::borrow::Cow::Owned(c.decrypt(id, self.genno, &self.raw, Some(&self.dict))),
            _ => std::borrow::Cow::Borrowed(&self.raw),
        };
        let get_any = |keys: &[&str]| keys.iter().find_map(|k| self.dict.get(*k)).cloned();
        let filters = get_any(&["F", "Filter"]).map(|f| r.resolve(&f));
        let params = get_any(&["DP", "DecodeParms", "FDecodeParms"]).map(|p| r.resolve(&p));
        let filters: Vec<Obj> = match filters {
            None | Some(Obj::Null) => return raw.into_owned(),
            Some(Obj::Array(a)) if a.is_empty() => return raw.into_owned(),
            Some(Obj::Array(a)) => a.iter().map(|x| r.resolve(x)).collect(),
            Some(other) => vec![other],
        };
        let params: Vec<Obj> = match params {
            Some(Obj::Array(a)) => a.iter().map(|x| r.resolve(x)).collect(),
            Some(p) => vec![p; filters.len()],
            None => vec![Obj::Dict(Rc::new(Dict::new())); filters.len()],
        };
        let mut data = raw.into_owned();
        for (k, f) in filters.iter().enumerate() {
            let name = literal_name(f);
            match name.as_str() {
                "FlateDecode" | "Fl" => {
                    data = match zlib_decompress(&data) {
                        Ok(d) => d,
                        Err(_) => decompress_corrupted(&data).unwrap_or_default(),
                    }
                }
                "LZWDecode" | "LZW" => data = lzw_decode(&data),
                "ASCII85Decode" | "A85" => data = ascii85_decode(&data),
                "ASCIIHexDecode" | "AHx" => data = asciihex_decode(&data),
                "RunLengthDecode" | "RL" => data = rl_decode(&data),
                "CCITTFaxDecode" | "CCF" | "DCTDecode" | "DCT" | "JBIG2Decode" | "JPXDecode" => {}
                _ => return Vec::new(),
            }
            if let Some(Obj::Dict(p)) = params.get(k) {
                if let Some(pred) = p.get("Predictor").map(|x| r.resolve(x)) {
                    let pred = if let Obj::Int(i) = pred { i } else { 0 };
                    let iv = |key: &str, d: i64| match p.get(key).map(|x| r.resolve(x)) {
                        Some(Obj::Int(i)) => i,
                        Some(_) => 0,
                        None => d,
                    };
                    let colors = iv("Colors", 1);
                    let columns = iv("Columns", 1);
                    let bpc = iv("BitsPerComponent", 8);
                    if pred == 2 {
                        data = tiff_predictor(colors, columns, bpc, &data);
                    } else if pred >= 10 {
                        data = png_predictor(pred, colors, columns, bpc, &data);
                    }
                }
            }
        }
        data
    }
}
