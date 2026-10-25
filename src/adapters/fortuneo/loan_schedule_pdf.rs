//! Fortuneo: the dated amortization table ("Tableau d'amortissement") downloaded from the customer area.
//!
//! Unlike the one in the offer it carries real dates, and it describes the loan as it is now: after an early repayment
//! it starts from the capital still owed that day (the amount printed is not the amount borrowed) and ends at the new
//! end date.

use chrono::NaiveDate;
use serde_json::{Map, Value};

use super::loan::{check_rows, loan_label, money, numeric_date, parse_rows, row_texts, AMT, REFERENCE_RE};
use super::loan_offer_pdf::{d, s};
use crate::adapters::base::{round2, score, Adapter, Detection, ParseResult, ParsedStatement, SourceDocument};
use crate::adapters::common::norm;
use crate::adapters::re::{findall, py_match, re, search, sub};
use crate::py;

pub struct FortuneoLoanSchedulePdf;

impl Adapter for FortuneoLoanSchedulePdf {
    fn id(&self) -> &'static str {
        "fortuneo.loan_schedule_pdf"
    }
    fn institution(&self) -> Option<&'static str> {
        Some("Fortuneo")
    }
    fn kind(&self) -> &'static str {
        "loan_schedule"
    }
    fn label(&self) -> &'static str {
        "Fortuneo · tableau d'amortissement daté (PDF)"
    }
    fn description(&self) -> &'static str {
        "Le tableau d'amortissement de l'espace client : chaque échéance avec sa date, du jour à la dernière."
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
            (c.contains("fortuneo"), 0.25, "mentionne Fortuneo"),
            (c.contains("tableaudamortissement"), 0.25, "titre « Tableau d'amortissement »"),
            (c.contains("montantrestant") && c.contains("totalapayer"), 0.25, "colonnes « Total à payer » et « Montant restant dû »"),
            (c.contains("datedecheance") || c.contains("dateprevisionnelledefinduprt") || c.contains("dateprevisionnelledefindupret"), 0.15, "échéances datées"),
            (c.contains("votreprojet") && c.contains("listedu"), 0.1, "« Votre projet »"),
        ]);
        if !c.contains("fortuneo") {
            s = s.min(0.3);
        }
        // The offer also has an (undated) amortization table: the offer adapter is the one that must win there.
        if c.contains("offredepret") && c.contains("conditionsparticulieres") {
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
        for text in &texts {
            let n = norm(text);
            if n.starts_with("emprunteur") && !loan.contains_key("borrower") {
                let rest = sub(r"(?i)^emprunteur\s*:?\s*", "", text);
                let head = match re(r"(?i)\s+co-?\s?emprunteur").find(&rest) {
                    Some(m) => rest[..m.start()].to_string(),
                    None => rest.clone(),
                };
                let name = py::strip(&head).to_string();
                if !name.is_empty() {
                    loan.insert("borrower".into(), name.into());
                }
            }
            if n.starts_with("projet") && !loan.contains_key("project") {
                if let Some(m) = search(r"N°\s*(\S+)", text) {
                    loan.insert("project".into(), m[1].to_string().into());
                }
            }
            if n.starts_with("projet") && py::upper(text).contains("RÉFÉRENCE") {
                let refs = findall(REFERENCE_RE, text);
                if let Some(r) = refs.last() {
                    loan.insert("reference".into(), r.clone().into());
                }
            }
            if n.starts_with("montant du financement") {
                if let Some(m) = re(AMT).find(text) {
                    loan.insert("project_amount".into(), py::dec_str(&money(m.as_str())).into());
                }
            }
            if n.starts_with("date previsionnelle de fin") {
                if let Some(w) = numeric_date(text) {
                    loan.insert("end_date_stated".into(), w.format("%Y-%m-%d").to_string().into());
                }
            }
            if n.starts_with("pret") && n.contains("montant") && n.contains("(euros)") && n.split_once("pret").map(|(_, b)| b.contains("montant")).unwrap_or(false) {
                if let Some(m) = search(&format!(r"(?i)MONTANT\s*\(euros\)\s*({AMT})"), text) {
                    loan.insert("balance".into(), py::dec_str(&money(&m[1])).into());
                }
                if let Some(m) = py_match(r"(?i)^pr[eê]t\s+(.*?)\s+MONTANT", text) {
                    loan.insert("product".into(), py::strip(&m[1]).to_string().into());
                }
            }
            if n.starts_with("taux de base") {
                if let Some(m) = search(r"(\d+,\d+)", text) {
                    loan.insert("rate".into(), py::dec_str(&money(&m[1])).into());
                    if let Some(dd) = search(r"(?i)DUR[ÉE]E\s+(\d+)\s+termes", text) {
                        loan.insert("term".into(), py::py_int(&dd[1])?.into());
                    }
                    if norm(text).contains("mensuel") {
                        loan.insert("periodicity".into(), "mensuelle".into());
                    }
                }
            }
        }
        // First page: the list of the project's loans: "FIXIMMO EXEMPLE 52 000,00 PRET 2,40 63 FT99000002 IMMOBILIER".
        for text in &texts {
            if loan.contains_key("reference") {
                continue;
            }
            let Some(m) = search(REFERENCE_RE, text) else { continue };
            let reference = m[1].to_string();
            // AMT is an alternation: pasted bare, like the Python f-string, it splits the whole pattern in two.
            if search(&format!(r"{AMT}\s+.*\d+,\d{{2}}\s+\d+\s+{reference}"), text).is_some() {
                loan.insert("reference".into(), reference.into());
                if !loan.contains_key("product") {
                    if let Some(p) = py_match(&format!(r"^(.*?)\s+{AMT}"), text) {
                        let g = p.get(1).ok_or_else(|| "'NoneType' object has no attribute 'strip'".to_string())?;
                        loan.insert("product".into(), py::strip(g.as_str()).to_string().into());
                    }
                }
            }
        }
        let rows = parse_rows(&texts, true)?;
        loan.insert("rows".into(), Value::Array(rows.iter().cloned().map(Value::Object).collect()));
        st.holder_name = s(&loan, "borrower");
        st.account_label = Some(loan_label(s(&loan, "product").as_deref(), s(&loan, "reference").as_deref())?);
        let date_of = |r: &Map<String, Value>| s(r, "date").and_then(|x| NaiveDate::parse_from_str(&x, "%Y-%m-%d").ok());
        if let (Some(first), Some(last)) = (rows.first(), rows.last()) {
            st.period_start = date_of(first);
            st.period_end = date_of(last);
        }
        check(&mut st, &loan, &rows);
        st.set_extra("loan", Value::Object(loan));
        st.set_extra("title", "Tableau d'amortissement");
        Ok(vec![st])
    }
}

