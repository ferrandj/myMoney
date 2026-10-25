//! Readers for the documents banks hand out: each adapter recognises one kind of document and turns it into
//! parsed statements.

pub mod base;
pub mod boursobank;
pub mod common;
pub mod fortuneo;
pub mod generic;
pub mod lines;
pub mod pdftools;
pub mod re;
pub mod registry;
pub mod revolut;

pub use base::{Adapter, Detection, HoldingData, ParsedStatement, SourceDocument, TransactionData};
