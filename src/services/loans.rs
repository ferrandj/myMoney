//! Loans: what the documents of a loan say, put together and checked against each other.
//!
//! Three documents describe one loan, each from its own angle, and none of them is enough alone:
//! - the offer: amount, duration, rate, cost, and the original amortization table (numbered, not dated);
//! - the notice of an early repayment: the capital owed, the last instalment paid, the new instalment and the new end
//!   date;
//! - the dated amortization table: every instalment from today to the end, starting from the capital owed that day.
//!
//! `build` replays them in order (the contract until the repayment, a computed bridge up to the dated table, then the
//! table) and every junction is a check: the capital owed before the repayment must be the one the contract gives at
//! that point, the computed bridge must land on the capital the dated table starts from, the end dates must agree,
//! and in the end the capital must reach zero. The instalments found in the account statements are compared with the
//! plan too.

use std::str::FromStr;

use chrono::{Datelike, NaiveDate};
use rusqlite::Connection;
use rust_decimal::{Decimal, RoundingStrategy};
use serde_json::{Map, Value};

use crate::adapters::base::ParsedStatement;
use crate::adapters::boursobank::table::fmt2;
use crate::adapters::common::name_key;
use crate::error::{value_err, AppError, AppResult};
use crate::models::{Account, Document, Loan, LoanEvent, LoanInstallment, Transaction};
use crate::py;

pub const LOAN_KINDS: &[&str] = &["loan_offer", "loan_event", "loan_schedule"];
pub const LOAN_ACCOUNT_TYPES: &[&str] = &["mortgage", "loan"];

fn d(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

pub fn cents(value: Decimal) -> Decimal {
    value.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero).max_scale2()
}

trait Scale2 {
    fn max_scale2(self) -> Decimal;
}

impl Scale2 for Decimal {
    /// `quantize(Decimal("0.01"))` always has two decimals.
    fn max_scale2(mut self) -> Decimal {
        self.rescale(2);
        self
    }
}

fn days_in_month(year: i32, month: u32) -> u32 {
    let next = if month == 12 { NaiveDate::from_ymd_opt(year + 1, 1, 1) } else { NaiveDate::from_ymd_opt(year, month + 1, 1) };
    (next.unwrap() - NaiveDate::from_ymd_opt(year, month, 1).unwrap()).num_days() as u32
}

pub fn add_months(day: NaiveDate, months: i64) -> NaiveDate {
    let index = day.year() as i64 * 12 + day.month() as i64 - 1 + months;
    let (year, month) = (index.div_euclid(12) as i32, index.rem_euclid(12) as u32 + 1);
    NaiveDate::from_ymd_opt(year, month, day.day().min(days_in_month(year, month))).unwrap()
}

pub fn months_between(start: NaiveDate, end: NaiveDate) -> i64 {
    (end.year() as i64 - start.year() as i64) * 12 + end.month() as i64 - start.month() as i64
}

fn json_s<'a>(m: &'a Map<String, Value>, k: &str) -> Option<&'a str> {
    m.get(k).and_then(|v| v.as_str())
}

fn date_of(v: Option<&str>) -> Option<NaiveDate> {
    v.filter(|s| !s.is_empty()).and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
}

fn dec_of(v: Option<&str>) -> Option<Decimal> {
    v.filter(|s| !s.is_empty()).and_then(py::dec_parse)
}

fn key_error(k: &str) -> AppError {
    AppError::Other(format!("'{k}'"))
}

fn loan_data(st: &ParsedStatement) -> Map<String, Value> {
    st.extra.get("loan").and_then(|v| v.as_object()).cloned().unwrap_or_default()
}

pub fn loan_of(c: &Connection, account_id: i64) -> AppResult<Option<Loan>> {
    Ok(Loan::query_one(c, "WHERE account_id = ?", [account_id])?)
}

pub fn installments(c: &Connection, loan_id: i64) -> AppResult<Vec<LoanInstallment>> {
    Ok(LoanInstallment::query(c, "WHERE loan_id = ? ORDER BY id", [loan_id])?)
}

pub fn events(c: &Connection, loan_id: i64) -> AppResult<Vec<LoanEvent>> {
    Ok(LoanEvent::query(c, "WHERE loan_id = ? ORDER BY on_date", [loan_id])?)
}

// ---------------------------------------------------------------------------------------------- finding

/// The loan accounts a loan document can belong to. The offer and the dated table carry the loan's number; the early
/// repayment notice does not, so it is matched by figures: the capital owed before must be one the loan's table has.
pub fn find_accounts(c: &Connection, st: &ParsedStatement) -> AppResult<Vec<Account>> {
    let data = loan_data(st);
    let loans = Loan::query(c, "", [])?;
    let mut found: Vec<&Loan> = vec![];
    if let Some(r) = json_s(&data, "reference").filter(|r| !r.is_empty()) {
        found = loans.iter().filter(|l| l.reference.as_deref() == Some(r)).collect();
        if found.is_empty() && st.document_kind.as_deref() == Some("loan_offer") {
            // a loan first seen through its repayment notice, with no number yet
            let remaining: Vec<Decimal> = data
                .get("rows")
                .and_then(|r| r.as_array())
                .map(|rows| rows.iter().filter_map(|r| r.get("remaining").and_then(|v| v.as_str()).and_then(py::dec_parse)).collect())
                .unwrap_or_default();
            for l in &loans {
                if l.reference.is_none() && events(c, l.id)?.iter().any(|e| remaining.contains(&e.capital_before)) {
                    found.push(l);
                }
            }
        }
    } else if let Some(capital) = data.get("event").and_then(|e| e.get("before")).and_then(|b| b.get("capital")).filter(|v| crate::models::truthy(v)) {
        let capital = capital.as_str().and_then(py::dec_parse).unwrap_or_default();
        for l in &loans {
            if installments(c, l.id)?.iter().any(|i| i.remaining == capital) {
                found.push(l);
            }
        }
        if found.is_empty() {
            if let Some(h) = st.holder_name.as_deref().filter(|h| !h.is_empty()) {
                // no table to compare with yet (the offer is missing): the borrower's name is the only link
                found = loans.iter().filter(|l| l.borrower.as_deref().filter(|b| !b.is_empty()).map(|b| name_key(b) == name_key(h)).unwrap_or(false)).collect();
            }
        }
    }
    let mut out = vec![];
    for l in found {
        if let Some(a) = Account::get(c, l.account_id)? {
            out.push(a);
        }
    }
    Ok(out)
}

