//! Contacts: remember who an IBAN or a name belongs to, so the next import recognises them by itself.

use std::collections::{BTreeSet, HashMap};

use chrono::NaiveDate;
use rusqlite::Connection;
use rust_decimal::Decimal;

use crate::adapters::common::{iban_valid, name_key};
use crate::error::{value_err, AppError, AppResult};
use crate::models::{Col, Counterparty, CounterpartyIdentifier, Transaction, COUNTERPARTY_KINDS};
use crate::py;
use crate::services::access;

/// Card payments are shops: listing each merchant as "someone to identify" would drown the people and organisations
/// that matter. They are still matched when an identifier exists, and can be shown on request.
pub const SHOP_KINDS: &[&str] = &["card"];

pub fn normalize_iban(value: Option<&str>) -> Option<String> {
    let iban = py::upper(&py::split_ws(value.unwrap_or("")).concat());
    if iban.is_empty() {
        None
    } else {
        Some(iban)
    }
}

pub type Index = HashMap<(String, String), i64>;

pub fn load_index(c: &Connection) -> AppResult<Index> {
    Ok(CounterpartyIdentifier::query(c, "", [])?.into_iter().map(|i| ((i.kind, i.value), i.counterparty_id)).collect())
}

/// The contact an operation belongs to: the IBAN is exact, the name is compared in a normalised form.
pub fn matching(index: &Index, name: Option<&str>, iban: Option<&str>) -> Option<i64> {
    if let Some(iban) = normalize_iban(iban) {
        if let Some(&hit) = index.get(&("iban".to_string(), iban)) {
            if hit != 0 {
                return Some(hit);
            }
        }
    }
    match name.filter(|n| !n.is_empty()) {
        Some(n) => index.get(&("name".to_string(), name_key(n))).copied(),
        None => None,
    }
}

#[derive(Clone, Debug)]
pub struct Unresolved {
    /// iban | name
    pub kind: String,
    /// the IBAN or the name key
    pub key: String,
    pub name: Option<String>,
    pub iban: Option<String>,
    pub count: i64,
    pub total_in: Decimal,
    pub total_out: Decimal,
    pub last: NaiveDate,
    pub accounts: BTreeSet<i64>,
}

const UNLINKED: &str = "WHERE counterparty_id IS NULL AND (counterparty_name IS NOT NULL OR counterparty_iban IS NOT NULL)";

/// Counterparties seen in the operations but not linked to a contact yet, most frequent first.
pub fn unresolved(c: &Connection, include_shops: bool, document_id: Option<i64>, account_id: Option<i64>) -> AppResult<Vec<Unresolved>> {
    let mut order: Vec<(String, String)> = vec![];
    let mut groups: HashMap<(String, String), Unresolved> = HashMap::new();
    let mut tail = UNLINKED.to_string();
    let mut params: Vec<i64> = vec![];
    if let Some(d) = document_id {
        tail.push_str(" AND document_id = ?");
        params.push(d);
    }
    if let Some(a) = account_id {
        tail.push_str(" AND account_id = ?");
        params.push(a);
    }
    let visible = access::readable_ids(c)?;
    for t in Transaction::query(c, &tail, rusqlite::params_from_iter(params))? {
        if let Some(v) = &visible {
            if !v.contains(&t.account_id) {
                continue;
            }
        }
        let shop = t.kind.as_deref().map(|k| SHOP_KINDS.contains(&k)).unwrap_or(false);
        if t.internal() || (shop && !include_shops && t.counterparty_iban.as_deref().map(|s| s.is_empty()).unwrap_or(true)) {
            continue;
        }
        let iban = normalize_iban(t.counterparty_iban.as_deref());
        let key = match &iban {
            Some(i) => ("iban".to_string(), i.clone()),
            None => ("name".to_string(), name_key(t.counterparty_name.as_deref().unwrap_or(""))),
        };
        let g = groups.entry(key.clone()).or_insert_with(|| {
            order.push(key.clone());
            Unresolved {
                kind: key.0.clone(),
                key: key.1.clone(),
                name: t.counterparty_name.clone(),
                iban: iban.clone(),
                count: 0,
                total_in: Decimal::ZERO,
                total_out: Decimal::ZERO,
                last: t.date,
                accounts: BTreeSet::new(),
            }
        });
        g.count += 1;
        g.accounts.insert(t.account_id);
        if g.name.as_deref().map(|s| s.is_empty()).unwrap_or(true) {
            g.name = t.counterparty_name.clone();
        }
        if t.amount >= Decimal::ZERO {
            g.total_in += t.amount;
        } else {
            g.total_out += py::neg(t.amount);
        }
        g.last = g.last.max(t.date);
    }
    let mut out: Vec<Unresolved> = order.into_iter().map(|k| groups.remove(&k).unwrap()).collect();
    out.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.clone().unwrap_or_default().cmp(&b.name.clone().unwrap_or_default())));
    Ok(out)
}

