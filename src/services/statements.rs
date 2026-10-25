//! Turn a parsed statement into accounts and operations, checking that everything stays consistent.
//!
//! Nothing here writes before `plan_import` has looked at the statement against what is already stored: which account
//! it belongs to, which lines are already there (same operation seen twice, or probably the same one from another
//! source), whether its period overlaps or leaves a hole, and whether its balances continue the previous statement.

use std::collections::{HashMap, HashSet};

use chrono::{Datelike, Duration, NaiveDate};
use rusqlite::Connection;
use rust_decimal::Decimal;
use serde_json::{Map, Value};

use crate::adapters::base::ParsedStatement;
use crate::error::{value_err, AppError, AppResult};
use crate::models::{account_type, Account, Document, FeeSummary, Holding, Statement, Transaction};
use crate::py;
use crate::services::{access, coverage, holders, ledger, loans, naming};

pub type Form = HashMap<String, String>;

/// Documents worth keeping and reading, with no operation in them: they are attached to an account or filed.
pub fn document_only(kind: Option<&str>) -> bool {
    matches!(kind, Some("fees_summary") | Some("info")) || kind.map(|k| loans::LOAN_KINDS.contains(&k)).unwrap_or(false)
}

fn is_loan(kind: Option<&str>) -> bool {
    kind.map(|k| loans::LOAN_KINDS.contains(&k)).unwrap_or(false)
}

/// A statement for a period that was already imported: does the new file change anything?
#[derive(Clone, Debug)]
pub struct DedupReport {
    /// the statement already imported for this account and period
    pub previous: Statement,
    /// the file it came from (None once that file was deleted)
    pub document: Option<Document>,
    /// lines the new file has and the account does not (indexes into the statement)
    pub extra: Vec<usize>,
    /// lines the old file brought that the new file no longer has
    pub missing: Vec<Transaction>,
    /// (old, new) when the opening balance differs
    pub opening: Option<(Decimal, Decimal)>,
    pub closing: Option<(Decimal, Decimal)>,
    pub new_count: usize,
    pub probable_count: usize,
}

impl DedupReport {
    pub fn identical(&self) -> bool {
        self.extra.is_empty() && self.missing.is_empty() && self.opening.is_none() && self.closing.is_none()
    }
    /// Replacing needs the old file: it is what tells which lines to remove.
    pub fn can_replace(&self) -> bool {
        self.document.is_some()
    }
}

#[derive(Clone, Debug)]
pub struct ImportPlan {
    pub statement: ParsedStatement,
    pub account: Option<Account>,
    /// every account the identity could match (more than one = ask the user)
    pub candidates: Vec<Account>,
    pub classified: ledger::Classified,
    /// same account, same period: this statement was imported before
    pub already_imported: Option<Statement>,
    pub overlaps: Vec<Statement>,
    /// consistency problems with the neighbouring statements
    pub issues: Vec<String>,
    /// this statement ends after the balance currently stored
    pub newer_balance: bool,
    /// The holder line is unclear (cut by the bank, several people, no civility) and the names were never confirmed: ask once, before importing.
    pub ask_holder: bool,
    pub dedup: Option<DedupReport>,
    pub fee_existing: Option<FeeSummary>,
    pub loan_existing: Option<String>,
    pub loan_unverified: Option<String>,
}

impl ImportPlan {
    fn new(statement: ParsedStatement, account: Option<Account>, candidates: Vec<Account>, classified: ledger::Classified) -> Self {
        ImportPlan {
            statement,
            account,
            candidates,
            classified,
            already_imported: None,
            overlaps: vec![],
            issues: vec![],
            newer_balance: true,
            ask_holder: false,
            dedup: None,
            fee_existing: None,
            loan_existing: None,
            loan_unverified: None,
        }
    }