/// A repayment notice whose capital cannot be found in the loan's own table: it may still be right, but it is not
/// proven.
pub fn unverified(c: &Connection, st: &ParsedStatement, account: Option<&Account>) -> AppResult<Option<String>> {
    let loan = match account {
        Some(a) => loan_of(c, a.id)?,
        None => None,
    };
    let event = loan_data(st).get("event").cloned().filter(crate::models::truthy);
    let (Some(loan), Some(event), true) = (loan, event, st.document_kind.as_deref() == Some("loan_event")) else { return Ok(None) };
    let capital = event.get("before").ok_or_else(|| key_error("before"))?.get("capital").ok_or_else(|| key_error("capital"))?;
    let capital = capital.as_str().and_then(py::dec_parse).unwrap_or_default();
    if installments(c, loan.id)?.iter().any(|i| i.remaining == capital) {
        return Ok(None);
    }
    Ok(Some(format!(
        "Le capital restant dû avant remboursement ({}) ne figure dans aucun tableau de ce prêt : il manque peut-être le contrat (offre) pour le vérifier. Rattachement proposé d'après le nom de l'emprunteur.",
        euros(&capital)
    )))
}

/// A sentence when this account already has this very document's content (the user may still replace it).
pub fn existing_for(c: &Connection, st: &ParsedStatement, account: Option<&Account>) -> AppResult<Option<String>> {
    let loan = match account {
        Some(a) => loan_of(c, a.id)?,
        None => None,
    };
    let Some(loan) = loan else { return Ok(None) };
    let data = loan_data(st);
    match st.document_kind.as_deref() {
        Some("loan_offer") if loan.offer_document_id.is_some() => return Ok(Some("Ce prêt a déjà son offre (contrat) importée".into())),
        Some("loan_schedule") if loan.schedule_document_id.is_some() => return Ok(Some("Ce prêt a déjà un tableau d'amortissement daté importé".into())),
        Some("loan_event") => {
            let ev = data.get("event").ok_or_else(|| key_error("event"))?;
            let before = ev.get("before").ok_or_else(|| key_error("before"))?;
            let on = ev.get("on_date").and_then(|v| v.as_str());
            let cap = before.get("capital").and_then(|v| v.as_str());
            if events(c, loan.id)?.iter().any(|e| Some(e.on_date.format("%Y-%m-%d").to_string().as_str()) == on && Some(py::dec_str(&e.capital_before).as_str()) == cap) {
                return Ok(Some("Ce remboursement anticipé est déjà enregistré pour ce prêt".into()));
            }
        }
        _ => {}
    }
    Ok(None)
}

// --------------------------------------------------------------------------------------------- applying

pub fn import_result(account_id: Option<i64>, kind: Option<&str>) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("account_id".into(), account_id.map(Value::from).unwrap_or(Value::Null));
    if let Some(k) = kind {
        m.insert("kind".into(), k.into());
    }
    m.insert("added".into(), 0.into());
    m.insert("skipped_exact".into(), 0.into());
    m.insert("skipped_probable".into(), 0.into());
    m
}

fn installment(loan: &Loan, doc: &Document, source: &str, r: &Map<String, Value>) -> AppResult<LoanInstallment> {
    let req = |k: &str| -> AppResult<Decimal> { json_s(r, k).and_then(py::dec_parse).ok_or_else(|| key_error(k)) };
    Ok(LoanInstallment {
        loan_id: loan.id,
        document_id: Some(doc.id),
        source: source.into(),
        number: r.get("n").and_then(|v| v.as_i64()),
        due_date: date_of(json_s(r, "date")),
        capital: req("capital")?,
        interest: req("interest")?,
        deferred_interest: req("deferred")?,
        insurance: req("insurance")?,
        total: req("total")?,
        remaining: req("remaining")?,
        ..Default::default()
    })
}

fn drop_source(c: &Connection, loan: &Loan, source: &str) -> AppResult<()> {
    c.execute("DELETE FROM loan_installments WHERE loan_id = ? AND source = ?", rusqlite::params![loan.id, source])?;
    Ok(())
}

fn truthy_dec(d: &Option<Decimal>) -> bool {
    d.map(|x| !x.is_zero()).unwrap_or(false)
}

