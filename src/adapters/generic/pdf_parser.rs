//! PDF statements and other documents (life-insurance statements, account summaries...).
//!
//! PDFs have no structure, so this works from the position of each word on the page:
//!   * the "Débit" / "Crédit" (or "Montant", "Solde") headers give the column of each amount;
//!   * a line starting with a date is an operation, the lines below it continue its label;
//!   * "ancien solde" / "nouveau solde" lines give the balances, used to check the result.
//! What was understood is always shown for review before anything is saved.

use std::str::FromStr;

use chrono::{Datelike, Duration, NaiveDate};
use rust_decimal::Decimal;

use crate::adapters::base::{HoldingData, ParsedStatement, TransactionData};
use crate::adapters::boursobank::table::fmt2;
use crate::adapters::common::{find_iban, find_institution, norm, parse_amount, parse_date, py_date};
use crate::adapters::re::{findall, fullmatch, py_match, search};
use crate::py;
use crate::services::rates::{guess_account_type, today};

const AMOUNT_RE: &str = r"^[+-]?\d{1,3}(?:[.\x{a0}\x{202f} ]?\d{3})*,\d{2}-?€?$|^[+-]?\d+\.\d{2}-?€?$";
const INT_GROUP_RE: &str = r"^[+-]?\d{1,3}$";
const DATE_RE: &str = r"^\d{1,2}[/.]\d{1,2}(?:[/.]\d{2,4})?$";
const FULL_DATE_RE: &str = r"\b(\d{1,2}[/.]\d{1,2}[/.]\d{4})\b";
const PERIOD_RE: &str = r"(?i)(?:du|periode du)\s+(\d{1,2}[/.]\d{1,2}[/.]\d{2,4})\s+au\s+(\d{1,2}[/.]\d{1,2}[/.]\d{2,4})";
const ISIN_RE: &str = r"\b([A-Z]{2}[A-Z0-9]{9}\d)\b";

const OPENING: &[&str] = &["ancien solde", "solde precedent", "solde initial", "solde au debut", "solde debut", "solde reporte", "report solde"];
const CLOSING: &[&str] = &["nouveau solde", "solde final", "solde en fin", "solde au", "solde crediteur au", "solde debiteur au", "solde fin", "solde a la date"];
const CREDIT_HINTS: &[&str] = &[
    "vir recu", "virement recu", "vir sepa recu", "virement de", "vir inst recu", "remise", "interets", "remboursement", "avoir ", "depot", "versement",
    "salaire", "prime", "retrocession",
];
const CANDIDATE_KEYS: &[&str] = &["solde", "valeur de rachat", "epargne acquise", "valorisation", "encours", "montant total", "capital", "total"];
const PRODUCT_WORDS: &[&str] = &[
    "livret", "compte courant", "compte de depot", "compte cheque", "ldds", " lep", "plan epargne", "pel", " cel ", "assurance vie", "assurance-vie",
    "pea", "compte titres", "compte-titres", "compte sur livret", "contrat", "per ",
];

#[derive(Clone, Debug)]
struct Word {
    text: String,
    x0: f64,
    x1: f64,
    top: f64,
}

fn is_amount(t: &str) -> bool {
    py_match(AMOUNT_RE, t).is_some()
}

fn is_date(t: &str) -> bool {
    py_match(DATE_RE, t).is_some()
}

fn lines(words: &[Word]) -> Vec<Vec<Word>> {
    let mut items = words.to_vec();
    items.sort_by(|a, b| a.top.partial_cmp(&b.top).unwrap().then(a.x0.partial_cmp(&b.x0).unwrap()));
    let mut out: Vec<Vec<Word>> = vec![];
    for w in items {
        match out.last_mut() {
            Some(l) if (l[0].top - w.top).abs() <= 3.0 => l.push(w),
            _ => out.push(vec![w]),
        }
    }
    for l in out.iter_mut() {
        l.sort_by(|a, b| a.x0.partial_cmp(&b.x0).unwrap());
    }
    out
}

