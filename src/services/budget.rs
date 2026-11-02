//! Budget mode: every operation of the accounts the user can see, one by one, each with its category.
//!
//! The category of an operation comes (see `categories`) from the choice made for that very operation, else from the rule
//! kept for its merchant ("Carrefour is Alimentation"), else from the guess. Changing the category of a row keeps a rule
//! for the merchant by default, so the operations that read the same, past and future, follow.

use std::collections::HashMap;

use chrono::NaiveDate;
use rusqlite::Connection;
use rust_decimal::Decimal;

use crate::adapters::common::norm;
use crate::error::{value_err, AppError, AppResult};
use crate::models::{Account, Counterparty, Transaction};
use crate::py;
use crate::services::analytics::contact_names;
use crate::services::categories::{self, Categorizer};
use crate::services::access;

#[derive(Clone, Debug, Default)]
pub struct Filter {
    pub account: Option<i64>,
    /// a category id, or empty for all
    pub category: String,
    /// "tout" | "depenses" | "entrees"
    pub sens: String,
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
    pub text: String,
    /// "date" | "compte" | "libelle" | "montant" | "categorie"
    pub sort: String,
    pub desc: bool,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub t: Transaction,
    pub account: String,
    pub category: &'static str,
    /// the merchant as the rules know it, and what to call it
    pub key: String,
    pub merchant: String,
    /// this very operation was given its category
    pub own: bool,
    /// a rule kept for its merchant decides
    pub ruled: bool,
}

#[derive(Clone, Debug)]
pub struct Total {
    pub id: &'static str,
    pub label: &'static str,
    pub total: Decimal,
    pub count: usize,
    pub share: Decimal,
}

#[derive(Clone, Debug, Default)]
pub struct Listing {
    pub rows: Vec<Row>,
    /// spending by category over the operations selected by everything but the category filter
    pub totals: Vec<Total>,
    pub total_out: Decimal,
    pub total_in: Decimal,
    /// transfers between my accounts, left out of both totals
    pub transfers: Decimal,
    pub accounts: Vec<Account>,
}

