//! BoursoBank (Boursorama): "Extrait de votre <produit d'épargne>" PDF (Plan Épargne Logement, and the like).
//!
//! Same movements table as the current account, but the title names the product and there is a plain "Numéro
//! de compte" instead of the RIB banner, so no IBAN or BIC on the document.

use super::statement_pdf::last4;
use super::table::{parse_table, read_holder, Header, PERIOD_RE};
use crate::adapters::base::{round2, score, Adapter, Detection, ParseResult, ParsedStatement, SourceDocument, W};
use crate::adapters::common::{norm, parse_date_s};
use crate::adapters::pdftools::row_text;
use crate::adapters::re::{search, sub};
use crate::py;
use crate::services::rates::guess_account_type;

const ACCOUNT_ROW_RE: &str = r"\b(\d{2}/\d{2}/\d{4})\s+([0-9A-Z]{8,})\s+([A-Z]{3})\b";
const SAVINGS_WORDS: &[&str] = &["epargne", "livret", "pel", "ldds", "lep", "cel", "plan"];

pub struct BoursoBankSavingsPdf;

impl Header for BoursoBankSavingsPdf {
    fn default_account_type(&self) -> &'static str {
        "savings"
    }

    fn read_header(&self, st: &mut ParsedStatement, rows: &[Vec<W>]) {
        let texts: Vec<String> = rows.iter().map(|r| row_text(r)).collect();
        for text in &texts {
            let row = search(ACCOUNT_ROW_RE, text);
            let period = search(PERIOD_RE, text);
            if let (Some(row), Some(period)) = (row, period) {
                let issued = row[1].to_string();
                st.account_number = Some(row[2].to_string());
                st.currency = row[3].to_string();
                st.period_start = parse_date_s(&period[1]);
                st.period_end = parse_date_s(&period[2]);
                st.set_extra(
                    "issued_on",
                    parse_date_s(&issued).map(|d| serde_json::Value::String(d.format("%Y-%m-%d").to_string())).unwrap_or(serde_json::Value::Null),
                );
            }
        }
        let title = texts.iter().find(|t| norm(t).contains("extrait de votre")).cloned().unwrap_or_default();
        let product = py::strip(&sub(r"(?i)^.*extrait de votre\s+", "", &title)).to_string();
        if !product.is_empty() {
            st.set_extra("product", product.clone());
            st.account_type = Some(guess_account_type(&product, "savings"));
        }
        read_holder(st, rows, &texts);
        if let Some(n) = &st.account_number {
            let p = if product.is_empty() { "Épargne".to_string() } else { product.clone() };
            st.account_label = Some(format!("{p} BoursoBank …{}", last4(n)));
        }
    }
}

impl Adapter for BoursoBankSavingsPdf {
    fn id(&self) -> &'static str {
        "boursobank.savings_pdf"
    }
    fn institution(&self) -> Option<&'static str> {
        Some("BoursoBank")
    }
    fn kind(&self) -> &'static str {
        "savings_statement"
    }
    fn label(&self) -> &'static str {
        "BoursoBank · extrait d'épargne : PEL, livret… (PDF)"
    }
    fn description(&self) -> &'static str {
        "Le PDF « Extrait de votre Plan Epargne Logement » (ou d'un autre produit d'épargne) : numéro de compte, pas de RIB."
    }
    fn formats(&self) -> &'static [&'static str] {
        &["pdf"]
    }
    fn detect(&self, doc: &SourceDocument) -> Result<Detection, String> {
        if doc.format() != "pdf" {
            return Ok(Detection::default());
        }
        let c = doc.compact()?;
        let (mut s, mut reasons) = score(&[
            (c.contains("boursobank") || c.contains("boursorama"), 0.3, "mentionne BoursoBank / Boursorama"),
            (c.contains("extraitdevotre"), 0.15, "titre « Extrait de votre … »"),
            (c.contains("numerodecompte"), 0.15, "bandeau « Numéro de compte » (sans RIB)"),
            (c.contains("mouvementsen"), 0.15, "tableau « Mouvements en EUR »"),
            (c.contains("soldeeneurau") && c.contains("nouveausolde"), 0.1, "« Solde en EUR au » et « Nouveau solde »"),
            (SAVINGS_WORDS.iter().any(|w| c.contains(&format!("extraitdevotre{w}"))), 0.15, "produit d'épargne dans le titre"),
        ]);
        if c.contains("clerib") || c.contains("extraitdevotrecompte") {
            // The current-account layout (RIB banner): that is the other adapter's document.
            s = s.min(0.3);
            reasons.push("bandeau RIB : plutôt un compte courant".into());
        }
        Ok(Detection { score: round2(s.min(1.0)), reasons })
    }
    fn parse(&self, doc: &SourceDocument) -> ParseResult {
        parse_table(self, self.id(), "BoursoBank", self.kind(), doc)
    }
}
