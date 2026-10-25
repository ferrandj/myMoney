//! python/tests/test_users.py: several users, key slots, ownership, sharing, and what each person can see.

use std::collections::BTreeSet;

use regex::Regex;
use rust_decimal::Decimal;

use crate::app::MyMoney;
use crate::error::AppError;
use crate::models::{Account, AccountShare, Counterparty, Document, Transaction, User};
use crate::security::keystore::Keystore;
use crate::services::access::{self, Identity};
use crate::services::{counterparties, ledger, users as user_service};
use crate::testkit::env::{bare_core, client, csrf_of, Client, Core, Dir, Upload, ADMIN_EMAIL, PASSWORD};
use crate::testkit::fixtures::*;
use crate::testkit::{db, form, num};

fn d(s: &str) -> Decimal {
    dec(s)
}

const BOB: &str = "bob@example.com";
const BOB_PASSWORD: &str = "Another-Strong-Pass-7";
const CAROL: &str = "carol@example.com";
const CAROL_PASSWORD: &str = "Third-Strong-Pass-3!";

fn s(v: &str) -> Option<String> {
    Some(v.to_string())
}

// ------------------------------------------------------------------------------------------------ key slots
fn keystore(dir: &Dir) -> Keystore {
    Keystore { path: dir.0.join("keys.json"), time_cost: 1, memory_kib: 8192 }
}

#[test]
fn test_each_user_has_a_key_slot_to_the_same_vault() {
    let dir = Dir::new();
    let store = keystore(&dir);
    let (alice, bob) = (user_service::slot_id("alice@example.com").unwrap(), user_service::slot_id(BOB).unwrap());
    let (dek, recovery) = store.create("Alice-Password-1!", Some(&alice)).unwrap();
    store.add_slot(&dek, &bob, BOB_PASSWORD).unwrap();
    assert_eq!(store.unlock("Alice-Password-1!", Some(&alice)).unwrap(), dek);
    assert_eq!(store.unlock(BOB_PASSWORD, Some(&bob)).unwrap(), dek);
    // a wrong password, someone else's, an unknown user and a missing user all look the same
    for (password, slot) in [("wrong", Some(alice.as_str())), (BOB_PASSWORD, Some(alice.as_str())), ("Alice-Password-1!", Some(bob.as_str())), ("x", Some("unknown-slot")), ("Alice-Password-1!", None)] {
        assert!(store.unlock(password, slot).is_err(), "{password} {slot:?}");
    }
    store.set_password(&dek, "Alice-New-Password-2!", Some(&alice)).unwrap(); // changing one password leaves the others alone
    assert_eq!(store.unlock(BOB_PASSWORD, Some(&bob)).unwrap(), dek);
    assert!(store.unlock("Alice-Password-1!", Some(&alice)).is_err());
    store.remove_slot(&bob).unwrap();
    assert!(store.unlock(BOB_PASSWORD, Some(&bob)).is_err());
    assert_eq!(store.unlock_with_recovery(&recovery).unwrap(), dek);
    let text = std::fs::read_to_string(&store.path).unwrap();
    assert!(!text.contains("Alice") && !text.contains("alice"));
}

#[test]
fn test_a_vault_from_before_users_keeps_its_password_for_the_first_user() {
    let dir = Dir::new();
    let store = keystore(&dir);
    let (dek, _) = store.create(PASSWORD, None).unwrap(); // version 1 layout: a single password
    assert!(store.has_legacy_slot());
    assert_eq!(store.unlock(PASSWORD, None).unwrap(), dek);
    let slot = user_service::slot_id(ADMIN_EMAIL).unwrap();
    store.adopt_legacy_slot(&slot).unwrap();
    assert!(!store.has_legacy_slot());
    assert_eq!(store.unlock(PASSWORD, Some(&slot)).unwrap(), dek);
    assert_eq!(store.slots(), vec![slot]);
}

#[test]
fn test_emails_are_normalised_and_checked() {
    assert_eq!(user_service::normalize_email(Some("  Bob@Example.COM ")).unwrap(), BOB);
    assert_eq!(user_service::slot_id("BOB@example.com").unwrap(), user_service::slot_id(BOB).unwrap());
    for bad in [Some(""), Some("bob"), Some("bob@"), Some("@example.com"), Some("a b@example.com"), None] {
        assert!(matches!(user_service::normalize_email(bad), Err(AppError::Value(_))), "{bad:?}");
    }
}

// --------------------------------------------------------------------------------------- core level
fn users_core() -> Core {
    let core = bare_core();
    core.setup(ADMIN_EMAIL, Some("Alice"), PASSWORD).unwrap();
    db(&core, |c| {
        user_service::add_with_password(c, &core.vault, BOB, Some("Bob"), BOB_PASSWORD, false)?;
        user_service::add_with_password(c, &core.vault, CAROL, Some("Carol"), CAROL_PASSWORD, false)?;
        Ok(())
    });
    core
}

fn identity(core: &MyMoney, email: &str) -> Identity {
    core.identity_for_email(Some(email)).unwrap().unwrap()
}

