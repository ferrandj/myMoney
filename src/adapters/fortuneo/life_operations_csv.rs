//! Fortuneo Vie: the CSV "Opérations" export of a life-insurance contract (Date;Libellé;Type;État;Montant (€)).
//!
//! The file names neither the contract nor the bank, so the account is the only life-insurance account of the bank
//! (several are asked about). What is an operation of the contract is imported; what is not is kept apart:
//!
//! * payments (Versement, Adhésion) come in as credits of the contract, marked as moves between the user's own
//!   accounts (the money comes from one of them), and are paired with the debit of the other account when it exists;
//! * distributions and revalorisations of the euro fund are interest, the social contributions taken on them and the
//!   yearly management fees are debits;
//! * arbitrages move money between the funds of the contract: no money enters or leaves, they are not operations (the
//!   amounts are kept in the reading, and the confirmation letters tell which funds were sold and bought);
//! * set-up lines (Paramétrage) have no amount, and the fees of the running year, still without a state, are
//!   provisional: they will come back with their final amount in the next export.
//!
//! Running the same export again, or a later one, adds only the lines that are new.

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde_json::{json, Map, Value};

use crate::adapters::base::{round2, score, Adapter, Detection, ParseResult, ParsedStatement, SourceDocument, TransactionData};
use crate::adapters::common::{norm, parse_amount, parse_date};
use crate::adapters::generic::pycsv;
use crate::py;

const EXPECTED: &[&str] = &["date", "libelle", "type", "etat", "montant (€)"];
const KNOWN_TYPES: &[&str] = &["versement", "arbitrage", "distribution", "revalorisation", "prelevement", "frais", "parametrage"];

pub struct FortuneoLifeOperationsCsv;

fn rows(doc: &SourceDocument) -> Result<Vec<Vec<String>>, String> {
    let text = doc.text()?.trim_start_matches('\u{feff}');
    pycsv::reader(text, ';')
}

fn header_ok(row: &[String]) -> bool {
    let header: Vec<String> = row.iter().map(|c| norm(c)).collect();
    EXPECTED.iter().all(|e| header.iter().any(|h| h == e))
}