pub fn list(c: &Connection, f: &Filter) -> AppResult<Listing> {
    let accounts = access::only_readable(c, Account::query(c, "ORDER BY name, id", [])?)?;
    let names: HashMap<i64, String> = accounts.iter().map(|a| (a.id, a.name.clone())).collect();
    let ids: Vec<i64> = accounts.iter().map(|a| a.id).filter(|i| f.account.map(|x| x == *i).unwrap_or(true)).collect();
    let mut listing = Listing { accounts, ..Default::default() };
    if ids.is_empty() {
        return Ok(listing);
    }
    let marks = vec!["?"; ids.len()].join(", ");
    let mut params: Vec<rusqlite::types::Value> = ids.iter().map(|i| rusqlite::types::Value::Integer(*i)).collect();
    let mut tail = format!("WHERE account_id IN ({marks})");
    if let Some(d) = f.from {
        tail.push_str(" AND date >= ?");
        params.push(rusqlite::types::Value::Text(d.format("%Y-%m-%d").to_string()));
    }
    if let Some(d) = f.to {
        tail.push_str(" AND date <= ?");
        params.push(rusqlite::types::Value::Text(d.format("%Y-%m-%d").to_string()));
    }
    let transactions = Transaction::query(c, &tail, rusqlite::params_from_iter(params))?;
    let rules = categories::load_rules(c)?;
    let contacts = contact_names(c)?;
    let mut categorize = Categorizer::new(&rules);
    let needle = norm(&f.text);
    let mut base: Vec<Row> = vec![];
    for t in transactions {
        if f.sens == "depenses" && t.amount >= Decimal::ZERO || f.sens == "entrees" && t.amount < Decimal::ZERO {
            continue;
        }
        let contact = t.counterparty_id.and_then(|i| contacts.get(&i)).map(|s| s.as_str());
        let category = categorize.decide(&t, contact);
        let (key, merchant, _) = categories::merchant(&t, contact);
        let account = names.get(&t.account_id).cloned().unwrap_or_default();
        if !needle.is_empty() {
            let hay = norm(&format!("{} {} {} {} {}", t.label, t.counterparty_name.as_deref().unwrap_or(""), t.memo.as_deref().unwrap_or(""), account, categories::get(category).map(|c| c.label).unwrap_or("")));
            if !hay.contains(&needle) {
                continue;
            }
        }
        let own = t.category.as_deref().and_then(categories::get).is_some();
        let ruled = !own && rules.get(&key).and_then(|r| categories::get(r)).is_some();
        base.push(Row { t, account, category, key, merchant, own, ruled });
    }

    // spending by category: what comes out, without the transfers between my own accounts
    let mut totals: HashMap<&'static str, Total> = HashMap::new();
    for r in &base {
        if r.category == "interne" {
            listing.transfers += r.t.amount.abs();
            continue;
        }
        if r.t.amount < Decimal::ZERO {
            listing.total_out += py::neg(r.t.amount);
            let e = totals.entry(r.category).or_insert_with(|| Total { id: r.category, label: categories::get(r.category).map(|c| c.label).unwrap_or(""), total: Decimal::ZERO, count: 0, share: Decimal::ZERO });
            e.total += py::neg(r.t.amount);
            e.count += 1;
        } else {
            listing.total_in += r.t.amount;
        }
    }
    let mut totals: Vec<Total> = totals.into_values().collect();
    for t in totals.iter_mut() {
        t.share = if listing.total_out.is_zero() { Decimal::ZERO } else { (t.total * Decimal::from(100) / listing.total_out).round_dp(1) };
    }
    totals.sort_by(|a, b| b.total.cmp(&a.total).then(a.label.cmp(b.label)));
    listing.totals = totals;

    let mut rows: Vec<Row> = base.into_iter().filter(|r| f.category.is_empty() || r.category == f.category).collect();
    rows.sort_by(|a, b| {
        let by = match f.sort.as_str() {
            "compte" => norm(&a.account).cmp(&norm(&b.account)),
            "libelle" => norm(&a.t.label).cmp(&norm(&b.t.label)),
            "montant" => a.t.amount.cmp(&b.t.amount),
            "categorie" => categories::get(a.category).map(|c| norm(c.label)).cmp(&categories::get(b.category).map(|c| norm(c.label))),
            _ => a.t.date.cmp(&b.t.date),
        };
        by.then(a.t.date.cmp(&b.t.date)).then(a.t.id.cmp(&b.t.id))
    });
    if f.desc {
        rows.reverse();
    }
    listing.rows = rows;
    Ok(listing)
}

/// Give an operation its category. With `remember` the choice is kept for its merchant (the operations that read the same
/// follow, now and later) and the operation itself goes back to following the rule; without it only this operation is
/// concerned. "auto" gives the decision back to the guess. Returns what to tell the user.
pub fn set_category(c: &Connection, transaction_id: i64, category: &str, remember: bool) -> AppResult<String> {
    let Some(mut t) = Transaction::get(c, transaction_id)? else { return Err(AppError::Denied("Opération introuvable".into())) };
    access::check_edit(c, t.account_id)?;
    if category != "auto" && categories::get(category).is_none() {
        return value_err("Catégorie inconnue");
    }
    let contact = match t.counterparty_id {
        Some(id) => Counterparty::get(c, id)?.map(|x| x.name),
        None => None,
    };
    let (key, merchant, _) = categories::merchant(&t, contact.as_deref());
    if remember {
        categories::set_rule(c, &key, category)?;
        t.category = None;
    } else {
        t.category = if category == "auto" { None } else { Some(category.to_string()) };
    }
    t.update(c)?;
    Ok(match (category, remember) {
        ("auto", true) => format!("« {merchant} » : classement automatique rétabli"),
        ("auto", false) => "Cette opération : classement automatique rétabli".to_string(),
        (cat, true) => format!("« {merchant} » est classé dans « {} » (cette opération et les opérations équivalentes, y compris les prochaines)", categories::get(cat).unwrap().label),
        (cat, false) => format!("Cette opération est classée dans « {} » (elle seule)", categories::get(cat).unwrap().label),
    })
}
