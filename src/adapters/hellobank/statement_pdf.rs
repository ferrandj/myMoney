//! Hello bank!: the "Relevé de compte chèques" PDF (a BNP Paribas group layout).
//!
//! A title with the period ("du 23 septembre 2022 au 15 octobre 2022"), a block on the left with the RIB, the IBAN
//! and the BIC, the holder and the address on the right (the first name comes before the last name), then the table
//! Date | Nature des opérations | Valeur | Débit | Crédit between "SOLDE CREDITEUR AU <date>" (the opening balance) and
//! the same line again (the closing balance), with "TOTAL DES OPERATIONS" just before. Dates carry no year: it comes
//! from the period. A debit is an amount under the "Débit" heading, a credit one under "Crédit" (both right-aligned).

use rust_decimal::Decimal;
use serde_json::{Map, Value};

use crate::adapters::base::{round2, score, Adapter, Detection, ParseResult, ParsedStatement, SourceDocument, TransactionData, W};
use crate::adapters::boursobank::statement_pdf::last4;
use crate::adapters::common::{find_iban, french_date, name_key, norm, parse_amount, ymd};
use crate::adapters::fortuneo::identity::read_address_block;
use crate::adapters::fortuneo::statement_pdf::{check, resolve, AMOUNT_RE};
use crate::adapters::lines::{describe, is_own, label as make_label};
use crate::adapters::pdftools;
use crate::adapters::re::{fullmatch, py_match, search, sub};
use crate::py;
use crate::services::rates::guess_account_type;

const DAY_MONTH_RE: &str = r"^(\d{2})\.(\d{2})$";
const FULL_DATE_RE: &str = r"(\d{2})\.(\d{2})\.(\d{4})";
const RIGHT_X: f64 = 340.0; // the holder and the address are printed from here
const CIVILITIES: &[&str] = &["M", "MR", "MME", "MLLE", "MONSIEUR", "MADAME", "MADEMOISELLE"];
const ICS_RE: &str = r"^[A-Z]{2}\d{2}ZZZ[A-Z0-9]+$";

struct Entry {
    day: i64,
    month: i64,
    value: Option<(i64, i64)>,
    amount: Decimal,
    first: String,
    cont: Vec<String>,
}

pub struct HelloBankStatementPdf;

fn some(s: String) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

