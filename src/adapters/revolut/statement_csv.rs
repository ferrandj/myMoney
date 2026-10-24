//! Revolut: the CSV export of an account ("Account statement" in the app).

use crate::adapters::base::{Adapter, Detection, ParseResult, SourceDocument};
use crate::adapters::common::norm;
use crate::adapters::generic::{csv_parser, pycsv};
use crate::py;

const EXPECTED: &[&str] = &["type", "product", "started date", "completed date", "description", "amount", "fee", "currency", "state", "balance"];

pub struct RevolutStatementCsv;

impl Adapter for RevolutStatementCsv {
    fn id(&self) -> &'static str {
        "revolut.statement_csv"
    }
    fn institution(&self) -> Option<&'static str> {
        Some("Revolut")
    }
    fn kind(&self) -> &'static str {
        "statement"
    }
    fn label(&self) -> &'static str {
        "Revolut · relevé de compte (CSV)"
    }
    fn description(&self) -> &'static str {
        "L'export CSV d'un compte Revolut (colonnes Type, Product, Started Date, Completed Date…)."
    }
    fn formats(&self) -> &'static [&'static str] {
        &["csv"]
    }

    fn detect(&self, doc: &SourceDocument) -> Result<Detection, String> {
        if doc.format() != "csv" {
            return Ok(Detection::default());
        }
        let text = doc.text()?.trim_start_matches('\u{feff}');
        let Some(first) = py::splitlines(text).into_iter().next() else {
            return Ok(Detection::default());
        };
        let header: Vec<String> = pycsv::reader(first, ',')?.into_iter().next().unwrap_or_default().iter().map(|c| norm(c)).collect();
        let found = EXPECTED.iter().filter(|e| header.iter().any(|h| h == *e)).count();
        if found >= 8 {
            return Ok(Detection { score: 0.95, reasons: vec![format!("{found} colonnes sur {} du relevé Revolut", EXPECTED.len())] });
        }
        Ok(Detection::default())
    }

    fn parse(&self, doc: &SourceDocument) -> ParseResult {
        let mut statements = csv_parser::parse_csv(&doc.data, &doc.filename)?;
        for st in statements.iter_mut() {
            st.institution = Some("Revolut".into());
        }
        Ok(statements)
    }
}
