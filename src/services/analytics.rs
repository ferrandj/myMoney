//! Numbers behind the analysis views: balance history, repartition, income and spending.
//!
//! History of an account is rebuilt from what was imported, not from when the app happened to be opened: starting
//! from the latest known balance and walking back through the operations (balance before = balance after minus the
//! operation), or forward for operations dated after it. Accounts without operations (manual savings, positions) fall
//! back to the daily snapshots. An account counts from its first known day: before that it is simply not in the total.

use std::collections::{BTreeMap, HashMap};

use chrono::{Datelike, Duration, NaiveDate};
use rusqlite::Connection;
use rust_decimal::Decimal;
use serde_json::Value;

use crate::error::AppResult;
use crate::models::{Account, Col, FeeSummary, Holding, Transaction, CATEGORIES};
use crate::py;
use crate::services::categories::{self, Categorizer};
use crate::services::valuation::AccountValue;

pub const PERIODS: &[(&str, &str, Option<i64>)] = &[("1m", "1 mois", Some(30)), ("3m", "3 mois", Some(91)), ("6m", "6 mois", Some(182)), ("1a", "1 an", Some(365)), ("tout", "Tout", None)];
pub const MAX_POINTS: i64 = 150;

pub type History = BTreeMap<NaiveDate, Decimal>;

#[derive(Clone, Debug)]
pub struct Series {
    pub key: String,
    pub label: String,
    pub points: Vec<(NaiveDate, Decimal)>,
}

#[derive(Clone, Debug)]
pub struct Stats {
    pub first: Decimal,
    pub last: Decimal,
    pub low: Decimal,
    pub high: Decimal,
}

impl Stats {
    pub fn delta(&self) -> Decimal {
        self.last - self.first
    }
    pub fn pct(&self) -> Option<py::PyDec> {
        if self.first.is_zero() {
            return None;
        }
        Some(py::PyDec::from(&self.delta()).div(&py::PyDec::from(&self.first.abs()))?.mul(&py::PyDec::int(100)))
    }
}

#[derive(Clone, Debug)]
pub struct MonthFlow {
    /// first day of the month
    pub month: NaiveDate,
    pub income: Decimal,
    /// positive number: what went out
    pub expense: Decimal,
}

impl MonthFlow {
    pub fn net(&self) -> Decimal {
        self.income - self.expense
    }
}

#[derive(Clone, Debug)]
pub struct Ranking {
    pub label: String,
    pub total: Decimal,
    pub count: i64,
    pub contact_id: Option<i64>,
}

#[derive(Clone, Debug, Default)]
pub struct Spending {
    pub by_kind: Vec<Ranking>,
    pub top_out: Vec<Ranking>,
    pub top_in: Vec<Ranking>,
    pub total_out: Decimal,
    pub total_in: Decimal,
}