fn new_account(core: &MyMoney, who: &str, name: &str) -> i64 {
    access::as_user(Some(identity(core, who)), || core.save_account(None, &form(&[("name", name), ("account_type", "checking"), ("balance", "100"), ("iban", "")])).unwrap())
}

fn names(core: &MyMoney, who: &str) -> Vec<String> {
    access::as_user(Some(identity(core, who)), || {
        let mut v: Vec<String> = core.portfolio(false).unwrap().accounts.iter().map(|v| v.account.name.clone()).collect();
        v.sort();
        v
    })
}

fn user(core: &MyMoney, email: &str) -> User {
    db(core, |c| Ok(User::query_one(c, "WHERE email = ?", [email])?.unwrap()))
}

/// The same, from inside a session (the database lock is not re-entrant).
fn user_in(c: &rusqlite::Connection, email: &str) -> User {
    User::query_one(c, "WHERE email = ?", [email]).unwrap().unwrap()
}

fn denied<T: std::fmt::Debug>(result: Result<T, AppError>, needle: &str) {
    match result {
        Err(AppError::Denied(m)) => assert!(m.contains(needle), "{m:?} does not contain {needle:?}"),
        other => panic!("expected an AccessDenied containing {needle:?}, got {other:?}"),
    }
}

fn value<T: std::fmt::Debug>(result: Result<T, AppError>, needle: &str) {
    match result {
        Err(AppError::Value(m)) => assert!(m.contains(needle), "{m:?} does not contain {needle:?}"),
        other => panic!("expected a ValueError containing {needle:?}, got {other:?}"),
    }
}

#[test]
fn test_first_user_is_the_administrator_and_login_needs_the_right_password() {
    let core = users_core();
    let alice = core.authenticate(ADMIN_EMAIL, PASSWORD).unwrap();
    assert!(alice.is_admin && alice.email == ADMIN_EMAIL && alice.name.as_deref() == Some("Alice"));
    assert!(!core.authenticate(BOB, BOB_PASSWORD).unwrap().is_admin);
    for (email, password) in [(ADMIN_EMAIL, BOB_PASSWORD), (BOB, PASSWORD), ("nobody@example.com", PASSWORD), ("not-an-email", PASSWORD)] {
        core.reset_throttle();
        assert!(matches!(core.authenticate(email, password), Err(AppError::InvalidPassword)), "{email}");
    }
}

#[test]
fn test_everyone_sees_their_own_accounts_only_until_something_is_shared() {
    let core = users_core();
    let a = new_account(&core, ADMIN_EMAIL, "Compte Alice");
    let b = new_account(&core, BOB, "Compte Bob");
    assert_eq!(names(&core, ADMIN_EMAIL), vec!["Compte Alice"]);
    assert_eq!(names(&core, BOB), vec!["Compte Bob"]);
    assert!(names(&core, CAROL).is_empty());
    let (alice_id, bob_id) = (identity(&core, ADMIN_EMAIL).id, identity(&core, BOB).id);
    db(&core, |c| {
        assert_eq!(Account::get(c, a)?.unwrap().owner_id, Some(alice_id));
        assert_eq!(Account::get(c, b)?.unwrap().owner_id, Some(bob_id));
        Ok(())
    });
    access::as_user(Some(identity(&core, BOB)), || {
        db(&core, |c| {
            denied(access::check_read(c, a), "");
            assert_eq!(access::readable_ids(c)?, Some([b].into_iter().collect::<BTreeSet<_>>()));
            Ok(())
        });
    });
}

#[test]
fn test_a_share_gives_read_or_edit_and_only_the_owner_can_delete() {
    let core = users_core();
    let a = new_account(&core, ADMIN_EMAIL, "Compte Alice");
    db(&core, |c| {
        let account = Account::get(c, a)?.unwrap();
        user_service::share(c, &account, &user_in(c, BOB), false)?;
        user_service::share(c, &account, &user_in(c, CAROL), true)?;
        Ok(())
    });
    assert_eq!(names(&core, BOB), vec!["Compte Alice"]);
    assert_eq!(names(&core, CAROL), vec!["Compte Alice"]);
    access::as_user(Some(identity(&core, BOB)), || {
        denied(core.save_account(Some(a), &form(&[("name", "Renommé")])), "lecture seule");
        denied(core.add_holding(a, &form(&[("quantity", "1"), ("symbol", "BTC")])), "");
        denied(core.delete_account(a), "propriétaire");
    });
    access::as_user(Some(identity(&core, CAROL)), || {
        core.save_account(Some(a), &form(&[("name", "Compte Alice (corrigé)")])).unwrap(); // may edit...
        denied(core.delete_account(a), "propriétaire"); // ...but not delete
    });
    access::as_user(Some(identity(&core, ADMIN_EMAIL)), || core.delete_account(a).unwrap());
    assert!(names(&core, BOB).is_empty());
}