/// Re-join amounts split by a thousands space: "1" "234,56" -> "1 234,56".
fn merge_amounts(line: Vec<Word>) -> Vec<Word> {
    let mut out: Vec<Word> = vec![];
    for w in line {
        let joins = match out.last() {
            Some(p) => is_amount(&w.text) && py_match(INT_GROUP_RE, &p.text).is_some() && w.x0 - p.x1 < 6.0,
            None => false,
        };
        if joins {
            let prev = out.pop().unwrap();
            out.push(Word { text: format!("{} {}", prev.text, w.text), x0: prev.x0, x1: w.x1, top: w.top });
        } else {
            out.push(w);
        }
    }
    out
}

/// The columns named by a header line, in the order they were first seen.
fn header_columns(line: &[Word]) -> Option<Vec<(&'static str, f64)>> {
    let mut cols: Vec<(&'static str, f64)> = vec![];
    let mut set = |k: &'static str, v: f64| match cols.iter_mut().find(|(c, _)| *c == k) {
        Some(e) => e.1 = v,
        None => cols.push((k, v)),
    };
    for w in line {
        let n = norm(&w.text);
        let t = n.trim_matches(|c| " :()€".contains(c));
        match t {
            "debit" | "debits" | "debit eur" => set("debit", w.x1),
            "credit" | "credits" | "credit eur" => set("credit", w.x1),
            "montant" | "montants" => set("amount", w.x1),
            "solde" => set("balance", w.x1),
            _ => {}
        }
    }
    let has = |k: &str| cols.iter().any(|(c, _)| *c == k);
    if (has("debit") && has("credit")) || (has("amount") && line.len() <= 12) {
        Some(cols)
    } else {
        None
    }
}