pub fn attach(c: &Connection, doc: &mut Document, st: &ParsedStatement, account: &mut Account, form: &std::collections::HashMap<String, String>) -> AppResult<Map<String, Value>> {
    let data = loan_data(st);
    let force = form.get("force").map(|s| s.as_str()) == Some("on");
    if !LOAN_ACCOUNT_TYPES.contains(&account.account_type.as_str()) {
        return value_err("Ce document décrit un prêt : choisis un compte de type crédit");
    }
    let mut loan = match loan_of(c, account.id)? {
        Some(l) => l,
        None => {
            let mut l = Loan { account_id: account.id, ..Default::default() };
            l.insert(c)?;
            l
        }
    };
    let reference = json_s(&data, "reference").filter(|r| !r.is_empty()).map(|s| s.to_string());
    if let (Some(r), Some(have)) = (&reference, loan.reference.as_deref().filter(|s| !s.is_empty())) {
        if r != have {
            return value_err(format!("Ce compte porte déjà le prêt n° {have}, pas le n° {r}"));
        }
    }
    if let Some(msg) = existing_for(c, st, Some(account))? {
        if !force {
            return value_err(msg);
        }
    }
    let before_loan = loan.clone();
    let some = |k: &str| json_s(&data, k).filter(|s| !s.is_empty()).map(|s| s.to_string());
    loan.reference = reference.or(loan.reference.clone().filter(|s| !s.is_empty()));
    loan.project_reference = some("project").or(loan.project_reference.clone().filter(|s| !s.is_empty()));
    loan.product = some("product").or(loan.product.clone().filter(|s| !s.is_empty()));
    loan.borrower = some("borrower").or(st.holder_name.clone().filter(|s| !s.is_empty())).or(loan.borrower.clone().filter(|s| !s.is_empty()));
    match st.document_kind.as_deref() {
        Some("loan_offer") => {
            drop_source(c, &loan, "offer")?;
            let purpose: Vec<String> = [some("object"), some("destination")].into_iter().flatten().collect();
            let purpose = purpose.join(" · ");
            if !purpose.is_empty() {
                loan.purpose = Some(purpose);
            }
            loan.principal = dec_of(json_s(&data, "principal"));
            loan.duration_months = data.get("duration").and_then(|v| v.as_i64());
            loan.rate = dec_of(json_s(&data, "rate"));
            loan.taeg = dec_of(json_s(&data, "taeg"));
            loan.interest_total = dec_of(json_s(&data, "interest_total"));
            loan.total_cost = dec_of(json_s(&data, "total_cost"));
            loan.insurance_mandatory = dec_of(json_s(&data, "insurance_mandatory"));
            loan.insurance_optional = dec_of(json_s(&data, "insurance_optional"));
            loan.guarantee_fees = dec_of(json_s(&data, "guarantee_fees"));
            loan.offer_date = date_of(json_s(&data, "offer_date"));
            loan.offer_document_id = Some(doc.id);
            for r in data.get("rows").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
                installment(&loan, doc, "offer", r.as_object().unwrap_or(&Map::new()))?.insert(c)?;
            }
        }
        Some("loan_schedule") => {
            let rate = dec_of(json_s(&data, "rate"));
            if let (Some(have), Some(new)) = (loan.rate, rate) {
                if new != have && !force {
                    return value_err(format!(
                        "Le taux du tableau ({} %) n'est pas celui du prêt ({} %)",
                        json_s(&data, "rate").unwrap_or(""),
                        py::dec_str(&have)
                    ));
                }
            }
            drop_source(c, &loan, "schedule")?;
            if !truthy_dec(&loan.rate) {
                loan.rate = rate;
            }
            loan.schedule_document_id = Some(doc.id);
            if !truthy_dec(&loan.principal) {
                loan.principal = dec_of(json_s(&data, "project_amount"));
            }
            for r in data.get("rows").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
                installment(&loan, doc, "schedule", r.as_object().unwrap_or(&Map::new()))?.insert(c)?;
            }
        }
        _ => {
            let ev = data.get("event").and_then(|v| v.as_object()).ok_or_else(|| key_error("event"))?.clone();
            let before = ev.get("before").and_then(|v| v.as_object()).ok_or_else(|| key_error("before"))?.clone();
            let after = ev.get("after").and_then(|v| v.as_object()).ok_or_else(|| key_error("after"))?.clone();
            let cap_before = json_s(&before, "capital").ok_or_else(|| key_error("capital"))?.to_string();
            let on = json_s(&ev, "on_date").map(|s| s.to_string());
            for old in events(c, loan.id)? {
                if Some(old.on_date.format("%Y-%m-%d").to_string()) == on && py::dec_str(&old.capital_before) == cap_before {
                    LoanEvent::delete(c, old.id)?;
                }
            }
            let on_date = date_of(on.as_deref()).ok_or_else(|| key_error("on_date"))?;
            LoanEvent {
                loan_id: loan.id,
                document_id: Some(doc.id),
                kind: "prepayment".into(),
                on_date,
                issued_on: date_of(json_s(&ev, "issued_on")),
                capital_before: py::dec_parse(&cap_before).unwrap_or_default(),
                capital_after: json_s(&after, "capital").and_then(py::dec_parse).ok_or_else(|| key_error("capital"))?,
                last_due_date: date_of(json_s(&before, "due")),
                last_payment: dec_of(json_s(&before, "payment")),
                first_due_date: date_of(json_s(&after, "due")),
                first_payment: dec_of(json_s(&after, "payment")),
                end_before: date_of(json_s(&before, "end")),
                end_after: date_of(json_s(&after, "end")),
                ..Default::default()
            }
            .insert(c)?;
        }
    }
    loan.save(c, &before_loan)?;
    let before_account = account.clone();
    if let Some(h) = st.holder_name.as_deref().filter(|h| !h.is_empty()) {
        if account.holder_name.as_deref().map(|s| s.is_empty()).unwrap_or(true) {
            account.holder_name = Some(h.to_string());
        }
    }
    if account.institution.as_deref().map(|s| s.is_empty()).unwrap_or(true) {
        account.institution = st.institution.clone();
    }
    account.save(c, &before_account)?;
    let before_doc = doc.clone();
    doc.status = "imported".into();
    doc.account_id = Some(account.id);
    doc.save(c, &before_doc)?;
    rebuild(c, &mut loan, None)?;
    if let Some(a) = Account::get(c, account.id)? {
        *account = a;
    }
    Ok(import_result(Some(account.id), st.document_kind.as_deref()))
}

/// Take back what a document brought to its loan. A loan left with nothing is removed (its account stays).
pub fn detach(c: &Connection, doc: &Document) -> AppResult<()> {
    c.execute("DELETE FROM loan_installments WHERE document_id = ?", [doc.id])?;
    c.execute("DELETE FROM loan_events WHERE document_id = ?", [doc.id])?;
    for mut loan in Loan::query(c, "WHERE offer_document_id = ? OR schedule_document_id = ?", [doc.id, doc.id])? {
        let before = loan.clone();
        if loan.offer_document_id == Some(doc.id) {
            loan.offer_document_id = None;
            loan.principal = None;
            loan.duration_months = None;
            loan.rate = None;
            loan.taeg = None;
            loan.interest_total = None;
            loan.total_cost = None;
            loan.insurance_mandatory = None;
            loan.insurance_optional = None;
            loan.guarantee_fees = None;
            loan.offer_date = None;
            loan.purpose = None;
        }
        if loan.schedule_document_id == Some(doc.id) {
            loan.schedule_document_id = None;
        }
        loan.save(c, &before)?;
    }
    for mut loan in Loan::query(c, "", [])? {
        if installments(c, loan.id)?.is_empty() && events(c, loan.id)?.is_empty() {
            Loan::delete(c, loan.id)?;
        } else {
            rebuild(c, &mut loan, None)?;
        }
    }
    Ok(())
}