#[test]
fn test_sharing_rules() {
    let core = users_core();
    let a = new_account(&core, ADMIN_EMAIL, "Compte Alice");
    db(&core, |c| {
        let account = Account::get(c, a)?.unwrap();
        let (alice, bob) = (user_in(c, ADMIN_EMAIL), user_in(c, BOB));
        value(user_service::share(c, &account, &alice, false), "déjà propriétaire");
        user_service::share(c, &account, &bob, false)?;
        user_service::share(c, &account, &bob, true)?; // sharing again changes the rights, it does not add a second share
        assert_eq!(user_service::shares_of(c, a)?.iter().map(|sh| (sh.user_id, sh.can_edit)).collect::<Vec<_>>(), vec![(bob.id, true)]);
        user_service::unshare(c, &account, bob.id)?;
        assert!(user_service::shares_of(c, a)?.is_empty() && AccountShare::query(c, "", [])?.is_empty());
        Ok(())
    });
}

#[test]
fn test_documents_follow_the_same_rules() {
    let core = users_core();
    let data = boursobank_statement_pdf(&Spec::new(sample_entries()));
    let doc = access::as_user(Some(identity(&core, ADMIN_EMAIL)), || {
        let (doc_id, _) = core.store_document("releve.pdf", &data).unwrap();
        core.analyze_document(doc_id, None, Some(data.clone())).unwrap();
        doc_id
    });
    access::as_user(Some(identity(&core, BOB)), || {
        assert!(!core.document_visible(doc).unwrap());
        assert!(core.review_document(doc).unwrap().is_none());
        denied(core.read_document(doc), "");
        denied(core.delete_document(doc), "");
        denied(core.import_statement(doc, 0, &form(&[("account_id", "new")])), "");
        denied(core.undo_import(doc), "");
    });
    let account_id = access::as_user(Some(identity(&core, ADMIN_EMAIL)), || {
        num(&core.import_statement(doc, 0, &form(&[("account_id", "new"), ("name", "Courant"), ("import_transactions", "on")])).unwrap(), "account_id")
    });
    db(&core, |c| {
        // once the account is shared, its documents are readable (read only: not changeable)
        user_service::share(c, &Account::get(c, account_id)?.unwrap(), &user_in(c, BOB), false)?;
        Ok(())
    });
    access::as_user(Some(identity(&core, BOB)), || {
        assert!(core.document_visible(doc).unwrap());
        assert_eq!(core.read_document(doc).unwrap().0, "releve.pdf");
        denied(core.delete_document(doc), "");
        denied(core.undo_import(doc), "");
    });
}

#[test]
fn test_importing_into_someone_elses_account_is_refused_and_a_statement_of_it_is_not_matched_to_it() {
    let core = users_core();
    let data = boursobank_statement_pdf(&Spec::new(sample_entries()));
    let account_id = access::as_user(Some(identity(&core, ADMIN_EMAIL)), || {
        let (doc, _) = core.store_document("a.pdf", &data).unwrap();
        core.analyze_document(doc, None, Some(data.clone())).unwrap();
        num(&core.import_statement(doc, 0, &form(&[("account_id", "new"), ("name", "Courant"), ("import_transactions", "on")])).unwrap(), "account_id")
    });
    // Bob drops the next month of the same account (same IBAN): he has no right on it, so it is not offered as the target.
    let next_month = boursobank_statement_pdf(&Spec {
        opening: Spec::new(sample_entries()).closing(),
        opening_date: ymd(2026, 9, 30),
        period: (ymd(2026, 10, 1), ymd(2026, 10, 31)),
        ..Spec::new(vec![entry(ymd(2026, 10, 3), &["CARTE 01/10/26 BOULANGERIE CB*1234"], "-4.20")])
    });
    let doc2 = access::as_user(Some(identity(&core, BOB)), || {
        let (doc2, _) = core.store_document("b.pdf", &next_month).unwrap();
        core.analyze_document(doc2, None, Some(next_month.clone())).unwrap();
        assert!(core.auto_import(doc2).unwrap().is_none()); // nothing to attach to: it would be a new account, which he must decide
        let review = core.review_document(doc2).unwrap().unwrap();
        assert!(review.plans[0].account.is_none() && review.accounts.is_empty());
        denied(core.import_statement(doc2, 0, &form(&[("account_id", &account_id.to_string()), ("import_transactions", "on")])), "");
        doc2
    });
    db(&core, |c| {
        // edit rights make it his target
        user_service::share(c, &Account::get(c, account_id)?.unwrap(), &user_in(c, BOB), true)?;
        Ok(())
    });
    access::as_user(Some(identity(&core, BOB)), || {
        assert_eq!(num(&core.auto_import(doc2).unwrap().unwrap(), "account_id"), account_id);
    });
}

fn add_transaction(c: &rusqlite::Connection, account_id: i64, fingerprint: &str, label: &str, amount: Decimal, kind: &str, counterparty: Option<i64>) {
    let mut t = Transaction::default();
    t.account_id = account_id;
    t.fingerprint = fingerprint.into();
    t.date = ymd(2026, 9, 5);
    t.label = label.into();
    t.amount = amount;
    t.kind = s(kind);
    t.counterparty_id = counterparty;
    t.insert(c).unwrap();
}

