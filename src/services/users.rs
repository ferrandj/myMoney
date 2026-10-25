//! Users: who can sign in, and the key slot that holds each one's password.

use std::sync::OnceLock;

use regex::Regex;
use rusqlite::Connection;
use sha2::{Digest, Sha256};

use crate::error::{value_err, AppResult};
use crate::models::{Account, AccountShare, Document, User};
use crate::py;
use crate::security::vault::{check_password_strength, Vault, VaultError};
use crate::services::access::Identity;

fn email_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^[^@\s]+@[^@\s]+\.[^@\s]+$").unwrap())
}

pub fn normalize_email(email: Option<&str>) -> AppResult<String> {
    let value = py::lower(py::strip(email.unwrap_or("")));
    if !email_re().is_match(&value) || value.chars().count() > 320 {
        return value_err("Adresse e-mail invalide");
    }
    Ok(value)
}

/// Name of the user's key slot in the keystore: a hash, so the keystore file does not list e-mail addresses.
pub fn slot_id(email: &str) -> AppResult<String> {
    let e = normalize_email(Some(email))?;
    let h = Sha256::digest(format!("mymoney-user|{e}").as_bytes());
    Ok(hex::encode(h)[..32].to_string())
}

pub fn identity(user: &User) -> Identity {
    Identity { id: user.id, email: user.email.clone(), name: user.name.clone(), is_admin: user.is_admin }
}

pub fn count(c: &Connection) -> AppResult<i64> {
    Ok(c.query_row("SELECT count(*) FROM users", [], |r| r.get(0))?)
}

pub fn get_by_email(c: &Connection, email: Option<&str>) -> AppResult<Option<User>> {
    match normalize_email(email) {
        Ok(e) => Ok(User::query_one(c, "WHERE email = ?", [e])?),
        Err(_) => Ok(None),
    }
}

pub fn all_users(c: &Connection) -> AppResult<Vec<User>> {
    Ok(User::query(c, "ORDER BY name, email", [])?)
}

pub fn first_admin(c: &Connection) -> AppResult<Option<User>> {
    Ok(User::query_one(c, "WHERE is_admin = 1 ORDER BY id", [])?)
}

pub fn create(c: &Connection, email: &str, name: Option<&str>, is_admin: bool) -> AppResult<User> {
    let email = normalize_email(Some(email))?;
    if get_by_email(c, Some(&email))?.is_some() {
        return value_err("Cette adresse e-mail a déjà un utilisateur");
    }
    let name = py::take_chars(py::strip(name.unwrap_or("")), 200);
    let mut user = User { email, name: if name.is_empty() { None } else { Some(name) }, is_admin, ..Default::default() };
    user.insert(c)?;
    Ok(user)
}

/// Accounts and documents that existed before there were users belong to the first user.
pub fn claim_legacy_data(c: &Connection, user: &User) -> AppResult<()> {
    c.execute("UPDATE accounts SET owner_id = ?, updated_at = ? WHERE owner_id IS NULL", rusqlite::params![user.id, crate::models::fmt_datetime(&crate::models::utcnow())])?;
    c.execute("UPDATE documents SET owner_id = ? WHERE owner_id IS NULL", [user.id])?;
    Ok(())
}

fn vault_err(e: VaultError) -> crate::error::AppError {
    match e {
        VaultError::Weak(m) => crate::error::AppError::Value(m),
        VaultError::Invalid => crate::error::AppError::InvalidPassword,
        VaultError::Store(m) | VaultError::Open(m) => crate::error::AppError::Other(m),
    }
}

/// A new user with their own password to the vault (checked for strength before anything is written).
pub fn add_with_password(c: &Connection, vault: &Vault, email: &str, name: Option<&str>, password: &str, is_admin: bool) -> AppResult<User> {
    check_password_strength(password).map_err(crate::error::AppError::Value)?;
    let user = create(c, email, name, is_admin)?;
    vault.add_user_slot(&slot_id(&user.email)?, password).map_err(vault_err)?;
    Ok(user)
}

/// Delete a user and their key slot. Their accounts go to `heir`; shares they hold disappear. Returns accounts moved.
pub fn remove(c: &Connection, vault: &Vault, user: &User, heir: &User) -> AppResult<i64> {
    if user.id == heir.id {
        return value_err("Impossible de se supprimer soi-même");
    }
    if user.is_admin && all_users(c)?.iter().filter(|u| u.is_admin).count() <= 1 {
        return value_err("Il doit rester au moins un administrateur");
    }
    let mut moved = 0;
    for mut account in Account::query(c, "WHERE owner_id = ?", [user.id])? {
        account.owner_id = Some(heir.id);
        account.update(c)?;
        moved += 1;
        // The heir owns it now: a share to themselves would be meaningless.
        c.execute("DELETE FROM account_shares WHERE account_id = ? AND user_id = ?", [account.id, heir.id])?;
    }
    c.execute("UPDATE documents SET owner_id = ? WHERE owner_id = ?", [heir.id, user.id])?;
    c.execute("DELETE FROM account_shares WHERE user_id = ?", [user.id])?;
    vault.remove_user_slot(&slot_id(&user.email)?).map_err(vault_err)?;
    User::delete(c, user.id)?;
    Ok(moved)
}

pub fn share(c: &Connection, account: &Account, user: &User, can_edit: bool) -> AppResult<AccountShare> {
    if account.owner_id == Some(user.id) {
        return value_err("Cette personne est déjà propriétaire du compte");
    }
    if let Some(mut existing) = AccountShare::query_one(c, "WHERE account_id = ? AND user_id = ? ORDER BY id", [account.id, user.id])? {
        existing.can_edit = can_edit;
        existing.update(c)?;
        return Ok(existing);
    }
    let mut row = AccountShare { account_id: account.id, user_id: user.id, can_edit, ..Default::default() };
    row.insert(c)?;
    Ok(row)
}

pub fn unshare(c: &Connection, account: &Account, user_id: i64) -> AppResult<()> {
    c.execute("DELETE FROM account_shares WHERE account_id = ? AND user_id = ?", [account.id, user_id])?;
    Ok(())
}

pub fn shares_of(c: &Connection, account_id: i64) -> AppResult<Vec<AccountShare>> {
    Ok(AccountShare::query(c, "WHERE account_id = ? ORDER BY id", [account_id])?)
}

pub fn documents_of_owner(c: &Connection, owner: i64) -> AppResult<Vec<Document>> {
    Ok(Document::query(c, "WHERE owner_id = ?", [owner])?)
}