fn check(st: &mut ParsedStatement, loan: &Map<String, Value>, rows: &[Map<String, Value>]) {
    let warnings = &mut st.warnings;
    let missing: Vec<&str> = ["reference", "balance", "rate"].into_iter().filter(|k| !loan.contains_key(*k)).collect();
    if !missing.is_empty() {
        warnings.push(format!("Informations introuvables dans le tableau : {}", missing.join(", ")));
    }
    if rows.is_empty() {
        warnings.push("Aucune échéance lue dans le tableau".into());
    } else {
        check_rows(rows, if loan.contains_key("balance") { Some(d(loan, "balance")) } else { None }, warnings);
        let last = s(rows.last().unwrap(), "date").unwrap_or_default();
        if let Some(stated) = s(loan, "end_date_stated").filter(|x| !x.is_empty()) {
            if stated != last {
                warnings.push(format!("La dernière échéance lue ({last}) n'est pas la date de fin annoncée ({stated})"));
            }
        }
        let dates: Vec<String> = rows.iter().map(|r| s(r, "date").unwrap_or_default()).collect();
        if dates.windows(2).any(|w| w[1] <= w[0]) {
            warnings.push("Les dates des échéances ne se suivent pas".into());
        }
    }
    st.confidence = if st.warnings.is_empty() { "high".into() } else { "low".into() };
}