impl Adapter for HelloBankStatementPdf {
    fn id(&self) -> &'static str {
        "hellobank.statement_pdf"
    }
    fn institution(&self) -> Option<&'static str> {
        Some("Hello bank!")
    }
    fn kind(&self) -> &'static str {
        "statement"
    }
    fn label(&self) -> &'static str {
        "Hello bank! · relevé de compte (PDF)"
    }
    fn description(&self) -> &'static str {
        "Le PDF « Relevé de compte chèques » de l'espace client Hello bank!."
    }
    fn formats(&self) -> &'static [&'static str] {
        &["pdf"]
    }

    fn detect(&self, doc: &SourceDocument) -> Result<Detection, String> {
        if doc.format() != "pdf" {
            return Ok(Detection::default());
        }
        let c = doc.compact()?;
        let hello = c.contains("hellobank");
        let (mut s, reasons) = score(&[
            (hello, 0.3, "mentionne Hello bank!"),
            (c.contains("relevedecompte"), 0.25, "titre « Relevé de compte »"),
            (c.contains("soldecrediteurau") || c.contains("soldedebiteurau"), 0.2, "« Solde créditeur / débiteur au »"),
            (c.contains("totaldesoperations"), 0.15, "« Total des opérations »"),
            (c.contains("valeur") && c.contains("debit") && c.contains("credit"), 0.1, "colonnes Valeur, Débit et Crédit"),
        ]);
        if !hello {
            s = s.min(0.3);
        }
        Ok(Detection { score: round2(s.min(1.0)), reasons })
    }

    fn parse(&self, doc: &SourceDocument) -> ParseResult {
        let mut st = ParsedStatement::new("pdf");
        st.institution = Some("Hello bank!".into());
        st.adapter_id = Some(self.id().into());
        st.document_kind = Some(self.kind().into());
        st.account_type = Some("checking".into());
        let mut entries: Vec<Entry> = vec![];
        let mut current: Option<usize> = None;
        let (mut boundary, mut amount_x, mut label_x, mut value_x) = (498.0f64, 352.0f64, 74.0f64, (310.0f64, 352.0f64));
        let mut declared: Map<String, Value> = Map::new();
        let mut declared_d: Vec<(String, Decimal)> = vec![];
        let mut opening_seen = false;
        let mut closing_seen = false;

        let pages = doc.words(1.0)?;
        for (page_no, words) in pages.iter().enumerate() {
            let rows: Vec<Vec<W>> = pdftools::lines(words, 3.0, true).into_iter().filter(|r| !r.is_empty()).collect();
            let texts: Vec<String> = rows.iter().map(|r| pdftools::row_text(r)).collect();
            if page_no == 0 {
                read_header(&mut st, &rows, &texts);
            }
            let mut in_table = false;
            for (row, text) in rows.iter().zip(texts.iter()) {
                let tokens: Vec<&str> = row.iter().map(|w| w.text.as_str()).collect();
                let compact = norm(&tokens.concat()).replace(' ', "");
                let normed: Vec<String> = tokens.iter().map(|t| norm(t)).collect();
                if ["date", "valeur", "debit", "credit"].iter().all(|k| normed.iter().any(|t| t == k)) {
                    let pick = |k: &str| row.iter().find(|w| norm(&w.text) == k).unwrap();
                    // Amounts are right-aligned: the middle of the two headings' right edges splits debit and credit.
                    boundary = (pick("debit").x1 + pick("credit").x1) / 2.0;
                    value_x = (pick("valeur").x0 - 10.0, pick("valeur").x1 + 10.0);
                    amount_x = pick("valeur").x1 + 5.0;
                    if let Some(n) = row.iter().find(|w| norm(&w.text) == "nature") {
                        label_x = n.x0;
                    }
                    in_table = true;
                    continue;
                }
                if !in_table || closing_seen {
                    continue;
                }
                let amount_idx: Vec<usize> = (0..row.len()).filter(|&i| fullmatch(AMOUNT_RE, &row[i].text).is_some() && row[i].x0 > amount_x).collect();
                if compact.starts_with("soldecrediteurau") || compact.starts_with("soldedebiteurau") {
                    current = None;
                    let Some(&i) = amount_idx.last() else { continue };
                    let value = py::or_zero(parse_amount(Some(&row[i].text))).abs();
                    let signed = if compact.starts_with("soldedebiteur") { py::neg(value) } else { value };
                    let when = search(FULL_DATE_RE, text).and_then(|m| ymd(m[3].parse().unwrap(), m[2].parse().unwrap(), m[1].parse().unwrap()));
                    if !opening_seen {
                        opening_seen = true;
                        st.opening_balance = Some(signed);
                        st.opening_date = when;
                    } else {
                        st.balance = Some(signed);
                        st.balance_date = when.or(st.period_end);
                        closing_seen = true;
                        in_table = false;
                    }
                } else if compact.starts_with("totaldesoperations") {
                    current = None;
                    for &i in &amount_idx {
                        let key = if row[i].x1 > boundary { "credit" } else { "debit" };
                        let v = py::or_zero(parse_amount(Some(&row[i].text)));
                        declared.insert(key.into(), Value::String(py::dec_str(&v)));
                        match declared_d.iter_mut().find(|(k, _)| k == key) {
                            Some(e) => e.1 = v,
                            None => declared_d.push((key.into(), v)),
                        }
                    }
                } else if tokens.len() >= 2 && py_match(DAY_MONTH_RE, tokens[0]).is_some() && row[0].x0 < 60.0 {
                    if amount_idx.is_empty() {
                        continue;
                    }
                    let m = py_match(DAY_MONTH_RE, tokens[0]).unwrap();
                    let (day, month): (i64, i64) = (m[1].parse().unwrap(), m[2].parse().unwrap());
                    let value_idx = (1..row.len()).find(|&i| py_match(DAY_MONTH_RE, &row[i].text).is_some() && row[i].x0 >= value_x.0 && row[i].x1 <= value_x.1);
                    let value = value_idx.map(|i| {
                        let m = py_match(DAY_MONTH_RE, &row[i].text).unwrap();
                        (m[1].parse().unwrap(), m[2].parse().unwrap())
                    });
                    let first = py::strip(&(1..row.len()).filter(|i| !amount_idx.contains(i) && Some(*i) != value_idx).map(|i| row[i].text.as_str()).collect::<Vec<_>>().join(" ")).to_string();
                    let a0 = &row[amount_idx[0]];
                    let amount = py::or_zero(parse_amount(Some(&a0.text))).abs();
                    let signed = if a0.x1 > boundary { amount } else { py::neg(amount) };
                    entries.push(Entry { day, month, value, amount: signed, first, cont: vec![] });
                    current = Some(entries.len() - 1);
                } else if let Some(c) = current {
                    let skipped = ["page", "suite", "report"].iter().any(|k| compact.starts_with(k));
                    if row[0].x0 >= label_x - 5.0 && amount_idx.is_empty() && !skipped {
                        let extra = py::strip(&tokens.join(" ")).to_string();
                        if !extra.is_empty() {
                            entries[c].cont.push(extra);
                        }
                    }
                }
            }
        }

        if st.period_start.is_none() {
            st.period_start = st.opening_date;
        }
        st.period_end = st.period_end.or(st.balance_date);
        let holder_key = st.holder_name.as_deref().filter(|s| !s.is_empty()).map(name_key);
        st.transactions = entries.iter().filter_map(|e| to_transaction(e, &st, holder_key.as_deref())).collect();
        check(&mut st, &declared, &declared_d);
        Ok(vec![st])
    }
}

