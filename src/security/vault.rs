//! In-memory vault: holds the data key only while the app is unlocked.
//!
//! When locked (at start-up, on demand, or after inactivity) the key is wiped and the encrypted database is closed,
//! so nothing on the NAS disk is readable.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use zeroize::Zeroize;

use super::crypto;
use super::keystore::{InvalidPassword, Keystore, KeystoreError};

pub const MIN_PASSWORD_LENGTH: usize = 12;

#[derive(Debug)]
pub struct VaultLocked;

type Callback = Box<dyn Fn() -> Result<(), String> + Send + Sync>;

pub struct Vault {
    pub keystore: Keystore,
    dek: Mutex<Option<Vec<u8>>>,
    last_activity: Mutex<Instant>,
    on_unlock: Mutex<Vec<Arc<Callback>>>,
    on_lock: Mutex<Vec<Arc<Callback>>>,
}

#[derive(Debug)]
pub enum VaultError {
    Weak(String),
    Invalid,
    Store(String),
    Open(String),
}

impl From<InvalidPassword> for VaultError {
    fn from(_: InvalidPassword) -> Self {
        VaultError::Invalid
    }
}

impl From<KeystoreError> for VaultError {
    fn from(e: KeystoreError) -> Self {
        match e {
            KeystoreError::Exists => VaultError::Store("Le coffre est déjà initialisé".into()),
            KeystoreError::Io(s) => VaultError::Store(s),
        }
    }
}

impl Vault {
    pub fn new(keystore: Keystore) -> Self {
        Vault {
            keystore,
            dek: Mutex::new(None),
            last_activity: Mutex::new(Instant::now()),
            on_unlock: Mutex::new(vec![]),
            on_lock: Mutex::new(vec![]),
        }
    }

    pub fn on_unlock(&self, cb: Callback) {
        self.on_unlock.lock().unwrap().push(Arc::new(cb));
    }

    pub fn on_lock(&self, cb: Callback) {
        self.on_lock.lock().unwrap().push(Arc::new(cb));
    }

    pub fn initialized(&self) -> bool {
        self.keystore.exists()
    }

    pub fn unlocked(&self) -> bool {
        self.dek.lock().unwrap().is_some()
    }

    pub fn setup(&self, password: &str, slot: Option<&str>) -> Result<String, VaultError> {
        check_password_strength(password).map_err(VaultError::Weak)?;
        let (dek, recovery) = self.keystore.create(password, slot)?;
        self.open(dek)?;
        Ok(recovery)
    }

    pub fn unlock(&self, password: &str, slot: Option<&str>) -> Result<(), VaultError> {
        let dek = self.keystore.unlock(password, slot)?;
        self.open(dek)
    }

    pub fn unlock_with_recovery(&self, code: &str) -> Result<(), VaultError> {
        let dek = self.keystore.unlock_with_recovery(code)?;
        self.open(dek)
    }

    fn open(&self, dek: Vec<u8>) -> Result<(), VaultError> {
        *self.dek.lock().unwrap() = Some(dek);
        self.touch();
        let callbacks: Vec<Arc<Callback>> = self.on_unlock.lock().unwrap().clone();
        for cb in callbacks {
            if let Err(e) = cb() {
                self.lock();
                return Err(VaultError::Open(e));
            }
        }
        Ok(())
    }

    pub fn lock(&self) {
        let callbacks: Vec<Arc<Callback>> = self.on_lock.lock().unwrap().clone();
        for cb in callbacks {
            let _ = cb();
        }
        if let Some(mut k) = self.dek.lock().unwrap().take() {
            k.zeroize();
        }
    }

    pub fn touch(&self) {
        *self.last_activity.lock().unwrap() = Instant::now();
    }

    pub fn idle_seconds(&self) -> f64 {
        self.last_activity.lock().unwrap().elapsed().as_secs_f64()
    }

    pub fn master(&self) -> Result<Vec<u8>, VaultLocked> {
        self.dek.lock().unwrap().clone().ok_or(VaultLocked)
    }

    pub fn subkey(&self, purpose: &str) -> Result<Vec<u8>, VaultLocked> {
        Ok(crypto::hkdf(&self.master()?, &format!("mymoney/{purpose}")))
    }

    pub fn change_password(&self, current: &str, new: &str, slot: Option<&str>) -> Result<(), VaultError> {
        check_password_strength(new).map_err(VaultError::Weak)?;
        let dek = self.keystore.unlock(current, slot)?;
        if Some(&dek) != self.dek.lock().unwrap().as_ref() {
            return Err(VaultError::Invalid);
        }
        self.keystore.set_password(&dek, new, slot)?;
        Ok(())
    }

    /// Give a new user their own password to the same vault (the vault must be open).
    pub fn add_user_slot(&self, slot: &str, password: &str) -> Result<(), VaultError> {
        check_password_strength(password).map_err(VaultError::Weak)?;
        let dek = self.master().map_err(|_| VaultError::Invalid)?;
        self.keystore.add_slot(&dek, slot, password)?;
        Ok(())
    }

    pub fn remove_user_slot(&self, slot: &str) -> Result<(), VaultError> {
        self.keystore.remove_slot(slot)?;
        Ok(())
    }

    pub fn regenerate_recovery(&self) -> Result<String, VaultError> {
        let dek = self.master().map_err(|_| VaultError::Invalid)?;
        Ok(self.keystore.regenerate_recovery(&dek)?)
    }

    pub fn encrypt_file(&self, data: &[u8], aad: &str) -> Result<Vec<u8>, VaultLocked> {
        Ok(crypto::encrypt(&self.subkey("files")?, data, aad.as_bytes()))
    }

    pub fn decrypt_file(&self, blob: &[u8], aad: &str) -> Result<Vec<u8>, String> {
        let key = self.subkey("files").map_err(|_| "Coffre verrouillé".to_string())?;
        crypto::decrypt(&key, blob, aad.as_bytes()).map_err(|e| e.0.to_string())
    }
}

pub fn check_password_strength(password: &str) -> Result<(), String> {
    let n = password.chars().count();
    if n < MIN_PASSWORD_LENGTH {
        return Err(format!("Le mot de passe maître doit faire au moins {MIN_PASSWORD_LENGTH} caractères"));
    }
    let classes = [
        password.chars().any(|c| c.is_lowercase()),
        password.chars().any(|c| c.is_uppercase()),
        password.chars().any(crate::py::is_digit_char),
        password.chars().any(|c| !crate::py::is_alnum_char(c)),
    ]
    .iter()
    .filter(|x| **x)
    .count();
    if classes < 3 && n < 20 {
        return Err("Mot de passe trop simple : mélange majuscules, minuscules, chiffres et symboles, ou utilise une phrase de passe de 20 caractères ou plus".into());
    }
    Ok(())
}
