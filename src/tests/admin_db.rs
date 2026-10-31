//! The administrator's database tools: roles, reading everyone's rows, editing, emptying, structure update and the
//! synchronisation with the documents.

use super::cleanup::{assert_nothing_left, feed, import_new};
use crate::models::{Account, Document, Holding, Transaction};
use crate::services::admin_db::{self, Scope};
use crate::testkit::env::{client, csrf_of};
use crate::testkit::fixtures::*;
use crate::testkit::fortuneo::*;
use crate::testkit::loan::*;
use crate::testkit::{db, drop_file, form};

const BOB: &str = "bob@example.com";
const BOB_PASSWORD: &str = "Another-Strong-Pass-7";

#[test]
fn test_roles_are_shown_and_changed_and_only_the_admin_reaches_the_base() {
    let mut alice = client();
    let csrf = alice.login();
    alice.post("/settings/users", &[("csrf", &csrf), ("email", BOB), ("name", "Bob"), ("password", BOB_PASSWORD), ("role", "user")]);
    let settings = alice.get("/settings").text();
    assert!(settings.contains(">Admin<") && settings.contains(">Utilisateur<"), "{settings}");
    assert!(settings.contains("<option value=\"admin\"") && settings.contains("Le premier compte créé est administrateur"));
    // the nav shows « Base » to the admin only
    assert!(alice.get("/").text().contains("href=\"/admin/db\""));
    let mut bob = alice.another();
    bob.post("/login", &[("email", BOB), ("password", BOB_PASSWORD)]);
    assert_eq!(bob.get("/admin/db").status, 403);
    assert_eq!(bob.get("/admin/db/table/accounts").status, 403);
    assert!(!bob.get("/").text().contains("href=\"/admin/db\""));
    // the admin makes Bob an admin through the role selector, then takes it back
    let bob_id = db(&alice.core, |c| Ok(crate::services::users::get_by_email(c, Some(BOB))?.unwrap().id));
    alice.post(&format!("/settings/users/{bob_id}/role"), &[("csrf", &csrf), ("role", "admin")]);
    assert!(db(&alice.core, |c| Ok(crate::services::users::get_by_email(c, Some(BOB))?.unwrap().is_admin)));
    alice.post(&format!("/settings/users/{bob_id}/role"), &[("csrf", &csrf), ("role", "user")]);
    assert!(!db(&alice.core, |c| Ok(crate::services::users::get_by_email(c, Some(BOB))?.unwrap().is_admin)));
}

#[test]
fn test_the_admin_reads_edits_and_deletes_rows_of_every_user() {
    let mut alice = client();
    let csrf = alice.login();
    alice.post("/settings/users", &[("csrf", &csrf), ("email", BOB), ("name", "Bob"), ("password", BOB_PASSWORD)]);
    let bob_id = db(&alice.core, |c| Ok(crate::services::users::get_by_email(c, Some(BOB))?.unwrap().id));
    db(&alice.core, |c| {
        Account { name: "Compte de Bob".into(), owner_id: Some(bob_id), balance: dec("12.50"), ..Default::default() }.insert(c)?;
        Ok(())
    });
    let tables = alice.get("/admin/db").text();
    for t in ["accounts", "documents", "transactions", "users", "settings"] {
        assert!(tables.contains(&format!("/admin/db/table/{t}")), "{t}");
    }
    let page = alice.get("/admin/db/table/accounts").text();
    assert!(page.contains("Compte de Bob") && page.contains("12.50"), "{page}");
    assert!(alice.get("/admin/db/table/accounts?q=zzz").text().contains("Aucune ligne"));
    assert!(alice.get("/admin/db/table/accounts?q=Bob").text().contains("Compte de Bob"));
    assert!(alice.get("/admin/db/table/inconnue").text().contains("Table inconnue") || alice.get("/admin/db/table/inconnue").status != 200);
    // the row, edited
    let row = alice.get("/admin/db/table/accounts/row/1").text();
    assert!(row.contains("name=\"v_name\"") && row.contains("Compte de Bob"));
    alice.post("/admin/db/table/accounts/row/1", &[("csrf", &csrf), ("v_name", "Renommé par l'admin"), ("v_balance", "99")]);
    db(&alice.core, |c| {
        let a = Account::get(c, 1)?.unwrap();
        assert_eq!((a.name.as_str(), a.balance), ("Renommé par l'admin", dec("99")));
        Ok(())
    });
    // a value set to NULL
    alice.post("/admin/db/table/accounts/row/1", &[("csrf", &csrf), ("v_name", "Renommé par l'admin"), ("null_institution", "on")]);
    // deleted
    alice.post("/admin/db/table/accounts/row/1/delete", &[("csrf", &csrf)]);
    assert!(db(&alice.core, |c| Ok(Account::get(c, 1)?.is_none())));
    // a request without the token does nothing
    assert_eq!(alice.post("/admin/db/table/settings/empty", &[]).status, 403);
    // the users' table is not emptied from the button
    let kept = alice.post("/admin/db/table/users/empty", &[("csrf", &csrf)]).text();
    assert!(kept.contains("ne se vide pas ici") || db(&alice.core, |c| Ok(crate::services::users::all_users(c)?.len())) == 2);
    assert_eq!(db(&alice.core, |c| Ok(crate::services::users::all_users(c)?.len())), 2);
}