    /// For each line of the statement: new | exact (already stored) | probable (same day and amount as a stored line).
    pub fn statuses(&self) -> Vec<&'static str> {
        let exact: HashSet<usize> = self.classified.exact.iter().copied().collect();
        let probable: HashSet<usize> = self.classified.probable.iter().map(|(i, _)| *i).collect();
        (0..self.statement.transactions.len()).map(|i| if exact.contains(&i) { "exact" } else if probable.contains(&i) { "probable" } else { "new" }).collect()
    }

    /// Nothing to ask and nothing suspicious: safe to import without a review.
    pub fn clean(&self) -> bool {
        let st = &self.statement;
        let one = self.account.is_some() && self.candidates.len() == 1 && st.confidence == "high" && st.warnings.is_empty();
        match st.document_kind.as_deref() {
            Some("info") => true, // nothing to ask: it is just filed
            Some("fees_summary") => one && self.fee_existing.is_none() && !self.ask_holder,
            k if is_loan(k) => one && self.loan_existing.is_none() && self.loan_unverified.is_none() && !self.ask_holder,
            _ => {
                one && self.issues.is_empty()
                    && self.overlaps.is_empty()
                    && self.already_imported.is_none()
                    && self.classified.probable.is_empty()
                    && !self.ask_holder
            }
        }
    }
}

fn nonempty(v: &Option<String>) -> Option<&str> {
    v.as_deref().filter(|s| !s.is_empty())
}

/// Accounts this statement can belong to, best match first. Identity comes from the document, not the label.
pub fn find_accounts(c: &Connection, st: &ParsedStatement) -> AppResult<Vec<Account>> {
    if is_loan(st.document_kind.as_deref()) {
        let found = loans::find_accounts(c, st)?;
        return access::only_editable(c, found);
    }
    let accounts = access::only_editable(c, Account::query(c, "", [])?)?; // an account one cannot edit is not a target
    let iban = st.iban.clone().unwrap_or_default().replace(' ', "");
    let mut scored: Vec<(i64, Account)> = vec![];
    let number = nonempty(&st.account_number);
    for a in accounts {
        let mut score = 0;
        if !iban.is_empty() && nonempty(&a.iban).map(|x| x.replace(' ', "") == iban).unwrap_or(false) {
            score = 3;
        } else if number.is_some() && nonempty(&st.bank_code).is_some() && (a.bank_code.as_deref(), a.account_number.as_deref()) == (st.bank_code.as_deref(), st.account_number.as_deref()) {
            score = 3;
        } else if number.is_some() && a.account_number == st.account_number && (nonempty(&st.institution).is_none() || a.institution == st.institution) {
            score = 3; // documents without IBAN (savings plans): the account number is unique within a bank
        } else if let Some(n) = number {
            let chars: Vec<char> = n.chars().collect();
            let tail: String = chars[chars.len().saturating_sub(6)..].iter().collect();
            if nonempty(&a.iban).map(|i| i.contains(n)).unwrap_or(false) || nonempty(&a.account_number).map(|x| x.ends_with(&tail)).unwrap_or(false) {
                score = 1; // weak: same trailing digits
            }
        }
        if score > 0 {
            scored.push((score, a));
        }
    }
    let best = scored.iter().map(|(s, _)| *s).max().unwrap_or(0);
    Ok(scored.into_iter().filter(|(s, _)| *s == best).map(|(_, a)| a).collect())
}