/// "M PAUL MARTIN" (first name first) as "M MARTIN PAUL", the order the rest of the application reads. Anything
/// that is not a civility and two words (a joint holder, a compound name) is left as printed: it is asked once.
fn last_name_first(name: &str) -> String {
    let words = py::split_ws(name);
    let (civility, rest): (Option<&str>, &[&str]) = match words.first() {
        Some(w) if CIVILITIES.contains(&py::upper(w.trim_end_matches('.')).as_str()) => (Some(words[0]), &words[1..]),
        _ => (None, &words[..]),
    };
    let joint = rest.iter().any(|w| matches!(py::upper(w).as_str(), "OU" | "ET"));
    if rest.len() != 2 || joint {
        return py::strip(name).to_string();
    }
    let mut out: Vec<&str> = civility.into_iter().collect();
    out.extend([rest[1], rest[0]]);
    out.join(" ")
}

fn read_header(st: &mut ParsedStatement, rows: &[Vec<W>], texts: &[String]) {
    let left = |row: &Vec<W>| row.iter().filter(|w| w.x0 < RIGHT_X).map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
    let (mut rib_row, mut bic_row) = (None, None);
    for (i, row) in rows.iter().enumerate() {
        let l = left(row);
        if let Some(m) = search(r"\bRIB\s*:\s*(\d{5})\s+(\d{5})\s+([0-9A-Z]{11})\s+(\d{2})\b", &l) {
            st.bank_code = Some(m[1].to_string());
            st.branch_code = Some(m[2].to_string());
            st.account_number = Some(m[3].to_string());
            st.rib_key = Some(m[4].to_string());
            rib_row = Some(i);
        }
        if l.contains("IBAN") {
            st.iban = find_iban(&l).or(st.iban.take());
        }
        if let Some(m) = search(r"\bBIC\s*:\s*([A-Z]{6}[A-Z0-9]{2}(?:[A-Z0-9]{3})?)\b", &l) {
            st.bic = Some(m[1].to_string());
            bic_row = Some(i);
        }
    }
    if let (Some(r), Some(b)) = (rib_row, bic_row) {
        if r > 0 && b >= r {
            let (name, address) = read_address_block(rows, r - 1, b + 1, RIGHT_X);
            if let Some(name) = name {
                st.set_extra("holder_printed", name.clone());
                st.holder_name = Some(last_name_first(&name));
            }
            st.holder_address = address;
        }
    }
    // "RELEVE DE COMPTE CHEQUES": what the account is
    let mut product = "Compte".to_string();
    for text in texts {
        let n = norm(text);
        if let Some(rest) = n.strip_prefix("releve de compte") {
            let rest = py::strip(&sub(r"\bp\.?\s*\d+\s*/\s*\d+.*$", "", rest)).to_string();
            if rest.contains("cheque") {
                product = "Compte chèques".into();
            } else if !rest.is_empty() {
                product = format!("Compte {rest}");
                st.account_type = Some(guess_account_type(&rest, "checking"));
            }
            break;
        }
    }
    for text in texts {
        if let Some(m) = search(r"\bdu\s+(\d{1,2}(?:er)?\s+\S+\s+\d{4})\s+au\s+(\d{1,2}(?:er)?\s+\S+\s+\d{4})", text) {
            st.period_start = french_date(Some(&m[1]));
            st.period_end = french_date(Some(&m[2]));
        }
        if norm(text).starts_with("montant de votre autorisation de decouvert") {
            if let Some(m) = search(r":\s*(\d[\d ]*,\d{2})", text) {
                st.set_extra("overdraft_limit", parse_amount(Some(&m[1])).map(|d| py::dec_str(&d)).unwrap_or_else(|| "None".into()));
            }
        }
    }
    st.set_extra("product", product.clone());
    if let Some(n) = &st.account_number {
        st.account_label = Some(format!("{product} Hello bank …{}", last4(n)));
    }
}