#[test]
fn test_the_sql_console_runs_statements_and_refuses_what_leaves_the_database() {
    let mut alice = client();
    let csrf = alice.login();
    alice.core.save_account(None, &form(&[("name", "Livret"), ("account_type", "livret_a"), ("balance", "10")])).unwrap();
    let ok = alice.post("/admin/db/sql", &[("csrf", &csrf), ("sql", "SELECT id, name FROM accounts")]).text();
    assert!(ok.contains("Livret") && ok.contains("1 ligne(s)"), "{ok}");
    assert!(alice.post("/admin/db/sql", &[("csrf", &csrf), ("sql", "UPDATE accounts SET name = 'Autre'")]).text().contains("1 ligne(s) touchée(s)"));
    for refused in ["ATTACH DATABASE '/tmp/x' AS x", "PRAGMA key = 'x'", "VACUUM", "SELECT load_extension('x')"] {
        let page = alice.post("/admin/db/sql", &[("csrf", &csrf), ("sql", refused)]).text();
        assert!(page.contains("permis") || page.contains("Seuls les PRAGMA"), "{refused}: {page}");
    }
    assert!(alice.post("/admin/db/sql", &[("csrf", &csrf), ("sql", "SELECT nope FROM accounts")]).text().contains("no such column"));
    assert_eq!(db(&alice.core, |c| Ok(Account::get(c, 1)?.unwrap().name)), "Autre");
}

#[test]
fn test_updating_the_database_adds_what_this_version_expects() {
    let mut alice = client();
    let csrf = alice.login();
    let up_to_date = alice.post("/admin/db/migrate", &[("csrf", &csrf)]).text();
    assert!(up_to_date.contains("déjà à la dernière version"), "{up_to_date}");
    // a database from before the column existed
    db(&alice.core, |c| {
        c.execute_batch("ALTER TABLE accounts DROP COLUMN origin; DROP TABLE people;")?;
        Ok(())
    });
    let done = alice.post("/admin/db/migrate", &[("csrf", &csrf)]).text();
    assert!(done.contains("colonne ajoutée : accounts.origin") && done.contains("table créée : people"), "{done}");
    assert!(alice.post("/admin/db/migrate", &[("csrf", &csrf)]).text().contains("déjà à la dernière version"));
}

#[test]
fn test_the_data_of_one_user_or_of_everybody_can_be_emptied() {
    let mut alice = client();
    let csrf = alice.login();
    alice.post("/settings/users", &[("csrf", &csrf), ("email", BOB), ("name", "Bob"), ("password", BOB_PASSWORD)]);
    let (alice_id, bob_id) = db(&alice.core, |c| Ok((crate::services::users::get_by_email(c, Some(crate::testkit::env::ADMIN_EMAIL))?.unwrap().id, crate::services::users::get_by_email(c, Some(BOB))?.unwrap().id)));
    let _ = alice_id;
    db(&alice.core, |c| {
        Account { name: "A".into(), owner_id: Some(alice_id), ..Default::default() }.insert(c)?;
        Account { name: "B".into(), owner_id: Some(bob_id), ..Default::default() }.insert(c)?;
        Ok(())
    });
    let one = alice.post("/admin/db/clear", &[("csrf", &csrf), ("scope", &format!("user:{bob_id}"))]).text();
    assert!(one.contains("Données de l&#39;utilisateur supprimées") || one.contains("Données de l'utilisateur supprimées"), "{}", &one[..one.len().min(3000)]);
    assert_eq!(db(&alice.core, |c| Ok(Account::query(c, "", [])?.iter().map(|a| a.name.clone()).collect::<Vec<_>>())), vec!["A".to_string()]);
    alice.post("/admin/db/clear", &[("csrf", &csrf), ("scope", "all")]);
    assert!(db(&alice.core, |c| Ok(Account::query(c, "", [])?.is_empty())));
    assert_eq!(db(&alice.core, |c| Ok(crate::services::users::all_users(c)?.len())), 2, "the users stay");
    let _ = Scope::Everyone;
}