#[test]
fn test_two_people_accounts_are_not_my_own_accounts_for_transfers() {
    // An opposite amount on the same day is "between my own accounts" only when both accounts have the same owner.
    let core = users_core();
    let (alice1, alice2) = (new_account(&core, ADMIN_EMAIL, "A1"), new_account(&core, ADMIN_EMAIL, "A2"));
    let bob1 = new_account(&core, BOB, "B1");
    db(&core, |c| {
        for (account_id, amount) in [(alice1, d("-50")), (alice2, d("50")), (alice1, d("-70")), (bob1, d("70"))] {
            add_transaction(c, account_id, &format!("{account_id}{amount}"), "VIR", amount, "transfer", None);
        }
        assert_eq!(ledger::pair_transfers(c)?, 1); // 50 between Alice's two accounts; the 70 from Alice to Bob is a real payment
        let internal: std::collections::HashMap<(i64, Decimal), bool> = Transaction::query(c, "", [])?.into_iter().map(|t| ((t.account_id, t.amount), t.internal())).collect();
        let expected: std::collections::HashMap<(i64, Decimal), bool> = [((alice1, d("-50")), true), ((alice2, d("50")), true), ((alice1, d("-70")), false), ((bob1, d("70")), false)].into_iter().collect();
        assert_eq!(internal, expected);
        Ok(())
    });
}

#[test]
fn test_contacts_statistics_only_count_what_the_user_can_see() {
    let core = users_core();
    let (a, b) = (new_account(&core, ADMIN_EMAIL, "A"), new_account(&core, BOB, "B"));
    db(&core, |c| {
        let mut cp = Counterparty::default();
        cp.name = "Garage".into();
        cp.insert(c)?;
        for (account_id, amount) in [(a, d("-10")), (b, d("-25"))] {
            add_transaction(c, account_id, &format!("x{account_id}"), "PRLV", amount, "direct_debit", Some(cp.id));
        }
        Ok(())
    });
    for (who, expected) in [(ADMIN_EMAIL, d("10")), (BOB, d("25")), (CAROL, d("0"))] {
        access::as_user(Some(identity(&core, who)), || {
            db(&core, |c| {
                let totals: Vec<Decimal> = counterparties::summaries(c)?.into_iter().filter(|s| s.contact.name == "Garage").map(|s| s.total_out).collect();
                assert_eq!(totals, vec![expected], "{who}");
                Ok(())
            });
        });
    }
}

#[test]
fn test_removing_a_user_hands_their_accounts_to_the_administrator() {
    let core = users_core();
    let a = new_account(&core, BOB, "Compte Bob");
    db(&core, |c| {
        user_service::share(c, &Account::get(c, a)?.unwrap(), &user_in(c, CAROL), false)?;
        Ok(())
    });
    let (alice, bob) = (user(&core, ADMIN_EMAIL), user(&core, BOB));
    db(&core, |c| {
        assert_eq!(user_service::remove(c, &core.vault, &bob, &alice)?, 1);
        Ok(())
    });
    db(&core, |c| {
        let carol = User::query_one(c, "WHERE email = ?", [CAROL])?.unwrap();
        assert_eq!(Account::get(c, a)?.unwrap().owner_id, Some(alice.id));
        assert_eq!(user_service::shares_of(c, a)?.iter().map(|sh| sh.user_id).collect::<Vec<_>>(), vec![carol.id]); // Carol keeps her share
        assert!(User::query_one(c, "WHERE email = ?", [BOB])?.is_none());
        Ok(())
    });
    core.reset_throttle();
    assert!(matches!(core.authenticate(BOB, BOB_PASSWORD), Err(AppError::InvalidPassword))); // their key slot is gone too
}

#[test]
fn test_the_last_administrator_cannot_be_removed_nor_oneself() {
    let core = users_core();
    let (alice, bob) = (user(&core, ADMIN_EMAIL), user(&core, BOB));
    db(&core, |c| {
        value(user_service::remove(c, &core.vault, &alice, &alice), "soi-même");
        value(user_service::remove(c, &core.vault, &alice, &bob), "au moins un administrateur");
        Ok(())
    });
}

#[test]
fn test_adding_a_user_checks_the_password_and_the_email_before_writing_anything() {
    let core = users_core();
    db(&core, |c| {
        value(user_service::add_with_password(c, &core.vault, "dave@example.com", Some("Dave"), "short", false), "mot de passe");
        value(user_service::add_with_password(c, &core.vault, &BOB.to_uppercase(), Some("Bob again"), BOB_PASSWORD, false), "déjà");
        Ok(())
    });
    assert_eq!(core.vault.keystore.slots().len(), 3);
    assert!(core.has_users().unwrap());
}

