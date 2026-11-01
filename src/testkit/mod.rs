#![allow(dead_code)]
//! What the tests share: a small PDF writer, the synthetic documents (ported from the Python tests' fixtures) and a
//! few helpers. Only compiled for tests.

pub mod cloudflare;
pub mod env;
pub mod fixtures;
pub mod fortuneo;
pub mod hellobank;
pub mod loan;
pub mod pdf;

use crate::adapters::base::{ParsedStatement, SourceDocument};
use crate::adapters::registry::{identify, parse_with};

/// A PDF of rows of (x, text) blocks in millimetres, like the generic tests of the Python version draw them.
pub fn rows_pdf(rows: &[Vec<(f64, &str)>]) -> Vec<u8> {
    let mut pdf = pdf::Pdf::new("");
    let mm = 72.0 / 25.4;
    let mut y = 20.0;
    for row in rows {
        for (x, text) in row {
            pdf.text(x * mm, y * mm, text);
        }
        y += 6.0;
    }
    pdf.bytes()
}

/// What the app does: pick the adapter by itself, then read.
pub fn parse_document(filename: &str, data: Vec<u8>) -> Vec<ParsedStatement> {
    let doc = SourceDocument::new(filename, data);
    let found = identify(&doc);
    let chosen = found.chosen.as_ref().unwrap_or_else(|| panic!("{}", found.message));
    parse_with(chosen.adapter.id(), &doc).unwrap()
}

/// The single statement an adapter reads from a document.
pub fn parse_one(adapter: &str, filename: &str, data: Vec<u8>) -> ParsedStatement {
    let doc = SourceDocument::new(filename, data);
    let mut statements = parse_with(adapter, &doc).unwrap();
    assert_eq!(statements.len(), 1);
    statements.remove(0)
}

/// The Windows-1252 byte of a character (for the French CSV exports).
pub fn cp1252_byte(c: char) -> u8 {
    pdf::cp1252(c)
}

use crate::app::MyMoney;
use crate::error::AppResult;
use crate::services::statements::Form;

/// A form of the core API (`{"account_id": "new", ...}`).
pub fn form(pairs: &[(&str, &str)]) -> Form {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// `with core.db.session() as s:` — run `f` on a connection, panicking on a database error.
pub fn db<T>(core: &MyMoney, f: impl FnOnce(&rusqlite::Connection) -> AppResult<T>) -> T {
    core.session(f).unwrap()
}

/// An integer of a result map (`result["account_id"]`).
pub fn num(result: &serde_json::Map<String, serde_json::Value>, key: &str) -> i64 {
    result[key].as_i64().unwrap_or_else(|| panic!("{key} is not an integer in {result:?}"))
}

/// What `core.store_document` + `core.analyze_document` do for a freshly dropped file.
pub fn drop_file(core: &MyMoney, name: &str, data: &[u8]) -> i64 {
    let (id, _) = core.store_document(name, data).unwrap();
    core.analyze_document(id, None, Some(data.to_vec())).unwrap();
    id
}

/// The (name, content) pairs of a zip archive (stored or deflated entries), read from its central directory.
pub fn unzip(data: &[u8]) -> Vec<(String, Vec<u8>)> {
    use std::io::Read;
    let u16_at = |i: usize| u16::from_le_bytes([data[i], data[i + 1]]) as usize;
    let u32_at = |i: usize| u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize;
    let end = (0..data.len().saturating_sub(21)).rev().find(|&i| &data[i..i + 4] == b"PK\x05\x06").expect("end of central directory");
    let (count, mut at) = (u16_at(end + 10), u32_at(end + 16));
    let mut out = vec![];
    for _ in 0..count {
        assert_eq!(&data[at..at + 4], b"PK\x01\x02");
        let (method, size, name_len, extra_len, comment_len, local) = (u16_at(at + 10), u32_at(at + 20), u16_at(at + 28), u16_at(at + 30), u16_at(at + 32), u32_at(at + 42));
        let name = String::from_utf8_lossy(&data[at + 46..at + 46 + name_len]).into_owned();
        let start = local + 30 + u16_at(local + 26) + u16_at(local + 28);
        let raw = &data[start..start + size];
        let content = if method == 8 {
            let mut v = vec![];
            flate2::read::DeflateDecoder::new(raw).read_to_end(&mut v).unwrap();
            v
        } else {
            raw.to_vec()
        };
        out.push((name, content));
        at += 46 + name_len + extra_len + comment_len;
    }
    out
}

/// A decoded 8-bit RGBA PNG (the icons are drawn that way).
pub struct Png {
    pub width: usize,
    pub height: usize,
    pixels: Vec<u8>,
}

impl Png {
    pub fn pixel(&self, x: usize, y: usize) -> [u8; 4] {
        let i = (y * self.width + x) * 4;
        [self.pixels[i], self.pixels[i + 1], self.pixels[i + 2], self.pixels[i + 3]]
    }
}

pub fn decode_png(data: &[u8]) -> Png {
    use std::io::Read;
    assert_eq!(&data[..8], b"\x89PNG\r\n\x1a\n", "not a PNG");
    let (mut at, mut width, mut height, mut idat) = (8, 0usize, 0usize, vec![]);
    while at + 8 <= data.len() {
        let len = u32::from_be_bytes(data[at..at + 4].try_into().unwrap()) as usize;
        let kind = &data[at + 4..at + 8];
        let body = &data[at + 8..at + 8 + len];
        match kind {
            b"IHDR" => {
                width = u32::from_be_bytes(body[0..4].try_into().unwrap()) as usize;
                height = u32::from_be_bytes(body[4..8].try_into().unwrap()) as usize;
                assert_eq!((body[8], body[9], body[12]), (8, 6, 0), "only 8-bit RGBA, not interlaced");
            }
            b"IDAT" => idat.extend(body),
            _ => {}
        }
        at += 12 + len;
    }
    let mut raw = vec![];
    flate2::read::ZlibDecoder::new(&idat[..]).read_to_end(&mut raw).unwrap();
    let stride = width * 4;
    let mut pixels = vec![0u8; stride * height];
    for y in 0..height {
        let filter = raw[y * (stride + 1)];
        let line = &raw[y * (stride + 1) + 1..(y + 1) * (stride + 1)];
        for x in 0..stride {
            let a = if x >= 4 { pixels[y * stride + x - 4] as i32 } else { 0 };
            let b = if y > 0 { pixels[(y - 1) * stride + x] as i32 } else { 0 };
            let c = if x >= 4 && y > 0 { pixels[(y - 1) * stride + x - 4] as i32 } else { 0 };
            let predicted = match filter {
                0 => 0,
                1 => a,
                2 => b,
                3 => (a + b) / 2,
                4 => {
                    let p = a + b - c;
                    let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
                    if pa <= pb && pa <= pc {
                        a
                    } else if pb <= pc {
                        b
                    } else {
                        c
                    }
                }
                other => panic!("PNG filter {other}"),
            };
            pixels[y * stride + x] = (line[x] as i32 + predicted) as u8;
        }
    }
    Png { width, height, pixels }
}
