//! Store imported operations without duplicating what is already there.

use std::collections::{BTreeSet, HashMap, HashSet};

use chrono::NaiveDate;
use rusqlite::Connection;
use rust_decimal::Decimal;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::adapters::base::TransactionData;
use crate::adapters::common::name_key;
use crate::adapters::re::re;
use crate::error::AppResult;
use crate::models::{utcnow, Account, Snapshot, Transaction};
use crate::py;
use crate::services::counterparties;

fn norm_label(label: &str) -> String {
    re(r"[^a-z0-9]").replace_all(&py::lower(label), "").into_owned()
}

/// Stable ids for de-duplication. Identical lines on the same day are told apart by their rank.
pub fn fingerprints(transactions: &[TransactionData]) -> Vec<String> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    transactions
        .iter()
        .map(|t| {
            let key = match t.external_id.as_deref().filter(|s| !s.is_empty()) {
                Some(id) => format!("id:{id}"),
                None => {
                    let text = t.raw.as_deref().filter(|s| !s.is_empty()).unwrap_or(&t.label);
                    let base = format!("{}|{}|{}", t.date.format("%Y-%m-%d"), py::dec_normalize_str(&t.amount), norm_label(text));
                    let n = seen.entry(base.clone()).or_insert(0);
                    *n += 1;
                    format!("{base}|{n}")
                }
            };
            hex::encode(Sha256::digest(key.as_bytes()))
        })
        .collect()
}

/// The lines of a statement, sorted by what importing them would do (indexes into the statement's lines).
#[derive(Default, Clone, Debug)]
pub struct Classified {
    pub new: Vec<(usize, String)>,
    /// Same fingerprint as a line already stored: the very same operation, seen again (overlapping statements).
    pub exact: Vec<usize>,
    /// Same day and amount as a stored line but not the same text: probably the same operation from another source.
    pub probable: Vec<(usize, String)>,
}

pub fn classify(c: &Connection, account_id: Option<i64>, transactions: &[TransactionData]) -> AppResult<Classified> {
    let mut result = Classified::default();
    let fps = fingerprints(transactions);
    let Some(account_id) = account_id else {
        result.new = fps.into_iter().enumerate().collect();
        return Ok(result);
    };
    let mut st = c.prepare_cached("SELECT fingerprint FROM transactions WHERE account_id = ?")?;
    let existing: HashSet<String> = st.query_map([account_id], |r| r.get::<_, String>(0))?.collect::<Result<_, _>>()?;
    let mut known: HashMap<(NaiveDate, String), i64> = HashMap::new();
    let mut st = c.prepare_cached("SELECT date, amount FROM transactions WHERE account_id = ?")?;
    let rows = st.query_map([account_id], |r| {
        Ok((<NaiveDate as crate::models::Col>::from_sql(r.get_ref(0)?), <Decimal as crate::models::Col>::from_sql(r.get_ref(1)?)))
    })?;
    for row in rows {
        let (d, a) = row?;
        *known.entry((d, py::dec_normalize_str(&a))).or_insert(0) += 1;
    }
    for (i, (t, fp)) in transactions.iter().zip(fps).enumerate() {
        if existing.contains(&fp) {
            result.exact.push(i);
            continue;
        }
        let key = (t.date, py::dec_normalize_str(&t.amount));
        match known.get_mut(&key) {
            Some(n) if *n > 0 => {
                *n -= 1;
                result.probable.push((i, fp));
            }
            _ => result.new.push((i, fp)),
        }
    }
    Ok(result)
}

pub const TRANSFER_KINDS: &[&str] = &["transfer", "instant_transfer"];

/// Per owner, the names that stand for them: the holder of an account held alone, and the holder line of every
/// account as the bank prints it (a transfer to the joint account shows that line, even cut). Another user's name is
/// not "me".
pub fn own_name_keys(c: &Connection) -> AppResult<HashMap<Option<i64>, HashSet<String>>> {
    let mut keys: HashMap<Option<i64>, HashSet<String>> = HashMap::new();
    let mut st = c.prepare_cached("SELECT owner_id, holder_name, holder_alias FROM accounts")?;
    let rows = st.query_map([], |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?)))?;
    for row in rows {
        let (owner, holder, alias) = row?;
        let mine = keys.entry(owner).or_default();
        if let Some(h) = holder.filter(|h| !h.is_empty()) {
            if re(r"(?i)\s+(?:OU|ET)\s+").find(&h).is_none() {
                mine.insert(name_key(&h));
            }
        }
        if let Some(a) = alias.filter(|a| !a.is_empty()) {
            mine.insert(name_key(&a));
        }
    }
    Ok(keys)
}

fn owners(c: &Connection) -> AppResult<HashMap<i64, Option<i64>>> {
    let mut st = c.prepare_cached("SELECT id, owner_id FROM accounts")?;
    let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?)))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// `{**(details or {}), key: value, ...}`.
pub fn with_details(details: &Option<Value>, pairs: &[(&str, Value)]) -> Value {
    let mut m: Map<String, Value> = details.as_ref().and_then(|d| d.as_object().cloned()).unwrap_or_default();
    for (k, v) in pairs {
        m.insert(k.to_string(), v.clone());
    }
    Value::Object(m)
}

pub fn detail(t: &Transaction, key: &str) -> Option<Value> {
    t.details.as_ref().and_then(|d| d.get(key)).cloned()
}

fn transfers(c: &Connection, tail: &str) -> AppResult<Vec<Transaction>> {
    Ok(Transaction::query(c, &format!("WHERE kind IN ('transfer', 'instant_transfer'){tail}"), [])?)
}