// --------------------------------------------------------------------------------------------- the plan

#[derive(Clone, Debug)]
pub struct Row {
    /// installment | prepayment
    pub kind: &'static str,
    pub date: Option<NaiveDate>,
    pub number: Option<i64>,
    pub capital: Decimal,
    pub interest: Decimal,
    pub insurance: Decimal,
    pub total: Decimal,
    pub remaining: Decimal,
    /// contrat | calculé | échéancier | arrêté
    pub source: &'static str,
    /// paid | next | future
    pub status: &'static str,
    /// the amount debited, when the account statements show this instalment
    pub confirmed: Option<Decimal>,
    pub note: String,
}

#[allow(clippy::too_many_arguments)]
fn row(kind: &'static str, date: Option<NaiveDate>, number: Option<i64>, capital: Decimal, interest: Decimal, insurance: Decimal, total: Decimal, remaining: Decimal, source: &'static str) -> Row {
    Row { kind, date, number, capital, interest, insurance, total, remaining, source, status: "future", confirmed: None, note: String::new() }
}

#[derive(Clone, Debug)]
pub struct Check {
    /// ok | warn | info
    pub status: &'static str,
    pub text: String,
}

fn check(status: &'static str, text: impl Into<String>) -> Check {
    Check { status, text: text.into() }
}

#[derive(Clone, Debug, Default)]
pub struct Saved {
    pub interest: Decimal,
    pub insurance: Decimal,
    pub months: i64,
}

#[derive(Clone, Debug)]
pub struct Summary {
    pub as_of: NaiveDate,
    pub owed: Option<Decimal>,
    pub end_date: Option<NaiveDate>,
    pub paid_count: usize,
    pub remaining_count: usize,
    pub total_count: usize,
    pub next: Option<Row>,
    pub dated: bool,
    pub future_interest: Decimal,
    pub future_insurance: Decimal,
    pub future_total: Decimal,
    pub paid_total: Decimal,
    pub paid_interest: Decimal,
    pub lifetime_interest: Decimal,
    pub repaid_early: Decimal,
    pub saved: Option<Saved>,
}

#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub rows: Vec<Row>,
    pub checks: Vec<Check>,
    pub summary: Option<Summary>,
}

impl Plan {
    pub fn installments(&self) -> Vec<&Row> {
        self.rows.iter().filter(|r| r.kind == "installment").collect()
    }
    pub fn problems(&self) -> Vec<&Check> {
        self.checks.iter().filter(|c| c.status == "warn").collect()
    }
}

fn sorted_offer(inst: &[LoanInstallment]) -> Vec<LoanInstallment> {
    let mut offer: Vec<LoanInstallment> = inst.iter().filter(|i| i.source == "offer").cloned().collect();
    offer.sort_by_key(|i| i.number.unwrap_or(0));
    offer
}

fn sorted_schedule(inst: &[LoanInstallment]) -> Vec<LoanInstallment> {
    let mut schedule: Vec<LoanInstallment> = inst.iter().filter(|i| i.source == "schedule").cloned().collect();
    schedule.sort_by_key(|i| i.due_date);
    schedule
}

/// The date of instalment n°1, from a dated document that points at one of the contract's numbered instalments.
pub fn infer_first_due(c: &Connection, loan: &Loan) -> AppResult<Option<NaiveDate>> {
    let inst = installments(c, loan.id)?;
    let offer = sorted_offer(&inst);
    if offer.is_empty() {
        return Ok(None);
    }
    let evs = events(c, loan.id)?;
    for ev in &evs {
        if let Some(last) = ev.last_due_date {
            for r in &offer {
                if r.remaining == ev.capital_before {
                    return Ok(Some(add_months(last, -(r.number.unwrap_or(0) - 1))));
                }
            }
        }
    }
    let schedule = sorted_schedule(&inst);
    if !schedule.is_empty() && evs.is_empty() {
        let balance = schedule[0].remaining + schedule[0].capital;
        for r in &offer {
            if r.remaining == balance {
                return Ok(Some(add_months(schedule[0].due_date.unwrap(), -r.number.unwrap_or(0))));
            }
        }
    }
    Ok(None)
}

/// Instalments of a fixed-rate loan from `capital`: interest = capital × rate / 12 (rounded), the rest of the payment
/// repays capital, the last instalment clears what is left.
pub fn project(mut capital: Decimal, rate: Decimal, payment: Decimal, insurance: Decimal, first_due: NaiveDate, source: &'static str) -> Vec<Row> {
    let mut rows = vec![];
    for k in 0..1200 {
        if capital <= Decimal::ZERO {
            break;
        }
        let interest = cents(py::py_div(&py::py_mul(&capital, &rate), &Decimal::from(1200)).unwrap());
        let mut principal = payment - insurance - interest;
        if principal <= Decimal::ZERO {
            break;
        }
        if principal >= capital || k == 1199 {
            principal = capital;
        }
        capital -= principal;
        rows.push(row("installment", Some(add_months(first_due, k)), None, principal, interest, insurance, principal + interest + insurance, capital, source));
    }
    rows
}