pub fn plan_import(c: &Connection, st: &ParsedStatement, account: Option<&Account>) -> AppResult<ImportPlan> {
    let candidates = find_accounts(c, st)?;
    let kind = st.document_kind.as_deref();
    if document_only(kind) {
        let mut plan = ImportPlan::new(st.clone(), account.cloned(), candidates, ledger::Classified::default());
        if kind != Some("info") {
            plan.ask_holder = holder_unconfirmed(st, account);
        }
        if is_loan(kind) {
            plan.loan_existing = loans::existing_for(c, st, account)?;
            plan.loan_unverified = loans::unverified(c, st, account)?;
        }
        if let (Some(a), Some("fees_summary")) = (account, kind) {
            plan.fee_existing = fee_for(c, a.id, st.period_start, st.period_end)?;
        }
        return Ok(plan);
    }
    let classified = ledger::classify(c, account.map(|a| a.id), &st.transactions)?;
    let mut plan = ImportPlan::new(st.clone(), account.cloned(), candidates, classified);
    plan.ask_holder = holder_unconfirmed(st, account);
    let Some(account) = account else { return Ok(plan) };
    let existing = Statement::query(c, "WHERE account_id = ? ORDER BY period_start", [account.id])?;
    if let (Some(ps), Some(pe)) = (st.period_start, st.period_end) {
        for other in &existing {
            let (Some(os), Some(oe)) = (other.period_start, other.period_end) else { continue };
            if (os, oe) == (ps, pe) {
                plan.already_imported = Some(other.clone());
            } else if os <= pe && oe >= ps {
                plan.overlaps.push(other.clone());
            }
        }
        let already = plan.already_imported.as_ref().map(|s| s.id);
        let others: Vec<Statement> = existing.iter().filter(|s| Some(s.id) != already).cloned().collect();
        plan.issues = continuity(st, &others, &coverage::acknowledged_ranges(c, account.id)?, &waiting_periods(c, account, Some(st))?);
        if let Some(previous) = plan.already_imported.clone() {
            plan.dedup = Some(compare(c, st, &previous, &plan.classified)?);
        }
    }
    if let (Some(_), Some(abd), Some(sbd)) = (st.balance, account.balance_date, st.balance_date) {
        plan.newer_balance = sbd >= abd.date();
    }
    Ok(plan)
}

fn fee_for(c: &Connection, account_id: i64, start: Option<NaiveDate>, end: Option<NaiveDate>) -> AppResult<Option<FeeSummary>> {
    let (Some(s), Some(e)) = (start, end) else { return Ok(None) };
    Ok(FeeSummary::query_one(c, "WHERE account_id = ? AND period_start = ? AND period_end = ?", rusqlite::params![account_id, s.format("%Y-%m-%d").to_string(), e.format("%Y-%m-%d").to_string()])?)
}

/// Periods of the statements read but not validated yet that belong to this account: a period they cover is not
/// missing, it is waiting. (Judging a hole only on what is already imported would call "missing" what the next click
/// is about to import.) Their balances prove nothing yet, so only the periods count.
pub fn waiting_periods(c: &Connection, account: &Account, st: Option<&ParsedStatement>) -> AppResult<Vec<(NaiveDate, NaiveDate)>> {
    let mut waiting = vec![];
    for doc in access::visible_documents(c)? {
        if doc.status != "parsed" {
            continue;
        }
        let found = doc.parse_result.as_ref().and_then(|r| r.get("statements")).and_then(|s| s.as_array()).cloned().unwrap_or_default();
        if found.len() != 1 {
            continue;
        }
        let other = ParsedStatement::from_json(&found[0]);
        if document_only(other.document_kind.as_deref()) {
            continue;
        }
        let (Some(os), Some(oe)) = (other.period_start, other.period_end) else { continue };
        if let Some(st) = st {
            if (Some(os), Some(oe)) == (st.period_start, st.period_end) {
                continue; // this very document, or a copy of it
            }
        }
        if !find_accounts(c, &other)?.iter().any(|a| a.id == account.id) {
            continue;
        }
        waiting.push((os, oe));
    }
    Ok(waiting)
}

/// What the new file has that the account lacks, and what the old file had that it lost.
fn compare(c: &Connection, st: &ParsedStatement, previous: &Statement, classified: &ledger::Classified) -> AppResult<DedupReport> {
    let document = match previous.document_id {
        Some(id) => Document::get(c, id)?,
        None => None,
    };
    let mut missing = vec![];
    if let Some(doc) = &document {
        let new_fps: HashSet<String> = ledger::fingerprints(&st.transactions).into_iter().collect();
        missing = Transaction::query(c, "WHERE document_id = ?", [doc.id])?.into_iter().filter(|t| !new_fps.contains(&t.fingerprint)).collect();
    }
    let differ = |a: Option<Decimal>, b: Option<Decimal>| match (a, b) {
        (Some(x), Some(y)) if x != y => Some((x, y)),
        _ => None,
    };
    let extra: Vec<usize> = classified.new.iter().map(|(i, _)| *i).chain(classified.probable.iter().map(|(i, _)| *i)).collect();
    Ok(DedupReport {
        previous: previous.clone(),
        document,
        extra,
        missing,
        opening: differ(previous.opening_balance, st.opening_balance),
        closing: differ(previous.closing_balance, st.balance),
        new_count: classified.new.len(),
        probable_count: classified.probable.len(),
    })
}

