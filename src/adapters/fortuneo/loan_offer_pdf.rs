//! Fortuneo: the loan offer ("Offre de prêt immobilier"), the contract the loan starts from.
//!
//! A long PDF (information sheet, particular and general conditions, insurance, guarantee) of which three things
//! matter: the particular conditions (amount, duration, rate, APR, what the loan costs), the borrower, and the original
//! amortization table at the end. That table is numbered, not dated: the dates depend on when the funds are released,
//! so they come from a later document (the early repayment notice, the dated table) or from the user.

use rust_decimal::Decimal;
use serde_json::{Map, Value};

use super::loan::{amounts, check_rows, loan_label, money, numeric_date, parse_rows, row_texts, AMT, REFERENCE_RE};
use crate::adapters::base::{round2, score, Adapter, Detection, ParseResult, ParsedStatement, SourceDocument};
use crate::adapters::common::norm;
use crate::adapters::re::{py_match, search};
use crate::py;

const NAME_RE: &str = r"(?i)^(?:M\.|MME|MLLE|M|MADAME|MONSIEUR)\s+(.+?)\s*(?:\(\d+\)|né\(e\))";

pub struct FortuneoLoanOfferPdf;

pub fn s(v: &Map<String, Value>, k: &str) -> Option<String> {
    v.get(k).and_then(|x| x.as_str()).map(|x| x.to_string())
}

pub fn d(v: &Map<String, Value>, k: &str) -> Decimal {
    v.get(k).and_then(|x| x.as_str()).and_then(py::dec_parse).unwrap_or_default()
}

pub fn sum_of(rows: &[Map<String, Value>], k: &str) -> Decimal {
    rows.iter().map(|r| d(r, k)).fold(Decimal::ZERO, |a, b| a + b)
}

/// `text.split(":", 1)[1].strip()`.
pub fn after_colon(text: &str) -> String {
    py::strip(text.split_once(':').map(|(_, b)| b).unwrap_or("")).to_string()
}