/// Names of the contacts, for the operations linked to one.
pub fn contact_names(c: &Connection) -> AppResult<HashMap<i64, String>> {
    let mut st = c.prepare_cached("SELECT id, name FROM counterparties")?;
    let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, String::from_sql(r.get_ref(1)?))))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn date_param(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

// ------------------------------------------------------------------------------------------ history

/// End-of-day value in EUR of one account, one entry per day from its first known day to today.
/// A loan has no operations to walk back through: its debt is read from the repayment plan (capital left after each
/// instalment or early repayment), from the month before the first instalment. None when the plan has no dates yet.
fn loan_history(c: &Connection, account: &Account, today: NaiveDate) -> AppResult<Option<History>> {
    use crate::services::loans;
    let Some(loan) = loans::loan_of(c, account.id)? else { return Ok(None) };
    let plan = loans::build(c, &loan, Some(today))?;
    let mut rows: Vec<&loans::Row> = plan.rows.iter().filter(|r| r.date.is_some()).collect();
    if rows.is_empty() {
        return Ok(None);
    }
    rows.sort_by_key(|r| r.date);
    let first = rows[0].date.unwrap();
    let start = loan.first_due_date.map(|d| loans::add_months(d, -1)).unwrap_or_else(|| loans::add_months(first, -1)).min(first);
    let mut owed = loan.principal.unwrap_or(rows[0].remaining + rows[0].capital);
    let (mut next, mut day) = (0, start);
    let mut history = History::new();
    while day <= today {
        while next < rows.len() && rows[next].date.unwrap() <= day {
            owed = rows[next].remaining;
            next += 1;
        }
        history.insert(day, -owed);
        day += Duration::days(1);
    }
    Ok(Some(history))
}

pub fn account_history(c: &Connection, account: &Account, holdings: &[Holding], current: Decimal, today: NaiveDate) -> AppResult<History> {
    if matches!(account.account_type.as_str(), "mortgage" | "loan") {
        if let Some(history) = loan_history(c, account, today)? {
            return Ok(history);
        }
    }
    let mut st = c.prepare_cached("SELECT date, amount FROM transactions WHERE account_id = ?")?;
    let rows: Vec<(NaiveDate, Decimal)> = st.query_map([account.id], |r| Ok((NaiveDate::from_sql(r.get_ref(0)?), Decimal::from_sql(r.get_ref(1)?))))?.collect::<Result<_, _>>()?;
    if let (false, true, Some(bd)) = (rows.is_empty(), holdings.is_empty(), account.balance_date) {
        // Value in EUR of one unit of the account's balance: 1 for euro accounts, the rate for others, -1 for debts.
        let factor = if account.balance.is_zero() { py::PyDec::int(1) } else { py::PyDec::from(&current).div(&py::PyDec::from(&account.balance)).unwrap() };
        let mut moves: HashMap<NaiveDate, Decimal> = HashMap::new();
        for (day, amount) in &rows {
            let e = moves.entry(*day).or_insert(Decimal::ZERO);
            *e += *amount;
        }
        let (anchor_day, anchor) = (bd.date(), account.balance);
        let first = *moves.keys().min().unwrap();
        let last_move = *moves.keys().max().unwrap();
        let mut history = History::new();
        let (mut cursor, mut day) = (anchor, anchor_day);
        while day >= first - Duration::days(1) {
            history.insert(day, py::PyDec::from(&cursor).mul(&factor).to_decimal());
            cursor -= moves.get(&day).copied().unwrap_or(Decimal::ZERO);
            day -= Duration::days(1);
        }
        let (mut cursor, mut day) = (anchor, anchor_day + Duration::days(1));
        while day <= today.max(last_move) {
            cursor += moves.get(&day).copied().unwrap_or(Decimal::ZERO);
            history.insert(day, py::PyDec::from(&cursor).mul(&factor).to_decimal());
            day += Duration::days(1);
        }
        return Ok(extend(history, today));
    }
    let mut st = c.prepare_cached("SELECT day, value FROM snapshots WHERE account_id = ?")?;
    let mut snapshots: HashMap<NaiveDate, Decimal> =
        st.query_map([account.id], |r| Ok((NaiveDate::from_sql(r.get_ref(0)?), Decimal::from_sql(r.get_ref(1)?))))?.collect::<Result<_, _>>()?;
    snapshots.insert(today, current);
    let start = *snapshots.keys().min().unwrap();
    let mut history = History::new();
    let (mut last, mut day) = (snapshots[&start], start);
    while day <= today {
        if let Some(v) = snapshots.get(&day) {
            last = *v;
        }
        history.insert(day, last);
        day += Duration::days(1);
    }
    Ok(history)
}

fn extend(mut history: History, today: NaiveDate) -> History {
    let (&last_day, &value) = history.iter().next_back().unwrap();
    let mut day = last_day + Duration::days(1);
    while day <= today {
        history.insert(day, value);
        day += Duration::days(1);
    }
    history
}

pub fn period_days(period: &str) -> Option<i64> {
    PERIODS.iter().find(|p| p.0 == period).or_else(|| PERIODS.iter().find(|p| p.0 == "1a")).unwrap().2
}

pub fn period_start(period: &str, today: NaiveDate, earliest: NaiveDate) -> NaiveDate {
    match period_days(period) {
        None => earliest,
        Some(d) => earliest.max(today - Duration::days(d)),
    }
}

pub(crate) fn days(start: NaiveDate, end: NaiveDate) -> Vec<NaiveDate> {
    let count = (end - start).num_days() + 1;
    let step = 1.max((count + MAX_POINTS - 1) / MAX_POINTS);
    let mut out: Vec<NaiveDate> = (0..count).step_by(step as usize).map(|i| start + Duration::days(i)).collect();
    if out.last() != Some(&end) {
        out.push(end);
    }
    out
}

/// Lines to draw: the total of the selection, one line per account, or one per category.
pub fn build_series(histories: &HashMap<i64, History>, accounts: &HashMap<i64, Account>, selected: &[i64], group: &str, start: NaiveDate, end: NaiveDate) -> Vec<Series> {
    let chosen: Vec<i64> = selected.iter().copied().filter(|i| histories.contains_key(i)).collect();
    let days = days(start, end);
    let line = |ids: &[i64]| -> Vec<(NaiveDate, Decimal)> {
        let mut points = vec![];
        for day in &days {
            let known: Vec<Decimal> = ids.iter().filter_map(|i| histories[i].get(day).copied()).collect();
            if !known.is_empty() {
                points.push((*day, known.iter().fold(Decimal::ZERO, |a, b| a + b)));
            }
        }
        points
    };
    match group {
        "compte" => chosen
            .iter()
            .filter_map(|i| {
                let p = line(&[*i]);
                if p.is_empty() {
                    None
                } else {
                    Some(Series { key: i.to_string(), label: accounts[i].name.clone(), points: p })
                }
            })
            .collect(),
        "categorie" => {
            let mut out = vec![];
            for (code, label) in CATEGORIES {
                let ids: Vec<i64> = chosen.iter().copied().filter(|i| accounts[i].category() == *code).collect();
                if ids.is_empty() {
                    continue;
                }
                let points = line(&ids);
                if !points.is_empty() {
                    out.push(Series { key: code.to_string(), label: label.to_string(), points });
                }
            }
            out
        }
        _ => {
            let points = line(&chosen);
            if points.is_empty() {
                vec![]
            } else {
                vec![Series { key: "total".into(), label: "Total".into(), points }]
            }
        }
    }
}

pub fn stats(points: &[(NaiveDate, Decimal)]) -> Option<Stats> {
    let values: Vec<Decimal> = points.iter().map(|p| p.1).collect();
    let first = *values.first()?;
    let mut low = first;
    let mut high = first;
    for v in &values[1..] {
        if *v < low {
            low = *v;
        }
        if *v > high {
            high = *v;
        }
    }
    Some(Stats { first, last: *values.last().unwrap(), low, high })
}

pub fn earliest_day(histories: &HashMap<i64, History>, selected: &[i64], today: NaiveDate) -> NaiveDate {
    selected.iter().filter_map(|i| histories.get(i)).filter_map(|h| h.keys().next().copied()).min().unwrap_or(today)
}

// --------------------------------------------------------------------------------------- repartition

/// (key, label, amount) of the positive holdings, largest first. Debts are reported apart.
pub fn repartition(values: &[AccountValue], group: &str) -> Vec<(String, String, Decimal)> {
    let mut order: Vec<String> = vec![];
    let mut amounts: HashMap<String, (String, Decimal)> = HashMap::new();
    for v in values {
        let total = v.total();
        if total <= Decimal::ZERO {
            continue;
        }
        let (key, label) = if group == "categorie" {
            (v.account.category().to_string(), crate::models::category_label(v.account.category()).to_string())
        } else {
            (v.account.id.to_string(), v.account.name.clone())
        };
        let prev = amounts.get(&key).map(|x| x.1).unwrap_or(Decimal::ZERO);
        if !amounts.contains_key(&key) {
            order.push(key.clone());
        }
        amounts.insert(key, (label, prev + total));
    }
    let mut out: Vec<(String, String, Decimal)> = order.into_iter().map(|k| {
        let (l, t) = amounts.remove(&k).unwrap();
        (k, l, t)
    }).collect();
    out.sort_by(|a, b| b.2.cmp(&a.2));
    out
}

// ---------------------------------------------------------------------------------- income and spending

fn transactions(c: &Connection, account_ids: &[i64], start: NaiveDate, end: NaiveDate, include_internal: bool) -> AppResult<Vec<Transaction>> {
    if account_ids.is_empty() {
        return Ok(vec![]);
    }
    let marks = vec!["?"; account_ids.len()].join(", ");
    let mut params: Vec<rusqlite::types::Value> = account_ids.iter().map(|i| rusqlite::types::Value::Integer(*i)).collect();
    params.push(rusqlite::types::Value::Text(date_param(start)));
    params.push(rusqlite::types::Value::Text(date_param(end)));
    let rows = Transaction::query(c, &format!("WHERE account_id IN ({marks}) AND date >= ? AND date <= ?"), rusqlite::params_from_iter(params))?;
    Ok(rows.into_iter().filter(|t| include_internal || !t.internal()).collect())
}

fn next_month(d: NaiveDate) -> NaiveDate {
    let (y, m) = (d.year() + (d.month() as i32) / 12, d.month() % 12 + 1);
    NaiveDate::from_ymd_opt(y, m, 1).unwrap()
}

fn month_of(d: NaiveDate) -> NaiveDate {
    NaiveDate::from_ymd_opt(d.year(), d.month(), 1).unwrap()
}

pub fn monthly_flows(c: &Connection, account_ids: &[i64], start: NaiveDate, end: NaiveDate, include_internal: bool) -> AppResult<Vec<MonthFlow>> {
    let mut months: BTreeMap<NaiveDate, MonthFlow> = BTreeMap::new();
    let mut cursor = month_of(start);
    while cursor <= end {
        months.insert(cursor, MonthFlow { month: cursor, income: Decimal::ZERO, expense: Decimal::ZERO });
        cursor = next_month(cursor);
    }
    for t in transactions(c, account_ids, start, end, include_internal)? {
        let Some(flow) = months.get_mut(&month_of(t.date)) else { continue };
        if t.amount >= Decimal::ZERO {
            flow.income += t.amount;
        } else {
            flow.expense += py::neg(t.amount);
        }
    }
    Ok(months.into_values().collect())
}

pub fn kind_name(kind: &str) -> &'static str {
    match kind {
        "card" => "Cartes",
        "transfer" => "Virements",
        "instant_transfer" => "Virements instantanés",
        "direct_debit" => "Prélèvements",
        "loan_payment" => "Prêts",
        "fee" => "Frais",
        "cash" => "Retraits",
        "cheque" => "Chèques",
        "interest" => "Intérêts",
        _ => "Autres",
    }
}

