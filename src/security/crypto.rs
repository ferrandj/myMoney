//! Low-level primitives: AES-256-GCM authenticated encryption, Argon2id and HKDF (same formats as before).

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use rand::RngCore;

pub const MAGIC: &[u8] = b"MM1";
pub const NONCE_SIZE: usize = 12;
pub const KEY_SIZE: usize = 32;

#[derive(Debug)]
pub struct DecryptionError(pub &'static str);

impl std::fmt::Display for DecryptionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

pub fn random_bytes(n: usize) -> Vec<u8> {
    let mut v = vec![0u8; n];
    rand::rngs::OsRng.fill_bytes(&mut v);
    v
}

pub fn random_key() -> Vec<u8> {
    random_bytes(KEY_SIZE)
}

pub fn encrypt(key: &[u8], plaintext: &[u8], aad: &[u8]) -> Vec<u8> {
    let nonce = random_bytes(NONCE_SIZE);
    let cipher = Aes256Gcm::new_from_slice(key).expect("key size");
    let ct = cipher.encrypt(Nonce::from_slice(&nonce), Payload { msg: plaintext, aad }).expect("encrypt");
    let mut out = Vec::with_capacity(MAGIC.len() + NONCE_SIZE + ct.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    out
}

pub fn decrypt(key: &[u8], blob: &[u8], aad: &[u8]) -> Result<Vec<u8>, DecryptionError> {
    if !blob.starts_with(MAGIC) {
        return Err(DecryptionError("Format de données chiffrées inconnu"));
    }
    if blob.len() < MAGIC.len() + NONCE_SIZE {
        return Err(DecryptionError("Clé invalide ou données altérées"));
    }
    let nonce = &blob[MAGIC.len()..MAGIC.len() + NONCE_SIZE];
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| DecryptionError("Clé invalide ou données altérées"))?;
    cipher
        .decrypt(Nonce::from_slice(nonce), Payload { msg: &blob[MAGIC.len() + NONCE_SIZE..], aad })
        .map_err(|_| DecryptionError("Clé invalide ou données altérées"))
}

pub fn argon2id(secret: &[u8], salt: &[u8], time_cost: u32, memory_kib: u32) -> Vec<u8> {
    let params = argon2::Params::new(memory_kib, time_cost, 2, Some(KEY_SIZE)).expect("argon2 params");
    let a = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut out = vec![0u8; KEY_SIZE];
    a.hash_password_into(secret, salt, &mut out).expect("argon2");
    out
}

pub fn hkdf(master: &[u8], info: &str) -> Vec<u8> {
    let h = hkdf::Hkdf::<sha2::Sha256>::new(None, master);
    let mut out = vec![0u8; KEY_SIZE];
    h.expand(info.as_bytes(), &mut out).expect("hkdf");
    out
}