/// Everything an import can make, then the state an older version left after « invalidate »: documents back to
/// waiting, the data still there.
#[test]
fn test_with_no_validated_document_the_synchronisation_empties_what_imports_made() {
    let core = crate::testkit::env::core();
    let spec = Spec::new(sample_entries());
    import_new(&core, "septembre.pdf", &boursobank_statement_pdf(&spec), &[("name", "Compte courant"), ("holder_0_civility", "M"), ("holder_0_last_0", "MARTIN"), ("holder_0_first_0", "PAUL")]);
    let (_, fortuneo) = import_new(&core, "fortuneo.pdf", &fortuneo_statement_pdf(&FSpec::new(fortuneo_entries())), &[("name", "Fortuneo")]);
    feed(&core, "frais.pdf", &fortuneo_fees_pdf(&FeesSpec::new()), Some(fortuneo));
    let loan = LoanSpec::new();
    let (_, loan_account) = feed(&core, "offre.pdf", &offer_pdf(&loan), None);
    feed(&core, "tableau.pdf", &schedule_pdf(&loan, None), Some(loan_account));
    import_new(&core, "annuel.pdf", &fortuneo_life_annual_pdf(&LifeAnnualSpec::new(2025)), &[("name", "Assurance-vie")]);
    feed(&core, "arbitrage.pdf", &fortuneo_arbitrage_v2_plain_pdf("100,00", "40,00"), None);
    core.portfolio(false).unwrap();
    // the state the user has: no validated document, the data still in the base, accounts from before the origin mark
    db(&core, |c| {
        c.execute_batch("UPDATE documents SET status = 'parsed', account_id = NULL; UPDATE accounts SET origin = NULL;")?;
        Ok(())
    });
    assert!(db(&core, |c| Ok(Transaction::query(c, "", [])?.len())) > 0);
    let report = core.admin_sync().unwrap();
    assert!(report.iter().any(|(l, n)| l.contains("opérations") && *n > 0), "{report:?}");
    let left = super::cleanup::counts(&core).into_iter().filter(|(_, n)| *n != 0).collect::<Vec<_>>();
    assert!(left.is_empty(), "the base should be empty: {left:?}");
}

#[test]
fn test_the_synchronisation_keeps_what_validated_documents_say_and_imports_missing_data_again() {
    let core = crate::testkit::env::core();
    let spec = Spec::new(sample_entries());
    let (kept_doc, kept_account) = import_new(&core, "septembre.pdf", &boursobank_statement_pdf(&spec), &[("name", "Compte courant")]);
    let (gone_doc, gone_account) = import_new(&core, "fortuneo.pdf", &fortuneo_statement_pdf(&FSpec::new(fortuneo_entries())), &[("name", "Fortuneo")]);
    let kept = db(&core, |c| Ok(Transaction::query(c, "WHERE account_id = ?", [kept_account])?.len()));
    // one validated document lost its data; another stopped being validated
    db(&core, |c| {
        c.execute("DELETE FROM transactions WHERE document_id = ?", [kept_doc])?;
        c.execute("DELETE FROM statements WHERE document_id = ?", [kept_doc])?;
        c.execute("UPDATE documents SET status = 'parsed' WHERE id = ?", [gone_doc])?;
        Ok(())
    });
    let report = core.admin_sync().unwrap();
    assert!(report.iter().any(|(l, _)| l.contains("à importer de nouveau")), "{report:?}");
    db(&core, |c| {
        assert_eq!(Transaction::query(c, "WHERE account_id = ?", [kept_account])?.len(), kept, "the data of the validated document came back");
        assert_eq!(Document::get(c, kept_doc)?.unwrap().status, "imported");
        assert!(Account::get(c, gone_account)?.is_none() || Transaction::query(c, "WHERE account_id = ?", [gone_account])?.is_empty(), "what the other document brought is gone");
        Ok(())
    });
}

#[test]
fn test_the_synchronisation_never_touches_what_was_typed_by_hand() {
    let core = crate::testkit::env::core();
    let manual = core.save_account(None, &form(&[("name", "Livret à la main"), ("account_type", "livret_a"), ("balance", "500")])).unwrap();
    core.add_holding(manual, &form(&[("symbol", "BTC"), ("quantity", "1")]).into_iter().collect()).unwrap();
    db(&core, |c| {
        let mut t = Transaction { account_id: manual, fingerprint: "m".into(), date: ymd(2026, 9, 1), label: "Saisie".into(), amount: dec("5"), source: "manual".into(), ..Default::default() };
        t.insert(c)?;
        Ok(())
    });
    core.admin_sync().unwrap();
    db(&core, |c| {
        assert!(Account::get(c, manual)?.is_some());
        assert_eq!(Transaction::query(c, "WHERE account_id = ?", [manual])?.len(), 1);
        assert_eq!(Holding::query(c, "WHERE account_id = ?", [manual])?.len(), 1);
        Ok(())
    });
    let _ = (drop_file, csrf_of, admin_db::PER_PAGE, assert_nothing_left);
}