struct Bucket {
    order: Vec<String>,
    items: HashMap<String, Ranking>,
}

impl Bucket {
    fn new() -> Self {
        Bucket { order: vec![], items: HashMap::new() }
    }
    fn add(&mut self, key: &str, label: &str, contact: Option<i64>, amount: Decimal) {
        if !self.items.contains_key(key) {
            self.order.push(key.to_string());
            self.items.insert(key.to_string(), Ranking { label: label.to_string(), total: Decimal::ZERO, count: 0, contact_id: contact });
        }
        let e = self.items.get_mut(key).unwrap();
        e.total += amount;
        e.count += 1;
    }
    fn ranked(mut self) -> Vec<Ranking> {
        let mut out: Vec<Ranking> = self.order.iter().map(|k| self.items.remove(k).unwrap()).collect();
        out.sort_by(|a, b| b.total.cmp(&a.total));
        out
    }
}

pub fn spending(c: &Connection, account_ids: &[i64], start: NaiveDate, end: NaiveDate, include_internal: bool, limit: usize) -> AppResult<Spending> {
    let mut result = Spending::default();
    let names = contact_names(c)?;
    let (mut kinds, mut out, mut inc) = (Bucket::new(), Bucket::new(), Bucket::new());
    for t in transactions(c, account_ids, start, end, include_internal)? {
        let amount = t.amount.abs();
        let contact = t.counterparty_id.filter(|i| *i != 0);
        let who = match t.counterparty_id.and_then(|i| names.get(&i)) {
            Some(n) => n.clone(),
            None => t.counterparty_name.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| t.label.clone()),
        };
        let key = match contact {
            Some(id) => format!("c{id}"),
            None => py::lower(&who),
        };
        if t.amount < Decimal::ZERO {
            result.total_out += amount;
            let label = kind_name(t.kind.as_deref().unwrap_or(""));
            kinds.add(label, label, None, amount);
            out.add(&key, &who, contact, amount);
        } else {
            result.total_in += amount;
            inc.add(&key, &who, contact, amount);
        }
    }
    result.by_kind = kinds.ranked();
    result.top_out = out.ranked().into_iter().take(limit).collect();
    result.top_in = inc.ranked().into_iter().take(limit).collect();
    Ok(result)
}