/// Normalise an identifier and make sure it is usable: (value, the identifier already holding it), or None if empty.
///
/// Fails for an invalid IBAN, or one that already belongs to another contact (never silently stolen).
fn check_identifier(c: &Connection, kind: &str, value: Option<&str>, owner_id: Option<i64>) -> AppResult<Option<(String, Option<CounterpartyIdentifier>)>> {
    if py::strip(value.unwrap_or("")).is_empty() {
        return Ok(None);
    }
    let value = if kind == "iban" {
        let v = normalize_iban(value).unwrap_or_default();
        if !iban_valid(&v) {
            return value_err("IBAN invalide (vérifie qu'il est complet)");
        }
        v
    } else {
        name_key(value.unwrap_or(""))
    };
    let existing = CounterpartyIdentifier::query_one(c, "WHERE kind = ? AND value = ?", [kind, value.as_str()])?;
    if let Some(e) = &existing {
        if Some(e.counterparty_id) != owner_id {
            let name = Counterparty::get(c, e.counterparty_id)?.map(|p| p.name).unwrap_or_default();
            return value_err(format!("Déjà associé à « {name} » : fusionne les deux contacts si c'est la même personne"));
        }
    }
    Ok(Some((value, existing)))
}

/// Attach an IBAN or a printed name to a contact. False when there is nothing to add or it is already attached.
pub fn add_identifier(c: &Connection, cp: &Counterparty, kind: &str, value: Option<&str>, label: Option<&str>) -> AppResult<bool> {
    let checked = check_identifier(c, kind, value, Some(cp.id))?;
    let Some((value, None)) = checked else { return Ok(false) };
    let label = label.filter(|l| !l.is_empty()).map(|l| py::take_chars(l, 300));
    CounterpartyIdentifier { counterparty_id: cp.id, kind: kind.into(), value, label, ..Default::default() }.insert(c)?;
    Ok(true)
}