fn holder_unconfirmed(st: &ParsedStatement, account: Option<&Account>) -> bool {
    let unclear = st.extra_truthy("holder_maybe_truncated") || nonempty(&st.holder_name).map(holders::is_unclear).unwrap_or(false);
    if !unclear {
        return false;
    }
    // Once the user has typed the full names they differ from what the bank prints: never ask again for this account.
    match account {
        None => true,
        Some(a) if a.holders.as_ref().map(|h| !holders::from_json(h).is_empty()).unwrap_or(false) => false,
        Some(a) => match nonempty(&a.holder_name) {
            None => true,
            Some(h) => Some(h) == nonempty(&a.holder_alias).or(st.holder_name.as_deref()),
        },
    }
}

fn dmy(d: NaiveDate) -> String {
    d.format("%d/%m/%Y").to_string()
}

/// The statement just before and just after must touch this one, and their balances must chain. A hole the user
/// marked as normal is not reported again, nor the part of it that a statement waiting for validation covers.
pub(crate) fn continuity(st: &ParsedStatement, existing: &[Statement], acknowledged: &[(NaiveDate, NaiveDate)], waiting: &[(NaiveDate, NaiveDate)]) -> Vec<String> {
    let mut issues = vec![];
    let (ps, pe) = (st.period_start.unwrap(), st.period_end.unwrap());
    let hole = |start: NaiveDate, end: NaiveDate| -> Vec<String> {
        if acknowledged.iter().any(|(a, b)| *a <= start && end <= *b) {
            return vec![];
        }
        uncovered(start, end, waiting).into_iter().map(|(a, b)| format!("Il manque une période : rien entre le {} et le {}", dmy(a), dmy(b))).collect()
    };
    let before: Vec<&Statement> = existing.iter().filter(|s| s.period_end.map(|e| e < ps).unwrap_or(false)).collect();
    let after: Vec<&Statement> = existing.iter().filter(|s| s.period_start.map(|s| s > pe).unwrap_or(false)).collect();
    if !before.is_empty() {
        let mut prev = before[0];
        for s in &before[1..] {
            if s.period_end > prev.period_end {
                prev = s;
            }
        }
        let prev_end = prev.period_end.unwrap();
        if (ps - prev_end).num_days() > 1 {
            issues.extend(hole(prev_end + Duration::days(1), ps - Duration::days(1)));
        } else if let (Some(cb), Some(ob)) = (prev.closing_balance, st.opening_balance) {
            if cb != ob {
                issues.push(format!(
                    "Le solde initial ({} {}) ne reprend pas le solde final du relevé précédent ({} au {}) : un relevé ou une opération manque peut-être",
                    py::dec_str(&ob),
                    st.currency,
                    py::dec_str(&cb),
                    dmy(prev_end)
                ));
            }
        }
    }
    if !after.is_empty() {
        let mut nxt = after[0];
        for s in &after[1..] {
            if s.period_start < nxt.period_start {
                nxt = s;
            }
        }
        let next_start = nxt.period_start.unwrap();
        if (next_start - pe).num_days() > 1 {
            issues.extend(hole(pe + Duration::days(1), next_start - Duration::days(1)));
        } else if let (Some(ob), Some(b)) = (nxt.opening_balance, st.balance) {
            if ob != b {
                issues.push(format!(
                    "Le solde final ({} {}) ne correspond pas au solde initial du relevé suivant ({} au {})",
                    py::dec_str(&b),
                    st.currency,
                    py::dec_str(&ob),
                    dmy(next_start)
                ));
            }
        }
    }
    issues
}

