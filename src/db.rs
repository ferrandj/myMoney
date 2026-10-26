//! Encrypted database (SQLCipher, AES-256) opened only while the vault is unlocked.

use std::path::PathBuf;
use std::sync::Mutex;

use rusqlite::Connection;

use crate::error::{AppError, AppResult};
use crate::schema;

pub struct Database {
    pub path: PathBuf,
    conn: Mutex<Option<Connection>>,
    /// A second connection, for reads that Python made in a session of their own (the price cache, read while
    /// a portfolio is being computed in the main one).
    reader: Mutex<Option<Connection>>,
}

impl Database {
    pub fn new(path: PathBuf) -> Self {
        Database { path, conn: Mutex::new(None), reader: Mutex::new(None) }
    }

    /// Open with the raw 256-bit key (SQLCipher skips its own KDF: the key already comes from Argon2id).
    pub fn open(&self, key: &[u8]) -> Result<(), String> {
        let conn = connect(&self.path, key)?;
        create_all(&conn).map_err(|e| e.to_string())?;
        add_missing_columns(&conn).map_err(|e| e.to_string())?;
        let reader = connect(&self.path, key)?;
        *self.conn.lock().unwrap() = Some(conn);
        *self.reader.lock().unwrap() = Some(reader);
        Ok(())
    }

    pub fn close(&self) {
        self.conn.lock().unwrap().take();
        self.reader.lock().unwrap().take();
    }

    pub fn is_open(&self) -> bool {
        self.conn.lock().unwrap().is_some()
    }

    /// A read on the second connection: it sees what is committed, like a separate session.
    pub fn read<T>(&self, f: impl FnOnce(&Connection) -> AppResult<T>) -> AppResult<T> {
        let guard = self.reader.lock().unwrap();
        let conn = guard.as_ref().ok_or(AppError::Locked)?;
        f(conn)
    }

    /// Run `f` in a transaction: committed when it returns Ok, rolled back otherwise (the Python session).
    pub fn session<T>(&self, f: impl FnOnce(&Connection) -> AppResult<T>) -> AppResult<T> {
        let mut guard = self.conn.lock().unwrap();
        let conn = guard.as_mut().ok_or(AppError::Locked)?;
        let tx = conn.transaction()?;
        let out = f(&tx);
        match out {
            Ok(v) => {
                tx.commit()?;
                Ok(v)
            }
            Err(e) => {
                let _ = tx.rollback();
                Err(e)
            }
        }
    }
}

fn connect(path: &std::path::Path, key: &[u8]) -> Result<Connection, String> {
    let conn = Connection::open(path).map_err(|e| e.to_string())?;
    let hex_key = hex::encode(key);
    conn.execute_batch(&format!("PRAGMA key = \"x'{hex_key}'\";")).map_err(|e| e.to_string())?;
    conn.execute_batch("PRAGMA cipher_memory_security = ON;").map_err(|e| e.to_string())?;
    // Fails here (instead of later) when the key is wrong or the file is corrupted.
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get::<_, i64>(0)).map_err(|e| e.to_string())?;
    conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA secure_delete = ON;").map_err(|e| e.to_string())?;
    conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(())).map_err(|e| e.to_string())?;
    conn.busy_timeout(std::time::Duration::from_secs(5)).map_err(|e| e.to_string())?;
    Ok(conn)
}

fn table_exists(c: &Connection, name: &str) -> rusqlite::Result<bool> {
    c.query_row("SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?", [name], |r| r.get::<_, i64>(0)).map(|n| n > 0)
}

/// `Base.metadata.create_all`: the missing tables, with their indexes, in SQLAlchemy's order and text.
pub(crate) fn create_all(c: &Connection) -> rusqlite::Result<()> {
    for (name, ddl, indexes) in schema::TABLES {
        if table_exists(c, name)? {
            continue;
        }
        c.execute_batch(ddl)?;
        for ix in *indexes {
            c.execute_batch(ix)?;
        }
    }
    for (name, ddl) in EXTRA_TABLES {
        if !table_exists(c, name)? {
            c.execute_batch(ddl)?;
        }
    }
    Ok(())
}

/// Tables added since the Python models were frozen (the Python version ignores them).
const EXTRA_TABLES: &[(&str, &str)] = &[(
    "people",
    "CREATE TABLE people (id INTEGER NOT NULL, owner_id INTEGER, civility VARCHAR(10) NOT NULL, last_names TEXT, birth_names TEXT, first_names TEXT, notes TEXT, created_at DATETIME NOT NULL, PRIMARY KEY (id))",
)];

/// Columns added since the Python models were frozen (nullable, so the Python version still opens the same database).
const EXTRA_COLUMNS: &[(&str, &str, &str, bool)] = &[("accounts", "holders", "TEXT", true), ("accounts", "interest_basis", "VARCHAR(10)", true), ("documents", "original_filename", "VARCHAR(300)", true)];

/// Light schema upgrade: a database created by an older version gets the columns added since (all nullable).
pub(crate) fn add_missing_columns(c: &Connection) -> rusqlite::Result<()> {
    for (table, column, sqltype, nullable) in schema::COLUMNS.iter().chain(EXTRA_COLUMNS) {
        if !nullable || !table_exists(c, table)? {
            continue;
        }
        let mut st = c.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
        let have: Vec<String> = st.query_map([], |r| r.get::<_, String>(1))?.collect::<Result<_, _>>()?;
        if !have.iter().any(|h| h == column) {
            c.execute_batch(&format!("ALTER TABLE \"{table}\" ADD COLUMN \"{column}\" {sqltype}"))?;
        }
    }
    Ok(())
}