pub fn build(c: &Connection, loan: &Loan, today: Option<NaiveDate>) -> AppResult<Plan> {
    let today = today.unwrap_or_else(crate::services::rates::today);
    let mut plan = Plan::default();
    let mut checks: Vec<Check> = vec![];
    let inst = installments(c, loan.id)?;
    let offer = sorted_offer(&inst);
    let schedule = sorted_schedule(&inst);
    let mut evs = events(c, loan.id)?;
    evs.sort_by_key(|e| e.on_date);
    let first_due = loan.first_due_date;
    let rate = loan.rate;
    let insurance = offer.last().map(|i| i.insurance).or(schedule.last().map(|i| i.insurance)).unwrap_or(Decimal::ZERO);
    let rate_truthy = truthy_dec(&rate);

    // 1. The contract.
    let mut rows: Vec<Row> = vec![];
    for i in &offer {
        let when = first_due.map(|f| add_months(f, i.number.unwrap_or(0) - 1));
        rows.push(row("installment", when, i.number, i.capital, i.interest, i.insurance, i.total, i.remaining, "contrat"));
    }
    if !offer.is_empty() {
        check_offer(loan, &offer, rate, &mut checks);
        if first_due.is_none() {
            checks.push(check("info", "Les dates des échéances du contrat ne sont pas connues : dépose l'arrêté de remboursement anticipé ou le tableau daté, ou indique la date de la première échéance."));
        }
    }

    // 2. Early repayments: the contract stops where the capital owed matches, the new plan starts from what is left.
    let mut start_of_new: Option<usize> = None;
    for ev in &evs {
        let cut = (0..rows.len()).rev().find(|&k| rows[k].kind == "installment" && rows[k].remaining == ev.capital_before);
        if let Some(cut) = cut {
            let label = match rows[cut].number.filter(|n| *n != 0) {
                Some(n) => format!("échéance n° {n}"),
                None => "échéance".into(),
            };
            let at = rows[cut].date.map(|d| format!(" ({})", fr(Some(d)))).unwrap_or_default();
            checks.push(check(
                "ok",
                format!(
                    "Remboursement anticipé du {} : le capital restant dû avant ({}) est exactement celui du plan après l'{label}{at}.",
                    fr(Some(ev.on_date)),
                    euros(&ev.capital_before)
                ),
            ));
            if let (Some(rd), Some(ld)) = (rows[cut].date, ev.last_due_date) {
                if rd != ld {
                    checks.push(check("warn", format!("La dernière échéance avant le remboursement est le {} dans le courrier, le {} dans le plan.", fr(Some(ld)), fr(Some(rd)))));
                }
            }
            rows.truncate(cut + 1);
        } else if !rows.is_empty() {
            checks.push(check(
                "warn",
                format!(
                    "Le capital restant dû avant le remboursement anticipé du {} ({}) ne correspond à aucune échéance du plan : une échéance ou un autre remboursement manque peut-être.",
                    fr(Some(ev.on_date)),
                    euros(&ev.capital_before)
                ),
            ));
            rows.retain(|r| matches!((r.date, ev.last_due_date), (Some(a), Some(b)) if a <= b));
        } else {
            checks.push(check(
                "info",
                format!(
                    "Remboursement anticipé du {} : sans le contrat, impossible de vérifier le capital restant dû avant ({}).",
                    fr(Some(ev.on_date)),
                    euros(&ev.capital_before)
                ),
            ));
        }
        if let (Some(_), Some(eb), false, Some(f)) = (rows.last().and_then(|r| r.date), ev.end_before, offer.is_empty(), first_due) {
            let planned_end = add_months(f, offer.len() as i64 - 1);
            checks.push(check(
                if planned_end == eb { "ok" } else { "warn" },
                format!("Date de fin avant remboursement : {} dans le courrier, {} d'après le contrat ({} échéances).", fr(Some(eb)), fr(Some(planned_end)), offer.len()),
            ));
        }
        let repaid = ev.capital_before - ev.capital_after;
        let mut prepay = row("prepayment", Some(ev.on_date), None, repaid, Decimal::ZERO, Decimal::ZERO, repaid, ev.capital_after, "arrêté");
        prepay.note = format!(
            "Remboursement anticipé partiel de {} : capital restant dû {} → {}",
            euros(&repaid),
            euros(&ev.capital_before),
            euros(&ev.capital_after)
        );
        rows.push(prepay);
        let payment_truthy = truthy_dec(&ev.first_payment);
        if let (true, Some(fd), true) = (rate_truthy, ev.first_due_date, payment_truthy) {
            let r = rate.unwrap();
            let payment = ev.first_payment.unwrap();
            let new = project(ev.capital_after, r, payment, insurance, fd, "calculé");
            let n_new = new.len();
            let last_date = new.last().and_then(|x| x.date);
            rows.extend(new);
            start_of_new = Some(rows.len() - n_new);
            if let (true, Some(ea)) = (n_new > 0, ev.end_after) {
                let same = last_date == Some(ea);
                checks.push(check(
                    if same { "ok" } else { "warn" },
                    format!(
                        "Après le remboursement : {n_new} échéances de {} au taux de {} mènent au {} ({} {} dans le courrier).",
                        euros(&payment),
                        pct(&r),
                        fr(last_date),
                        if same { "comme" } else { "courrier :" },
                        fr(Some(ea))
                    ),
                ));
            }
        } else if ev.first_due_date.is_some() && payment_truthy && !rate_truthy {
            checks.push(check("info", "Le taux du prêt est inconnu (dépose l'offre ou le tableau daté) : l'échéancier après le remboursement ne peut pas être calculé."));
        }
    }

    // 3. The dated table: it takes over from its first date and must start from the capital the plan has reached.
    if !schedule.is_empty() {
        let first = &schedule[0];
        let first_date = first.due_date.unwrap();
        let balance = first.remaining + first.capital;
        if rate_truthy {
            check_interest(&schedule, rate.unwrap(), "du tableau daté", &mut checks, Some(balance));
        }
        let before: Vec<Row> = rows.iter().filter(|r| r.date.map(|d| d < first_date).unwrap_or(false)).cloned().collect();
        if !offer.is_empty() && before.is_empty() && evs.is_empty() && rows.iter().any(|r| r.date.is_none()) {
            let mut near = &offer[0];
            for o in &offer[1..] {
                if (o.remaining - balance).abs() < (near.remaining - balance).abs() {
                    near = o;
                }
            }
            checks.push(check(
                "warn",
                format!(
                    "Le tableau daté part de {} au {} mais aucune échéance du contrat ne mène à ce capital (la plus proche : n° {}, {}). Un remboursement anticipé a probablement eu lieu : dépose son courrier pour que tout soit reconstitué.",
                    euros(&balance),
                    fr(Some(first_date)),
                    near.number.map(|n| n.to_string()).unwrap_or_else(|| "None".into()),
                    euros(&near.remaining)
                ),
            ));
            rows.clear(); // the contract's undated instalments cannot be placed against the dated table
        } else if let Some(last) = before.last() {
            let gap = last.remaining - balance;
            if gap.is_zero() {
                checks.push(check(
                    "ok",
                    format!(
                        "Le tableau daté part de {} au {} : c'est exactement le capital auquel mène le plan ({}) après le {}.",
                        euros(&balance),
                        fr(Some(first_date)),
                        last.source,
                        fr(last.date)
                    ),
                ));
            } else if gap.abs() <= d("0.05") {
                checks.push(check(
                    "ok",
                    format!("Le tableau daté part de {} ; le plan calculé donne {} (écart d'arrondi de {}).", euros(&balance), euros(&last.remaining), euros(&gap.abs())),
                ));
            } else {
                let hint = if evs.is_empty() && gap > Decimal::ZERO { " Un remboursement anticipé manque peut-être (dépose son courrier)." } else { "" };
                checks.push(check(
                    "warn",
                    format!(
                        "Le tableau daté part de {} au {} mais le plan donne {} : écart de {}.{hint}",
                        euros(&balance),
                        fr(Some(first_date)),
                        euros(&last.remaining),
                        euros(&gap.abs())
                    ),
                ));
            }
        } else if rows.is_empty() {
            checks.push(check(
                "info",
                format!("Le tableau daté part de {} au {} ; sans contrat ni courrier, rien d'antérieur n'est reconstitué.", euros(&balance), fr(Some(first_date))),
            ));
        }
        // Does the computed plan (when there is one) fall on the dated table?
        if let Some(start) = start_of_new {
            let computed: Vec<&Row> = rows.iter().skip(start).filter(|r| r.date.map(|d| d >= first_date).unwrap_or(false)).collect();
            let diffs: Vec<Decimal> = computed.iter().zip(schedule.iter()).map(|(c, s)| (c.remaining - s.remaining).abs()).collect();
            let same_end = !computed.is_empty() && computed.last().unwrap().date == schedule.last().unwrap().due_date && computed.len() == schedule.len();
            let max = diffs.iter().copied().max();
            if !computed.is_empty() && same_end && max.map(|m| m <= d("0.10")).unwrap_or(false) {
                checks.push(check(
                    "ok",
                    format!(
                        "Le plan recalculé retombe sur le tableau daté : mêmes {} échéances, écart maximal de {} sur le capital restant dû.",
                        schedule.len(),
                        euros(&max.unwrap())
                    ),
                ));
            } else if !computed.is_empty() {
                checks.push(check("warn", "Le plan recalculé après le remboursement ne correspond pas au tableau daté (dates ou capitaux différents)."));
            }
        }
        rows.retain(|r| !r.date.map(|d| d >= first_date).unwrap_or(false));
        for s in &schedule {
            rows.push(row("installment", s.due_date, None, s.capital, s.interest, s.insurance, s.total, s.remaining, "échéancier"));
        }
        let last_due = schedule.last().unwrap().due_date;
        match evs.last().and_then(|e| e.end_after) {
            Some(ea) if last_due != Some(ea) => {
                checks.push(check("warn", format!("Le tableau daté se termine le {}, le courrier annonce le {}.", fr(last_due), fr(Some(ea)))));
            }
            _ if !evs.is_empty() => {
                checks.push(check("ok", format!("Fin du prêt le {} : la date du tableau daté et celle du courrier de remboursement anticipé concordent.", fr(last_due))));
            }
            _ => {}
        }
    } else if !evs.is_empty() && !rows.is_empty() {
        checks.push(check("info", "Pas de tableau daté : l'échéancier après le remboursement est calculé à partir du courrier et du taux."));
    }

    plan.rows = rows;
    plan.checks = checks;
    statuses(&mut plan, today);
    payments(c, loan, &mut plan)?;
    finish(loan, &mut plan, today, &offer, &evs);
    Ok(plan)
}

