//! The administrator's view of the database: its tables and rows (everyone's), edits, emptying, the update of its
//! structure and the synchronisation with the documents.
//!
//! There is one database and one data key for all users (each user's password unlocks the same key), so the
//! administrator reads everything without any other key. Table and column names always come from the database's own
//! schema, never straight from a request.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rusqlite::types::ValueRef;
use rusqlite::Connection;

use crate::error::{value_err, AppResult};
use crate::models::{setting_delete, Account, Document, Holding, LoanInstallment, PersonCard};
use crate::services::{life_insurance, loans, people, statements};

pub const PER_PAGE: i64 = 50;
/// Cells longer than this are cut in the list (the row page shows them whole).
const CELL_CUT: usize = 160;
const SQL_ROW_LIMIT: usize = 500;

#[derive(Clone, Debug)]
pub struct Column {
    pub name: String,
    pub sqltype: String,
    pub primary: bool,
    pub required: bool,
}

#[derive(Clone, Debug)]
pub struct TableInfo {
    pub name: String,
    pub rows: i64,
    pub columns: Vec<Column>,
}

#[derive(Clone, Debug)]
pub struct Cell {
    pub text: String,
    pub null: bool,
    pub blob: bool,
    pub cut: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Page {
    pub columns: Vec<String>,
    pub rows: Vec<(i64, Vec<Cell>)>,
    pub total: i64,
}

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn cell(v: ValueRef, cut: bool) -> Cell {
    let (text, null, blob) = match v {
        ValueRef::Null => ("NULL".to_string(), true, false),
        ValueRef::Integer(i) => (i.to_string(), false, false),
        ValueRef::Real(f) => (f.to_string(), false, false),
        ValueRef::Text(t) => (String::from_utf8_lossy(t).to_string(), false, false),
        ValueRef::Blob(b) => (format!("<{} octets>", b.len()), false, true),
    };
    if cut && text.chars().count() > CELL_CUT {
        return Cell { text: text.chars().take(CELL_CUT).collect::<String>() + "…", null, blob, cut: true };
    }
    Cell { text, null, blob, cut: false }
}

pub fn table_names(c: &Connection) -> AppResult<Vec<String>> {
    let mut st = c.prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")?;
    let names = st.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
    Ok(names)
}

/// The table's exact name, as the schema spells it (an unknown name is refused).
pub fn check_table(c: &Connection, name: &str) -> AppResult<String> {
    match table_names(c)?.into_iter().find(|t| t == name) {
        Some(t) => Ok(t),
        None => value_err(format!("Table inconnue : {name}")),
    }
}

pub fn columns(c: &Connection, table: &str) -> AppResult<Vec<Column>> {
    let mut st = c.prepare(&format!("PRAGMA table_info({})", quote(table)))?;
    let cols = st
        .query_map([], |r| Ok(Column { name: r.get::<_, String>(1)?, sqltype: r.get::<_, String>(2)?, required: r.get::<_, i64>(3)? != 0, primary: r.get::<_, i64>(5)? != 0 }))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(cols)
}

pub fn tables(c: &Connection) -> AppResult<Vec<TableInfo>> {
    let mut out = vec![];
    for name in table_names(c)? {
        let rows: i64 = c.query_row(&format!("SELECT COUNT(*) FROM {}", quote(&name)), [], |r| r.get(0))?;
        out.push(TableInfo { columns: columns(c, &name)?, name, rows });
    }
    Ok(out)
}

/// A page of rows (with their rowid), optionally those where any column contains the text.
pub fn page(c: &Connection, table: &str, page_no: i64, search: &str) -> AppResult<Page> {
    let table = check_table(c, table)?;
    let cols = columns(c, &table)?;
    let names: Vec<String> = cols.iter().map(|x| x.name.clone()).collect();
    let like = format!("%{}%", search.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
    let filter = if search.is_empty() { String::new() } else { format!(" WHERE {}", names.iter().map(|n| format!("CAST({} AS TEXT) LIKE ?1 ESCAPE '\\'", quote(n))).collect::<Vec<_>>().join(" OR ")) };
    let total: i64 = if search.is_empty() { c.query_row(&format!("SELECT COUNT(*) FROM {}", quote(&table)), [], |r| r.get(0))? } else { c.query_row(&format!("SELECT COUNT(*) FROM {}{filter}", quote(&table)), [&like], |r| r.get(0))? };
    let select = format!("SELECT rowid, {} FROM {}{filter} ORDER BY rowid LIMIT {} OFFSET {}", names.iter().map(|n| quote(n)).collect::<Vec<_>>().join(", "), quote(&table), PER_PAGE, page_no.max(0) * PER_PAGE);
    let mut st = c.prepare(&select)?;
    let mut rows = vec![];
    let mut q = if search.is_empty() { st.query([])? } else { st.query([&like])? };
    while let Some(r) = q.next()? {
        let rowid: i64 = r.get(0)?;
        let cells = (0..names.len()).map(|i| r.get_ref(i + 1).map(|v| cell(v, true))).collect::<Result<Vec<_>, _>>()?;
        rows.push((rowid, cells));
    }
    Ok(Page { columns: names, rows, total })
}

/// One row, every value whole.
pub fn row(c: &Connection, table: &str, rowid: i64) -> AppResult<Option<Vec<(Column, Cell)>>> {
    let table = check_table(c, table)?;
    let cols = columns(c, &table)?;
    let select = format!("SELECT {} FROM {} WHERE rowid = ?", cols.iter().map(|x| quote(&x.name)).collect::<Vec<_>>().join(", "), quote(&table));
    let mut st = c.prepare(&select)?;
    let mut q = st.query([rowid])?;
    match q.next()? {
        None => Ok(None),
        Some(r) => Ok(Some(cols.iter().enumerate().map(|(i, col)| Ok((col.clone(), cell(r.get_ref(i)?, false)))).collect::<Result<Vec<_>, rusqlite::Error>>()?)),
    }
}

pub fn delete_row(c: &Connection, table: &str, rowid: i64) -> AppResult<usize> {
    let table = check_table(c, table)?;
    Ok(c.execute(&format!("DELETE FROM {} WHERE rowid = ?", quote(&table)), [rowid])?)
}

/// Change the given columns of a row (`None` = NULL). Returns how many columns changed.
pub fn set_cells(c: &Connection, table: &str, rowid: i64, changes: &[(String, Option<String>)]) -> AppResult<usize> {
    let table = check_table(c, table)?;
    let known: BTreeSet<String> = columns(c, &table)?.into_iter().map(|x| x.name).collect();
    let mut n = 0;
    for (column, value) in changes {
        if !known.contains(column) {
            return value_err(format!("Colonne inconnue : {column}"));
        }
        c.execute(&format!("UPDATE {} SET {} = ? WHERE rowid = ?", quote(&table), quote(column)), rusqlite::params![value, rowid])?;
        n += 1;
    }
    Ok(n)
}

/// Empty a table. The users' table is kept: emptying it would lock everybody out (the console can still do it).
pub fn empty_table(c: &Connection, table: &str) -> AppResult<usize> {
    let table = check_table(c, table)?;
    if table == "users" {
        return value_err("La table des utilisateurs ne se vide pas ici : plus personne ne pourrait se connecter. Supprime un utilisateur depuis Paramètres, ou utilise la console SQL.");
    }
    Ok(c.execute(&format!("DELETE FROM {}", quote(&table)), [])?)
}

// ------------------------------------------------------------------------------------------------- SQL console

#[derive(Clone, Debug, Default)]
pub struct SqlOutcome {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Cell>>,
    pub more: bool,
    pub affected: Option<usize>,
}

/// One statement typed by the administrator. What opens the door to the rest of the machine (attaching files, loading
/// code, changing the key) is refused; everything else is allowed.
pub fn run_sql(c: &Connection, text: &str) -> AppResult<SqlOutcome> {
    let sql = text.trim().trim_end_matches(';').trim();
    if sql.is_empty() {
        return value_err("Rien à exécuter");
    }
    let lower = sql.to_lowercase();
    let words: Vec<&str> = lower.split(|ch: char| !ch.is_alphanumeric() && ch != '_').filter(|w| !w.is_empty()).collect();
    for banned in ["attach", "detach", "load_extension", "vacuum", "readfile", "writefile", "edit"] {
        if words.contains(&banned) {
            return value_err(format!("« {banned} » n'est pas permis dans la console"));
        }
    }
    if words.first() == Some(&"pragma") {
        let allowed = ["table_info", "table_xinfo", "index_list", "index_info", "foreign_key_list", "foreign_key_check", "integrity_check", "quick_check", "schema_version"];
        if !words.get(1).map(|w| allowed.contains(w)).unwrap_or(false) {
            return value_err("Seuls les PRAGMA de lecture (table_info, index_list, foreign_key_list, foreign_key_check, integrity_check…) sont permis");
        }
    }
    let mut st = c.prepare(sql)?;
    if st.column_count() == 0 {
        let n = st.execute([])?;
        return Ok(SqlOutcome { affected: Some(n), ..Default::default() });
    }
    let columns: Vec<String> = st.column_names().into_iter().map(|s| s.to_string()).collect();
    let width = columns.len();
    let mut q = st.query([])?;
    let mut rows = vec![];
    let mut more = false;
    while let Some(r) = q.next()? {
        if rows.len() >= SQL_ROW_LIMIT {
            more = true;
            break;
        }
        rows.push((0..width).map(|i| r.get_ref(i).map(|v| cell(v, true))).collect::<Result<Vec<_>, _>>()?);
    }
    Ok(SqlOutcome { columns, rows, more, affected: None })
}

// ------------------------------------------------------------------------------------- structure update

/// What the structure of the database is made of: (table, columns).
fn structure(c: &Connection) -> AppResult<Vec<(String, Vec<String>)>> {
    let mut out = vec![];
    for t in table_names(c)? {
        let cols = columns(c, &t)?.into_iter().map(|x| x.name).collect::<Vec<_>>();
        out.push((t, cols));
    }
    Ok(out)
}

/// Bring the database to the structure of this version: the missing tables, the missing columns (what the application
/// does every time it opens), the cards of the people who have none. Returns what changed.
pub fn migrate(c: &Connection) -> AppResult<Vec<String>> {
    let before = structure(c)?;
    crate::db::create_all(c)?;
    crate::db::add_missing_columns(c)?;
    let cards = people::backfill(c)?;
    let after = structure(c)?;
    let mut changes = vec![];
    for (table, cols) in &after {
        match before.iter().find(|(t, _)| t == table) {
            None => changes.push(format!("table créée : {table}")),
            Some((_, old)) => {
                for col in cols.iter().filter(|x| !old.contains(x)) {
                    changes.push(format!("colonne ajoutée : {table}.{col}"));
                }
            }
        }
    }
    if cards > 0 {
        changes.push(format!("{cards} fiche(s) de personne créée(s) pour des comptes qui n'en avaient pas"));
    }
    let problems: i64 = c.prepare("PRAGMA foreign_key_check")?.query_map([], |_| Ok(()))?.count() as i64;
    if problems > 0 {
        changes.push(format!("⚠ {problems} lien(s) cassé(s) entre tables (utilise « Synchroniser avec les documents »)"));
    }
    Ok(changes)
}

// ------------------------------------------------------------------------------------------------- emptying

#[derive(Clone, Copy, Debug)]
pub enum Scope {
    User(i64),
    Everyone,
}

/// Delete the data of one user, or of everybody: accounts (with their operations, statements, positions, history, loans
/// and shares), documents (rows; the files are returned to be wiped), person cards, category rules and, for everybody,
/// the contacts and the contract numbers kept in the settings. The users themselves and the settings stay.
pub fn clear(c: &Connection, scope: Scope, documents_dir: &Path) -> AppResult<(Vec<(String, i64)>, Vec<PathBuf>)> {
    let (filter, owner): (&str, Vec<i64>) = match scope {
        Scope::User(id) => (" WHERE owner_id = ?", vec![id]),
        Scope::Everyone => ("", vec![]),
    };
    let mut report = vec![];
    let mut paths = vec![];
    {
        let mut st = c.prepare(&format!("SELECT blob_name FROM documents{filter}"))?;
        for blob in st.query_map(rusqlite::params_from_iter(owner.iter()), |r| r.get::<_, String>(0))? {
            paths.push(documents_dir.join(blob?));
        }
    }
    let count = |c: &Connection, sql: &str, owner: &[i64]| -> AppResult<i64> { Ok(c.query_row(sql, rusqlite::params_from_iter(owner.iter()), |r| r.get(0))?) };
    for (table, label) in [("transactions", "opérations"), ("statements", "relevés"), ("holdings", "positions"), ("snapshots", "valeurs d'historique")] {
        let n = match scope {
            Scope::Everyone => count(c, &format!("SELECT COUNT(*) FROM {table}"), &[])?,
            Scope::User(id) => count(c, &format!("SELECT COUNT(*) FROM {table} WHERE account_id IN (SELECT id FROM accounts WHERE owner_id = ?)"), &[id])?,
        };
        report.push((label.to_string(), n));
    }
    for (table, label) in [("accounts", "comptes"), ("documents", "documents"), ("people", "fiches de personnes"), ("category_rules", "règles de catégories")] {
        report.push((label.to_string(), count(c, &format!("SELECT COUNT(*) FROM {table}{filter}"), &owner)?));
        c.execute(&format!("DELETE FROM {table}{filter}"), rusqlite::params_from_iter(owner.iter()))?;
    }
    if let Scope::Everyone = scope {
        report.push(("contacts".to_string(), c.query_row("SELECT COUNT(*) FROM counterparties", [], |r| r.get(0))?));
        c.execute("DELETE FROM counterparties", [])?;
        c.execute("DELETE FROM settings WHERE \"key\" LIKE 'life_estimate_%'", [])?;
    }
    purge_orphans(c)?;
    Ok((report, paths))
}

/// What cannot exist without its account.
fn purge_orphans(c: &Connection) -> AppResult<()> {
    for table in ["transactions", "statements", "fee_summaries", "holdings", "snapshots", "gap_acks", "loans", "account_shares"] {
        c.execute(&format!("DELETE FROM {table} WHERE account_id NOT IN (SELECT id FROM accounts)"), [])?;
    }
    let mut st = c.prepare("SELECT \"key\" FROM settings WHERE \"key\" LIKE 'life_estimate_%'")?;
    let keys = st.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
    for key in keys {
        let id: i64 = key.trim_start_matches("life_estimate_").parse().unwrap_or(0);
        let exists: i64 = c.query_row("SELECT COUNT(*) FROM accounts WHERE id = ?", [id], |r| r.get(0))?;
        if exists == 0 {
            setting_delete(c, &key)?;
        }
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------ synchronisation

/// Documents the data of which is missing: a validated document must have left something in the database.
fn imported_without_data(c: &Connection) -> AppResult<Vec<Document>> {
    let mut out = vec![];
    for doc in Document::query(c, "WHERE status = 'imported'", [])? {
        let parsed = doc.parse_result.as_ref().and_then(|r| r.get("statements")).and_then(|s| s.as_array()).and_then(|a| a.first()).cloned();
        let Some(st) = parsed else { continue };
        let kind = st.get("document_kind").and_then(|k| k.as_str()).unwrap_or("").to_string();
        let one = |sql: &str| -> AppResult<i64> { Ok(c.query_row(sql, [doc.id], |r| r.get(0))?) };
        let has = match kind.as_str() {
            "info" => continue, // a letter leaves nothing but its account link
            "fees_summary" => one("SELECT COUNT(*) FROM fee_summaries WHERE document_id = ?1")? > 0,
            k if loans::LOAN_KINDS.contains(&k) => {
                one("SELECT COUNT(*) FROM loan_installments WHERE document_id = ?1")? + one("SELECT COUNT(*) FROM loan_events WHERE document_id = ?1")? + one("SELECT COUNT(*) FROM loans WHERE offer_document_id = ?1 OR schedule_document_id = ?1")? > 0
            }
            _ => one("SELECT COUNT(*) FROM statements WHERE document_id = ?1")? + one("SELECT COUNT(*) FROM transactions WHERE document_id = ?1")? > 0,
        };
        if !has {
            out.push(doc);
        }
    }
    Ok(out)
}

/// Make the database say what the documents say: what no validated document backs goes (operations, statements, recaps,
/// loan data, positions, then the accounts and cards an import made and nothing rests on), the balances come back to
/// what the remaining statements say, and the validated documents whose data is missing go back to « à valider » so
/// that they are imported again. Returns what was done, line by line, and the documents to import again.
pub fn sync(c: &Connection) -> AppResult<(Vec<(String, i64)>, Vec<i64>)> {
    let mut report: Vec<(String, i64)> = vec![];
    let mut note = |label: &str, n: usize| {
        if n > 0 {
            report.push((label.to_string(), n as i64));
        }
    };
    let backed = "SELECT id FROM documents WHERE status = 'imported'";

    // 1. what no validated document backs
    note("opérations importées sans document validé", c.execute(&format!("DELETE FROM transactions WHERE (document_id IS NOT NULL AND document_id NOT IN ({backed})) OR (source = 'import' AND document_id IS NULL)"), [])?);
    note("relevés sans document validé", c.execute(&format!("DELETE FROM statements WHERE document_id IS NULL OR document_id NOT IN ({backed})"), [])?);
    note("récapitulatifs de frais sans document validé", c.execute(&format!("DELETE FROM fee_summaries WHERE document_id IS NULL OR document_id NOT IN ({backed})"), [])?);
    note("échéances de prêt sans document validé", c.execute(&format!("DELETE FROM loan_installments WHERE (document_id IS NOT NULL AND document_id NOT IN ({backed})) OR (document_id IS NULL AND source IN ('offer', 'schedule'))"), [])?);
    note("événements de prêt sans document validé", c.execute(&format!("DELETE FROM loan_events WHERE document_id IS NULL OR document_id NOT IN ({backed})"), [])?);
    let mut stale_loans = 0;
    let mut stale_loan_accounts: BTreeSet<i64> = BTreeSet::new();
    for mut loan in crate::models::Loan::query(c, "", [])? {
        let before = loan.clone();
        let valid = |id: i64| -> AppResult<bool> { Ok(c.query_row("SELECT COUNT(*) FROM documents WHERE id = ? AND status = 'imported'", [id], |r| r.get::<_, i64>(0))? > 0) };
        if let Some(id) = loan.offer_document_id {
            if !valid(id)? {
                loan.offer_document_id = None;
            }
        }
        if let Some(id) = loan.schedule_document_id {
            if !valid(id)? {
                loan.schedule_document_id = None;
            }
        }
        if loan.offer_document_id.is_none() && before.offer_document_id.is_some() {
            loan.principal = None;
            loan.duration_months = None;
            loan.rate = None;
            loan.taeg = None;
            loan.interest_total = None;
            loan.total_cost = None;
            loan.purpose = None;
        }
        let none_left = LoanInstallment::query(c, "WHERE loan_id = ?", [loan.id])?.is_empty() && c.query_row("SELECT COUNT(*) FROM loan_events WHERE loan_id = ?", [loan.id], |r| r.get::<_, i64>(0))? == 0;
        if none_left {
            stale_loan_accounts.insert(loan.account_id); // a loan only ever comes from an import: so does its account
            crate::models::Loan::delete(c, loan.id)?;
            stale_loans += 1;
        } else {
            if loan.offer_document_id != before.offer_document_id || loan.schedule_document_id != before.schedule_document_id {
                loan.save(c, &before)?;
            }
            loans::rebuild(c, &mut loan, None)?;
        }
    }
    note("prêts sans aucun document", stale_loans);
    let loan_accounts: BTreeSet<i64> = stale_loan_accounts;
    let mut stale_holdings = 0;
    for h in Holding::query(c, "", [])? {
        let source = h.extra.as_ref().and_then(|e| e.get("source")).and_then(|s| s.as_str());
        let doc = h.extra.as_ref().and_then(|e| e.get("document_id")).and_then(|d| d.as_i64());
        let backed_doc = match doc {
            Some(d) => c.query_row("SELECT COUNT(*) FROM documents WHERE id = ? AND status = 'imported'", [d], |r| r.get::<_, i64>(0))? > 0,
            None => true,
        };
        if source == Some("import") && !backed_doc {
            Holding::delete(c, h.id)?;
            stale_holdings += 1;
        }
    }
    note("positions sans document validé", stale_holdings);

    // 2. accounts an import made and nothing rests on, and the cards that went with them
    let mut removed_accounts = 0;
    let mut removed_people: BTreeSet<i64> = BTreeSet::new();
    for account in Account::query(c, "", [])? {
        let from_import = account.origin.as_deref().map(|o| o.starts_with("import")).unwrap_or(false) || account.holder_alias.as_deref().map(|a| !a.trim().is_empty()).unwrap_or(false) || loan_accounts.contains(&account.id);
        if !from_import {
            continue;
        }
        let documents: i64 = c.query_row("SELECT COUNT(*) FROM documents WHERE account_id = ? AND status = 'imported'", [account.id], |r| r.get(0))?;
        if documents == 0 {
            // positions an older version brought are not marked: with no document behind the account they are its leftovers
            for h in Holding::query(c, "WHERE account_id = ?", [account.id])? {
                if h.manual && h.extra.as_ref().map(|e| e.is_null()).unwrap_or(true) {
                    Holding::delete(c, h.id)?;
                }
            }
        }
        if statements::content_of(c, account.id)? == 0 {
            if let Some(h) = &account.holders {
                removed_people.extend(crate::services::holders::from_json(h).into_iter().filter_map(|p| p.id));
            }
            statements::remove_account(c, &account)?;
            removed_accounts += 1;
        } else {
            statements::rebalance(c, account.id)?;
        }
    }
    note("comptes sans rien dessous", removed_accounts);
    let mut cards = 0;
    for id in removed_people {
        let Some(card) = PersonCard::get(c, id)? else { continue };
        let used = Account::query(c, "", [])?.iter().any(|a| a.holders.as_ref().map(|h| crate::services::holders::from_json(h).iter().any(|p| p.id == Some(id))).unwrap_or(false));
        let plain = card.notes.as_deref().map(|n| n.trim().is_empty()).unwrap_or(true) && !card.birth_names.as_ref().map(crate::models::truthy).unwrap_or(false);
        if !used && plain {
            PersonCard::delete(c, id)?;
            cards += 1;
        }
    }
    note("fiches de personnes devenues inutiles", cards);
    purge_orphans(c)?;
    // empty accounts nothing says an import made (typed by hand, or imported by a version that left no trace): kept, said
    let mut kept: Vec<String> = vec![];
    for a in Account::query(c, "", [])? {
        if statements::content_of(c, a.id)? == 0 {
            kept.push(a.name.clone());
        }
    }
    if !kept.is_empty() {
        let names = kept.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
        report.push((format!("comptes sans rien dessous, gardés car rien ne prouve qu'un import les a faits ({names}{}) : à supprimer à la main depuis la page du compte ou la table accounts", if kept.len() > 5 { "…" } else { "" }), kept.len() as i64));
    }

    // 3. documents validated whose data is missing: back to « à valider » to be imported again
    let mut again = vec![];
    for mut doc in imported_without_data(c)? {
        let before = doc.clone();
        doc.status = "parsed".into();
        doc.account_id = None;
        doc.save(c, &before)?;
        again.push(doc.id);
    }
    note("documents validés sans leurs données (à importer de nouveau)", again.len());
    for id in Account::query(c, "", [])?.iter().map(|a| a.id) {
        life_insurance::refresh(c, id);
    }
    Ok((report, again))
}

/// Users with what they own, for the emptying choices.
pub fn owners(c: &Connection) -> AppResult<Vec<(i64, String, i64, i64)>> {
    let mut out = vec![];
    for u in crate::services::users::all_users(c)? {
        let accounts: i64 = c.query_row("SELECT COUNT(*) FROM accounts WHERE owner_id = ?", [u.id], |r| r.get(0))?;
        let documents: i64 = c.query_row("SELECT COUNT(*) FROM documents WHERE owner_id = ?", [u.id], |r| r.get(0))?;
        out.push((u.id, u.email.clone(), accounts, documents));
    }
    Ok(out)
}
