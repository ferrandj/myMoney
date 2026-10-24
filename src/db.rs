//! Encrypted database (SQLCipher, AES-256) opened only while the vault is unlocked.

use std::path::PathBuf;
use std::sync::Mutex;

use rusqlite::Connection;

use crate::error::{AppError, AppResult};
use crate::schema;

pub struct Database {
    pub path: PathBuf,
    conn: Mutex<Option<Connection>>,
}

impl Database {
    pub fn new(path: PathBuf) -> Self {
        Database { path, conn: Mutex::new(None) }
    }

    /// Open with the raw 256-bit key (SQLCipher skips its own KDF: the key already comes from Argon2id).
    pub fn open(&self, key: &[u8]) -> Result<(), String> {
        let conn = Connection::open(&self.path).map_err(|e| e.to_string())?;
        let hex_key = hex::encode(key);
        conn.execute_batch(&format!("PRAGMA key = \"x'{hex_key}'\";")).map_err(|e| e.to_string())?;
        conn.execute_batch("PRAGMA cipher_memory_security = ON;").map_err(|e| e.to_string())?;
        // Fails here (instead of later) when the key is wrong or the file is corrupted.
        conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get::<_, i64>(0)).map_err(|e| e.to_string())?;
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA secure_delete = ON;").map_err(|e| e.to_string())?;
        conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(())).map_err(|e| e.to_string())?;
        create_all(&conn).map_err(|e| e.to_string())?;
        add_missing_columns(&conn).map_err(|e| e.to_string())?;
        *self.conn.lock().unwrap() = Some(conn);
        Ok(())
    }

    pub fn close(&self) {
        self.conn.lock().unwrap().take();
    }

    pub fn is_open(&self) -> bool {
        self.conn.lock().unwrap().is_some()
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

fn table_exists(c: &Connection, name: &str) -> rusqlite::Result<bool> {
    c.query_row("SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?", [name], |r| r.get::<_, i64>(0)).map(|n| n > 0)
}

/// `Base.metadata.create_all`: the missing tables, with their indexes, in SQLAlchemy's order and text.
fn create_all(c: &Connection) -> rusqlite::Result<()> {
    for (name, ddl, indexes) in schema::TABLES {
        if table_exists(c, name)? {
            continue;
        }
        c.execute_batch(ddl)?;
        for ix in *indexes {
            c.execute_batch(ix)?;
        }
    }
    Ok(())
}

/// Light schema upgrade: a database created by an older version gets the columns added since (all nullable).
fn add_missing_columns(c: &Connection) -> rusqlite::Result<()> {
    for (table, column, sqltype, nullable) in schema::COLUMNS {
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
