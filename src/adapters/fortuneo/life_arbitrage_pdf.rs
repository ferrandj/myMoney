//! Fortuneo Vie (contract managed by Suravenir): the confirmation of an arbitrage.
//!
//! A document to keep, not to import: it says which funds were sold and which were bought, for the same amount. It
//! changes no cash balance, so it is recognised, dated and filed with the holder's other papers; the amounts and the
//! funds are kept and shown. The sums are checked (what was bought adds up to the net amount), which is what the
//! reliability reflects.
//!
//! Some of these PDFs lose their spaces ("Montantbrutdel'arbitrage"), so the markers are looked for in the text with
//! its spaces and punctuation removed.

use std::str::FromStr;

use chrono::{Datelike, NaiveDate};
use rust_decimal::Decimal;
use serde_json::{Map, Value};

use super::fees_pdf::amount_str;
use crate::adapters::base::{round2, score, Adapter, Detection, ParseResult, ParsedStatement, SourceDocument};
use crate::adapters::common::{french_date, norm, parse_amount};
use crate::adapters::re::{py_match, search, sub};
use crate::py;

const MONTH: &str = r"(janvier|fevrier|mars|avril|mai|juin|juillet|aout|septembre|octobre|novembre|decembre)";
const ISIN: &str = r"[A-Z]{2}[A-Z0-9]{9}\d";

fn line_re() -> String {
    // fund (no spaces guaranteed), ISIN (the euro fund has none), valuation date, units, unit value, amount
    format!(
        r"^(?P<name>\S.*?)\s+(?:(?P<isin>{ISIN})\s+)?(?P<day>\d{{2}}/\d{{2}}/\d{{4}})\s+(?P<units>\d[\d ]*,\d+)\s+(?P<price>\d[\d ]*,\d+)\s+(?P<amount>\d[\d ]*,\d{{2}})$"
    )
}

fn squash(text: &str) -> String {
    sub(r"[^a-z0-9,]", "", &norm(text).replace(' ', ""))
}

fn iso(day: &str) -> String {
    let p: Vec<&str> = day.split('/').collect();
    format!("{}-{}-{}", p[2], p[1], p[0])
}

pub struct FortuneoLifeArbitragePdf;