/// What is left of start..end once the covered periods are taken out.
pub fn uncovered(start: NaiveDate, end: NaiveDate, covered: &[(NaiveDate, NaiveDate)]) -> Vec<(NaiveDate, NaiveDate)> {
    let mut sorted = covered.to_vec();
    sorted.sort();
    let mut left = vec![];
    let mut cursor = start;
    for (a, b) in sorted {
        if b < cursor || a > end {
            continue;
        }
        if a > cursor {
            left.push((cursor, a - Duration::days(1)));
        }
        cursor = cursor.max(b + Duration::days(1));
    }
    if cursor <= end {
        left.push((cursor, end));
    }
    left
}

// --------------------------------------------------------------------------------------------- applying

fn fill_identity(account: &mut Account, st: &ParsedStatement, latest: bool) {
    let pairs: [(&Option<String>, &mut Option<String>); 6] = [
        (&st.iban, &mut account.iban),
        (&st.bic, &mut account.bic),
        (&st.bank_code, &mut account.bank_code),
        (&st.branch_code, &mut account.branch_code),
        (&st.account_number, &mut account.account_number),
        (&st.rib_key, &mut account.rib_key),
    ];
    for (value, target) in pairs {
        if nonempty(value).is_some() && nonempty(target).is_none() {
            *target = value.clone();
        }
    }
    if nonempty(&st.holder_name).is_some() && nonempty(&account.holder_alias).is_none() {
        account.holder_alias = st.holder_name.clone();
    }
    // Holder and address change over time: the newest statement wins, older ones only fill the blanks. Names the
    // user completed by hand (different from the printed line) are never overwritten by the cut version of the bank.
    let typed = account.holders.as_ref().map(|h| !holders::from_json(h).is_empty()).unwrap_or(false);
    let edited = typed || matches!((nonempty(&account.holder_name), nonempty(&account.holder_alias)), (Some(a), Some(b)) if a != b);
    if nonempty(&st.holder_name).is_some() && (latest || nonempty(&account.holder_name).is_none()) && !edited {
        account.holder_name = st.holder_name.clone();
    }
    if nonempty(&st.holder_address).is_some() && (latest || nonempty(&account.holder_address).is_none()) {
        account.holder_address = st.holder_address.clone();
    }
}

fn form_decimal(raw: Option<&String>) -> AppResult<Option<Decimal>> {
    let raw = raw.cloned().unwrap_or_default().replace(',', ".").replace(' ', "");
    if raw.is_empty() {
        return Ok(None);
    }
    crate::adapters::common::py_decimal(&raw).map(Some).ok_or_else(|| AppError::Other("[<class 'decimal.ConversionSyntax'>]".into()))
}

fn on(form: &Form, key: &str) -> bool {
    form.get(key).map(|s| s.as_str()) == Some("on")
}

/// An information document (no account, no operation): recognised, dated and kept with the others.
pub fn file_document(c: &Connection, doc: &mut Document) -> AppResult<Map<String, Value>> {
    let before = doc.clone();
    doc.status = "imported".into();
    doc.account_id = None;
    doc.save(c, &before)?;
    Ok(loans::import_result(None, Some("info")))
}

