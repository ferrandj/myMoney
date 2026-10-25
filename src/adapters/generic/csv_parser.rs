//! CSV exports: bank statements (any bank, columns detected automatically), Revolut, and position lists.

use std::collections::BTreeSet;

use rust_decimal::Decimal;

use super::pycsv;
use crate::adapters::base::{decode_text, HoldingData, ParsedStatement, TransactionData};
use crate::adapters::common::{find_iban, norm, parse_amount, parse_date};
use crate::py;
use crate::services::rates::guess_account_type;

pub const COLUMNS: &[(&str, &[&str])] = &[
    (
        "date",
        &[
            "date operation", "date de l'operation", "date d'operation", "date comptable", "completed date", "booking date", "date", "dateop", "date op",
            "date de comptabilisation", "date valeur", "value date", "started date", "date/heure", "date / time",
        ],
    ),
    (
        "label",
        &[
            "libelle", "libelle operation", "libelle de l'operation", "description", "label", "detail", "details", "operation", "intitule", "wording",
            "payee", "beneficiaire", "reference", "nature de l'operation", "libelle simplifie",
        ],
    ),
    ("amount", &["montant", "amount", "montant (eur)", "montant(euros)", "montant en euros", "montant eur", "valeur"]),
    ("debit", &["debit", "debit (eur)", "montant debit", "debit euros", "sortie", "withdrawal"]),
    ("credit", &["credit", "credit (eur)", "montant credit", "credit euros", "entree", "deposit"]),
    ("currency", &["devise", "currency", "monnaie"]),
    ("balance", &["solde", "balance", "solde (eur)"]),
    ("fee", &["fee", "frais"]),
    ("state", &["state", "statut", "etat"]),
    ("symbol", &["symbol", "symbole", "ticker", "asset", "actif", "crypto", "coin"]),
    ("isin", &["isin", "code isin"]),
    ("quantity", &["quantity", "quantite", "qty", "nombre", "parts", "units"]),
    ("name", &["name", "nom", "libelle valeur", "support"]),
    ("value", &["value", "valorisation", "montant total", "market value"]),
];