// --------------------------------------------------------------------- vault from before users existed
#[test]
fn test_upgrading_a_single_password_vault_gives_the_data_to_the_first_user() {
    let dir = Dir::new();
    let settings = crate::testkit::env::settings(&dir);
    let old = MyMoney::new(settings.clone()).unwrap();
    old.vault.setup(PASSWORD, None).unwrap(); // the old layout: one master password, no user
    let account_id = old.save_account(None, &form(&[("name", "Ancien compte"), ("account_type", "checking"), ("balance", "42")])).unwrap();
    let data = boursobank_statement_pdf(&Spec::new(sample_entries()));
    let (doc_id, _) = old.store_document("ancien.pdf", &data).unwrap();
    db(&old, |c| {
        assert_eq!(Account::get(c, account_id)?.unwrap().owner_id, None);
        assert_eq!(Document::get(c, doc_id)?.unwrap().owner_id, None);
        Ok(())
    });
    old.vault.lock();

    let app = MyMoney::new(settings).unwrap();
    assert!(app.legacy_login() && !app.vault.unlocked());
    app.check_password(PASSWORD, None).unwrap(); // no e-mail yet: the master password opens it
    assert!(!app.has_users().unwrap());
    let me = app.create_first_user(" Admin@Example.com ", Some("Alice")).unwrap();
    assert!(me.is_admin && me.email == ADMIN_EMAIL && !app.legacy_login());
    access::as_user(Some(me.clone()), || {
        db(&app, |c| {
            assert_eq!(Account::get(c, account_id)?.unwrap().owner_id, Some(me.id));
            assert_eq!(Document::get(c, doc_id)?.unwrap().owner_id, Some(me.id));
            assert_eq!(access::readable_ids(c)?, Some([account_id].into_iter().collect::<BTreeSet<_>>()));
            Ok(())
        });
    });
    value(app.create_first_user("other@example.com", None), "déjà");
    app.vault.lock();
    assert!(app.authenticate(ADMIN_EMAIL, PASSWORD).unwrap().is_admin); // the same password, now through the user's slot
}

// ------------------------------------------------------------------------------------------------- web
fn sign_up(client: &mut Client) -> String {
    let token = client.core.setup_token.clone();
    client.post("/setup", &[("setup_token", &token), ("email", ADMIN_EMAIL), ("name", "Alice"), ("password", PASSWORD), ("confirm", PASSWORD)]);
    csrf_of(&client.get("/settings").text())
}

fn add_user(client: &mut Client, csrf: &str, email: &str, name: &str, password: &str, admin: bool) -> crate::testkit::env::Page {
    let mut fields = vec![("csrf", csrf), ("email", email), ("name", name), ("password", password)];
    if admin {
        fields.push(("is_admin", "on"));
    }
    client.post("/settings/users", &fields)
}

fn second_browser(web: &Client, email: &str, password: &str) -> Client {
    let mut other = web.another();
    let response = other.post_raw("/login", &[("email", email), ("password", password)]);
    assert_eq!(response.status, 303, "{}", response.text());
    other
}

fn create_account(client: &mut Client, csrf: &str, name: &str, balance: &str) -> i64 {
    let done = client.post("/accounts/new", &[("csrf", csrf), ("name", name), ("account_type", "checking"), ("balance", balance)]);
    Regex::new(r"/accounts/(\d+)").unwrap().captures(&done.url).unwrap()[1].parse().unwrap()
}

fn user_id(client: &Client, email: &str) -> i64 {
    db(&client.core, |c| Ok(User::query_one(c, "WHERE email = ?", [email])?.unwrap().id))
}

#[test]
fn test_web_each_user_logs_in_with_email_and_password_and_sees_only_their_accounts() {
    let mut web = client();
    let csrf = sign_up(&mut web);
    assert!(!web.get("/").text().contains("Compte administrateur"));
    let created = add_user(&mut web, &csrf, BOB, "Bob", BOB_PASSWORD, false);
    assert!(created.text().contains("Bob peut maintenant se connecter"));
    let a = create_account(&mut web, &csrf, "Compte Alice", "1000");
    let mut bob = second_browser(&web, BOB, BOB_PASSWORD);
    let bob_csrf = csrf_of(&bob.get("/settings").text());
    let b = create_account(&mut bob, &bob_csrf, "Compte Bob", "1000");
    assert!(web.get("/").text().contains("Compte Alice") && !web.get("/").text().contains("Compte Bob"));
    assert!(bob.get("/").text().contains("Compte Bob") && !bob.get("/").text().contains("Compte Alice"));
    // What Bob cannot see does not exist for him.
    for url in [format!("/accounts/{a}"), format!("/accounts/{a}/edit")] {
        assert_eq!(bob.get(&url).status, 404, "{url}");
    }
    assert_eq!(bob.post(&format!("/accounts/{a}/delete"), &[("csrf", &bob_csrf)]).status, 404);
    assert_eq!(web.get(&format!("/accounts/{b}")).status, 404);
    assert_eq!(bob.post("/login", &[("email", BOB), ("password", "wrong")]).status, 401);
    assert_eq!(bob.post("/login", &[("email", "ghost@example.com"), ("password", BOB_PASSWORD)]).status, 401);
}