fn attach_fee_summary(c: &Connection, doc: &mut Document, st: &ParsedStatement, account: &mut Account, form: &Form) -> AppResult<Map<String, Value>> {
    if let Some(existing) = fee_for(c, account.id, st.period_start, st.period_end)? {
        if !on(form, "force") {
            return value_err("Ce récapitulatif des frais est déjà rattaché à ce compte pour cette période");
        }
        FeeSummary::delete(c, existing.id)?;
    }
    let before = account.clone();
    fill_identity(account, st, false); // an old recap never overwrites a newer address
    account.save(c, &before)?;
    let amount = |key: &str| -> AppResult<Option<Decimal>> {
        match st.extra.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => py::dec_parse(&lines_str(v)).map(Some).ok_or_else(|| AppError::Other("[<class 'decimal.ConversionSyntax'>]".into())),
        }
    };
    let (Some(ps), Some(pe)) = (st.period_start, st.period_end) else {
        return Err(AppError::Other("NOT NULL constraint failed: fee_summaries.period_start".into()));
    };
    let services = st.extra.get("services").filter(|v| crate::models::truthy(v)).cloned().unwrap_or(Value::Array(vec![]));
    FeeSummary {
        account_id: account.id,
        document_id: Some(doc.id),
        period_start: ps,
        period_end: pe,
        fees_total: py::or_zero(amount("fees_total")?),
        interest_paid: amount("interest_paid")?,
        interest_earned: amount("interest_earned")?,
        services: Some(services),
        ..Default::default()
    }
    .insert(c)?;
    let before_doc = doc.clone();
    doc.status = "imported".into();
    doc.account_id = Some(account.id);
    doc.save(c, &before_doc)?;
    let mut m = Map::new();
    m.insert("account_id".into(), account.id.into());
    m.insert("kind".into(), "fees_summary".into());
    m.insert("year".into(), pe.year().into());
    m.insert("added".into(), 0.into());
    m.insert("skipped_exact".into(), 0.into());
    m.insert("skipped_probable".into(), 0.into());
    Ok(m)
}

/// `str(value)` of a JSON scalar (the extra values are strings, a number would be printed as Python does).
fn lines_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        other => py::json_dumps(other),
    }
}

pub fn import_statement(c: &Connection, doc: &mut Document, st: &ParsedStatement, form: &Form) -> AppResult<Map<String, Value>> {
    let result = import_inner(c, doc, st, form)?;
    // imported: the file takes its standard name (the name it was dropped with is kept)
    naming::rename_document(c, doc)?;
    Ok(result)
}

/// An imported document read again: the blanks of its account are filled from the new reading (never overwriting what
/// is there), and the file gets its standard name.
pub fn refresh_identity(c: &Connection, doc: &mut Document) -> AppResult<()> {
    let st = doc.parse_result.as_ref().and_then(|r| r.get("statements")).and_then(|s| s.as_array()).and_then(|a| a.first()).map(ParsedStatement::from_json);
    if let (Some(account_id), Some(st)) = (doc.account_id, st) {
        if !document_only(st.document_kind.as_deref()) || st.document_kind.as_deref() == Some("fees_summary") {
            if let Some(mut account) = Account::get(c, account_id)? {
                let before = account.clone();
                fill_identity(&mut account, &st, false);
                account.save(c, &before)?;
            }
        }
    }
    naming::rename_document(c, doc)?;
    Ok(())
}

/// The people typed in the form become the account's holders; the line the bank prints is kept to recognise it.
fn apply_holders(c: &Connection, account: &mut Account, st: &ParsedStatement, form: &Form) -> AppResult<()> {
    let Some(people) = holders::from_form(form).filter(|p| !p.is_empty()) else { return Ok(()) };
    let before = account.clone();
    if nonempty(&st.holder_name).is_some() && nonempty(&account.holder_alias).is_none() {
        account.holder_alias = st.holder_name.clone();
    }
    account.holders = Some(holders::to_json(&people));
    account.holder_name = Some(holders::compose(&people));
    account.save(c, &before)?;
    Ok(())
}