/// `min(cols, key=lambda k: abs(cols[k] - x))`: the first closest column.
fn nearest(cols: &[(&'static str, f64)], x: f64) -> &'static str {
    let mut best = cols[0];
    for c in &cols[1..] {
        if (c.1 - x).abs() < (best.1 - x).abs() {
            best = *c;
        }
    }
    best.0
}

fn resolve_date(raw: &str, year: i64, period_end: Option<NaiveDate>) -> Result<Option<NaiveDate>, String> {
    let d = parse_date(Some(raw), Some(year));
    if let (Some(dd), Some(pe)) = (d, period_end) {
        if py::char_len(raw) <= 5 && dd > pe + Duration::days(5) {
            return py_date(dd.year() as i64 - 1, dd.month() as i64, dd.day() as i64).map(Some);
        }
    }
    Ok(d)
}

fn contains_any(text: &str, keys: &[&str]) -> bool {
    keys.iter().any(|k| text.contains(k))
}

pub fn parse_pdf(data: &[u8], _filename: &str) -> Result<Vec<ParsedStatement>, String> {
    let err = |e: crate::pdf::Error| match e {
        crate::pdf::Error::Password => String::new(),
        crate::pdf::Error::Invalid(s) => s,
    };
    let pages: Vec<Vec<Word>> = crate::pdf::extract_words(data, 1.5)
        .map_err(err)?
        .into_iter()
        .map(|p| p.into_iter().map(|w| Word { text: w.text, x0: w.x0, x1: w.x1, top: w.top }).collect())
        .collect();
    let text = crate::pdf::extract_text(data).map_err(err)?;
    if py::strip(&text).is_empty() {
        return Err("Ce PDF ne contient pas de texte (document scanné) : saisis le solde à la main".into());
    }

    let mut st = ParsedStatement::new("pdf");
    st.institution = find_institution(&text);
    st.iban = find_iban(&text);
    let ntext = norm(&text);

    if let Some(m) = search(PERIOD_RE, &ntext) {
        st.period_start = parse_date(Some(&m[1]), None);
        st.period_end = parse_date(Some(&m[2]), None);
    }
    let limit = today() + Duration::days(31);
    let full_dates: Vec<NaiveDate> = findall(FULL_DATE_RE, &text).iter().filter_map(|m| parse_date(Some(m), None)).filter(|d| *d <= limit).collect();
    if st.period_end.is_none() {
        st.period_end = full_dates.iter().max().copied();
    }
    let year = st.period_end.unwrap_or_else(today).year() as i64;

    for raw_line in py::splitlines(&text).into_iter().take(80) {
        let line = format!(" {} ", norm(raw_line));
        if contains_any(&line, PRODUCT_WORDS) && py::char_len(raw_line) < 90 {
            st.account_label = Some(py::strip(raw_line).to_string());
            break;
        }
    }
    if let Some(m) = search(r"(?:n°|numero|no\.?)\s*(?:de\s+)?(?:compte|contrat)?\s*:?\s*([0-9][0-9 ]{6,}[0-9])", &ntext) {
        st.account_number = Some(m[1].replace(' ', ""));
    }
    st.account_type = st.account_label.as_ref().map(|l| guess_account_type(l, "checking"));

    let mut cols: Option<Vec<(&'static str, f64)>> = None;
    let mut unsure_signs = 0usize;
    let mut last_tx: Option<usize> = None;
    let mut last_top = 0.0f64;
    let mut label_x = 0.0f64;
    let mut continuation = 0usize;
    let mut holdings: Vec<HoldingData> = vec![];
    let mut candidates: Vec<(String, Option<Decimal>)> = vec![];

    for words in &pages {
        for line in lines(words) {
            let line = merge_amounts(line);
            if let Some(h) = header_columns(&line) {
                cols = Some(h);
                last_tx = None;
                continue;
            }
            let texts: Vec<&str> = line.iter().map(|w| w.text.as_str()).collect();
            let joined = norm(&texts.join(" "));
            let amount_idx: Vec<usize> = (0..line.len()).filter(|&i| is_amount(&line[i].text)).collect();

            if let Some(isin) = search(ISIN_RE, &texts.join(" ")).map(|m| m[1].to_string()) {
                if !amount_idx.is_empty() {
                    let numbers: Vec<Option<Decimal>> = amount_idx.iter().map(|&i| parse_amount(Some(&line[i].text))).collect();
                    let qty = amount_idx.iter().zip(numbers.iter()).find(|(&i, n)| n.is_some() && search(r"[,.]\d{3,}", &line[i].text).is_some()).and_then(|(_, n)| *n);
                    let name = py::take_chars(&texts.iter().filter(|t| **t != isin && !is_amount(t)).copied().collect::<Vec<_>>().join(" "), 120);
                    holdings.push(HoldingData {
                        symbol: None,
                        isin: Some(isin.clone()),
                        name: Some(if name.is_empty() { isin.clone() } else { name }),
                        quantity: qty.filter(|q| !q.is_zero()).unwrap_or(Decimal::ZERO),
                        asset_class: "fund".into(),
                        value: *numbers.last().unwrap(),
                        value_currency: Some("EUR".into()),
                        ..Default::default()
                    });
                    continue;
                }
            }

            if joined.contains("solde") && !amount_idx.is_empty() {
                let w = &line[*amount_idx.last().unwrap()];
                if let Some(mut value) = parse_amount(Some(&w.text)) {
                    if cols.as_ref().map(|c| nearest(c, w.x1) == "debit").unwrap_or(false) || joined.contains("debiteur") {
                        value = py::neg(value.abs());
                    }
                    if contains_any(&joined, OPENING) {
                        st.opening_balance = st.opening_balance.or(Some(value));
                    } else if contains_any(&joined, CLOSING) || joined.contains("solde") {
                        st.balance = Some(value);
                        let found = texts.iter().find(|t| fullmatch(FULL_DATE_RE, t).is_some()).and_then(|t| parse_date(Some(t), None));
                        st.balance_date = found.or(st.period_end);
                    }
                }
                last_tx = None;
                continue;
            }
            if contains_any(&joined, CANDIDATE_KEYS) && !amount_idx.is_empty() {
                let label = py::take_chars(&texts.iter().filter(|t| !is_amount(t)).copied().collect::<Vec<_>>().join(" "), 80);
                candidates.push((label, parse_amount(Some(&line[*amount_idx.last().unwrap()].text))));
            }

            if !line.is_empty() && is_date(&line[0].text) && !amount_idx.is_empty() {
                let Some(when) = resolve_date(&line[0].text, year, st.period_end)? else { continue };
                let mut rest: Vec<usize> = (1..line.len()).filter(|i| !amount_idx.contains(i)).collect();
                while !rest.is_empty() && is_date(&line[rest[0]].text) {
                    rest.remove(0); // value date column
                }
                let label = py::strip(&rest.iter().map(|&i| line[i].text.as_str()).collect::<Vec<_>>().join(" ")).to_string();
                if norm(&label).contains("total") || label.is_empty() {
                    continue;
                }
                let tx_amounts: Vec<(usize, Option<&'static str>)> = match &cols {
                    Some(c) => amount_idx.iter().map(|&i| (i, nearest(c, line[i].x1))).filter(|(_, c)| *c != "balance").map(|(i, c)| (i, Some(c))).collect(),
                    None => vec![(amount_idx[0], None)],
                };
                let Some(&(wi, column)) = tx_amounts.first() else { continue };
                let w = &line[wi];
                let Some(mut value) = parse_amount(Some(&w.text)) else { continue };
                match column {
                    Some("debit") => value = py::neg(value.abs()),
                    Some("credit") => value = value.abs(),
                    _ => {
                        if !w.text.starts_with(['+', '-']) && !w.text.ends_with('-') {
                            unsure_signs += 1;
                            let padded = format!(" {} ", norm(&label));
                            value = if contains_any(&padded, CREDIT_HINTS) { value.abs() } else { py::neg(value.abs()) };
                        }
                    }
                }
                let mut t = TransactionData::new(when, label, value);
                t.currency = st.currency.clone();
                st.transactions.push(t);
                last_tx = Some(st.transactions.len() - 1);
                last_top = line[0].top;
                label_x = rest.first().map(|&i| line[i].x0).unwrap_or(line[0].x1);
                continuation = 0;
            } else if let Some(ti) = last_tx {
                if amount_idx.is_empty() && continuation < 2 && 0.0 < line[0].top - last_top && line[0].top - last_top < 20.0 && line[0].x0 >= label_x - 3.0 {
                    let extra = py::strip(&texts.join(" ")).to_string();
                    if !extra.is_empty() && !is_date(&line[0].text) {
                        let t = &mut st.transactions[ti];
                        t.label = py::take_chars(&format!("{} {}", t.label, extra), 500);
                        last_top = line[0].top;
                        continuation += 1;
                    }
                }
            }
        }
    }

    st.transactions.sort_by_key(|t| t.date);
    if !st.transactions.is_empty() {
        st.period_start = st.period_start.or(Some(st.transactions[0].date));
        st.period_end = st.period_end.or(Some(st.transactions.last().unwrap().date));
    }
    if !holdings.is_empty() && st.transactions.is_empty() {
        st.holdings = Some(holdings);
        let keep = matches!(st.account_type.as_deref(), Some("life_insurance") | Some("pea") | Some("cto") | Some("per"));
        if !keep {
            st.account_type = Some("life_insurance".into());
        }
    }

    // Check: opening balance + operations = closing balance.
    if let (false, Some(o), Some(b)) = (st.transactions.is_empty(), st.opening_balance, st.balance) {
        let total: Decimal = st.transactions.iter().map(|t| t.amount).fold(Decimal::ZERO, |a, x| a + x);
        let gap = b - o - total;
        if gap.abs() < Decimal::from_str("0.01").unwrap() {
            st.confidence = "high".into();
            st.warnings.clear();
        } else {
            st.confidence = "low".into();
            st.warnings.push(format!("Écart de {} entre les soldes et les opérations lues : vérifie les lignes", fmt2(&gap)));
        }
    } else if !st.transactions.is_empty() {
        st.confidence = if unsure_signs > 0 { "low".into() } else { "medium".into() };
    } else {
        st.confidence = "low".into();
    }
    if unsure_signs > 0 && st.confidence != "high" {
        st.warnings.push(format!("Sens (débit/crédit) deviné pour {unsure_signs} opération(s)"));
    }
    if st.transactions.is_empty() && st.balance.is_none() && st.holdings.is_none() {
        let mut seen: Vec<Decimal> = vec![];
        let mut kept: Vec<(String, Decimal)> = vec![];
        for (l, a) in candidates {
            let Some(a) = a else { continue };
            if seen.contains(&a) {
                continue;
            }
            seen.push(a);
            kept.push((l, a));
        }
        kept.truncate(10);
        if kept.is_empty() {
            st.warnings.push("Aucune opération ni montant reconnu : document conservé, saisis le solde à la main si besoin".into());
        }
        st.candidate_balances = kept;
    } else {
        st.candidate_balances = vec![];
    }
    Ok(vec![st])
}