// ---------------------------------------------------------------------------- spending by category

#[derive(Clone, Debug)]
pub struct CategoryTotal {
    pub id: &'static str,
    pub label: &'static str,
    pub total: Decimal,
    pub count: i64,
    /// % of the total of that direction
    pub share: py::PyDec,
}

#[derive(Clone, Debug)]
pub struct MerchantTotal {
    pub key: String,
    pub label: String,
    pub contact_id: Option<i64>,
    pub total: Decimal,
    pub count: i64,
    pub category: &'static str,
    /// the user chose this category (not a guess)
    pub ruled: bool,
}

/// The bank's own recap of fees for one account and period, next to what the statements' operations say.
/// A credit that gives fees back ("REMBOURSEMENT EXCEPTIONNEL", a waived card fee...): it is taken off the fees, never
/// counted as one.
fn is_fee_refund(t: &Transaction, category: &str) -> bool {
    if t.amount <= Decimal::ZERO {
        return false;
    }
    if category == "frais" {
        return true;
    }
    let label = crate::adapters::common::norm(&t.label);
    category == "remboursements" && ["frais", "exceptionnel", "cotisation", "commission", "agio", "inactivite"].iter().any(|w| label.contains(w))
}

#[derive(Clone, Debug)]
pub struct FeeYear {
    pub account_id: i64,
    pub account_name: String,
    pub document_id: Option<i64>,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    /// services billed
    pub charged: Decimal,
    /// refunds in the member's favour (positive number)
    pub refunded: Decimal,
    /// what the bank states as the total of fees paid
    pub net: Decimal,
    pub interest_paid: Option<Decimal>,
    pub interest_earned: Option<Decimal>,
    pub services: Vec<Value>,
    /// fee operations found in the imported statements over the same period
    pub operations: Decimal,
    pub operations_count: i64,
    /// fees given back found in the same operations (already taken off `operations`)
    pub operations_refunded: Decimal,
}