fn statuses(plan: &mut Plan, today: NaiveDate) {
    let mut seen_next = false;
    for r in plan.rows.iter_mut() {
        let Some(d) = r.date else { continue };
        if d <= today {
            r.status = "paid";
        } else if r.kind == "installment" && !seen_next {
            r.status = "next";
            seen_next = true;
        }
    }
}

fn check_offer(loan: &Loan, offer: &[LoanInstallment], rate: Option<Decimal>, checks: &mut Vec<Check>) {
    let capital: Decimal = offer.iter().fold(Decimal::ZERO, |a, i| a + i.capital);
    let interest: Decimal = offer.iter().fold(Decimal::ZERO, |a, i| a + i.interest);
    let mut problems: Vec<String> = vec![];
    if let Some(p) = loan.principal {
        if capital != p {
            problems.push("la somme des amortissements n'est pas le montant emprunté".into());
        }
    }
    if let Some(m) = loan.duration_months.filter(|m| *m != 0) {
        if offer.len() as i64 != m {
            problems.push(format!("{} échéances pour {m} mois", offer.len()));
        }
    }
    if let Some(it) = loan.interest_total {
        if interest != it {
            problems.push("les intérêts du tableau ne sont pas ceux des conditions particulières".into());
        }
    }
    if !problems.is_empty() {
        checks.push(check("warn", format!("Contrat : {}.", problems.join(" ; "))));
    } else {
        checks.push(check(
            "ok",
            format!(
                "Contrat : {} échéances, {} de capital remboursé (= montant emprunté), {} d'intérêts (= conditions particulières).",
                offer.len(),
                euros(&capital),
                euros(&interest)
            ),
        ));
    }
    if let (Some(tc), Some(it), Some(im), Some(gf)) = (loan.total_cost, loan.interest_total, loan.insurance_mandatory, loan.guarantee_fees) {
        let total = it + im + gf;
        checks.push(if total == tc {
            check(
                "ok",
                format!(
                    "Coût total du crédit {} = intérêts {} + assurance obligatoire {} + frais de garantie {}.",
                    euros(&tc),
                    euros(&it),
                    euros(&im),
                    euros(&gf)
                ),
            )
        } else {
            check("warn", format!("Le coût total annoncé ({}) n'est pas la somme de ses composantes ({}).", euros(&tc), euros(&total)))
        });
    }
    if truthy_dec(&rate) {
        check_interest(offer, rate.unwrap(), "du contrat", checks, loan.principal);
    }
}