fn import_inner(c: &Connection, doc: &mut Document, st: &ParsedStatement, form: &Form) -> AppResult<Map<String, Value>> {
    if st.document_kind.as_deref() == Some("info") {
        return file_document(c, doc);
    }
    let target = form.get("account_id").filter(|s| !s.is_empty()).cloned().unwrap_or_else(|| "new".into());
    let mut account = if target == "new" {
        let pick = |k: &str| form.get(k).filter(|s| !s.is_empty()).cloned();
        let mut a = Account {
            owner_id: access::owner_for_new(c)?,
            name: pick("name").or_else(|| st.account_label.clone().filter(|s| !s.is_empty())).unwrap_or_else(|| doc.filename.clone()),
            account_type: match pick("account_type") {
                Some(t) if account_type(&t).is_some() => t,
                _ => st.account_type.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| "checking".into()),
            },
            institution: pick("institution").or_else(|| st.institution.clone()),
            currency: st.currency.clone(),
            ..Default::default()
        };
        a.insert(c)?;
        a
    } else {
        let id = py::py_int(&target).map_err(AppError::Other)?;
        let a = Account::get(c, id)?.ok_or_else(|| AppError::Value("Compte introuvable".into()))?;
        access::check_edit(c, a.id)?;
        a
    };
    apply_holders(c, &mut account, st, form)?;
    match st.document_kind.as_deref() {
        Some("fees_summary") => return attach_fee_summary(c, doc, st, &mut account, form),
        k if is_loan(k) => return loans::attach(c, doc, st, &mut account, form),
        _ => {}
    }
    let plan = plan_import(c, st, Some(&account))?;
    if plan.already_imported.is_some() && !on(form, "force") {
        return value_err("Ce relevé a déjà été importé dans ce compte (même période)");
    }

    let statements = Statement::query(c, "WHERE account_id = ?", [account.id])?;
    let latest = !statements.iter().any(|s| matches!((s.period_end, st.period_end), (Some(a), Some(b)) if a > b));
    let before = account.clone();
    fill_identity(&mut account, st, latest);
    let full_names = py::split_ws(form.get("holder_name").map(|s| s.as_str()).unwrap_or("")).join(" ");
    if !full_names.is_empty() && holders::from_form(form).is_none() {
        // the user completed the names the bank cut
        account.holder_name = Some(full_names);
    }
    let balance = form_decimal(form.get("balance"))?;
    if on(form, "update_balance") {
        if let Some(b) = balance {
            account.balance = b;
            account.balance_date = Some(st.balance_date.unwrap_or_else(crate::services::rates::today).and_hms_opt(0, 0, 0).unwrap());
        }
    }
    account.save(c, &before)?;

    let mut added = 0;
    let skipped_exact = plan.classified.exact.len();
    let mut skipped_probable = plan.classified.probable.len();
    if on(form, "import_transactions") {
        let mut items: Vec<(&crate::adapters::base::TransactionData, String)> = plan.classified.new.iter().map(|(i, fp)| (&st.transactions[*i], fp.clone())).collect();
        if on(form, "import_probable") {
            items.extend(plan.classified.probable.iter().map(|(i, fp)| (&st.transactions[*i], fp.clone())));
            skipped_probable = 0;
        }
        added = ledger::store(c, &account, &items, "import", Some(doc.id), None)?;
    }
    let holdings = st.holdings.clone().unwrap_or_default();
    if !holdings.is_empty() && on(form, "import_holdings") {
        c.execute("DELETE FROM holdings WHERE account_id = ? AND manual = 1", [account.id])?;
        for h in &holdings {
            Holding {
                account_id: account.id,
                symbol: h.symbol.clone(),
                isin: h.isin.clone(),
                name: h.name.clone(),
                asset_class: h.asset_class.clone(),
                quantity: h.quantity,
                value: h.value,
                value_currency: h.value_currency.clone(),
                manual: true,
                ..Default::default()
            }
            .insert(c)?;
        }
    }
    if (on(form, "import_transactions") || !holdings.is_empty()) && plan.already_imported.is_none() {
        Statement {
            account_id: account.id,
            document_id: Some(doc.id),
            period_start: st.period_start,
            period_end: st.period_end,
            opening_balance: st.opening_balance,
            closing_balance: st.balance,
            tx_count: st.transactions.len() as i64,
            added_count: added,
            ..Default::default()
        }
        .insert(c)?;
    }
    let before_doc = doc.clone();
    doc.status = "imported".into();
    doc.account_id = Some(account.id);
    doc.save(c, &before_doc)?;
    ledger::mark_internal(c)?; // a new account (or new names) can turn earlier transfers into "between my own accounts"
    ledger::pair_transfers(c)?;
    let mut m = Map::new();
    m.insert("account_id".into(), account.id.into());
    m.insert("added".into(), added.into());
    m.insert("skipped_exact".into(), skipped_exact.into());
    m.insert("skipped_probable".into(), skipped_probable.into());
    Ok(m)
}