/// The columns found in a header row: (key, index), in the order of `COLUMNS`.
#[derive(Default, Clone, Debug)]
pub struct Columns(Vec<(&'static str, usize)>);

impl Columns {
    pub fn get(&self, key: &str) -> Option<usize> {
        self.0.iter().find(|(k, _)| *k == key).map(|(_, i)| *i)
    }
    pub fn has(&self, key: &str) -> bool {
        self.get(key).is_some()
    }
}

/// `str.strip(" :")`.
fn strip_space_colon(s: &str) -> &str {
    s.trim_matches(|c| c == ' ' || c == ':')
}

fn match_columns(header: &[String]) -> Columns {
    let mut found: Vec<(&'static str, usize)> = vec![];
    let normalized: Vec<String> = header.iter().map(|h| strip_space_colon(&norm(h)).to_string()).collect();
    for (key, names) in COLUMNS {
        for name in *names {
            // names are ordered by preference
            if let Some(idx) = normalized.iter().position(|n| n == name) {
                if !found.iter().any(|(_, i)| *i == idx) {
                    found.push((key, idx));
                    break;
                }
            }
        }
    }
    Columns(found)
}

type Located = (Vec<Vec<String>>, Option<usize>, Columns);

/// Rows of the file, index of the header row and the columns it names (None if it is not a statement).
fn locate(data: &[u8]) -> Result<Located, String> {
    let text = decode_text(data);
    let sample: String = text.chars().take(5000).collect();
    let delimiter = match pycsv::sniff_delimiter(&sample, ";,\t|") {
        Some(d) => d,
        None => {
            if sample.matches(';').count() > sample.matches(',').count() {
                ';'
            } else {
                ','
            }
        }
    };
    let rows: Vec<Vec<String>> = pycsv::reader(&text, delimiter)?.into_iter().filter(|row| row.iter().any(|c| !py::strip(c).is_empty())).collect();
    for (i, row) in rows.iter().take(30).enumerate() {
        let c = match_columns(row);
        if (c.has("date") && (c.has("amount") || c.has("debit") || c.has("credit"))) || (c.has("quantity") && (c.has("symbol") || c.has("isin"))) {
            return Ok((rows, Some(i), c));
        }
    }
    Ok((rows, None, Columns::default()))
}

pub fn looks_like_statement(data: &[u8]) -> Result<bool, String> {
    Ok(locate(data)?.1.is_some())
}

fn cell<'a>(row: &'a [String], columns: &Columns, key: &str) -> Option<&'a str> {
    let idx = columns.get(key)?;
    row.get(idx).map(|c| py::strip(c))
}

fn or_default<'a>(v: Option<&'a str>, default: &'a str) -> &'a str {
    match v {
        Some(s) if !s.is_empty() => s,
        _ => default,
    }
}

pub fn parse_csv(data: &[u8], filename: &str) -> Result<Vec<ParsedStatement>, String> {
    let (rows, header_index, columns) = locate(data)?;
    let Some(header_index) = header_index else {
        return Err("Colonnes non reconnues : il faut au moins une date et un montant (ou un symbole et une quantité)".into());
    };

    let preamble = rows[..header_index].iter().map(|r| r.join(";")).collect::<Vec<_>>().join("\n");
    let mut st = ParsedStatement::new("csv");
    st.iban = if preamble.is_empty() { None } else { find_iban(&preamble) };
    let header_norm: Vec<String> = rows[header_index].iter().map(|h| norm(h)).collect();
    let is_revolut = header_norm.iter().any(|h| h == "completed date") && header_norm.iter().any(|h| h == "product");
    if is_revolut {
        st.institution = Some("Revolut".into());
    }

    let body = &rows[header_index + 1..];
    if !columns.has("date") {
        return Ok(vec![parse_holdings(st, body, &columns)?]);
    }

    let mut balances: Vec<(chrono::NaiveDate, Decimal)> = vec![];
    let mut currencies: BTreeSet<String> = BTreeSet::new();
    let mut skipped = 0usize;
    for row in body {
        let Some(when) = parse_date(cell(row, &columns, "date"), None) else {
            skipped += 1;
            continue;
        };
        if is_revolut {
            let state = py::upper(cell(row, &columns, "state").unwrap_or(""));
            if state != "COMPLETED" && !state.is_empty() {
                continue;
            }
        }
        let amount = if columns.has("amount") {
            parse_amount(cell(row, &columns, "amount"))
        } else {
            let debit = py::or_zero(parse_amount(cell(row, &columns, "debit")));
            let credit = py::or_zero(parse_amount(cell(row, &columns, "credit")));
            Some(credit - debit.abs())
        };
        let Some(mut amount) = amount else {
            skipped += 1;
            continue;
        };
        if let Some(fee) = parse_amount(cell(row, &columns, "fee")) {
            if !fee.is_zero() {
                amount -= fee.abs();
            }
        }
        let currency = py::upper(or_default(cell(row, &columns, "currency"), "EUR"));
        currencies.insert(currency.clone());
        let mut t = TransactionData::new(when, or_default(cell(row, &columns, "label"), "Opération"), amount);
        t.currency = currency;
        st.transactions.push(t);
        if let Some(balance) = parse_amount(cell(row, &columns, "balance")) {
            balances.push((when, balance));
        }
    }

    if st.transactions.is_empty() {
        return Err("Aucune opération lisible dans ce fichier".into());
    }
    st.transactions.sort_by_key(|t| t.date); // stable, like Python's sort
    st.period_start = Some(st.transactions[0].date);
    st.period_end = Some(st.transactions.last().unwrap().date);
    if currencies.len() == 1 {
        st.currency = currencies.into_iter().next().unwrap();
    } else if currencies.len() > 1 {
        st.warnings.push("Plusieurs devises dans le fichier : les montants sont gardés dans leur devise d'origine".into());
    }
    if !balances.is_empty() {
        // Exports are sorted oldest-first or newest-first: the balance to keep is on the newest line.
        let ascending = balances[0].0 <= balances.last().unwrap().0;
        let (d, b) = if ascending { *balances.last().unwrap() } else { balances[0] };
        st.balance_date = Some(d);
        st.balance = Some(b);
    }
    if skipped > 0 {
        st.warnings.push(format!("{skipped} ligne(s) ignorée(s) (date ou montant illisible)"));
    }
    st.account_label = if filename.is_empty() { None } else { Some(filename.rsplit_once('.').map(|(a, _)| a).unwrap_or(filename).to_string()) };
    st.account_type = Some(guess_account_type(st.account_label.as_deref().unwrap_or(""), "checking"));
    st.confidence = "high".into();
    Ok(vec![st])
}

fn parse_holdings(mut st: ParsedStatement, body: &[Vec<String>], columns: &Columns) -> Result<ParsedStatement, String> {
    let mut holdings = vec![];
    for row in body {
        let quantity = parse_amount(cell(row, columns, "quantity"));
        let (symbol, isin) = (cell(row, columns, "symbol").filter(|s| !s.is_empty()), cell(row, columns, "isin").filter(|s| !s.is_empty()));
        let Some(quantity) = quantity else { continue };
        if symbol.is_none() && isin.is_none() {
            continue;
        }
        let value = parse_amount(cell(row, columns, "value"));
        let name = match cell(row, columns, "name") {
            Some(n) if !n.is_empty() => Some(n.to_string()),
            _ => symbol.or(isin).map(|s| s.to_string()),
        };
        holdings.push(HoldingData {
            symbol: symbol.map(py::upper),
            isin: isin.map(py::upper),
            name,
            quantity,
            asset_class: if isin.is_some() { "stock".into() } else { "crypto".into() },
            value,
            value_currency: if value.is_some() { Some(py::upper(or_default(cell(row, columns, "currency"), "EUR"))) } else { None },
            ..Default::default()
        });
    }
    if holdings.is_empty() {
        return Err("Aucune position lisible dans ce fichier".into());
    }
    st.account_type = Some(if holdings.iter().all(|h| h.asset_class == "crypto") { "crypto_exchange".into() } else { "cto".into() });
    st.holdings = Some(holdings);
    st.confidence = "high".into();
    Ok(st)
}
