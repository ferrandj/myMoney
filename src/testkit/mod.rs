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
