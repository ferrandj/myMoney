//! Master-key storage.
//!
//! A random 256-bit data key (DEK) encrypts everything. It is never written to disk in clear: the keystore only
//! holds it *wrapped* (AES-256-GCM) by keys derived with Argon2id from
//!   * one password per user (a "key slot", like the slots of an encrypted disk), and
//!   * a one-time recovery code shown at setup.
//! Changing a password re-wraps the DEK for that slot only; the database does not need re-encrypting.
//!
//! Version 1 keystores (a single "password" entry, before there were users) are still read: that entry is the
//! legacy slot, which the first user adopts (`adopt_legacy_slot`) without needing the password again.

use std::path::PathBuf;

use base64::Engine;
use rand::Rng;
use serde_json::{json, Map, Value};

use super::crypto;

pub const VERSION: i64 = 2;
const RECOVERY_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789"; // no 0/O/1/I ambiguity

#[derive(Debug)]
pub struct InvalidPassword;

#[derive(Debug)]
pub enum KeystoreError {
    Exists,
    Io(String),
}

fn b64(d: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(d)
}

fn unb64(s: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD.decode(s).unwrap_or_default()
}

pub fn generate_recovery_code() -> String {
    let mut rng = rand::rngs::OsRng;
    let raw: Vec<u8> = (0..32).map(|_| RECOVERY_ALPHABET[rng.gen_range(0..RECOVERY_ALPHABET.len())]).collect();
    raw.chunks(4).map(|c| String::from_utf8(c.to_vec()).unwrap()).collect::<Vec<_>>().join("-")
}

pub fn normalize_recovery_code(code: &str) -> String {
    code.to_uppercase().chars().filter(|c| crate::py::is_alnum_char(*c)).collect()
}

#[derive(Clone)]
pub struct Keystore {
    pub path: PathBuf,
    pub time_cost: u32,
    pub memory_kib: u32,
}

impl Keystore {
    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    fn wrap(&self, secret: &[u8], dek: &[u8], purpose: &str) -> Value {
        let salt = crypto::random_bytes(16);
        let kek = crypto::argon2id(secret, &salt, self.time_cost, self.memory_kib);
        let aad = format!("mymoney-keystore-v{VERSION}|{purpose}");
        json!({
            "salt": b64(&salt),
            "time_cost": self.time_cost,
            "memory_kib": self.memory_kib,
            "wrapped": b64(&crypto::encrypt(&kek, dek, aad.as_bytes())),
        })
    }

    fn unwrap(secret: &[u8], entry: &Value, purpose: &str) -> Result<Vec<u8>, InvalidPassword> {
        let salt = unb64(entry["salt"].as_str().unwrap_or(""));
        let t = entry["time_cost"].as_u64().unwrap_or(3) as u32;
        let m = entry["memory_kib"].as_u64().unwrap_or(131072) as u32;
        let kek = crypto::argon2id(secret, &salt, t, m);
        let aad = format!("mymoney-keystore-v{VERSION}|{purpose}");
        crypto::decrypt(&kek, &unb64(entry["wrapped"].as_str().unwrap_or("")), aad.as_bytes()).map_err(|_| InvalidPassword)
    }

    fn write(&self, data: &Value) -> Result<(), KeystoreError> {
        let tmp = self.path.with_extension("tmp");
        let text = serde_json::to_string_pretty(data).map_err(|e| KeystoreError::Io(e.to_string()))?;
        {
            use std::io::Write;
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut f = opts.open(&tmp).map_err(|e| KeystoreError::Io(e.to_string()))?;
            f.write_all(text.as_bytes()).map_err(|e| KeystoreError::Io(e.to_string()))?;
            f.flush().ok();
            f.sync_all().ok();
        }
        std::fs::rename(&tmp, &self.path).map_err(|e| KeystoreError::Io(e.to_string()))
    }

    fn read(&self) -> Value {
        std::fs::read_to_string(&self.path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({}))
    }