impl FeeYear {
    pub fn gap(&self) -> Decimal {
        self.net - self.operations
    }
}

#[derive(Clone, Debug, Default)]
pub struct FeesReport {
    /// net fees found in the operations of the period shown
    pub operations_total: Decimal,
    pub operations_count: i64,
    /// fees given back found in the operations of the period (already taken off `operations_total`)
    pub operations_refunded: Decimal,
    pub summaries: Vec<FeeYear>,
}

impl FeesReport {
    pub fn summaries_total(&self) -> Decimal {
        self.summaries.iter().fold(Decimal::ZERO, |a, f| a + f.net)
    }
    pub fn interest_paid(&self) -> Decimal {
        self.summaries.iter().fold(Decimal::ZERO, |a, f| a + py::or_zero(f.interest_paid))
    }
    pub fn refunded(&self) -> Decimal {
        self.summaries.iter().fold(Decimal::ZERO, |a, f| a + f.refunded)
    }
}

#[derive(Clone, Debug, Default)]
pub struct SpendingReport {
    /// out | in
    pub direction: String,
    /// of that direction
    pub total: Decimal,
    pub total_out: Decimal,
    pub total_in: Decimal,
    pub categories: Vec<CategoryTotal>,
    pub months: Vec<(NaiveDate, Decimal)>,
    pub category: Option<CategoryTotal>,
    pub merchants: Vec<MerchantTotal>,
    /// (operation, account name), newest first
    pub operations: Vec<(Transaction, String)>,
    /// all categories mixed
    pub top_merchants: Vec<MerchantTotal>,
    pub fees: FeesReport,
}

impl SpendingReport {
    pub fn saved(&self) -> Decimal {
        self.total_in - self.total_out
    }
}