impl Adapter for FortuneoLoanOfferPdf {
    fn id(&self) -> &'static str {
        "fortuneo.loan_offer_pdf"
    }
    fn institution(&self) -> Option<&'static str> {
        Some("Fortuneo")
    }
    fn kind(&self) -> &'static str {
        "loan_offer"
    }
    fn label(&self) -> &'static str {
        "Fortuneo · offre de prêt immobilier (PDF)"
    }
    fn description(&self) -> &'static str {
        "L'offre de prêt : montant, durée, taux, coût du crédit et tableau d'amortissement d'origine."
    }
    fn formats(&self) -> &'static [&'static str] {
        &["pdf"]
    }

    fn detect(&self, doc: &SourceDocument) -> Result<Detection, String> {
        if doc.format() != "pdf" {
            return Ok(Detection::default());
        }
        let c = doc.compact()?.replace('\'', "").replace('’', "");
        let (mut s, reasons) = score(&[
            (c.contains("fortuneo"), 0.2, "mentionne Fortuneo"),
            (c.contains("offredepret") && c.contains("immobilier"), 0.3, "« Offre de prêt(s) immobilier(s) »"),
            (c.contains("conditionsparticulieres"), 0.15, "« Conditions particulières »"),
            (c.contains("tableaudamortissement"), 0.15, "« Tableau d'amortissement »"),
            (c.contains("tauxannueleffectifglobal") || c.contains("taeg"), 0.1, "mentionne le TAEG"),
            (c.contains("montanttotalarembourser") || c.contains("couttotaldupret"), 0.1, "coût total du prêt"),
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
        st.account_type = Some("mortgage".into());
        let texts = row_texts(doc)?;
        let mut loan: Map<String, Value> = Map::new();
        let put = |loan: &mut Map<String, Value>, k: &str, v: Value| {
            loan.insert(k.to_string(), v);
        };
        let last = |text: &str| amounts(text).last().map(|a| py::dec_str(a));
        for (i, text) in texts.iter().enumerate() {
            let n = norm(text);
            if n.starts_with("le(s) emprunteur") && i + 1 < texts.len() && py_match(NAME_RE, &texts[i + 1]).is_some() {
                let m = py_match(NAME_RE, &texts[i + 1]).unwrap();
                st.holder_name = Some(py::strip(&m[1]).to_string());
            } else if n.starts_with("article ii") && n.contains("caracteristiques") && search(r"N°\s*(\S+)", text).is_some() {
                put(&mut loan, "project", search(r"N°\s*(\S+)", text).unwrap()[1].to_string().into());
            } else if n.starts_with("objet du financement") && text.contains(':') {
                put(&mut loan, "object", after_colon(text).into());
            } else if n.starts_with("destination") && text.contains(':') {
                put(&mut loan, "destination", after_colon(text).into());
            } else if n.starts_with("pret :") && search(r":\s*(\S+)\s+N°\s*(\S+)", text).is_some() {
                let m = search(r":\s*(\S+)\s+N°\s*(\S+)", text).unwrap();
                put(&mut loan, "product", m[1].to_string().into());
                put(&mut loan, "reference", m[2].to_string().into());
            } else if n.contains("montant (euros)") && n.contains("duree") {
                if let Some(m) = search(&format!(r"(?i)\(euros\)\s*:\s*({AMT})"), text) {
                    put(&mut loan, "principal", py::dec_str(&money(&m[1])).into());
                }
                if let Some(m) = search(r"Durée\s*:\s*(\d+)", text) {
                    put(&mut loan, "duration", py::py_int(&m[1])?.into());
                }
            } else if n.starts_with("taux de base") {
                if let Some(m) = search(r":\s*(\d+,\d+)", text) {
                    put(&mut loan, "rate", py::dec_str(&money(&m[1])).into());
                }
                if let Some(m) = search(r"TAEG\s*\(%\)\s*:\s*(\d+,\d+)", text) {
                    put(&mut loan, "taeg", py::dec_str(&money(&m[1])).into());
                }
            } else if n.starts_with("cout total du pret") && n.contains("interets") {
                let values = amounts(text);
                if values.len() >= 2 {
                    put(&mut loan, "total_cost", py::dec_str(&values[0]).into());
                    put(&mut loan, "interest_total", py::dec_str(&values[1]).into());
                }
            } else if n.starts_with("assurance obligatoire") && last(text).is_some() {
                put(&mut loan, "insurance_mandatory", last(text).unwrap().into());
            } else if n.starts_with("frais de garantie") && last(text).is_some() {
                put(&mut loan, "guarantee_fees", last(text).unwrap().into());
            } else if n.starts_with("frais de dossier") && last(text).is_some() {
                put(&mut loan, "file_fees", last(text).unwrap().into());
            } else if n.starts_with("frais de courtage") && last(text).is_some() {
                put(&mut loan, "broker_fees", last(text).unwrap().into());
            } else if n.starts_with("cout des assurances facultatives") && !amounts(text).is_empty() {
                put(&mut loan, "insurance_optional", py::dec_str(&amounts(text)[0]).into());
            } else if !loan.contains_key("loan_date") && py_match(r"^le\s+\d{2}/\d{2}/\d{4}", &n).is_some() {
                let when = numeric_date(text).ok_or_else(|| "'NoneType' object has no attribute 'isoformat'".to_string())?;
                put(&mut loan, "offer_date", when.format("%Y-%m-%d").to_string().into());
            }
        }
        if !loan.contains_key("reference") {
            // the guarantee clause repeats the number: "du crédit suivant : N° FT99000002"
            let r = texts.iter().filter(|t| norm(t).contains("credit suivant")).find_map(|t| search(REFERENCE_RE, t).map(|m| m[1].to_string()));
            if let Some(r) = r {
                put(&mut loan, "reference", r.into());
            }
        }
        let rows = parse_rows(&texts, false)?;
        put(&mut loan, "rows", Value::Array(rows.iter().cloned().map(Value::Object).collect()));
        st.account_label = Some(loan_label(s(&loan, "product").as_deref(), s(&loan, "reference").as_deref())?);
        check(&mut st, &loan, &rows);
        st.set_extra("loan", Value::Object(loan));
        st.set_extra("title", "Offre de prêt");
        Ok(vec![st])
    }
}

fn check(st: &mut ParsedStatement, loan: &Map<String, Value>, rows: &[Map<String, Value>]) {
    let warnings = &mut st.warnings;
    let missing: Vec<&str> = ["reference", "principal", "duration", "rate"].into_iter().filter(|k| !loan.contains_key(*k)).collect();
    if !missing.is_empty() {
        warnings.push(format!("Informations introuvables dans l'offre : {}", missing.join(", ")));
    }
    if rows.is_empty() {
        warnings.push("Tableau d'amortissement introuvable dans l'offre".into());
    } else {
        let principal = if loan.contains_key("principal") { Some(d(loan, "principal")) } else { None };
        check_rows(rows, principal, warnings);
        let duration = loan.get("duration").and_then(|v| v.as_i64()).unwrap_or(0);
        if duration != 0 && rows.len() as i64 != duration {
            warnings.push(format!("{} échéances lues pour une durée de {} mois", rows.len(), duration));
        }
        if let Some(p) = principal {
            if sum_of(rows, "capital") != p {
                warnings.push("La somme des amortissements n'est pas égale au montant emprunté".into());
            }
        }
        if loan.contains_key("interest_total") && sum_of(rows, "interest") != d(loan, "interest_total") {
            warnings.push("La somme des intérêts du tableau ne correspond pas au total annoncé dans les conditions particulières".into());
        }
        if loan.contains_key("insurance_mandatory") {
            let paid = sum_of(rows, "insurance");
            let declared = d(loan, "insurance_mandatory") + d(loan, "insurance_optional");
            if paid != declared {
                warnings.push("Le coût de l'assurance du tableau ne correspond pas aux assurances annoncées".into());
            }
        }
    }
    if ["total_cost", "interest_total", "insurance_mandatory", "guarantee_fees"].iter().all(|k| loan.contains_key(*k)) {
        let parts = d(loan, "interest_total") + d(loan, "insurance_mandatory") + d(loan, "guarantee_fees") + d(loan, "file_fees") + d(loan, "broker_fees");
        if parts != d(loan, "total_cost") {
            warnings.push("Le coût total du prêt ne correspond pas à la somme de ses composantes (intérêts, assurance obligatoire, frais)".into());
        }
    }
    st.confidence = if st.warnings.is_empty() { "high".into() } else { "low".into() };
}