/// Remove what a document brought in (its operations and statement records). The document itself stays.
pub fn undo_import(c: &Connection, doc: &mut Document) -> AppResult<i64> {
    let mut st = c.prepare_cached("SELECT id FROM transactions WHERE document_id = ?")?;
    let gone: HashSet<i64> = st.query_map([doc.id], |r| r.get::<_, i64>(0))?.collect::<Result<_, _>>()?;
    for mut t in Transaction::query(c, "WHERE kind IN ('transfer', 'instant_transfer')", [])? {
        let paired = ledger::detail(&t, "paired_with").and_then(|v| v.as_i64());
        if paired.map(|p| gone.contains(&p)).unwrap_or(false) {
            // its other half is leaving: this line is free again
            let rest: Map<String, Value> = t.details.as_ref().and_then(|d| d.as_object()).map(|m| m.iter().filter(|(k, _)| *k != "paired_with" && *k != "internal").map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
            let before = t.clone();
            t.details = Some(if rest.is_empty() { Value::Null } else { Value::Object(rest) });
            t.save(c, &before)?;
        }
    }
    let removed = c.execute("DELETE FROM transactions WHERE document_id = ?", [doc.id])? as i64;
    c.execute("DELETE FROM statements WHERE document_id = ?", [doc.id])?;
    c.execute("DELETE FROM fee_summaries WHERE document_id = ?", [doc.id])?;
    loans::detach(c, doc)?;
    let before = doc.clone();
    let has = doc.parse_result.as_ref().and_then(|r| r.get("statements")).map(crate::models::truthy).unwrap_or(false);
    doc.status = if has { "parsed".into() } else { "uploaded".into() };
    doc.account_id = None;
    doc.save(c, &before)?;
    naming::restore_name(c, doc)?; // waiting again: it shows the name it was dropped with
    ledger::mark_internal(c)?; // a line can still be internal by its name
    Ok(removed)
}

/// What a one-click import does: the account the identity points to, operations, balance if it is newer.
pub fn default_import_form(plan: &ImportPlan) -> Form {
    let st = &plan.statement;
    let mut form = Form::new();
    form.insert("account_id".into(), plan.account.as_ref().map(|a| a.id.to_string()).unwrap_or_else(|| "new".into()));
    form.insert("import_transactions".into(), "on".into());
    if let (Some(b), true) = (st.balance, plan.newer_balance) {
        form.insert("update_balance".into(), "on".into());
        form.insert("balance".into(), py::dec_str(&b));
    }
    if st.holdings.as_ref().map(|h| !h.is_empty()).unwrap_or(false) {
        form.insert("import_holdings".into(), "on".into());
    }
    form
}

/// Statements of an account in order, and the places where consecutive balances do not chain. (Missing periods are
/// in services/coverage, where they can be marked as normal.)
pub fn coverage(c: &Connection, account_id: i64) -> AppResult<(Vec<Statement>, Vec<String>)> {
    let rows: Vec<Statement> =
        Statement::query(c, "WHERE account_id = ? ORDER BY period_start", [account_id])?.into_iter().filter(|s| s.period_start.is_some() && s.period_end.is_some()).collect();
    let mut broken = vec![];
    for w in rows.windows(2) {
        let (prev, nxt) = (&w[0], &w[1]);
        if (nxt.period_start.unwrap() - prev.period_end.unwrap()).num_days() == 1 {
            if let (Some(cb), Some(ob)) = (prev.closing_balance, nxt.opening_balance) {
                if cb != ob {
                    broken.push(format!(
                        "Les soldes ne se suivent pas entre le {} ({}) et le {} ({})",
                        dmy(prev.period_end.unwrap()),
                        py::dec_str(&cb),
                        dmy(nxt.period_start.unwrap()),
                        py::dec_str(&ob)
                    ));
                }
            }
        }
    }
    Ok((rows, broken))
}