/// Interest = capital owed × rate / 12: recomputed on every instalment (the last one is often adjusted by the bank).
fn check_interest(rows: &[LoanInstallment], rate: Decimal, label: &str, checks: &mut Vec<Check>, start: Option<Decimal>) {
    let mut balance = start.unwrap_or(rows[0].remaining + rows[0].capital);
    let mut off = 0;
    for (i, r) in rows.iter().enumerate() {
        let expected = cents(py::py_div(&py::py_mul(&balance, &rate), &Decimal::from(1200)).unwrap());
        if (expected - r.interest).abs() > d("0.01") && i < rows.len() - 1 {
            off += 1;
        }
        balance = r.remaining;
    }
    let n = rows.len() as i64 - 1;
    if off == 0 {
        checks.push(check("ok", format!("Taux de {} : les intérêts de {n} échéances {label} se recalculent à un centime près.", pct(&rate))));
    } else {
        checks.push(check("warn", format!("Taux de {} : {off} échéance(s) {label} ont des intérêts différents du calcul capital × taux / 12.", pct(&rate))));
    }
}

/// `str(value)` of a JSON value read back from the details.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => if *b { "True".into() } else { "False".into() },
        other => py::json_dumps(other),
    }
}

/// The instalments the account statements show (lines of kind loan_payment naming this loan), against the plan.
fn payments(c: &Connection, loan: &Loan, plan: &mut Plan) -> AppResult<()> {
    let Some(reference) = loan.reference.as_deref().filter(|r| !r.is_empty()) else { return Ok(()) };
    let all = Transaction::query(c, "WHERE kind = 'loan_payment' ORDER BY date", [])?;
    let payments: Vec<Transaction> = all
        .into_iter()
        .filter(|t| {
            let number = t.details.as_ref().and_then(|d| d.get("loan_number")).filter(|v| crate::models::truthy(v)).map(py_str);
            number.or(t.reference.clone().filter(|r| !r.is_empty())).unwrap_or_default().ends_with(reference)
        })
        .collect();
    if payments.is_empty() {
        return Ok(());
    }
    let rows: Vec<usize> = (0..plan.rows.len()).filter(|&i| plan.rows[i].kind == "installment" && plan.rows[i].date.is_some()).collect();
    let mut good = 0;
    let mut odd: Vec<(Transaction, Decimal)> = vec![];
    let mut unknown: Vec<Transaction> = vec![];
    for t in payments {
        let gap = |i: usize| (plan.rows[i].date.unwrap() - t.date).num_days().abs();
        let near: Vec<usize> = rows.iter().copied().filter(|&i| gap(i) <= 5).collect();
        if near.is_empty() {
            unknown.push(t);
            continue;
        }
        let mut best = near[0];
        for &i in &near[1..] {
            if gap(i) < gap(best) {
                best = i;
            }
        }
        let paid = py::neg(t.amount);
        if (paid - plan.rows[best].total).abs() <= d("0.01") {
            good += 1;
            plan.rows[best].confirmed = Some(paid);
        } else {
            odd.push((t, plan.rows[best].total));
        }
    }
    if good > 0 {
        plan.checks.push(check("ok", format!("{good} prélèvement(s) retrouvé(s) dans tes relevés : montant et date conformes au plan.")));
    }
    for (t, total) in odd {
        plan.checks.push(check("warn", format!("Prélèvement du {} : {} débités, {} prévus au plan.", fr(Some(t.date)), euros(&py::neg(t.amount)), euros(&total))));
    }
    for t in unknown {
        plan.checks.push(check("warn", format!("Prélèvement du {} ({}) : aucune échéance du plan à cette date.", fr(Some(t.date)), euros(&py::neg(t.amount)))));
    }
    Ok(())
}

fn sum(rows: &[&Row], f: impl Fn(&Row) -> Decimal) -> Decimal {
    rows.iter().fold(Decimal::ZERO, |a, r| a + f(r))
}

