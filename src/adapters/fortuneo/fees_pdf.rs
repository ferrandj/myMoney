//! Fortuneo: "Relevé de frais", the yearly recap of the fees and interest of an account.
//!
//! No operations: the document says how much the account cost over a period (total fees, with each service, its unit
//! price and how many times it was billed), and the interest paid and earned. It is attached to the account, where it
//! can be checked against the fee lines of the imported statements.

use rust_decimal::Decimal;
use serde_json::{Map, Value};

use super::identity::{read_address_block, read_identity};
use crate::adapters::base::{round2, score, Adapter, Detection, ParseResult, ParsedStatement, SourceDocument, W};
use crate::adapters::boursobank::statement_pdf::last4;
use crate::adapters::common::{int, month_of, norm, parse_amount, py_date, strip_accents};
use crate::adapters::lines::identity_warnings;
use crate::adapters::pdftools;
use crate::adapters::re::{findall, py_match, search, sub};
use crate::py;
use crate::services::rates::guess_account_type;

const SIGNED_AMOUNT_RE: &str = r"^-?(?:\d{1,3}(?:[ .]\d{3})*|\d+),\d{2}$"; // a refund is a negative total in the list of services
const PERIOD_RE: &str = r"(?i)du\s+(\d{1,2})(?:er)?\s+([^\W\d_]+)\s+au\s+(\d{1,2})(?:er)?\s+([^\W\d_]+)\s+(\d{4})";

pub struct FortuneoFeesPdf;

/// `str(parse_amount(x))`: "None" when it does not parse.
pub fn amount_str(raw: &str) -> String {
    parse_amount(Some(raw)).map(|d| py::dec_str(&d)).unwrap_or_else(|| "None".into())
}

impl Adapter for FortuneoFeesPdf {
    fn id(&self) -> &'static str {
        "fortuneo.fees_pdf"
    }
    fn institution(&self) -> Option<&'static str> {
        Some("Fortuneo")
    }
    fn kind(&self) -> &'static str {
        "fees_summary"
    }
    fn label(&self) -> &'static str {
        "Fortuneo · récapitulatif annuel des frais (PDF)"
    }
    fn description(&self) -> &'static str {
        "Le « Relevé de frais » annuel : frais payés service par service, intérêts payés et acquis."
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
            (c.contains("relevedefrais"), 0.3, "titre « Relevé de frais »"),
            (c.contains("recapitulatifdesfraisetinterets"), 0.25, "« Récapitulatif des frais et intérêts »"),
            (c.contains("decomptedesfraisprelevessurlecompte"), 0.2, "« Décompte des frais prélevés sur le compte »"),
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
        st.account_type = Some("checking".into());
        let mut rows: Vec<Vec<W>> = vec![];
        for words in doc.words(1.0)?.iter() {
            rows.extend(pdftools::lines(words, 3.0, true));
        }
        let texts: Vec<String> = rows.iter().map(|r| pdftools::row_text(r)).collect();
        read_identity(&mut st, &texts);

        let product = texts
            .iter()
            .find(|t| norm(t).starts_with("intitule du compte"))
            .map(|t| py::strip(&sub(r"(?i)^intitul[eé] du compte\s*", "", t)).to_string())
            .unwrap_or_default();
        if !product.is_empty() {
            st.set_extra("product", py::capitalize(&product));
            st.account_type = Some(guess_account_type(&product, "checking"));
        }
        for text in &texts {
            if let Some(m) = search(PERIOD_RE, text) {
                let end_year = int(&m[5]);
                let sm = month_of(&strip_accents(&py::lower(&m[2])));
                let em = month_of(&strip_accents(&py::lower(&m[4])));
                if let (Some(sm), Some(em)) = (sm, em) {
                    st.period_end = Some(py_date(end_year, em as i64, int(&m[3]))?);
                    st.period_start = Some(py_date(end_year - if sm > em { 1 } else { 0 }, sm as i64, int(&m[1]))?);
                }
                break;
            }
        }
        let issued = texts.iter().find(|t| norm(t).starts_with("date ") && search(r"\d{2}/\d{2}/\d{4}", t).is_some());
        if let Some(m) = issued.and_then(|t| search(r"(\d{2}/\d{2}/\d{4})", t)) {
            let p: Vec<i64> = m[1].split('/').map(int).collect();
            st.set_extra("issued_on", py_date(p[2], p[1], p[0])?.format("%Y-%m-%d").to_string());
        }

        // Holder and address: the block between the bank's contact lines and the account box.
        let last_contact = texts.iter().enumerate().filter(|(_, t)| norm(t).contains("service clients")).map(|(i, _)| i).max();
        let bx = texts.iter().position(|t| norm(t).starts_with("intitule du compte"));
        if let (Some(lc), Some(b)) = (last_contact, bx) {
            if b > lc {
                let (name, address) = read_address_block(&rows, lc, b, 0.0);
                if st.holder_name.as_deref().map(|s| s.is_empty()).unwrap_or(true) {
                    st.holder_name = name;
                }
                st.holder_address = address;
            }
        }

        read_totals(&mut st, &texts);
        read_services(&mut st, &rows, &texts)?;
        if let Some(n) = &st.account_number {
            let product = st.extra_str("product").unwrap_or_else(|| "Compte".into());
            st.account_label = Some(format!("{product} Fortuneo …{}", last4(n)));
        }
        check(&mut st);
        Ok(vec![st])
    }
}

