//! Which adapter reads a dropped file.
//!
//! Every adapter scores the file (`Adapter::detect`). A clear winner is used straight away; when two adapters are
//! close, when the institution is recognised but not the kind of document, or when only a guessing fallback applies,
//! the decision is returned as "ask" or "fallback" so the app can put the question to the user instead of guessing
//! silently.

use serde_json::{Map, Value};

use super::base::{Adapter, ParsedStatement, SourceDocument};
use super::boursobank::savings_pdf::BoursoBankSavingsPdf;
use super::boursobank::statement_pdf::BoursoBankStatementPdf;
use super::common::{find_iban, find_institution};
use super::fortuneo::deposit_guarantee_pdf::FortuneoDepositGuaranteePdf;
use super::fortuneo::fees_pdf::FortuneoFeesPdf;
use super::fortuneo::life_arbitrage_pdf::FortuneoLifeArbitragePdf;
use super::fortuneo::loan_offer_pdf::FortuneoLoanOfferPdf;
use super::fortuneo::loan_prepayment_pdf::FortuneoLoanPrepaymentPdf;
use super::fortuneo::loan_schedule_pdf::FortuneoLoanSchedulePdf;
use super::fortuneo::statement_pdf::FortuneoStatementPdf;
use super::generic::{GenericCsv, GenericOfx, GenericPdf, GenericQif};
use super::revolut::statement_csv::RevolutStatementCsv;

/// A short fingerprint of the reading code (every adapter and the PDF reader). A document remembers the one that read
/// it: when it differs from the current one, the reading may be out of date and is redone.
pub fn reader_version() -> &'static str {
    env!("MYMONEY_READER_VERSION")
}

/// One entry per kind of document per institution. To support a new one: write the adapter, add it here.
pub static ADAPTERS: &[&dyn Adapter] = &[
    &BoursoBankStatementPdf,
    &BoursoBankSavingsPdf,
    &FortuneoStatementPdf,
    &FortuneoFeesPdf,
    &FortuneoDepositGuaranteePdf,
    &FortuneoLifeArbitragePdf,
    &FortuneoLoanOfferPdf,
    &FortuneoLoanSchedulePdf,
    &FortuneoLoanPrepaymentPdf,
    &RevolutStatementCsv,
    &GenericOfx,
    &GenericQif,
    &GenericCsv,
    &GenericPdf,
];

#[cfg(test)]
thread_local! {
    /// The tests replace the registry (the Python tests monkeypatch `ADAPTERS`).
    pub static OVERRIDE: std::cell::RefCell<Option<Vec<&'static dyn Adapter>>> = const { std::cell::RefCell::new(None) };
}

/// The adapters in use: the registry (the tests may replace it for the thread that runs them).
pub fn adapters() -> Vec<&'static dyn Adapter> {
    #[cfg(test)]
    if let Some(list) = OVERRIDE.with(|o| o.borrow().clone()) {
        return list;
    }
    ADAPTERS.to_vec()
}

pub const AUTO_SCORE: f64 = 0.8; // a candidate at least this sure, and clearly ahead of the others, is used without asking
pub const AUTO_MARGIN: f64 = 0.2;
pub const PLAUSIBLE: f64 = 0.4;

#[derive(Clone)]
pub struct Candidate {
    pub adapter: &'static dyn Adapter,
    pub score: f64,
    pub reasons: Vec<String>,
}

impl Candidate {
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("adapter".into(), self.adapter.id().into());
        m.insert("score".into(), Value::from(self.score));
        m.insert("reasons".into(), Value::Array(self.reasons.iter().map(|r| Value::String(r.clone())).collect()));
        Value::Object(m)
    }
}

pub struct Identification {
    pub candidates: Vec<Candidate>,
    pub institution: Option<String>,
    /// auto | ask | fallback | none
    pub decision: &'static str,
    pub chosen: Option<Candidate>,
    pub message: String,
}

pub fn get_adapter(adapter_id: &str) -> Result<&'static dyn Adapter, String> {
    adapters().into_iter().find(|a| a.id() == adapter_id).ok_or_else(|| format!("Adapter inconnu : {adapter_id}"))
}

