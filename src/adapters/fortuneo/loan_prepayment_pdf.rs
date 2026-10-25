//! Fortuneo: the notice of a partial early repayment ("Remboursement anticipé partiel").
//!
//! A letter that states the loan before and after the repayment: the capital still owed, the last instalment paid (or
//! the first of the new plan, with its amount) and the end date. It names no loan number: it is tied to its loan by the
//! figures, since the capital owed before must be the one the loan's own table gives at that point.

use chrono::NaiveDate;
use serde_json::{Map, Value};

use super::loan::{amounts, numeric_date, row_texts};
use super::loan_offer_pdf::{d, s};
use crate::adapters::base::{round2, score, Adapter, Detection, ParseResult, ParsedStatement, SourceDocument};
use crate::adapters::common::{french_date, norm};
use crate::py;

pub struct FortuneoLoanPrepaymentPdf;

impl Adapter for FortuneoLoanPrepaymentPdf {
    fn id(&self) -> &'static str {
        "fortuneo.loan_prepayment_pdf"
    }
    fn institution(&self) -> Option<&'static str> {
        Some("Fortuneo")
    }
    fn kind(&self) -> &'static str {
        "loan_event"
    }
    fn label(&self) -> &'static str {
        "Fortuneo · remboursement anticipé partiel (PDF)"
    }
    fn description(&self) -> &'static str {
        "Le courrier qui arrête le décompte d'un remboursement anticipé : capital, échéance et date de fin avant et après."
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
            (c.contains("remboursementanticipe"), 0.3, "« Remboursement anticipé »"),
            (c.contains("avantremboursementanticipe") && c.contains("apresremboursementanticipe"), 0.25, "situation avant / après remboursement"),
            (c.contains("capitalrestantdu"), 0.1, "« Capital restant dû »"),
            (c.contains("decomptearreteau"), 0.1, "décompte arrêté"),
        ]);
        if !c.contains("fortuneo") || (c.contains("offredepret") && c.contains("conditionsparticulieres")) {
            s = s.min(0.3); // the offer has a clause about early repayment, but it is not the notice
        }
        Ok(Detection { score: round2(s.min(1.0)), reasons })
    }

    fn parse(&self, doc: &SourceDocument) -> ParseResult {
        let mut st = ParsedStatement::new("pdf");
        st.institution = Some("Fortuneo".into());
        st.adapter_id = Some(self.id().into());
        st.document_kind = Some(self.kind().into());
        st.account_type = Some("mortgage".into());
        let mut side: Option<usize> = None;
        let mut sides: [Map<String, Value>; 2] = [Map::new(), Map::new()]; // before, after
        let mut event: Map<String, Value> = Map::new();
        event.insert("kind".into(), "prepayment".into());
        event.insert("partial".into(), true.into());
        let iso = |x: NaiveDate| x.format("%Y-%m-%d").to_string();
        for text in row_texts(doc)? {
            let n = norm(&text);
            if n.starts_with("paris, le") || (n.starts_with("paris") && french_date(Some(&text)).is_some()) {
                event.insert("issued_on".into(), french_date(Some(&text)).map(iso).unwrap_or_else(|| "0001-01-01".into()).into());
            } else if n.starts_with("l'(les)emprunteur") || n.starts_with("l'(les) emprunteur") {
                let name = py::strip(text.split_once(':').map(|(_, b)| b).unwrap_or(&text)).to_string();
                if !name.is_empty() {
                    st.holder_name = Some(name);
                }
            } else if n.contains("remboursement anticipe") && n.contains("partiel") {
                event.insert("partial".into(), true.into());
            } else if n.contains("decompte arrete au") && numeric_date(&text).is_some() {
                event.insert("on_date".into(), iso(numeric_date(&text).unwrap()).into());
            } else if n.starts_with("avant remboursement") {
                side = Some(0);
            } else if n.starts_with("apres remboursement") {
                side = Some(1);
            } else if let Some(i) = side {
                let found = amounts(&text);
                if n.starts_with("capital restant du") && !found.is_empty() {
                    sides[i].insert("capital".into(), py::dec_str(found.last().unwrap()).into());
                } else if (n.starts_with("derniere echeance") || n.starts_with("premiere echeance")) && numeric_date(&text).is_some() && !found.is_empty() {
                    sides[i].insert("due".into(), iso(numeric_date(&text).unwrap()).into());
                    sides[i].insert("payment".into(), py::dec_str(found.last().unwrap()).into());
                } else if n.starts_with("date de fin") && numeric_date(&text).is_some() {
                    sides[i].insert("end".into(), iso(numeric_date(&text).unwrap()).into());
                }
            }
        }
        let [before, after] = sides;
        event.insert("before".into(), Value::Object(before));
        event.insert("after".into(), Value::Object(after));
        if let Some(on) = s(&event, "on_date") {
            st.balance_date = NaiveDate::parse_from_str(&on, "%Y-%m-%d").ok();
        }
        check(&mut st, &event);
        let mut loan = Map::new();
        loan.insert("event".into(), Value::Object(event));
        loan.insert("borrower".into(), st.holder_name.clone().map(Value::String).unwrap_or(Value::Null));
        st.set_extra("loan", Value::Object(loan));
        st.set_extra("title", "Remboursement anticipé partiel");
        st.account_label = Some("Prêt Fortuneo".into());
        Ok(vec![st])
    }
}

fn check(st: &mut ParsedStatement, event: &Map<String, Value>) {
    let empty = Map::new();
    let before = event.get("before").and_then(|v| v.as_object()).unwrap_or(&empty);
    let after = event.get("after").and_then(|v| v.as_object()).unwrap_or(&empty);
    let warnings = &mut st.warnings;
    for (label, side) in [("avant", before), ("après", after)] {
        let missing: Vec<&str> = ["capital", "due", "payment", "end"].into_iter().filter(|k| !side.contains_key(*k)).collect();
        if !missing.is_empty() {
            warnings.push(format!("Informations introuvables ({label} remboursement) : {}", missing.join(", ")));
        }
    }
    if !event.contains_key("on_date") {
        warnings.push("Date d'arrêté du décompte introuvable".into());
    }
    if before.contains_key("capital") && after.contains_key("capital") && d(after, "capital") >= d(before, "capital") {
        warnings.push("Le capital restant dû n'a pas diminué".into());
    }
    if let (Some(b), Some(a)) = (s(before, "end"), s(after, "end")) {
        if a > b {
            warnings.push("La date de fin du prêt est repoussée au lieu d'être avancée".into());
        }
    }
    st.confidence = if st.warnings.is_empty() { "high".into() } else { "low".into() };
}