impl Adapter for FortuneoLifeArbitragePdf {
    fn id(&self) -> &'static str {
        "fortuneo.life_arbitrage_pdf"
    }
    fn institution(&self) -> Option<&'static str> {
        Some("Fortuneo")
    }
    fn kind(&self) -> &'static str {
        "info"
    }
    fn label(&self) -> &'static str {
        "Fortuneo Vie · confirmation d'arbitrage (PDF)"
    }
    fn description(&self) -> &'static str {
        "La lettre de l'assureur (Suravenir) qui confirme un arbitrage sur ton contrat d'assurance-vie : fonds vendus et achetés."
    }
    fn formats(&self) -> &'static [&'static str] {
        &["pdf"]
    }

    fn detect(&self, doc: &SourceDocument) -> Result<Detection, String> {
        if doc.format() != "pdf" {
            return Ok(Detection::default());
        }
        let c = sub(r"[^a-z0-9]", "", doc.compact()?);
        let (mut s, reasons) = score(&[
            (c.contains("fortuneo"), 0.25, "mentionne Fortuneo"),
            (c.contains("confirmationdarbitrage"), 0.45, "« Confirmation d'arbitrage »"),
            (c.contains("suravenir"), 0.15, "mentionne Suravenir"),
            (c.contains("desinvestissement") && c.contains("reinvestissement"), 0.15, "fonds désinvestis puis réinvestis"),
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
        st.confidence = "high".into();
        let squashed = squash(doc.text()?);

        let long_date = |prefix: &str| -> Option<NaiveDate> {
            let m = search(&format!(r"{prefix}(\d{{1,2}}){MONTH}(\d{{4}})"), &squashed)?;
            french_date(Some(&format!("{} {} {}", &m[1], &m[2], &m[3])))
        };
        let amount = |label: &str| -> Option<Decimal> {
            let m = search(&format!(r"{label}:?(\d+,\d{{2}})"), &squashed)?;
            parse_amount(Some(&m[1]))
        };

        let text = doc.text()?.replace('\u{a0}', " ");
        let letter = long_date("le");
        if let Some(l) = letter {
            st.balance_date = Some(l);
            st.set_extra("issued_on", l.format("%Y-%m-%d").to_string());
            st.set_extra("year", l.year());
        }
        if let Some(d) = long_date("datedelarbitrage") {
            st.set_extra("arbitrage_date", d.format("%Y-%m-%d").to_string());
        }
        if let Some(m) = search(r"Contrat\s*:\s*([^\n]+)", &text) {
            st.set_extra("contract", py::strip(&m[1]).to_string());
        }
        if let Some(m) = search(r"N\s*°\s*:?\s*(\d{8,})", &text) {
            st.set_extra("contract_number", m[1].to_string());
        }
        let (gross, fees, net) = (amount("montantbrutdelarbitrage"), amount("fraisdarbitrage"), amount("montantnet"));
        for (key, value) in [("gross", gross), ("fees", fees), ("net", net)] {
            if let Some(v) = value {
                st.set_extra(key, py::dec_str(&v));
            }
        }

        let mut sold: Vec<Map<String, Value>> = vec![];
        let mut bought: Vec<Map<String, Value>> = vec![];
        let mut target: Option<bool> = None; // Some(true): sold
        let lre = line_re();
        for raw in py::splitlines(&text) {
            let line = py::strip(raw);
            let key = squash(line);
            if py::char_len(&key) < 30 && key.contains("desinvestissement") {
                // a bullet may precede the heading
                target = Some(true);
            } else if py::char_len(&key) < 30 && key.contains("reinvestissement") {
                target = Some(false);
            } else if let (Some(t), Some(m)) = (target, py_match(&lre, line)) {
                let mut e = Map::new();
                e.insert("fund".into(), py::strip(&m["name"]).to_string().into());
                e.insert("isin".into(), m.name("isin").map(|x| Value::String(x.as_str().to_string())).unwrap_or(Value::Null));
                e.insert("date".into(), iso(&m["day"]).into());
                e.insert("units".into(), amount_str(&m["units"]).into());
                e.insert("unit_value".into(), amount_str(&m["price"]).into());
                e.insert("amount".into(), amount_str(&m["amount"]).into());
                if t {
                    sold.push(e);
                } else {
                    bought.push(e);
                }
            }
        }
        let total = |xs: &[Map<String, Value>]| -> Decimal {
            xs.iter().filter_map(|x| x.get("amount").and_then(|a| a.as_str()).and_then(py::dec_parse)).fold(Decimal::ZERO, |a, b| a + b)
        };
        let (bought_total, sold_total) = (total(&bought), total(&sold));
        st.set_extra("sold", Value::Array(sold.iter().cloned().map(Value::Object).collect()));
        st.set_extra("bought", Value::Array(bought.iter().cloned().map(Value::Object).collect()));

        let what = match st.extra.get("contract") {
            Some(c) if crate::models::truthy(c) => format!("Confirmation d'arbitrage · {}", c.as_str().unwrap_or("")),
            _ => "Confirmation d'arbitrage".into(),
        };
        st.set_extra("title", what);
        st.set_extra("category", "Assurance-vie");
        let mut problems: Vec<String> = vec![];
        let tolerance = Decimal::from_str("0.05").unwrap();
        if sold.is_empty() || bought.is_empty() {
            problems.push("Les fonds vendus ou achetés n'ont pas pu être lus en entier".into());
        }
        if let Some(n) = net {
            if !bought.is_empty() && (bought_total - n).abs() > tolerance {
                problems.push(format!("Les fonds achetés ({}) ne correspondent pas au montant net annoncé ({})", py::dec_str(&bought_total), py::dec_str(&n)));
            }
            if !sold.is_empty() && (sold_total - n).abs() > tolerance {
                problems.push(format!("Les fonds vendus ({}) ne correspondent pas au montant net annoncé ({})", py::dec_str(&sold_total), py::dec_str(&n)));
            }
        }
        if net.is_none() || letter.is_none() {
            problems.push("Le montant ou la date de la lettre n'a pas été trouvé".into());
        }
        let has_problems = !problems.is_empty();
        st.warnings.extend(problems);
        if has_problems {
            st.confidence = if !(!sold.is_empty() && !bought.is_empty()) { "low".into() } else { "medium".into() };
        }
        Ok(vec![st])
    }
}