    /// Create a new keystore, with one key slot (the first user's, or the legacy one). Returns (DEK, recovery code).
    pub fn create(&self, password: &str, slot: Option<&str>) -> Result<(Vec<u8>, String), KeystoreError> {
        if self.exists() {
            return Err(KeystoreError::Exists);
        }
        let dek = crypto::random_key();
        let recovery = generate_recovery_code();
        let mut data = Map::new();
        data.insert("version".into(), json!(VERSION));
        data.insert("slots".into(), json!({}));
        data.insert("recovery".into(), self.wrap(normalize_recovery_code(&recovery).as_bytes(), &dek, "recovery"));
        match slot {
            Some(s) => {
                data["slots"].as_object_mut().unwrap().insert(s.into(), self.wrap(password.as_bytes(), &dek, "password"));
            }
            None => {
                data.insert("password".into(), self.wrap(password.as_bytes(), &dek, "password"));
            }
        }
        self.write(&Value::Object(data))?;
        Ok((dek, recovery))
    }

    pub fn has_legacy_slot(&self) -> bool {
        self.exists() && self.read().get("password").is_some()
    }

    pub fn has_slot(&self, slot: &str) -> bool {
        self.read().get("slots").and_then(|s| s.get(slot)).is_some()
    }

    pub fn slots(&self) -> Vec<String> {
        self.read().get("slots").and_then(|s| s.as_object()).map(|m| m.keys().cloned().collect()).unwrap_or_default()
    }

    /// The DEK, from a user's password (`slot`) or, before users exist, from the legacy single password.
    pub fn unlock(&self, password: &str, slot: Option<&str>) -> Result<Vec<u8>, InvalidPassword> {
        let data = self.read();
        let entry = match slot {
            Some(s) => data.get("slots").and_then(|x| x.get(s)).cloned(),
            None => data.get("password").cloned(),
        };
        let Some(entry) = entry else { return Err(InvalidPassword) }; // unknown user: same answer as a wrong password
        Keystore::unwrap(password.as_bytes(), &entry, "password")
    }

    pub fn unlock_with_recovery(&self, code: &str) -> Result<Vec<u8>, InvalidPassword> {
        let data = self.read();
        let entry = data.get("recovery").cloned().ok_or(InvalidPassword)?;
        Keystore::unwrap(normalize_recovery_code(code).as_bytes(), &entry, "recovery")
    }

    pub fn add_slot(&self, dek: &[u8], slot: &str, password: &str) -> Result<(), KeystoreError> {
        let mut data = self.read();
        let entry = self.wrap(password.as_bytes(), dek, "password");
        let obj = data.as_object_mut().unwrap();
        if !obj.contains_key("slots") {
            obj.insert("slots".into(), json!({}));
        }
        obj["slots"].as_object_mut().unwrap().insert(slot.into(), entry);
        obj.insert("version".into(), json!(VERSION));
        self.write(&data)
    }

    pub fn set_password(&self, dek: &[u8], new_password: &str, slot: Option<&str>) -> Result<(), KeystoreError> {
        let mut data = self.read();
        let entry = self.wrap(new_password.as_bytes(), dek, "password");
        let obj = data.as_object_mut().unwrap();
        match slot {
            Some(s) => {
                if !obj.contains_key("slots") {
                    obj.insert("slots".into(), json!({}));
                }
                obj["slots"].as_object_mut().unwrap().insert(s.into(), entry);
            }
            None => {
                obj.insert("password".into(), entry);
            }
        }
        self.write(&data)
    }

    pub fn remove_slot(&self, slot: &str) -> Result<(), KeystoreError> {
        let mut data = self.read();
        if let Some(s) = data.get_mut("slots").and_then(|s| s.as_object_mut()) {
            s.shift_remove(slot);
        }
        self.write(&data)
    }

    /// The single password of a version 1 keystore becomes the slot of the first user (same password).
    pub fn adopt_legacy_slot(&self, slot: &str) -> Result<(), KeystoreError> {
        let mut data = self.read();
        let obj = data.as_object_mut().unwrap();
        if let Some(entry) = obj.shift_remove("password") {
            if !obj.contains_key("slots") {
                obj.insert("slots".into(), json!({}));
            }
            obj["slots"].as_object_mut().unwrap().insert(slot.into(), entry);
            obj.insert("version".into(), json!(VERSION));
            self.write(&data)?;
        }
        Ok(())
    }

    pub fn regenerate_recovery(&self, dek: &[u8]) -> Result<String, KeystoreError> {
        let mut data = self.read();
        let recovery = generate_recovery_code();
        let entry = self.wrap(normalize_recovery_code(&recovery).as_bytes(), dek, "recovery");
        data.as_object_mut().unwrap().insert("recovery".into(), entry);
        self.write(&data)?;
        Ok(recovery)
    }
}
