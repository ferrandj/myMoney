//! What the Fortuneo loan documents have in common: money and dates as printed, reference numbers, amortization rows.
//!
//! The documents of a loan (offer, early repayment notice, amortization table) never repeat each other's figures in
//! the same form, so every adapter turns what it reads into the same plain structure (`extra["loan"]`, strings only)
//! that the loans service stores and cross-checks.

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde_json::{Map, Value};

use crate::adapters::base::SourceDocument;
use crate::adapters::common::{int, norm, py_decimal, ymd};
use crate::adapters::pdftools;
use crate::adapters::re::{findall, fullmatch, py_match, search};
use crate::py;

/// "122 082,74" (the thousands are a separate word in the PDF) or "102399,31"
pub const AMT: &str = r"\d{1,3}(?: \d{3})*,\d{2}|\d+,\d{2}";
pub const REFERENCE_RE: &str = r"\b([A-Z]{2}\d{8,})\b";

fn row_re() -> String {
    format!(r"(?P<head>\S+)\s+({AMT})\s+({AMT})\s+({AMT})\s+({AMT})\s+({AMT})\s+({AMT})")
}

pub fn money(text: &str) -> Decimal {
    py_decimal(&text.replace(' ', "").replace('\u{a0}', "").replace(',', ".")).unwrap_or_default()
}

pub fn amounts(text: &str) -> Vec<Decimal> {
    findall(AMT, text).iter().map(|m| money(m)).collect()
}

pub fn numeric_date(text: &str) -> Option<NaiveDate> {
    let m = search(r"(\d{2})/(\d{2})/(\d{4})", text)?;
    ymd(int(&m[3]), int(&m[2]), int(&m[1]))
}

/// Every row of every page, words joined with a space (the PDF's own text loses most of its spaces).
pub fn row_texts(doc: &SourceDocument) -> Result<Vec<String>, String> {
    let mut out = vec![];
    for words in doc.words(1.0)?.iter() {
        out.extend(pdftools::lines(words, 3.0, false).iter().map(|r| pdftools::row_text(r)));
    }
    Ok(out)
}

/// The lines of an amortization table, in order. The first column is the date (current table) or the number (offer).
pub fn parse_rows(texts: &[String], dated: bool) -> Result<Vec<Map<String, Value>>, String> {
    let mut rows: Vec<Map<String, Value>> = vec![];
    let mut seen_header = false;
    let re = row_re();
    for text in texts {
        let n = norm(text);
        if n.contains("amortissement") && n.contains("interets") {
            seen_header = true;
            continue;
        }
        if !seen_header {
            continue;
        }
        let Some(m) = py_match(&re, text) else { continue };
        let head = &m["head"];
        let mut row = Map::new();
        if dated {
            let when = numeric_date(head);
            match when {
                Some(w) if fullmatch(r"\d{2}/\d{2}/\d{4}", head).is_some() => {
                    row.insert("date".into(), w.format("%Y-%m-%d").to_string().into());
                }
                _ => continue,
            }
        } else {
            if !py::isdigit(head) || py::py_int(head)? != rows.len() as i64 + 1 {
                continue;
            }
            row.insert("n".into(), Value::from(py::py_int(head)?));
        }
        for (i, key) in ["capital", "interest", "deferred", "insurance", "total", "remaining"].iter().enumerate() {
            row.insert((*key).into(), py::dec_str(&money(&m[i + 2])).into());
        }
        rows.push(row);
    }
    Ok(rows)
}

fn dec(r: &Map<String, Value>, k: &str) -> Decimal {
    r.get(k).and_then(|v| v.as_str()).and_then(py::dec_parse).unwrap_or_default()
}

/// Each line adds up, the remaining capital goes down line by line from `start` and ends at zero.
pub fn check_rows(rows: &[Map<String, Value>], start: Option<Decimal>, warnings: &mut Vec<String>) {
    let (mut bad_sum, mut bad_chain): (Vec<usize>, Vec<usize>) = (vec![], vec![]);
    let mut balance = start;
    for (i, r) in rows.iter().enumerate() {
        let i = i + 1;
        let (capital, remaining) = (dec(r, "capital"), dec(r, "remaining"));
        if capital + dec(r, "interest") + dec(r, "deferred") + dec(r, "insurance") != dec(r, "total") {
            bad_sum.push(i);
        }
        if let Some(b) = balance {
            if b - capital != remaining {
                bad_chain.push(i);
            }
        }
        balance = Some(remaining);
    }
    let first5 = |v: &[usize]| v.iter().take(5).map(|x| x.to_string()).collect::<Vec<_>>().join(", ");
    if !bad_sum.is_empty() {
        warnings.push(format!("Capital + intérêts + assurance ≠ total à payer sur {} échéance(s) (n° {}…)", bad_sum.len(), first5(&bad_sum)));
    }
    if !bad_chain.is_empty() {
        warnings.push(format!("Le capital restant dû ne suit pas les amortissements sur {} échéance(s) (n° {}…)", bad_chain.len(), first5(&bad_chain)));
    }
    if let Some(last) = rows.last() {
        if !dec(last, "remaining").is_zero() {
            warnings.push(format!(
                "Le capital restant dû après la dernière échéance n'est pas nul ({})",
                last.get("remaining").and_then(|v| v.as_str()).unwrap_or("")
            ));
        }
    }
}

pub fn loan_label(product: Option<&str>, reference: Option<&str>) -> Result<String, String> {
    let word = match product.filter(|p| !p.is_empty()) {
        Some(p) => py::capitalize(py::split_ws(p).first().ok_or_else(|| "list index out of range".to_string())?),
        None => String::new(),
    };
    let first = format!("Prêt {word}");
    let first = py::strip(&first).to_string();
    let second = match reference.filter(|r| !r.is_empty()) {
        Some(r) => format!("…{}", crate::adapters::boursobank::statement_pdf::last4(r)),
        None => String::new(),
    };
    Ok([first, second].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" "))
}