fn field(data: &HashMap<String, String>, key: &str) -> Option<String> {
    let v = py::strip(data.get(key).map(|s| s.as_str()).unwrap_or("")).to_string();
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

pub fn save_contact(c: &Connection, contact_id: Option<i64>, data: &HashMap<String, String>) -> AppResult<Counterparty> {
    let mut cp = match contact_id {
        Some(id) if id != 0 => Counterparty::get(c, id)?.ok_or_else(|| AppError::Other("'NoneType' object has no attribute 'name'".into()))?,
        _ => Counterparty { name: String::new(), kind: String::new(), ..Default::default() },
    };
    cp.name = field(data, "name").or_else(|| Some(cp.name.clone()).filter(|n| !n.is_empty())).unwrap_or_else(|| "Sans nom".into());
    let kind = data.get("kind").filter(|k| !k.is_empty()).cloned().or_else(|| Some(cp.kind.clone()).filter(|k| !k.is_empty())).unwrap_or_else(|| "person".into());
    cp.kind = if COUNTERPARTY_KINDS.iter().any(|(k, _)| *k == kind) { kind } else { "other".into() };
    cp.phone = field(data, "phone");
    cp.email = field(data, "email");
    cp.notes = field(data, "notes");
    if cp.id == 0 {
        cp.insert(c)?;
    } else {
        cp.update(c)?;
    }
    Ok(cp)
}

/// (id, name) of every contact, for the "already known?" lists.
pub fn choices(c: &Connection) -> AppResult<Vec<(i64, String)>> {
    Ok(Counterparty::query(c, "ORDER BY name", [])?.into_iter().map(|p| (p.id, p.name)).collect())
}

/// Name an unknown counterparty (new contact, or an existing one when `contact_id` is given) and remember it.
pub fn identify(c: &Connection, data: &HashMap<String, String>) -> AppResult<Counterparty> {
    let raw_id = data.get("contact_id").cloned().unwrap_or_default();
    let contact_id = if py::isdigit(&raw_id) { Some(py::py_int(&raw_id).map_err(AppError::Value)?) } else { None };
    if contact_id.is_none() && field(data, "name").is_none() {
        return value_err("Donne un nom, ou choisis un contact déjà enregistré");
    }
    // Everything is checked before anything is created: a refused identifier leaves no half-made contact behind.
    check_identifier(c, "iban", data.get("iban").map(|s| s.as_str()), contact_id)?;
    check_identifier(c, "name", data.get("printed_name").map(|s| s.as_str()), contact_id)?;
    let cp = match contact_id.filter(|id| *id != 0) {
        Some(id) => Counterparty::get(c, id)?.ok_or_else(|| AppError::Value("Contact introuvable".into()))?,
        None => save_contact(c, None, data)?,
    };
    add_identifier(c, &cp, "iban", data.get("iban").map(|s| s.as_str()), None)?;
    if let Some(printed) = data.get("printed_name").filter(|p| !p.is_empty()) {
        add_identifier(c, &cp, "name", Some(printed), Some(printed))?;
    }
    relink(c)?;
    Ok(cp)
}

/// Link every operation that now matches a contact (after an identifier was added).
pub fn relink(c: &Connection) -> AppResult<i64> {
    let index = load_index(c)?;
    if index.is_empty() {
        return Ok(0);
    }
    let mut linked = 0;
    for mut t in Transaction::query(c, UNLINKED, [])? {
        if let Some(hit) = matching(&index, t.counterparty_name.as_deref(), t.counterparty_iban.as_deref()).filter(|h| *h != 0) {
            t.counterparty_id = Some(hit);
            t.update(c)?;
            linked += 1;
        }
    }
    Ok(linked)
}

pub fn remove_identifier(c: &Connection, identifier_id: i64) -> AppResult<()> {
    let Some(ident) = CounterpartyIdentifier::get(c, identifier_id)? else { return Ok(()) };
    // Operations matched through this identifier lose their link (another identifier of the contact may restore it).
    let cp_id = ident.counterparty_id;
    CounterpartyIdentifier::delete(c, ident.id)?;
    let index = load_index(c)?;
    for mut t in Transaction::query(c, "WHERE counterparty_id = ?", [cp_id])? {
        if matching(&index, t.counterparty_name.as_deref(), t.counterparty_iban.as_deref()) != Some(cp_id) {
            t.counterparty_id = None;
            t.update(c)?;
        }
    }
    Ok(())
}

pub fn delete_contact(c: &Connection, contact_id: i64) -> AppResult<()> {
    if Counterparty::get(c, contact_id)?.is_some() {
        // operations keep their printed name and IBAN, only the link is dropped (ON DELETE SET NULL)
        c.execute("UPDATE transactions SET counterparty_id = NULL WHERE counterparty_id = ?", [contact_id])?;
        c.execute("DELETE FROM counterparty_identifiers WHERE counterparty_id = ?", [contact_id])?;
        Counterparty::delete(c, contact_id)?;
    }
    Ok(())
}

pub fn merge(c: &Connection, source_id: i64, target_id: i64) -> AppResult<()> {
    let (Some(source), Some(mut target)) = (Counterparty::get(c, source_id)?, Counterparty::get(c, target_id)?) else { return Ok(()) };
    if source.id == target.id {
        return Ok(());
    }
    c.execute("UPDATE counterparty_identifiers SET counterparty_id = ? WHERE counterparty_id = ?", [target.id, source.id])?;
    c.execute("UPDATE transactions SET counterparty_id = ? WHERE counterparty_id = ?", [target.id, source.id])?;
    let some = |v: &Option<String>| v.clone().filter(|s| !s.is_empty());
    target.phone = some(&target.phone).or(some(&source.phone));
    target.email = some(&target.email).or(some(&source.email));
    let notes: Vec<String> = [some(&target.notes), some(&source.notes)].into_iter().flatten().collect();
    target.notes = if notes.is_empty() { None } else { Some(notes.join("\n")) };
    target.update(c)?;
    Counterparty::delete(c, source.id)?;
    Ok(())
}

#[derive(Clone, Debug)]
pub struct ContactSummary {
    pub contact: Counterparty,
    pub identifiers: Vec<CounterpartyIdentifier>,
    pub count: i64,
    pub total_in: Decimal,
    pub total_out: Decimal,
    pub last: Option<NaiveDate>,
}

pub fn identifiers_of(c: &Connection, contact_id: i64) -> AppResult<Vec<CounterpartyIdentifier>> {
    Ok(CounterpartyIdentifier::query(c, "WHERE counterparty_id = ? ORDER BY id", [contact_id])?)
}

pub fn summaries(c: &Connection) -> AppResult<Vec<ContactSummary>> {
    let contacts = Counterparty::query(c, "ORDER BY name", [])?;
    let mut idents: HashMap<i64, Vec<CounterpartyIdentifier>> = HashMap::new();
    for i in CounterpartyIdentifier::query(c, "ORDER BY id", [])? {
        idents.entry(i.counterparty_id).or_default().push(i);
    }
    let mut stats: Vec<ContactSummary> = contacts
        .into_iter()
        .map(|p| ContactSummary { identifiers: idents.remove(&p.id).unwrap_or_default(), contact: p, count: 0, total_in: Decimal::ZERO, total_out: Decimal::ZERO, last: None })
        .collect();
    let pos: HashMap<i64, usize> = stats.iter().enumerate().map(|(i, s)| (s.contact.id, i)).collect();
    let visible = access::readable_ids(c)?;
    let mut st = c.prepare_cached("SELECT counterparty_id, amount, date, account_id FROM transactions WHERE counterparty_id IS NOT NULL")?;
    let rows = st.query_map([], |r| {
        Ok((r.get::<_, i64>(0)?, Decimal::from_sql(r.get_ref(1)?), NaiveDate::from_sql(r.get_ref(2)?), r.get::<_, i64>(3)?))
    })?;
    for row in rows {
        let (cid, amount, day, account_id) = row?;
        if let Some(v) = &visible {
            if !v.contains(&account_id) {
                continue;
            }
        }
        let Some(&i) = pos.get(&cid) else { continue };
        let s = &mut stats[i];
        s.count += 1;
        if amount >= Decimal::ZERO {
            s.total_in += amount;
        } else {
            s.total_out += py::neg(amount);
        }
        s.last = Some(s.last.map(|l| l.max(day)).unwrap_or(day));
    }
    Ok(stats)
}
