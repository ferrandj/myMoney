//! BoursoBank (Boursorama): monthly "Extrait de votre compte en EUR" PDF (current account).
//!
//! Beyond the shared table this document has a banner with the RIB, the currency and the period, and a line with
//! the BIC and the IBAN, both checked against the RIB.

use super::table::{parse_table, read_holder, Header, PERIOD_RE};
use crate::adapters::base::{round2, score, Adapter, Detection, ParseResult, ParsedStatement, SourceDocument, W};
use crate::adapters::common::{find_iban, iban_matches_rib, iban_valid, norm, parse_date_s, rib_valid};
use crate::adapters::pdftools::row_text;
use crate::adapters::re::{re, search};
use crate::py;
use crate::services::rates::guess_account_type;

const RIB_ROW_RE: &str = r"\b(\d{5})\s+(\d{5})\s+([0-9A-Z]{11})\s+(\d{2})\s+([A-Z]{3})\b";
const BIC_RE: &str = r"B\.?I\.?C\.?\s+([A-Z]{6}[A-Z0-9]{2}(?:[A-Z0-9]{3})?)\b";

pub struct BoursoBankStatementPdf;

impl Header for BoursoBankStatementPdf {
    fn default_account_type(&self) -> &'static str {
        "checking"
    }

    fn read_header(&self, st: &mut ParsedStatement, rows: &[Vec<W>]) {
        let texts: Vec<String> = rows.iter().map(|r| row_text(r)).collect();
        for text in &texts {
            let rib = search(RIB_ROW_RE, text);
            let period = search(PERIOD_RE, text);
            if let (Some(rib), Some(period)) = (rib, period) {
                st.bank_code = Some(rib[1].to_string());
                st.branch_code = Some(rib[2].to_string());
                st.account_number = Some(rib[3].to_string());
                st.rib_key = Some(rib[4].to_string());
                st.currency = rib[5].to_string();
                st.period_start = parse_date_s(&period[1]);
                st.period_end = parse_date_s(&period[2]);
                let first = py::split_ws(text).first().map(|s| s.to_string()).unwrap_or_default();
                if let Some(issued) = parse_date_s(&first) {
                    st.set_extra("issued_on", issued.format("%Y-%m-%d").to_string());
                }
            }
            if let Some(b) = search(BIC_RE, text) {
                st.bic = Some(b[1].to_string());
            }
            let up = py::upper(text);
            if up.contains("I.B.A.N") || up.contains("IBAN") {
                st.iban = find_iban(text).or(st.iban.take());
            }
        }
        read_holder(st, rows, &texts);
        // Savings accounts use the same layout, with one more line under the title: "Compte d'épargne : <product>".
        let product = texts
            .iter()
            .find(|t| norm(t).starts_with("compte d") && t.contains("pargne") && t.contains(':'))
            .map(|t| py::strip(t.split_once(':').unwrap().1).to_string());
        if let Some(p) = &product {
            st.set_extra("product", p.clone());
            st.account_type = Some(guess_account_type(p, "savings"));
        }
        let joint = re(r"\b(OU|ET)\b").is_match(st.holder_name.as_deref().unwrap_or(""));
        if let Some(n) = &st.account_number {
            let kind = product.clone().unwrap_or_else(|| if joint { "Compte joint".into() } else { "Compte".into() });
            st.account_label = Some(format!("{kind} BoursoBank …{}", last4(n)));
        }
    }

    fn check_identity(&self, st: &mut ParsedStatement) {
        if let Some(iban) = st.iban.clone().filter(|s| !s.is_empty()) {
            if !iban_valid(&iban) {
                st.warnings.push("L'IBAN lu n'est pas valide (clé de contrôle)".into());
            }
        }
        if let (Some(b), Some(g), Some(a), Some(k)) = (st.bank_code.clone(), st.branch_code.clone(), st.account_number.clone(), st.rib_key.clone()) {
            if !b.is_empty() && !g.is_empty() && !a.is_empty() && !k.is_empty() {
                if !rib_valid(&b, &g, &a, &k) {
                    st.warnings.push("La clé RIB lue ne correspond pas au numéro de compte".into());
                }
                if let Some(iban) = st.iban.clone().filter(|s| !s.is_empty()) {
                    if !iban_matches_rib(&iban, &b, &g, &a, &k) {
                        st.warnings.push("L'IBAN ne correspond pas au RIB du bandeau".into());
                    }
                }
            }
        }
    }
}

pub fn last4(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    c[c.len().saturating_sub(4)..].iter().collect()
}

impl Adapter for BoursoBankStatementPdf {
    fn id(&self) -> &'static str {
        "boursobank.statement_pdf"
    }
    fn institution(&self) -> Option<&'static str> {
        Some("BoursoBank")
    }
    fn kind(&self) -> &'static str {
        "statement"
    }
    fn label(&self) -> &'static str {
        "BoursoBank · extrait de compte mensuel (PDF)"
    }
    fn description(&self) -> &'static str {
        "Le PDF « Extrait de votre compte en EUR » téléchargé dans l'espace client BoursoBank / Boursorama."
    }
    fn formats(&self) -> &'static [&'static str] {
        &["pdf"]
    }
    fn detect(&self, doc: &SourceDocument) -> Result<Detection, String> {
        if doc.format() != "pdf" {
            return Ok(Detection::default());
        }
        let c = doc.compact()?;
        let (s, reasons) = score(&[
            (c.contains("boursobank") || c.contains("boursorama"), 0.3, "mentionne BoursoBank / Boursorama"),
            (c.contains("extraitdevotrecompte"), 0.25, "titre « Extrait de votre compte »"),
            (c.contains("mouvementsen"), 0.2, "tableau « Mouvements en EUR »"),
            (c.contains("clerib"), 0.1, "bandeau RIB (banque, guichet, compte, clé)"),
            (c.contains("soldeau") && c.contains("nouveausolde"), 0.15, "« Solde au » et « Nouveau solde »"),
        ]);
        Ok(Detection { score: round2(s.min(1.0)), reasons })
    }
    fn parse(&self, doc: &SourceDocument) -> ParseResult {
        parse_table(self, self.id(), "BoursoBank", self.kind(), doc)
    }
}