fn read_totals(st: &mut ParsedStatement, texts: &[String]) {
    for text in texts {
        let n = norm(text);
        let amounts = findall(r"\d{1,3}(?:[ .]\d{3})*,\d{2}", text);
        let last = amounts.last().map(|a| amount_str(a));
        if n.starts_with("total des frais payes") && last.is_some() && !st.extra.contains_key("fees_total") {
            st.set_extra("fees_total", last.unwrap());
        } else if n.starts_with("total des interets payes") && last.is_some() && !st.extra.contains_key("interest_paid") {
            st.set_extra("interest_paid", last.unwrap());
        } else if n.starts_with("total des interets acquis") && !st.extra.contains_key("interest_earned") {
            st.set_extra("interest_earned", last.map(Value::String).unwrap_or(Value::Null)); // "Intérêts non applicables"
        }
    }
}

/// Rows of "Décompte des frais prélevés sur le compte": a category line, then one line per service with how many
/// times it was used, its unit price, how many times it was billed and the total.
fn read_services(st: &mut ParsedStatement, rows: &[Vec<W>], texts: &[String]) -> Result<(), String> {
    let start = texts.iter().position(|t| norm(t).starts_with("decompte des frais"));
    let from = start.unwrap_or(0);
    let end = texts.iter().enumerate().position(|(i, t)| i > from && norm(t).starts_with("total des frais payes"));
    let mut services: Vec<Value> = vec![];
    let mut category: Option<String> = None;
    let (Some(start), Some(end)) = (start, end) else {
        st.set_extra("services", Value::Array(services));
        return Ok(());
    };
    let body: &[Vec<W>] = if start + 1 < end { &rows[start + 1..end] } else { &[] };
    struct P<'a> {
        numbers: Vec<&'a W>,
        name: String,
        top: f64,
    }
    let parsed: Vec<P> = body
        .iter()
        .map(|row| P {
            numbers: row.iter().filter(|w| w.x0 >= 220.0 && (py_match(SIGNED_AMOUNT_RE, &w.text).is_some() || py::isdigit(&w.text))).collect(),
            name: py::strip(&row.iter().filter(|w| w.x0 < 220.0).map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")).to_string(),
            top: row[0].top,
        })
        .collect();

    // Is the row i a piece of a long service name, printed just above or below the row that holds the numbers?
    let beside = |i: usize| -> bool {
        let p = &parsed[i];
        if !p.numbers.is_empty() || p.name.is_empty() {
            return false;
        }
        [i as i64 - 1, i as i64 + 1].iter().any(|&j| {
            j >= 0 && (j as usize) < parsed.len() && {
                let q = &parsed[j as usize];
                q.numbers.len() >= 4 && q.name.is_empty() && (q.top - p.top).abs() < 12.0
            }
        })
    };

    for (i, row) in body.iter().enumerate() {
        let p = &parsed[i];
        let mut name = p.name.clone();
        if p.numbers.len() >= 4 && name.is_empty() {
            // "REMBOURSEMENT(S)" / numbers / "EXCEPTIONNEL(S)": the name wraps around the figures
            let parts: Vec<String> = [i as i64 - 1, i as i64 + 1]
                .iter()
                .filter(|&&j| j >= 0 && (j as usize) < parsed.len() && beside(j as usize) && (parsed[j as usize].top - p.top).abs() < 12.0)
                .map(|&j| parsed[j as usize].name.clone())
                .collect();
            name = parts.join(" ");
        }
        if p.numbers.len() >= 4 && !name.is_empty() {
            let t: Vec<&str> = p.numbers[..4].iter().map(|w| w.text.as_str()).collect();
            let mut m = Map::new();
            m.insert("category".into(), category.clone().map(Value::String).unwrap_or(Value::Null));
            m.insert("name".into(), Value::String(name));
            m.insert("used".into(), Value::from(py::py_int(t[0])?));
            m.insert("unit_price".into(), Value::String(amount_str(t[1])));
            m.insert("billed".into(), Value::from(py::py_int(t[2])?));
            m.insert("total".into(), Value::String(amount_str(t[3])));
            services.push(Value::Object(m));
        } else if beside(i) {
            continue;
        } else if !name.is_empty() && p.numbers.is_empty() && !["service", "frais"].contains(&norm(&name).as_str()) && row.len() <= 6 && row[0].x0 < 60.0 {
            category = Some(name);
        }
    }
    st.set_extra("services", Value::Array(services));
    Ok(())
}

fn check(st: &mut ParsedStatement) {
    let total = st.extra.get("fees_total").and_then(|v| v.as_str()).map(|s| s.to_string());
    let services: Vec<Value> = st.extra.get("services").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    match total {
        None => {
            st.confidence = "low".into();
            st.warnings.push("Total des frais introuvable dans le document".into());
        }
        Some(total) => {
            let sum: Decimal = services.iter().filter_map(|s| s.get("total").and_then(|t| t.as_str()).and_then(py::dec_parse)).fold(Decimal::ZERO, |a, b| a + b);
            if !services.is_empty() && Some(sum) != py::dec_parse(&total) {
                st.confidence = "low".into();
                st.warnings.push("Le total des frais ne correspond pas à la somme des services listés : vérifie le document".into());
            } else {
                st.confidence = "high".into();
            }
        }
    }
    if !(st.period_start.is_some() && st.period_end.is_some()) {
        st.confidence = "low".into();
        st.warnings.push("Période du récapitulatif introuvable".into());
    }
    let w = identity_warnings(st);
    st.warnings.extend(w);
    if st.confidence == "high" && !st.warnings.is_empty() {
        st.confidence = "medium".into();
    }
}