/// Flag the transfers whose other party is the user (their other accounts): they are not income nor spending.
pub fn mark_internal(c: &Connection) -> AppResult<i64> {
    let keys = own_name_keys(c)?;
    if !keys.values().any(|k| !k.is_empty()) {
        return Ok(0);
    }
    let owners = owners(c)?;
    let empty = HashSet::new();
    let mut marked = 0;
    for mut t in transfers(c, " AND counterparty_name IS NOT NULL")? {
        let owner = owners.get(&t.account_id).copied().flatten();
        if !t.internal() && keys.get(&owner).unwrap_or(&empty).contains(&name_key(t.counterparty_name.as_deref().unwrap_or(""))) {
            t.details = Some(with_details(&t.details, &[("internal", Value::Bool(true))]));
            t.update(c)?;
            marked += 1;
        }
    }
    Ok(marked)
}

fn paired(t: &Transaction) -> bool {
    detail(t, "paired_with").map(|v| crate::models::truthy(&v)).unwrap_or(false)
}

/// Match a transfer leaving one account with the one arriving on another account the same day (or the day after).
///
/// Names are unreliable (a statement may print the other account's label, or the holder who ordered the transfer), but
/// an opposite amount on two of the user's accounts on the same day is not: both lines are then "between my own
/// accounts".
pub fn pair_transfers(c: &Connection) -> AppResult<i64> {
    let owners = owners(c)?; // two people's accounts are not "my own accounts"
    let mut pool: Vec<Transaction> = transfers(c, " ORDER BY date, id")?.into_iter().filter(|t| !paired(t)).collect();
    let mut by_amount: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, t) in pool.iter().enumerate() {
        by_amount.entry(py::dec_normalize_str(&t.amount)).or_default().push(i);
    }
    let mut changed: BTreeSet<usize> = BTreeSet::new();
    let mut count = 0;
    for i in 0..pool.len() {
        if paired(&pool[i]) || pool[i].amount >= Decimal::ZERO {
            continue;
        }
        let t = &pool[i];
        let owner = owners.get(&t.account_id).copied().flatten();
        let wanted = py::dec_normalize_str(&py::neg(t.amount));
        let candidates: Vec<usize> = by_amount
            .get(&wanted)
            .map(|v| {
                v.iter()
                    .copied()
                    .filter(|&j| {
                        let o = &pool[j];
                        o.account_id != t.account_id
                            && owners.get(&o.account_id).copied().flatten() == owner
                            && (o.date - t.date).num_days().abs() <= 1
                            && !paired(o)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let Some(&other) = candidates.iter().min_by_key(|&&j| ((pool[j].date - t.date).num_days().abs(), pool[j].id)) else { continue };
        let (tid, oid) = (pool[i].id, pool[other].id);
        pool[i].details = Some(with_details(&pool[i].details, &[("internal", Value::Bool(true)), ("paired_with", Value::from(oid))]));
        pool[other].details = Some(with_details(&pool[other].details, &[("internal", Value::Bool(true)), ("paired_with", Value::from(tid))]));
        changed.insert(i);
        changed.insert(other);
        count += 1;
    }
    for i in changed {
        pool[i].update(c)?;
    }
    Ok(count)
}

/// `(text or None) and text[:n]`.
pub fn cut(text: &Option<String>, n: usize) -> Option<String> {
    text.as_deref().filter(|s| !s.is_empty()).map(|s| py::take_chars(s, n))
}

pub fn store(
    c: &Connection,
    account: &Account,
    items: &[(&TransactionData, String)],
    source: &str,
    document_id: Option<i64>,
    counterparty_index: Option<&HashMap<(String, String), i64>>,
) -> AppResult<i64> {
    let loaded;
    let index = match counterparty_index {
        Some(i) => i,
        None => {
            loaded = counterparties::load_index(c)?;
            &loaded
        }
    };
    let mut added = 0;
    for (t, fp) in items {
        let mut row = Transaction {
            account_id: account.id,
            fingerprint: fp.clone(),
            date: t.date,
            label: py::take_chars(&t.label, 500),
            amount: t.amount,
            currency: t.currency.clone(),
            source: source.into(),
            document_id,
            value_date: t.value_date,
            kind: t.kind.clone(),
            counterparty_name: cut(&t.counterparty, 300),
            counterparty_iban: t.counterparty_iban.clone(),
            counterparty_id: counterparties::matching(index, t.counterparty.as_deref(), t.counterparty_iban.as_deref()),
            reference: cut(&t.reference, 200),
            memo: t.memo.clone(),
            raw_label: t.raw.clone(),
            details: Some(t.details.clone().map(Value::Object).unwrap_or(Value::Null)),
            created_at: utcnow(),
            ..Default::default()
        };
        row.insert(c)?;
        added += 1;
    }
    Ok(added)
}

pub fn record_snapshot(c: &Connection, account_id: i64, value: Decimal, day: Option<NaiveDate>) -> AppResult<()> {
    let day = day.unwrap_or_else(crate::services::rates::today);
    match Snapshot::query_one(c, "WHERE account_id = ? AND day = ?", rusqlite::params![account_id, day.format("%Y-%m-%d").to_string()])? {
        None => {
            Snapshot { account_id, day, value, ..Default::default() }.insert(c)?;
        }
        Some(mut s) => {
            s.value = value;
            s.update(c)?;
        }
    }
    Ok(())
}