impl Adapter for FortuneoLifeOperationsCsv {
    fn id(&self) -> &'static str {
        "fortuneo.life_operations_csv"
    }
    fn institution(&self) -> Option<&'static str> {
        Some("Fortuneo")
    }
    fn kind(&self) -> &'static str {
        "statement"
    }
    fn label(&self) -> &'static str {
        "Fortuneo Vie · opérations du contrat (CSV)"
    }
    fn description(&self) -> &'static str {
        "L'export CSV des opérations d'un contrat d'assurance-vie Fortuneo Vie : versements, distributions, revalorisations, prélèvements, frais (les arbitrages sont notés à part)."
    }
    fn formats(&self) -> &'static [&'static str] {
        &["csv"]
    }

    fn detect(&self, doc: &SourceDocument) -> Result<Detection, String> {
        if doc.format() != "csv" {
            return Ok(Detection::default());
        }
        let all = rows(doc)?;
        let Some(first) = all.first() else { return Ok(Detection::default()) };
        if !header_ok(first) {
            return Ok(Detection::default());
        }
        let known = all.iter().skip(1).filter(|r| r.get(2).map(|t| KNOWN_TYPES.contains(&norm(t).as_str())).unwrap_or(false)).count();
        let (s, reasons) = score(&[
            (true, 0.6, "colonnes Date, Libellé, Type, État, Montant (€)"),
            (known >= 1, 0.2, "types d'opérations d'assurance-vie (Versement, Arbitrage, Distribution…)"),
            (known >= 3, 0.15, "plusieurs opérations reconnues"),
        ]);
        Ok(Detection { score: round2(s.min(1.0)), reasons })
    }

    fn parse(&self, doc: &SourceDocument) -> ParseResult {
        let mut st = ParsedStatement::new("csv");
        st.institution = Some("Fortuneo".into());
        st.adapter_id = Some(self.id().into());
        st.document_kind = Some(self.kind().into());
        st.account_type = Some("life_insurance".into());
        st.account_label = Some("Fortuneo Vie".into());
        st.confidence = "high".into();

        struct Line {
            date: NaiveDate,
            label: String,
            kind: String,
            state: String,
            amount: Option<Decimal>,
            raw: String,
        }
        let mut lines: Vec<Line> = vec![];
        let mut skipped = 0;
        for row in rows(doc)?.iter().skip(1) {
            if row.iter().all(|c| py::strip(c).is_empty()) {
                continue;
            }
            let date = row.first().and_then(|d| parse_date(Some(d), None));
            let Some(date) = date.filter(|_| row.len() >= 5) else {
                skipped += 1;
                continue;
            };
            lines.push(Line {
                date,
                label: py::strip(&row[1]).to_string(),
                kind: norm(&row[2]),
                state: norm(&row[3]),
                amount: parse_amount(Some(&row[4])),
                raw: row.join(";"),
            });
        }
        if skipped > 0 {
            st.warnings.push(format!("{skipped} ligne(s) sans date lisible ont été ignorées"));
        }
        // the fees of the running year have no state yet: they are provisional
        let last_done = lines.iter().filter(|l| l.state == "traite").map(|l| l.date).max();

        let mut arbitrages: Vec<Value> = vec![];
        let mut events: Vec<Value> = vec![];
        let mut provisional = 0;
        let mut unknown = 0;
        for l in &lines {
            let iso = l.date.format("%Y-%m-%d").to_string();
            let Some(amount) = l.amount else {
                events.push(json!({"date": iso, "label": l.label}));
                continue;
            };
            let kind = match l.kind.as_str() {
                "arbitrage" => {
                    arbitrages.push(json!({"date": iso, "label": l.label, "amount": py::dec_str(&amount)}));
                    continue;
                }
                "versement" => "transfer",
                "distribution" | "revalorisation" => "interest",
                "frais" => {
                    if l.state.is_empty() && last_done.map(|d| l.date > d).unwrap_or(true) {
                        provisional += 1;
                        continue;
                    }
                    "fee"
                }
                "prelevement" => "other",
                _ => {
                    unknown += 1;
                    "other"
                }
            };
            let mut t = TransactionData::new(l.date, l.label.clone(), amount);
            t.kind = Some(kind.into());
            t.raw = Some(l.raw.clone());
            let mut details = Map::new();
            details.insert("life_insurance".into(), l.kind.clone().into());
            if l.kind == "versement" {
                details.insert("internal".into(), true.into()); // the money comes from the user's own account
            }
            t.details = Some(details);
            st.transactions.push(t);
        }
        let mut notes: Vec<Value> = vec![];
        if provisional > 0 {
            // informative only: nothing is wrong, the line comes back with its final amount in a later export
            notes.push(format!("{provisional} ligne(s) de frais de l'année en cours laissée(s) de côté : montant provisoire, elle reviendra dans un prochain export").into());
        }
        if !arbitrages.is_empty() {
            notes.push(format!("{} arbitrage(s) noté(s) à part : ils déplacent de l'argent entre les fonds du contrat, sans en faire entrer ni sortir", arbitrages.len()).into());
        }
        if unknown > 0 {
            st.warnings.push(format!("{unknown} ligne(s) d'un type inconnu importée(s) telle(s) quelle(s)"));
        }
        if st.transactions.is_empty() {
            st.warnings.push("Aucune opération trouvée dans ce fichier".into());
            st.confidence = "low".into();
        } else if unknown > 0 || skipped > 0 {
            st.confidence = "medium".into();
        }
        st.balance_date = lines.iter().filter(|l| l.state == "traite").map(|l| l.date).max();
        st.set_extra("category", "Opérations du contrat");
        st.set_extra("title", "Opérations du contrat d'assurance-vie");
        st.set_extra("match_by_type", true); // no contract number in the file
        st.set_extra("notes", Value::Array(notes));
        st.set_extra("arbitrages", Value::Array(arbitrages));
        st.set_extra("events", Value::Array(events));
        Ok(vec![st])
    }
}
