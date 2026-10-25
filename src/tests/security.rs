//! python/tests/test_security.py

use crate::error::AppError;
use crate::models::Account;
use crate::security::crypto;
use crate::security::vault::check_password_strength;
use crate::services::counterparties;
use crate::testkit::env::{core, PASSWORD};
use crate::testkit::{db, form};

#[test]
fn test_encrypt_roundtrip_and_tamper() {
    let key = crypto::random_key();
    let blob = crypto::encrypt(&key, b"secret", b"aad");
    assert_eq!(crypto::decrypt(&key, &blob, b"aad").unwrap(), b"secret");
    assert!(crypto::decrypt(&key, &blob, b"other-aad").is_err());
    let mut tampered = blob.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 1;
    assert!(crypto::decrypt(&key, &tampered, b"aad").is_err());
}

#[test]
fn test_weak_passwords_rejected() {
    for weak in ["short", "alllowercaseletters", "Password1"] {
        assert!(check_password_strength(weak).is_err(), "{weak}");
    }
    check_password_strength(PASSWORD).unwrap();
    check_password_strength("une phrase de passe assez longue").unwrap();
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn test_database_and_documents_are_encrypted_at_rest() {
    let dir = crate::testkit::env::Dir::new();
    let settings = crate::testkit::env::settings(&dir);
    let app = crate::app::MyMoney::new(settings.clone()).unwrap();
    let recovery = app.vault.setup(PASSWORD, None).unwrap();
    app.save_account(
        None,
        &form(&[
            ("name", "Compte joint MARQUEUR"),
            ("account_type", "checking"),
            ("balance", "1234.56"),
            ("iban", "FR7630001007941234567890185"),
            ("holder_name", "TITULAIRE MARQUEUR"),
            ("holder_address", "1 RUE MARQUEUR"),
            ("bank_code", "30001"),
            ("account_number", "ACCT-MARQUEUR"),
        ]),
    )
    .unwrap();
    db(&app, |c| counterparties::save_contact(c, None, &form(&[("name", "CONTACT MARQUEUR"), ("phone", "0611223344"), ("notes", "NOTE MARQUEUR")])).map(|_| ()));
    let (doc_id, _) = app.store_document("releve-MARQUEUR.csv", "Date;Libellé;Montant\n01/02/2026;LOYER MARQUEUR;-800,00\n".as_bytes()).unwrap();
    app.vault.lock();

    let raw_db = std::fs::read(settings.db_path()).unwrap();
    assert!(!raw_db.starts_with(b"SQLite format 3"));
    for secret in [&b"MARQUEUR"[..], b"FR7630001007941234567890185", b"0611223344"] {
        assert!(!contains(&raw_db, secret));
    }
    for blob in std::fs::read_dir(settings.documents_dir()).unwrap() {
        let blob = blob.unwrap();
        assert!(!contains(&std::fs::read(blob.path()).unwrap(), b"MARQUEUR") && !contains(blob.file_name().to_string_lossy().as_bytes(), b"MARQUEUR"));
    }
    assert!(!contains(&std::fs::read(settings.keystore_path()).unwrap(), PASSWORD.as_bytes()));

    assert!(matches!(app.session(|_| Ok(())), Err(AppError::Locked)));

    assert!(app.vault.unlock("Wrong-Password-1!", None).is_err());
    app.vault.unlock_with_recovery(&recovery.to_lowercase().replace('-', " ")).unwrap();
    db(&app, |c| {
        assert_eq!(Account::get(c, 1)?.unwrap().name, "Compte joint MARQUEUR");
        Ok(())
    });
    let (filename, content) = app.read_document(doc_id).unwrap();
    assert!(filename == "releve-MARQUEUR.csv" && contains(&content, b"MARQUEUR"));
}

#[test]
fn test_change_password_keeps_data() {
    let app = core();
    app.save_account(None, &form(&[("name", "Livret"), ("account_type", "livret_a"), ("balance", "10")])).unwrap();
    app.vault.change_password(PASSWORD, "Another-Strong-Pass-2", None).unwrap();
    app.vault.lock();
    assert!(app.vault.unlock(PASSWORD, None).is_err());
    app.vault.unlock("Another-Strong-Pass-2", None).unwrap();
    db(&app, |c| {
        assert_eq!(Account::get(c, 1)?.unwrap().name, "Livret");
        Ok(())
    });
}

#[test]
fn test_login_throttling() {
    let app = core();
    app.vault.lock();
    for _ in 0..3 {
        assert!(matches!(app.check_password("Wrong-Password-1!", None), Err(AppError::InvalidPassword)));
    }
    assert!(matches!(app.check_password(PASSWORD, None), Err(AppError::Throttled(_))));
}
