//! Fortuneo: the yearly letter about the deposit guarantee (FGDR).
//!
//! A document to keep, not to import: nothing in it changes a balance. It is recognised, dated and filed with the
//! holder's other papers (encrypted, downloadable), and the ceiling it announces is kept.

use chrono::Datelike;

use super::identity::read_address_block;
use crate::adapters::base::{round2, score, Adapter, Detection, ParseResult, ParsedStatement, SourceDocument};
use crate::adapters::common::{french_date, norm};
use crate::adapters::pdftools;
use crate::adapters::re::search;
use crate::py;

const CEILING_RE: &str = r"(\d[\d\s\x{a0}\x{202f}]*\d)\s*€\s*par\s*d[ée]posant";

pub struct FortuneoDepositGuaranteePdf;

/// `f"{n:,}".replace(",", " ")`.
pub fn group_thousands(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(' ');
        }
        out.push(c);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

impl Adapter for FortuneoDepositGuaranteePdf {
    fn id(&self) -> &'static str {
        "fortuneo.deposit_guarantee_pdf"
    }
    fn institution(&self) -> Option<&'static str> {
        Some("Fortuneo")
    }
    fn kind(&self) -> &'static str {
        "info"
    }
    fn label(&self) -> &'static str {
        "Fortuneo · information sur la garantie des dépôts (PDF)"
    }
    fn description(&self) -> &'static str {
        "La lettre annuelle « Informations générales sur la protection des dépôts » (FGDR), à classer avec tes papiers."
    }
    fn formats(&self) -> &'static [&'static str] {
        &["pdf"]
    }

    fn detect(&self, doc: &SourceDocument) -> Result<Detection, String> {
        if doc.format() != "pdf" {
            return Ok(Detection::default());
        }
        let c = doc.compact()?;
        let (mut s, reasons) = score(&[
            (c.contains("fortuneo"), 0.25, "mentionne Fortuneo"),
            (c.contains("informationsgeneralessurlaprotectiondesdepots"), 0.4, "« Informations générales sur la protection des dépôts »"),
            (c.contains("garantiedesdepotsetderesolution") || c.contains("fgdr"), 0.2, "mentionne le FGDR"),
            (c.contains("plafonddelaprotection"), 0.15, "« Plafond de la protection »"),
        ]);
        if !c.contains("fortuneo") {
            s = s.min(0.3);
        }
        Ok(Detection { score: round2(s.min(1.0)), reasons })
    }

    fn parse(&self, doc: &SourceDocument) -> ParseResult {
        let mut st = ParsedStatement::new("pdf");
        st.institution = Some("Fortuneo".into());
        st.adapter_id = Some(self.id().into());
        st.document_kind = Some(self.kind().into());
        st.confidence = "high".into();
        let pages = doc.words(1.0)?;
        let first = pages.first().ok_or_else(|| "list index out of range".to_string())?;
        let rows = pdftools::lines(first, 3.0, false);
        let texts: Vec<String> = rows.iter().map(|r| pdftools::row_text(r)).collect();
        let dated = texts.iter().position(|t| norm(t).starts_with("le ") && french_date(Some(t)).is_some());
        let salutation = texts.iter().position(|t| {
            let n = norm(t);
            n.starts_with("monsieur") || n.starts_with("madame")
        });
        if let Some(d) = dated {
            let when = french_date(Some(&texts[d])).unwrap();
            st.balance_date = Some(when);
            st.set_extra("year", when.year());
            st.set_extra("issued_on", when.format("%Y-%m-%d").to_string());
        }
        if let (Some(d), Some(s)) = (dated, salutation) {
            if s > d {
                let (name, address) = read_address_block(&rows, d, s, 280.0);
                st.holder_name = name;
                st.holder_address = address;
            }
        }
        st.set_extra("title", "Information sur la garantie des dépôts (FGDR)");
        let text = doc.text()?.replace('\u{a0}', " ");
        if let Some(m) = search(CEILING_RE, &text) {
            let digits: String = m[1].chars().filter(|c| py::is_digit_char(*c)).collect();
            st.set_extra("ceiling", format!("{} €", group_thousands(py::py_int(&digits)?))); // "100 000 €", whatever the PDF's spacing
        }
        Ok(vec![st])
    }
}