/// Where the money went (or came from): per category, then per merchant and operation inside one category.
pub fn spending_report(
    c: &Connection,
    account_ids: &[i64],
    start: NaiveDate,
    end: NaiveDate,
    include_internal: bool,
    direction: &str,
    category: Option<&str>,
) -> AppResult<SpendingReport> {
    let mut report = SpendingReport { direction: direction.to_string(), ..Default::default() };
    let rules = categories::load_rules(c)?;
    let contacts = contact_names(c)?;
    let mut categorize = Categorizer::new(&rules);
    let mut names: HashMap<i64, String> = HashMap::new();
    if !account_ids.is_empty() {
        let marks = vec!["?"; account_ids.len()].join(", ");
        for a in Account::query(c, &format!("WHERE id IN ({marks})"), rusqlite::params_from_iter(account_ids.iter()))? {
            names.insert(a.id, a.name);
        }
    }
    let mut totals_order: Vec<&'static str> = vec![];
    let mut totals: HashMap<&'static str, CategoryTotal> = HashMap::new();
    let mut merchants_order: Vec<(&'static str, String)> = vec![];
    let mut merchants: HashMap<(&'static str, String), MerchantTotal> = HashMap::new();
    let mut months: BTreeMap<NaiveDate, Decimal> = BTreeMap::new();
    let mut cursor = month_of(start);
    while cursor <= end {
        months.insert(cursor, Decimal::ZERO);
        cursor = next_month(cursor);
    }
    let mut chosen_ops: Vec<(Transaction, String)> = vec![];
    for t in transactions(c, account_ids, start, end, include_internal)? {
        let amount = t.amount;
        if amount.is_zero() {
            continue;
        }
        if amount < Decimal::ZERO {
            report.total_out += py::neg(amount);
        } else {
            report.total_in += amount;
        }
        let contact = t.counterparty_id.and_then(|i| contacts.get(&i)).map(|s| s.as_str());
        let cat = categorize.decide(&t, contact);
        if amount < Decimal::ZERO && cat == "frais" {
            report.fees.operations_total += py::neg(amount);
            report.fees.operations_count += 1;
        } else if is_fee_refund(&t, cat) {
            report.fees.operations_total -= amount; // given back: it cancels a fee, it is not one
            report.fees.operations_refunded += amount;
        }
        if (amount < Decimal::ZERO) != (direction == "out") {
            continue;
        }
        let (key, label, contact_id) = categories::merchant(&t, contact);
        let value = amount.abs();
        report.total += value;
        if !totals.contains_key(cat) {
            totals_order.push(cat);
            totals.insert(cat, CategoryTotal { id: cat, label: categories::get(cat).unwrap().label, total: Decimal::ZERO, count: 0, share: py::PyDec::int(0) });
        }
        let entry = totals.get_mut(cat).unwrap();
        entry.total += value;
        entry.count += 1;
        let mk = (cat, key.clone());
        if !merchants.contains_key(&mk) {
            merchants_order.push(mk.clone());
            let ruled = rules.get(&key).map(|r| categories::get(r).is_some()).unwrap_or(false);
            merchants.insert(mk.clone(), MerchantTotal { key: key.clone(), label, contact_id, total: Decimal::ZERO, count: 0, category: cat, ruled });
        }
        let m = merchants.get_mut(&mk).unwrap();
        m.total += value;
        m.count += 1;
        if category.is_none() || category == Some(cat) {
            if let Some(v) = months.get_mut(&month_of(t.date)) {
                *v += value;
            }
        }
        if category == Some(cat) {
            let n = names.get(&t.account_id).cloned().unwrap_or_default();
            chosen_ops.push((t, n));
        }
    }
    for entry in totals.values_mut() {
        entry.share = if report.total.is_zero() {
            py::PyDec::int(0)
        } else {
            py::PyDec::from(&entry.total).div(&py::PyDec::from(&report.total)).unwrap().mul(&py::PyDec::int(100))
        };
    }
    let mut cats: Vec<CategoryTotal> = totals_order.iter().map(|k| totals[k].clone()).collect();
    cats.sort_by(|a, b| b.total.cmp(&a.total));
    report.categories = cats;
    report.months = months.into_iter().collect();
    let mut by_order: Vec<String> = vec![];
    let mut by_key: HashMap<String, MerchantTotal> = HashMap::new();
    for mk in &merchants_order {
        let m = &merchants[mk];
        if !by_key.contains_key(&m.key) {
            by_order.push(m.key.clone());
            by_key.insert(m.key.clone(), MerchantTotal { total: Decimal::ZERO, count: 0, ..m.clone() });
        }
        let top = by_key.get_mut(&m.key).unwrap();
        top.total += m.total;
        top.count += m.count;
    }
    let mut tops: Vec<MerchantTotal> = by_order.iter().map(|k| by_key.remove(k).unwrap()).collect();
    tops.sort_by(|a, b| b.total.cmp(&a.total));
    tops.truncate(12);
    report.top_merchants = tops;
    report.fees.summaries = fee_summaries(c, account_ids, start, end, &names, &rules, &contacts)?;
    if let Some(cat) = category {
        if let Some(t) = totals.get(cat) {
            report.category = Some(t.clone());
            let mut ms: Vec<MerchantTotal> = merchants_order.iter().filter(|(c, _)| *c == cat).map(|mk| merchants[mk].clone()).collect();
            ms.sort_by(|a, b| b.total.cmp(&a.total));
            report.merchants = ms;
            chosen_ops.sort_by(|a, b| (b.0.date, b.0.id).cmp(&(a.0.date, a.0.id)));
            chosen_ops.truncate(300);
            report.operations = chosen_ops;
        }
    }
    Ok(report)
}

/// The bank's recaps of fees whose period touches start..end, each with the fee operations the statements show for
/// it.
pub fn fee_summaries(
    c: &Connection,
    account_ids: &[i64],
    start: NaiveDate,
    end: NaiveDate,
    names: &HashMap<i64, String>,
    rules: &HashMap<String, String>,
    contacts: &HashMap<i64, String>,
) -> AppResult<Vec<FeeYear>> {
    if account_ids.is_empty() {
        return Ok(vec![]);
    }
    let marks = vec!["?"; account_ids.len()].join(", ");
    let mut params: Vec<rusqlite::types::Value> = account_ids.iter().map(|i| rusqlite::types::Value::Integer(*i)).collect();
    params.push(rusqlite::types::Value::Text(date_param(start)));
    params.push(rusqlite::types::Value::Text(date_param(end)));
    let rows = FeeSummary::query(
        c,
        &format!("WHERE account_id IN ({marks}) AND period_end >= ? AND period_start <= ? ORDER BY period_start DESC, account_id"),
        rusqlite::params_from_iter(params),
    )?;
    let mut result = vec![];
    for r in rows {
        let services: Vec<Value> = r.services.as_ref().and_then(|s| s.as_array()).cloned().unwrap_or_default();
        let totals: Vec<Decimal> = services
            .iter()
            .map(|x| match x.get("total") {
                None => Decimal::ZERO,
                Some(Value::String(s)) => py::dec_parse(s).unwrap_or_default(),
                Some(v) => crate::services::prices::number_str(v).and_then(|s| py::dec_parse(&s)).unwrap_or_default(),
            })
            .collect();
        let charged = totals.iter().filter(|t| **t > Decimal::ZERO).fold(Decimal::ZERO, |a, b| a + b);
        let refunded = py::neg(totals.iter().filter(|t| **t < Decimal::ZERO).fold(Decimal::ZERO, |a, b| a + b));
        let mut year = FeeYear {
            account_id: r.account_id,
            account_name: names.get(&r.account_id).cloned().unwrap_or_default(),
            document_id: r.document_id,
            period_start: r.period_start,
            period_end: r.period_end,
            charged,
            refunded,
            net: r.fees_total,
            interest_paid: r.interest_paid,
            interest_earned: r.interest_earned,
            services,
            operations: Decimal::ZERO,
            operations_count: 0,
            operations_refunded: Decimal::ZERO,
        };
        for t in transactions(c, &[r.account_id], r.period_start, r.period_end, true)? {
            let contact = t.counterparty_id.and_then(|i| contacts.get(&i)).map(|s| s.as_str());
            let cat = categories::categorize(&t, contact, rules);
            if t.amount < Decimal::ZERO && cat == "frais" {
                year.operations += py::neg(t.amount);
                year.operations_count += 1;
            } else if is_fee_refund(&t, cat) {
                year.operations -= t.amount;
                year.operations_refunded += t.amount;
            }
        }
        result.push(year);
    }
    Ok(result)
}