pub fn identify(doc: &SourceDocument) -> Identification {
    let mut candidates: Vec<Candidate> = vec![];
    for adapter in adapters() {
        let detection = match adapter.detect(doc) {
            Ok(d) => d,
            Err(e) => {
                // a broken adapter must never block the others
                log::error!("Détection {} : {e}", adapter.id());
                continue;
            }
        };
        if detection.score > 0.0 {
            candidates.push(Candidate { adapter, score: detection.score, reasons: detection.reasons });
        }
    }
    // sorted(key=(score, not generic), reverse=True): stable, so equal keys keep the registry order
    candidates.sort_by(|a, b| {
        let ka = (a.score, !a.adapter.generic());
        let kb = (b.score, !b.adapter.generic());
        kb.partial_cmp(&ka).unwrap_or(std::cmp::Ordering::Equal)
    });

    let institution = if doc.format() != "unknown" { doc.text().ok().and_then(find_institution) } else { None };
    if candidates.is_empty() {
        return Identification {
            candidates: vec![],
            institution,
            decision: "none",
            chosen: None,
            message: "Format non reconnu (PDF, CSV, OFX/QFX ou QIF attendu).".into(),
        };
    }

    let best = candidates[0].clone();
    if best.score >= AUTO_SCORE && (candidates.len() == 1 || candidates[1].score <= best.score - AUTO_MARGIN) {
        let institution = best.adapter.institution().map(|s| s.to_string()).or(institution);
        return Identification { candidates, institution, decision: "auto", chosen: Some(best), message: String::new() };
    }

    let close: Vec<&Candidate> = candidates.iter().filter(|c| c.score >= PLAUSIBLE && c.score > best.score - AUTO_MARGIN).collect();
    let specific = candidates.iter().any(|c| c.adapter.institution().is_some() && c.score >= PLAUSIBLE);
    if (close.len() > 1 && close.iter().any(|c| !c.adapter.generic())) || specific {
        return Identification {
            candidates,
            institution,
            decision: "ask",
            chosen: None,
            message: "Plusieurs lectures sont possibles pour ce document : choisis celle qui correspond.".into(),
        };
    }
    let mut message = "Aucun adapter dédié à ce document : lecture générique, vérifie bien ce qui a été compris.".to_string();
    if let Some(i) = &institution {
        message = format!("{i} reconnu, mais aucun adapter ne connaît ce type de document. {message}");
    }
    Identification { candidates, institution, decision: "fallback", chosen: Some(best), message }
}

pub fn parse_with(adapter_id: &str, doc: &SourceDocument) -> Result<Vec<ParsedStatement>, String> {
    let adapter = get_adapter(adapter_id)?;
    let mut statements = adapter.parse(doc)?;
    for st in statements.iter_mut() {
        if st.adapter_id.as_deref().map(|s| s.is_empty()).unwrap_or(true) {
            st.adapter_id = Some(adapter.id().into());
        }
        if st.document_kind.as_deref().map(|s| s.is_empty()).unwrap_or(true) {
            st.document_kind = Some(adapter.kind().into());
        }
        if st.institution.as_deref().map(|s| s.is_empty()).unwrap_or(true) {
            st.institution = adapter.institution().map(|s| s.to_string());
        }
        enrich(st);
    }
    Ok(statements)
}

/// Facts that any adapter benefits from: the IBAN of the other party, when the line shows one.
fn enrich(st: &mut ParsedStatement) {
    let own = st.iban.clone().unwrap_or_default().replace(' ', "");
    for t in st.transactions.iter_mut() {
        if t.counterparty_iban.as_deref().map(|s| s.is_empty()).unwrap_or(true) {
            let parts: Vec<&str> = [t.raw.as_deref(), Some(t.label.as_str()), t.memo.as_deref()].into_iter().flatten().filter(|s| !s.is_empty()).collect();
            if let Some(found) = find_iban(&parts.join(" ")) {
                if found != own {
                    t.counterparty_iban = Some(found);
                }
            }
        }
    }
}

/// Adapters by institution, for the "supported documents" list.
pub fn catalog() -> Vec<(String, Vec<&'static dyn Adapter>)> {
    let mut groups: Vec<(String, Vec<&'static dyn Adapter>)> = vec![];
    for adapter in adapters() {
        let key = adapter.institution().unwrap_or("Formats génériques").to_string();
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some(g) => g.1.push(adapter),
            None => groups.push((key, vec![adapter])),
        }
    }
    groups
}