#[test]
fn test_web_sharing_read_only_then_edit_then_stop() {
    let mut web = client();
    let csrf = sign_up(&mut web);
    add_user(&mut web, &csrf, BOB, "Bob", BOB_PASSWORD, false);
    let a = create_account(&mut web, &csrf, "Compte Alice", "2500");
    let mut bob = second_browser(&web, BOB, BOB_PASSWORD);
    let bob_csrf = csrf_of(&bob.get("/settings").text());
    let bob_id = user_id(&web, BOB).to_string();
    let page = web.get(&format!("/accounts/{a}")).text();
    assert!(page.contains("Partager avec") && page.contains("Bob"));

    let shared = web.post(&format!("/accounts/{a}/shares"), &[("csrf", &csrf), ("user_id", &bob_id), ("mode", "read")]);
    assert!(shared.text().contains("Compte partagé avec Bob") && shared.text().contains("lecture seule"));
    let view = bob.get(&format!("/accounts/{a}"));
    let text = view.text();
    assert!(view.status == 200 && text.contains("partagé en lecture seule") && text.contains("Quitter ce partage"));
    assert!(!text.contains(&format!("href=\"/accounts/{a}/edit\"")) && !text.contains("Partager avec"));
    let home = bob.get("/").text();
    assert!(home.contains("Compte Alice") && home.contains("partagé par"));
    let edit = bob.get(&format!("/accounts/{a}/edit"));
    assert!(edit.status == 404 || edit.text().contains("lecture seule"));
    let refused = bob.post(&format!("/accounts/{a}/edit"), &[("csrf", &bob_csrf), ("name", "Piraté")]);
    assert!(refused.text().contains("lecture seule") || refused.status == 404);
    assert!(!web.get(&format!("/accounts/{a}")).text().contains("Piraté"));
    assert_eq!(bob.post(&format!("/accounts/{a}/shares"), &[("csrf", &bob_csrf), ("user_id", &bob_id), ("mode", "edit")]).status, 404); // he cannot widen his own share

    web.post(&format!("/accounts/{a}/shares"), &[("csrf", &csrf), ("user_id", &bob_id), ("mode", "edit")]);
    let view = bob.get(&format!("/accounts/{a}")).text();
    assert!(view.contains("partagé en modification") && view.contains(&format!("href=\"/accounts/{a}/edit\"")));
    let done = bob.post(&format!("/accounts/{a}/edit"), &[("csrf", &bob_csrf), ("name", "Compte commun")]);
    assert!(done.text().contains("Compte mis à jour") && web.get("/").text().contains("Compte commun"));
    assert_eq!(bob.post(&format!("/accounts/{a}/delete"), &[("csrf", &bob_csrf)]).status, 404);

    web.post(&format!("/accounts/{a}/shares/{bob_id}/delete"), &[("csrf", &csrf)]);
    assert_eq!(bob.get(&format!("/accounts/{a}")).status, 404);
    assert!(!bob.get("/").text().contains("Compte commun"));
}

#[test]
fn test_web_a_shared_account_can_be_left_by_the_person_it_is_shared_with() {
    let mut web = client();
    let csrf = sign_up(&mut web);
    add_user(&mut web, &csrf, BOB, "Bob", BOB_PASSWORD, false);
    let a = create_account(&mut web, &csrf, "Compte Alice", "1000");
    let bob_id = user_id(&web, BOB).to_string();
    web.post(&format!("/accounts/{a}/shares"), &[("csrf", &csrf), ("user_id", &bob_id), ("mode", "read")]);
    let mut bob = second_browser(&web, BOB, BOB_PASSWORD);
    let bob_csrf = csrf_of(&bob.get("/settings").text());
    let left = bob.post(&format!("/accounts/{a}/shares/{bob_id}/delete"), &[("csrf", &bob_csrf)]);
    assert!(left.text().contains("plus accès à ce compte"));
    assert_eq!(bob.get(&format!("/accounts/{a}")).status, 404);
}

fn upload(client: &mut Client, csrf: &str, name: &str, data: &[u8]) -> crate::testkit::env::Page {
    client.post_files("/documents", &[("csrf", csrf)], &[Upload { field: "files", filename: name, data, content_type: "application/pdf" }])
}