fn finish(loan: &Loan, plan: &mut Plan, today: NaiveDate, offer: &[LoanInstallment], evs: &[LoanEvent]) {
    let rows = &plan.rows;
    let inst: Vec<&Row> = rows.iter().filter(|r| r.kind == "installment").collect();
    if inst.is_empty() {
        return;
    }
    // Capital owed today: what the last line up to today leaves, or the amount borrowed before the first one.
    let mut owed = if rows[0].kind == "installment" { Some(rows[0].remaining + rows[0].capital) } else { loan.principal };
    for r in rows {
        if r.date.map(|d| d <= today).unwrap_or(false) {
            owed = Some(r.remaining);
        }
    }
    let past: Vec<&Row> = inst.iter().copied().filter(|r| r.date.map(|d| d <= today).unwrap_or(false)).collect();
    let future: Vec<&Row> = inst.iter().copied().filter(|r| r.date.map(|d| d > today).unwrap_or(false)).collect();
    let end = inst.last().unwrap().date;
    let mut summary = Summary {
        as_of: today,
        owed,
        end_date: end,
        paid_count: past.len(),
        remaining_count: future.len(),
        total_count: inst.len(),
        next: future.first().map(|r| (*r).clone()),
        dated: rows.iter().all(|r| r.date.is_some()),
        future_interest: sum(&future, |r| r.interest),
        future_insurance: sum(&future, |r| r.insurance),
        future_total: sum(&future, |r| r.total),
        paid_total: sum(&past, |r| r.total),
        paid_interest: sum(&past, |r| r.interest),
        lifetime_interest: sum(&inst, |r| r.interest),
        repaid_early: rows.iter().filter(|r| r.kind == "prepayment").fold(Decimal::ZERO, |a, r| a + r.capital),
        saved: None,
    };
    // What the early repayment saves: the contract's remaining instalments against the ones that replaced them.
    if let (Some(ev), false, Some(fd)) = (evs.first(), offer.is_empty(), loan.first_due_date) {
        if let Some(cut) = offer.iter().find(|o| o.remaining == ev.capital_before) {
            let cut_n = cut.number.unwrap_or(0);
            let old: Vec<&LoanInstallment> = offer.iter().filter(|o| o.number.unwrap_or(0) > cut_n).collect();
            let limit = add_months(fd, cut_n - 1);
            let after: Vec<&Row> = inst.iter().copied().filter(|r| r.date.map(|d| d > limit).unwrap_or(false)).collect();
            summary.saved = Some(Saved {
                interest: old.iter().fold(Decimal::ZERO, |a, o| a + o.interest) - sum(&after, |r| r.interest),
                insurance: old.iter().fold(Decimal::ZERO, |a, o| a + o.insurance) - sum(&after, |r| r.insurance),
                months: old.len() as i64 - after.len() as i64,
            });
        }
    }
    let all_dated = summary.dated;
    let last_zero = inst.last().unwrap().remaining.is_zero();
    let schedule_used = rows.iter().any(|r| r.source == "échéancier");
    plan.summary = Some(summary);
    if last_zero && all_dated {
        let ok = plan.problems().is_empty();
        plan.checks.push(check(
            if ok { "ok" } else { "warn" },
            format!(
                "En appliquant le contrat{}{}, le prêt est entièrement remboursé le {} (capital restant dû 0,00 €).{}",
                if evs.is_empty() { "" } else { ", le remboursement anticipé" },
                if schedule_used { " et le tableau daté" } else { "" },
                fr(end),
                if ok { "" } else { " Des incohérences sont signalées ci-dessus." }
            ),
        ));
    }
}

// --------------------------------------------------------------------------------------------- syncing

fn apply_plan(c: &Connection, loan: &mut Loan, plan: &Plan, set_rate: bool) -> AppResult<()> {
    if plan.rows.is_empty() {
        return Ok(());
    }
    let before_loan = loan.clone();
    if let Some(e) = plan.summary.as_ref().and_then(|s| s.end_date) {
        loan.end_date = Some(e);
    }
    loan.save(c, &before_loan)?;
    if let Some(mut account) = Account::get(c, loan.account_id)? {
        let before = account.clone();
        if let Some(s) = &plan.summary {
            if let Some(owed) = s.owed {
                account.balance = owed;
                account.balance_date = Some(s.as_of.and_hms_opt(0, 0, 0).unwrap());
            }
        }
        if set_rate && loan.rate.is_some() && account.interest_rate.is_none() {
            account.interest_rate = loan.rate;
        }
        account.save(c, &before)?;
    }
    Ok(())
}

/// Refresh what is derived from the documents (dates, end, the account's balance) and return the plan.
pub fn rebuild(c: &Connection, loan: &mut Loan, today: Option<NaiveDate>) -> AppResult<Plan> {
    if let Some(fresh) = Loan::get(c, loan.id)? {
        *loan = fresh;
    }
    if let Some(inferred) = infer_first_due(c, loan)? {
        let before = loan.clone();
        loan.first_due_date = Some(inferred);
        loan.save(c, &before)?;
    }
    let plan = build(c, loan, today)?;
    apply_plan(c, loan, &plan, true)?;
    Ok(plan)
}

/// The capital owed moves every month: bring the balance of every loan account up to date.
pub fn refresh_all(c: &Connection, today: Option<NaiveDate>) -> AppResult<()> {
    for mut loan in Loan::query(c, "", [])? {
        // everyone's loans: only balances are updated, nothing is shown
        if !installments(c, loan.id)?.is_empty() || !events(c, loan.id)?.is_empty() {
            rebuild(c, &mut loan, today)?;
        }
    }
    Ok(())
}

pub fn set_first_due(c: &Connection, loan: &mut Loan, day: NaiveDate) -> AppResult<Plan> {
    let before = loan.clone();
    loan.first_due_date = Some(day);
    loan.save(c, &before)?;
    rebuild_without_inference(c, loan)
}

pub fn rebuild_without_inference(c: &Connection, loan: &mut Loan) -> AppResult<Plan> {
    let plan = build(c, loan, None)?;
    if plan.summary.as_ref().map(|s| s.owed.is_some()).unwrap_or(false) {
        apply_plan(c, loan, &plan, false)?;
    }
    Ok(plan)
}

// ------------------------------------------------------------------------------------------- formatting

pub fn fr(day: Option<NaiveDate>) -> String {
    day.map(|d| d.format("%d/%m/%Y").to_string()).unwrap_or_else(|| "?".into())
}

/// "1 234,56 €", with a real minus sign.
pub fn euros(value: &Decimal) -> String {
    let s = fmt2(&value.abs());
    let (whole, cents) = s.split_once('.').unwrap();
    let mut groups: Vec<&str> = vec![];
    let mut w = whole;
    while w.len() > 3 {
        groups.insert(0, &w[w.len() - 3..]);
        w = &w[..w.len() - 3];
    }
    groups.insert(0, w);
    format!("{}{},{}\u{a0}€", if *value < Decimal::ZERO { "−" } else { "" }, groups.join("\u{202f}"), cents)
}

pub fn pct(value: &Decimal) -> String {
    format!("{} %", fmt2(value).replace('.', ","))
}
