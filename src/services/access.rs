//! Who may see and change what.
//!
//! Every account has an owner. The owner can share it with other users, one by one, read only or with the right to
//! edit. A user sees their own accounts and the ones shared with them, the documents they dropped and the documents
//! of accounts they can see; the owner alone deletes an account or changes who it is shared with.
//!
//! The signed-in user is carried by a thread-local set for each request (`as_user`), so the rule applies the same
//! way in every service without every function having to be handed the user. Outside a request (tests, maintenance
//! jobs) nothing is set and nothing is restricted: `restricted()` is false.

use std::cell::RefCell;
use std::collections::BTreeSet;

use rusqlite::Connection;

use crate::error::{AppError, AppResult};
use crate::models::{Account, Document};

#[derive(Clone, Debug, PartialEq)]
pub struct Identity {
    pub id: i64,
    pub email: String,
    pub name: Option<String>,
    pub is_admin: bool,
}

impl Identity {
    pub fn label(&self) -> String {
        self.name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| self.email.clone())
    }
}

thread_local! {
    static CURRENT: RefCell<Option<Identity>> = const { RefCell::new(None) };
}

pub fn current() -> Option<Identity> {
    CURRENT.with(|c| c.borrow().clone())
}

pub fn restricted() -> bool {
    current().is_some()
}

/// Run `f` as `identity` (None: unrestricted), restoring the previous user afterwards.
pub fn as_user<T>(identity: Option<Identity>, f: impl FnOnce() -> T) -> T {
    let previous = CURRENT.with(|c| c.replace(identity));
    struct Restore(Option<Identity>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let p = self.0.take();
            CURRENT.with(|c| *c.borrow_mut() = p);
        }
    }
    let _guard = Restore(previous);
    f()
}

/// Housekeeping that must see everything (purging set-aside duplicates, refreshing every loan).
pub fn unrestricted<T>(f: impl FnOnce() -> T) -> T {
    as_user(None, f)
}

fn denied<T>(msg: &str) -> AppResult<T> {
    Err(AppError::Denied(msg.to_string()))
}

/// Who a new account or document belongs to: the signed-in user, or the first administrator when nobody is signed
/// in (the command line, maintenance).
pub fn owner_for_new(c: &Connection) -> AppResult<Option<i64>> {
    if let Some(me) = current() {
        return Ok(Some(me.id));
    }
    Ok(c.query_row("SELECT id FROM users WHERE is_admin = 1 ORDER BY id LIMIT 1", [], |r| r.get(0)).ok())
}

fn ids(c: &Connection, sql: &str, user: i64) -> AppResult<BTreeSet<i64>> {
    let mut st = c.prepare_cached(sql)?;
    let rows = st.query_map([user], |r| r.get::<_, i64>(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Accounts the current user can see; None when nothing is restricted.
pub fn readable_ids(c: &Connection) -> AppResult<Option<BTreeSet<i64>>> {
    let Some(me) = current() else { return Ok(None) };
    let mut owned = ids(c, "SELECT id FROM accounts WHERE owner_id = ?", me.id)?;
    owned.extend(ids(c, "SELECT account_id FROM account_shares WHERE user_id = ?", me.id)?);
    Ok(Some(owned))
}

pub fn editable_ids(c: &Connection) -> AppResult<Option<BTreeSet<i64>>> {
    let Some(me) = current() else { return Ok(None) };
    let mut owned = ids(c, "SELECT id FROM accounts WHERE owner_id = ?", me.id)?;
    owned.extend(ids(c, "SELECT account_id FROM account_shares WHERE user_id = ? AND can_edit = 1", me.id)?);
    Ok(Some(owned))
}

pub fn only_readable(c: &Connection, accounts: Vec<Account>) -> AppResult<Vec<Account>> {
    Ok(match readable_ids(c)? {
        None => accounts,
        Some(ids) => accounts.into_iter().filter(|a| ids.contains(&a.id)).collect(),
    })
}

pub fn only_editable(c: &Connection, accounts: Vec<Account>) -> AppResult<Vec<Account>> {
    Ok(match editable_ids(c)? {
        None => accounts,
        Some(ids) => accounts.into_iter().filter(|a| ids.contains(&a.id)).collect(),
    })
}

pub fn check_read(c: &Connection, account_id: i64) -> AppResult<()> {
    if let Some(ids) = readable_ids(c)? {
        if !ids.contains(&account_id) {
            return denied("Compte introuvable");
        }
    }
    Ok(())
}

pub fn check_edit(c: &Connection, account_id: i64) -> AppResult<()> {
    check_read(c, account_id)?;
    if let Some(ids) = editable_ids(c)? {
        if !ids.contains(&account_id) {
            return denied("Ce compte t'est partagé en lecture seule");
        }
    }
    Ok(())
}

pub fn check_owner(c: &Connection, account_id: i64) -> AppResult<()> {
    check_read(c, account_id)?;
    let Some(me) = current() else { return Ok(()) };
    let owner: Option<i64> = c.query_row("SELECT owner_id FROM accounts WHERE id = ?", [account_id], |r| r.get(0)).unwrap_or(None);
    if owner != Some(me.id) {
        return denied("Seul le propriétaire du compte peut faire cela");
    }
    Ok(())
}

/// "owner", "edit" or "read" for the current user (an unrestricted caller counts as the owner).
pub fn role(c: &Connection, account: &Account) -> AppResult<&'static str> {
    let Some(me) = current() else { return Ok("owner") };
    if account.owner_id == Some(me.id) {
        return Ok("owner");
    }
    let share: Option<bool> =
        c.query_row("SELECT can_edit FROM account_shares WHERE account_id = ? AND user_id = ?", [account.id, me.id], |r| r.get(0)).ok();
    match share {
        None => denied("Compte introuvable"),
        Some(true) => Ok("edit"),
        Some(false) => Ok("read"),
    }
}

/// The user's own drops, and the documents attached to an account they can see.
pub fn document_visible(c: &Connection, doc: &Document) -> AppResult<bool> {
    let Some(me) = current() else { return Ok(true) };
    if doc.owner_id == Some(me.id) {
        return Ok(true);
    }
    let Some(account_id) = doc.account_id else { return Ok(false) };
    Ok(readable_ids(c)?.map(|ids| ids.contains(&account_id)).unwrap_or(true))
}

pub fn check_document(c: &Connection, doc: Option<Document>, edit: bool) -> AppResult<Document> {
    let Some(doc) = doc else { return denied("Document introuvable") };
    if !document_visible(c, &doc)? {
        return denied("Document introuvable");
    }
    if let Some(me) = current() {
        if edit && doc.owner_id != Some(me.id) {
            match doc.account_id {
                Some(a) => check_edit(c, a)?,
                None => return denied("Document introuvable"),
            }
        }
    }
    Ok(doc)
}

pub fn visible_documents(c: &Connection) -> AppResult<Vec<Document>> {
    let docs = Document::query(c, "ORDER BY uploaded_at DESC", [])?;
    let mut out = Vec::new();
    for d in docs {
        if document_visible(c, &d)? {
            out.push(d);
        }
    }
    Ok(out)
}