#[test]
fn test_web_documents_are_private_to_their_owner_and_to_those_the_account_is_shared_with() {
    let mut web = client();
    let csrf = sign_up(&mut web);
    add_user(&mut web, &csrf, BOB, "Bob", BOB_PASSWORD, false);
    let mut bob = second_browser(&web, BOB, BOB_PASSWORD);
    let bob_csrf = csrf_of(&bob.get("/settings").text());
    let data = boursobank_statement_pdf(&Spec::new(sample_entries()));
    let up = upload(&mut web, &csrf, "secret.pdf", &data);
    let doc: i64 = Regex::new(r"/documents/(\d+)").unwrap().captures(&up.url).unwrap()[1].parse().unwrap();
    let imported = web.post(&format!("/documents/{doc}/import/0"), &[("csrf", &csrf), ("account_id", "new"), ("name", "Courant"), ("import_transactions", "on")]);
    let account: i64 = Regex::new(r"/accounts/(\d+)").unwrap().captures(&imported.url).unwrap()[1].parse().unwrap();
    assert!(web.get("/documents").text().contains("secret.pdf") && !bob.get("/documents").text().contains("secret.pdf"));
    assert_eq!(bob.get(&format!("/documents/{doc}")).status, 404);
    assert_eq!(bob.get(&format!("/documents/{doc}/download")).status, 404);
    assert_eq!(bob.post(&format!("/documents/{doc}/delete"), &[("csrf", &bob_csrf)]).status, 404);
    // The same file dropped by Bob is not handed over to him.
    let again = upload(&mut bob, &bob_csrf, "copy.pdf", &data);
    assert!(again.text().contains("déjà dans le coffre") && !again.url.contains(&format!("/documents/{doc}")));
    web.post(&format!("/accounts/{account}/shares"), &[("csrf", &csrf), ("user_id", &user_id(&web, BOB).to_string()), ("mode", "read")]);
    assert_eq!(bob.get(&format!("/documents/{doc}")).status, 200);
    assert_eq!(bob.get(&format!("/documents/{doc}/download")).status, 200);
    assert_eq!(bob.post(&format!("/documents/{doc}/delete"), &[("csrf", &bob_csrf)]).status, 404); // read only
}

#[test]
fn test_web_settings_are_split_between_everyone_and_administrators() {
    let mut web = client();
    let csrf = sign_up(&mut web);
    add_user(&mut web, &csrf, BOB, "Bob", BOB_PASSWORD, false);
    let mut bob = second_browser(&web, BOB, BOB_PASSWORD);
    let bob_csrf = csrf_of(&bob.get("/settings").text());
    let page = bob.get("/settings").text();
    assert!(page.contains("Mon mot de passe") && !page.contains("Ajouter une personne") && !page.contains("Code de secours") && !page.contains("Clé API CoinGecko"));
    let admin_page = web.get("/settings").text();
    assert!(admin_page.contains("Ajouter une personne") && admin_page.contains("Code de secours") && admin_page.contains("Cloudflare Access"));
    let alice_id = user_id(&web, ADMIN_EMAIL);
    let attempts: Vec<(String, Vec<(&str, &str)>)> = vec![
        ("/settings/users".into(), vec![("email", CAROL), ("password", CAROL_PASSWORD)]),
        (format!("/settings/users/{alice_id}/delete"), vec![]),
        ("/settings/recovery".into(), vec![("current", BOB_PASSWORD)]),
        ("/settings/general".into(), vec![("coingecko_api_key", "x")]),
        (format!("/settings/users/{alice_id}/password"), vec![("password", "Hacked-Password-123!")]),
    ];
    for (url, extra) in attempts {
        let mut fields = vec![("csrf", bob_csrf.as_str())];
        fields.extend(extra);
        assert_eq!(bob.post(&url, &fields).status, 403, "{url}");
    }
    // Bob's "lock" only signs him out: the vault stays open for the others.
    bob.post("/logout", &[("csrf", &bob_csrf), ("lock", "1")]);
    assert!(web.core.vault.unlocked() && web.get("/").status == 200);
    // Alice's lock closes it for everyone.
    web.post("/logout", &[("csrf", &csrf), ("lock", "1")]);
    assert!(!web.core.vault.unlocked());
}

#[test]
fn test_web_changing_your_password_only_changes_yours_and_an_admin_can_reset_another() {
    let mut web = client();
    let csrf = sign_up(&mut web);
    add_user(&mut web, &csrf, BOB, "Bob", BOB_PASSWORD, false);
    let mut bob = second_browser(&web, BOB, BOB_PASSWORD);
    let bob_csrf = csrf_of(&bob.get("/settings").text());
    let new = "Bob-Brand-New-Pass-9!";
    assert!(bob.post("/settings/password", &[("csrf", &bob_csrf), ("current", BOB_PASSWORD), ("new", new), ("confirm", new)]).text().contains("Mot de passe changé"));
    assert!(bob.post("/settings/password", &[("csrf", &bob_csrf), ("current", "wrong"), ("new", new), ("confirm", new)]).text().contains("incorrect"));
    web.core.reset_throttle();
    assert_eq!(web.another().post("/login", &[("email", BOB), ("password", BOB_PASSWORD)]).status, 401);
    web.core.reset_throttle();
    assert_eq!(web.another().post_raw("/login", &[("email", BOB), ("password", new)]).status, 303);
    assert_eq!(web.another().post_raw("/login", &[("email", ADMIN_EMAIL), ("password", PASSWORD)]).status, 303); // Alice unaffected
    // Bob forgot it: the administrator sets another one.
    let reset = "Reset-By-Admin-Pass-4!";
    let bob_id = user_id(&web, BOB);
    assert!(web.post(&format!("/settings/users/{bob_id}/password"), &[("csrf", &csrf), ("password", reset)]).text().contains("Nouveau mot de passe défini"));
    assert_eq!(web.another().post_raw("/login", &[("email", BOB), ("password", reset)]).status, 303);
}

