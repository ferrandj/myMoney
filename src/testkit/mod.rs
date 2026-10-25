//! What the tests share: a small PDF writer, the synthetic documents (ported from the Python tests' fixtures) and a
//! few helpers. Only compiled for tests.

pub mod env;
pub mod fixtures;
pub mod fortuneo;
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