/// The `/TAG value` pieces of a transfer or direct-debit label, and what comes before the first one.
fn tagged(text: &str) -> (String, Vec<(String, String)>) {
    let mut pieces = text.split(" /");
    let head = py::strip(pieces.next().unwrap_or("")).to_string();
    let tags = pieces
        .map(|p| {
            let p = py::strip(p.trim_start_matches('/'));
            match p.split_once(char::is_whitespace) {
                Some((tag, value)) => (py::upper(tag), py::strip(value).to_string()),
                None => (py::upper(p), String::new()),
            }
        })
        .collect();
    (head, tags)
}

fn to_transaction(e: &Entry, st: &ParsedStatement, holder_key: Option<&str>) -> Option<TransactionData> {
    let when = resolve(e.day, e.month, st)?;
    let value_date = e.value.and_then(|(d, m)| resolve(d, m, st));
    let first = py::strip(&e.first).to_string();
    let full = std::iter::once(first.clone()).chain(e.cont.iter().cloned()).collect::<Vec<_>>().join(" ");
    let raw = std::iter::once(first.clone()).chain(e.cont.iter().cloned()).collect::<Vec<_>>().join(" | ");
    let (head, tags) = tagged(&full);
    let tag = |names: &[&str]| tags.iter().find(|(t, v)| names.contains(&t.as_str()) && !v.is_empty()).map(|(_, v)| v.clone());
    let mut details: Map<String, Value> = Map::new();
    let mut kind = "other".to_string();
    let mut counterparty: Option<String> = None;
    let mut reference: Option<String> = None;
    let mut memo: Option<String> = None;
    let label;

    if let Some(m) = py_match(r"(?i)^(?:PAIEMENT\s+|ACHAT\s+)?CARTE\s+(?:X?\d+\s+)?(?:(\d{2}/\d{2})\s+)?(.*)$", &first) {
        kind = "card".into();
        counterparty = some(py::strip(&m[2]).to_string());
        let bought = m.get(1).and_then(|d| {
            let p: Vec<i64> = d.as_str().split('/').map(|x| x.parse().unwrap()).collect();
            resolve(p[0], p[1], st)
        });
        details.insert("purchase_date".into(), bought.map(|b| Value::String(b.format("%Y-%m-%d").to_string())).unwrap_or(Value::Null));
        memo = some(e.cont.join(" "));
        label = make_label(counterparty.as_deref(), memo.as_deref());
    } else if py_match(r"(?i)^VIR(?:EMENT)?\b", &head).is_some() {
        kind = if py::upper(&head).contains("INST") { "instant_transfer".into() } else { "transfer".into() };
        counterparty = tag(&["FRM", "DE", "TO", "BEN", "DON", "BENEF", "DONNEUR", "A"]).and_then(|n| some(n));
        reference = tag(&["REF", "EID"]);
        let words = tag(&["MOTIF", "LIB", "RNF"]);
        memo = words.clone();
        label = make_label(Some(counterparty.as_deref().unwrap_or("Virement")), describe(&words.into_iter().collect::<Vec<_>>()).as_deref());
    } else if py_match(r"(?i)^PR[EÉ]?LV\b|^PR[EÉ]L[EÈ]VEMENT\b", &head).is_some() {
        kind = "direct_debit".into();
        let who = py::strip(&sub(r"(?i)^PR[EÉ]?L(?:V|[EÈ]VEMENT)\b\s*(?:SEPA\s+)?", "", &head)).to_string();
        counterparty = some(who).or_else(|| tag(&["CRE", "CREANCIER", "BEN"]));
        let ics = tags.iter().map(|(t, v)| format!("{t} {v}")).chain(tags.iter().map(|(_, v)| v.clone())).find_map(|c| c.split_whitespace().find(|w| py_match(ICS_RE, w).is_some()).map(str::to_string));
        details.insert("ics".into(), ics.map(Value::String).unwrap_or(Value::Null));
        reference = tag(&["REF", "MDT", "RUM"]);
        memo = tag(&["MOTIF", "LIB", "RNF"]);
        label = make_label(counterparty.as_deref(), memo.as_deref());
    } else if py_match(r"(?i)^RETRAIT\b", &first).is_some() {
        kind = "cash".into();
        memo = some(e.cont.join(" "));
        label = make_label(Some(&first), memo.as_deref());
    } else if py_match(r"(?i)^(COTISATION|FRAIS|COMMISSION|ABONNEMENT|AGIOS)\b", &first).is_some() {
        kind = "fee".into();
        memo = some(e.cont.join(" "));
        label = make_label(Some(&first), memo.as_deref());
    } else if py_match(r"(?i)^INT[EÉ]R[EÊ]TS?\b", &first).is_some() {
        kind = "interest".into();
        memo = some(e.cont.join(" "));
        label = make_label(Some(&first), memo.as_deref());
    } else if let Some(m) = py_match(r"(?i)^(?:REMISE\s+)?CH(?:Q|[EÈ]QUE)\b\s*(.*)$", &first) {
        kind = "cheque".into();
        reference = py::split_ws(&m[1]).first().map(|s| s.to_string());
        let what = if py::upper(&first).starts_with("REMISE") { "Remise de chèque" } else { "Chèque" };
        let n = reference.as_ref().map(|r| format!("n° {r}"));
        label = make_label(Some(what), n.as_deref());
    } else {
        memo = some(e.cont.join(" "));
        label = make_label(Some(&first), memo.as_deref());
    }
    if (kind == "transfer" || kind == "instant_transfer") && is_own(counterparty.as_deref(), holder_key) {
        details.insert("internal".into(), Value::Bool(true));
    }
    let details: Map<String, Value> = details.into_iter().filter(|(_, v)| !v.is_null()).collect();
    let mut t = TransactionData::new(when, if label.is_empty() { first.clone() } else { label }, e.amount);
    t.currency = st.currency.clone();
    t.value_date = value_date;
    t.kind = Some(kind);
    t.counterparty = counterparty;
    t.reference = reference;
    t.memo = memo;
    t.raw = Some(raw);
    t.details = if details.is_empty() { None } else { Some(details) };
    Some(t)
}