#[test]
fn test_web_admin_manages_users() {
    let mut web = client();
    let csrf = sign_up(&mut web);
    add_user(&mut web, &csrf, BOB, "Bob", BOB_PASSWORD, false);
    assert!(add_user(&mut web, &csrf, BOB, "Bob 2", BOB_PASSWORD, false).text().contains("a déjà un utilisateur"));
    assert!(add_user(&mut web, &csrf, "nope", "X", BOB_PASSWORD, false).text().contains("Adresse e-mail invalide"));
    assert!(add_user(&mut web, &csrf, CAROL, "Carol", "short", false).text().to_lowercase().contains("mot de passe"));
    let bob_id = user_id(&web, BOB);
    assert!(web.post(&format!("/settings/users/{bob_id}/role"), &[("csrf", &csrf), ("is_admin", "on")]).text().contains("Rôle modifié"));
    let mut bob = second_browser(&web, BOB, BOB_PASSWORD);
    assert!(bob.get("/settings").text().contains("Ajouter une personne")); // his open session knows at once
    web.post(&format!("/settings/users/{bob_id}/role"), &[("csrf", &csrf)]); // Bob is a plain user again
    assert!(web.post(&format!("/settings/users/{}/role", user_id(&web, ADMIN_EMAIL)), &[("csrf", &csrf)]).text().contains("au moins un administrateur"));
    // Bob owns an account, then is deleted: it goes to Alice, and his open session is closed.
    let bob_csrf = csrf_of(&bob.get("/settings").text());
    create_account(&mut bob, &bob_csrf, "Compte Bob", "1000");
    let gone = web.post(&format!("/settings/users/{bob_id}/delete"), &[("csrf", &csrf)]);
    assert!(gone.text().contains("Bob supprimé") && gone.text().contains("1 compte(s) sont maintenant à toi"));
    assert!(web.get("/").text().contains("Compte Bob"));
    assert_eq!(bob.get_raw("/").header("location"), Some("/login"));
}

#[test]
fn test_web_a_vault_from_before_users_asks_for_its_first_user_after_login() {
    let dir = Dir::new();
    let settings = crate::testkit::env::settings(&dir);
    let old = MyMoney::new(settings).unwrap();
    old.vault.setup(PASSWORD, None).unwrap();
    old.save_account(None, &form(&[("name", "Ancien compte"), ("account_type", "checking"), ("balance", "42")])).unwrap();
    old.vault.lock();
    drop(old);
    let mut client = crate::testkit::env::client_with(dir, |s| s);
    let login_page = client.get("/login").text();
    assert!(!login_page.contains("name=\"email\"") && login_page.contains("Mot de passe maître"));
    let done = client.post_raw("/login", &[("password", PASSWORD)]);
    assert!(done.status == 303 && done.header("location") == Some("/first-user"));
    assert_eq!(client.get_raw("/").header("location"), Some("/first-user")); // nothing else opens before that
    let page = client.get("/first-user");
    assert!(page.text().contains("multi-utilisateurs"));
    let bad = client.post("/first-user", &[("csrf", &csrf_of(&page.text())), ("email", "nope"), ("name", "X")]);
    assert!(bad.status == 400 && bad.text().contains("e-mail invalide"));
    let ok = client.post("/first-user", &[("csrf", &csrf_of(&page.text())), ("email", ADMIN_EMAIL), ("name", "Alice")]);
    assert!(ok.text().contains("Compte administrateur créé") && ok.text().contains("Ancien compte"));
    assert_eq!(client.get_raw("/first-user").header("location"), Some("/"));
    assert!(client.another().get("/login").text().contains("name=\"email\"")); // from now on the login asks for the e-mail
}

#[test]
fn test_a_database_from_before_users_gets_its_new_columns() {
    // An existing NAS database has no owner columns: they are added when it opens, and the rows stay.
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE accounts (id INTEGER PRIMARY KEY, name VARCHAR(200), balance TEXT);
         CREATE TABLE documents (id INTEGER PRIMARY KEY, filename VARCHAR(300));
         INSERT INTO accounts (id, name, balance) VALUES (1, 'Ancien compte', '42');",
    )
    .unwrap();
    crate::db::create_all(&conn).unwrap(); // creates the new tables (users, account_shares), leaves the old ones alone
    crate::db::add_missing_columns(&conn).unwrap();
    let columns = |table: &str| -> BTreeSet<String> {
        let mut st = conn.prepare(&format!("PRAGMA table_info(\"{table}\")")).unwrap();
        st.query_map([], |r| r.get::<_, String>(1)).unwrap().map(|c| c.unwrap()).collect()
    };
    assert!(columns("accounts").contains("owner_id") && columns("documents").contains("owner_id"));
    assert!(!columns("users").is_empty() && !columns("account_shares").is_empty());
    let rows: Vec<(String, Option<i64>)> = conn.prepare("SELECT name, owner_id FROM accounts").unwrap().query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(|r| r.unwrap()).collect();
    assert_eq!(rows, vec![("Ancien compte".to_string(), None)]);
}
