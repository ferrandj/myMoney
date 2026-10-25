//! Fallbacks for documents no institution-specific adapter knows.
//!
//! OFX and QIF are standards, so reading them is not a guess. CSV and PDF are: columns and layouts are guessed from
//! the content, which is why these two never run without the user seeing (and confirming) what was understood.

pub mod csv_parser;
pub mod htmlsoup;
pub mod ofx_parser;
pub mod ofx_tables;
pub mod pdf_parser;
pub mod pycodecs;
pub mod pycsv;

use crate::adapters::base::{Adapter, Detection, ParseResult, SourceDocument};
use crate::adapters::common::norm;
use crate::adapters::re::search;
use crate::py;

pub struct GenericOfx;
pub struct GenericQif;
pub struct GenericCsv;
pub struct GenericPdf;

fn detection(score: f64, why: &str) -> Detection {
    Detection { score, reasons: vec![why.to_string()] }
}

impl Adapter for GenericOfx {
    fn id(&self) -> &'static str {
        "generic.ofx"
    }
    fn institution(&self) -> Option<&'static str> {
        None
    }
    fn kind(&self) -> &'static str {
        "ofx"
    }
    fn label(&self) -> &'static str {
        "Fichier OFX / QFX (standard bancaire)"
    }
    fn description(&self) -> &'static str {
        "Export OFX ou QFX proposé par la plupart des banques."
    }
    fn formats(&self) -> &'static [&'static str] {
        &["ofx"]
    }
    fn detect(&self, doc: &SourceDocument) -> Result<Detection, String> {
        Ok(if doc.format() == "ofx" { detection(0.9, "fichier OFX") } else { Detection::default() })
    }
    fn parse(&self, doc: &SourceDocument) -> ParseResult {
        ofx_parser::parse_ofx(&doc.data, &doc.filename)
    }
}

impl Adapter for GenericQif {
    fn id(&self) -> &'static str {
        "generic.qif"
    }
    fn institution(&self) -> Option<&'static str> {
        None
    }
    fn kind(&self) -> &'static str {
        "qif"
    }
    fn label(&self) -> &'static str {
        "Fichier QIF"
    }
    fn description(&self) -> &'static str {
        "Ancien format d'export Quicken."
    }
    fn formats(&self) -> &'static [&'static str] {
        &["qif"]
    }
    fn detect(&self, doc: &SourceDocument) -> Result<Detection, String> {
        Ok(if doc.format() == "qif" { detection(0.9, "fichier QIF") } else { Detection::default() })
    }
    fn parse(&self, doc: &SourceDocument) -> ParseResult {
        ofx_parser::parse_qif(&doc.data, &doc.filename)
    }
}

impl Adapter for GenericCsv {
    fn id(&self) -> &'static str {
        "generic.csv"
    }
    fn institution(&self) -> Option<&'static str> {
        None
    }
    fn kind(&self) -> &'static str {
        "csv"
    }
    fn label(&self) -> &'static str {
        "CSV générique (colonnes devinées)"
    }
    fn description(&self) -> &'static str {
        "Cherche une colonne date et une colonne montant (ou débit / crédit), ou une liste de positions."
    }
    fn formats(&self) -> &'static [&'static str] {
        &["csv"]
    }
    fn generic(&self) -> bool {
        true
    }
    fn detect(&self, doc: &SourceDocument) -> Result<Detection, String> {
        if doc.format() != "csv" {
            return Ok(Detection::default());
        }
        Ok(if csv_parser::looks_like_statement(&doc.data)? {
            detection(0.5, "colonnes date et montant reconnues")
        } else {
            detection(0.1, "fichier texte, colonnes non reconnues")
        })
    }
    fn parse(&self, doc: &SourceDocument) -> ParseResult {
        csv_parser::parse_csv(&doc.data, &doc.filename)
    }
}

impl Adapter for GenericPdf {
    fn id(&self) -> &'static str {
        "generic.pdf"
    }
    fn institution(&self) -> Option<&'static str> {
        None
    }
    fn kind(&self) -> &'static str {
        "pdf"
    }
    fn label(&self) -> &'static str {
        "PDF générique (colonnes devinées)"
    }
    fn description(&self) -> &'static str {
        "Cherche des lignes datées avec des colonnes débit / crédit, ou des positions avec leur code ISIN."
    }
    fn formats(&self) -> &'static [&'static str] {
        &["pdf"]
    }
    fn generic(&self) -> bool {
        true
    }
    fn detect(&self, doc: &SourceDocument) -> Result<Detection, String> {
        if doc.format() != "pdf" || py::strip(doc.text()?).is_empty() {
            return Ok(Detection::default());
        }
        let text = norm(doc.text()?);
        Ok(if text.contains("debit") && text.contains("credit") && search(r"\d{2}/\d{2}", &text).is_some() {
            detection(0.4, "colonnes débit et crédit trouvées")
        } else {
            detection(0.3, "PDF avec du texte")
        })
    }
    fn parse(&self, doc: &SourceDocument) -> ParseResult {
        pdf_parser::parse_pdf(&doc.data, &doc.filename)
    }
}
