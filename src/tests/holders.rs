//! The people behind an account: civility, last names, birth names, first names; one block per person.

use serde_json::json;

use crate::models::Account;
use crate::services::holders::{self, Person};
use crate::testkit::env::{client, core, Client, Upload};
use crate::testkit::fixtures::*;
use crate::testkit::{db, form, num};

fn person(civility: &str, last: &[&str], birth: &[&str], first: &[&str]) -> Person {
    let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    Person { civility: civility.into(), last_names: v(last), birth_names: v(birth), first_names: v(first) }
}

#[test]
fn a_printed_name_is_read_as_people() {
    assert_eq!(holders::parse_printed("M DUPONT JEAN"), vec![person("M", &["DUPONT"], &[], &["JEAN"])]);
    assert_eq!(holders::parse_printed("MME LEROY CLAIRE"), vec![person("MME", &["LEROY"], &[], &["CLAIRE"])]);
    // a joint account: both people, even when the bank cut the line
    assert_eq!(holders::parse_printed("M DUPONT OU MME LEROY CLAIRE"), vec![person("M", &["DUPONT"], &[], &[]), person("MME", &["LEROY"], &[], &["CLAIRE"])]);
    assert_eq!(holders::parse_printed("M MARTIN PAUL ET MME DUPUIS-LEBLANC ANNE"), vec![person("M", &["MARTIN"], &[], &["PAUL"]), person("MME", &["DUPUIS-LEBLANC"], &[], &["ANNE"])]);
    // first name first, in mixed case
    assert_eq!(holders::parse_printed("Jean DUPONT"), vec![person("", &["DUPONT"], &[], &["Jean"])]);
    // several words in the last name
    assert_eq!(holders::parse_printed("M DE LA FONTAINE JEAN"), vec![person("M", &["DE LA FONTAINE"], &[], &["JEAN"])]);
    assert!(holders::parse_printed("   ").is_empty());
}

#[test]
fn the_single_line_is_composed_from_the_people() {
    let people = vec![person("M", &["DUPONT"], &["MARTIN"], &["JEAN", "PAUL"]), person("MME", &["LEROY", "DUPONT"], &[], &["CLAIRE"])];
    // birth names are not part of the line: they are only kept for the forms that ask for them
    assert_eq!(holders::compose(&people), "M DUPONT JEAN PAUL OU MME LEROY DUPONT CLAIRE");
    assert_eq!(holders::compose(&[]), "");
    assert_eq!(holders::from_json(&holders::to_json(&people)), people);
}

#[test]
fn a_form_is_read_person_by_person() {
    let none = form(&[("name", "x"), ("holder_name", "M DUPONT JEAN")]);
    assert_eq!(holders::from_form(&none), None); // an older form: no person fields
    let typed = form(&[
        ("holder_0_civility", "m"), ("holder_0_last_0", "  DUPONT "), ("holder_0_last_1", ""), ("holder_0_birth_0", "MARTIN"), ("holder_0_first_0", "JEAN"), ("holder_0_first_2", "PAUL"),
        ("holder_1_civility", "MME"), ("holder_1_last_0", "LEROY"), ("holder_1_first_0", "CLAIRE"),
        ("holder_2_civility", "M"),
    ]);
    assert_eq!(
        holders::from_form(&typed),
        Some(vec![person("M", &["DUPONT"], &["MARTIN"], &["JEAN", "PAUL"]), person("MME", &["LEROY"], &[], &["CLAIRE"])]) // an empty block is no person
    );
    assert_eq!(holders::from_form(&form(&[("holder_0_last_0", "")])), Some(vec![]));
}

#[test]
fn the_form_offers_padded_boxes() {
    let blocks = holders::form_blocks(&[person("M", &["DUPONT"], &[], &["JEAN"])], 2);
    assert_eq!(blocks[0]["last"], json!(["DUPONT", ""]));
    assert_eq!(blocks[0]["birth"], json!(["", ""]));
    assert_eq!(blocks[0]["first"], json!(["JEAN", "", ""]));
    assert_eq!(blocks.as_array().unwrap().len(), 2); // the second person is offered, empty
    assert_eq!(blocks[1]["first"], json!(["", "", ""]));
}

fn upload(client: &mut Client, csrf: &str, name: &str, data: &[u8]) -> crate::testkit::env::Page {
    client.post_files("/documents", &[("csrf", csrf)], &[Upload { field: "files", filename: name, data, content_type: "application/pdf" }])
}

#[test]
fn people_typed_when_importing_become_the_holders() {
    let core = core();
    let joint = Spec { holder: joint_holder(), address: joint_address(), ..Spec::new(sample_entries()[..3].to_vec()) };
    let data = boursobank_statement_pdf(&joint);
    let doc_id = crate::testkit::drop_file(&core, "releve.pdf", &data);
    let plan_asks = core.review_document(doc_id).unwrap().unwrap().plans[0].ask_holder;
    assert!(plan_asks); // the bank cut the line: the names are asked, once
    let result = core
        .import_statement(
            doc_id,
            0,
            &form(&[
                ("account_id", "new"), ("name", "Compte joint"), ("import_transactions", "on"),
                ("holder_0_civility", "M"), ("holder_0_last_0", "DUPONT"), ("holder_0_birth_0", "MARTIN"), ("holder_0_first_0", "JEAN"),
                ("holder_1_civility", "MME"), ("holder_1_last_0", "LEROY"), ("holder_1_first_0", "CLAIRE"), ("holder_1_first_1", "MARIE"),
            ]),
        )
        .unwrap();
    db(&core, |c| {
        let account = Account::get(c, num(&result, "account_id"))?.unwrap();
        assert_eq!(account.holder_name.as_deref(), Some("M DUPONT JEAN OU MME LEROY CLAIRE MARIE"));
        assert_eq!(account.holder_alias.as_deref(), Some("M DUPONT OU MME LEROY CLAIRE")); // what the bank prints, to recognise it
        let people = holders::of_account(&account);
        assert_eq!(people, vec![person("M", &["DUPONT"], &["MARTIN"], &["JEAN"]), person("MME", &["LEROY"], &[], &["CLAIRE", "MARIE"])]);
        Ok(())
    });
    // next month: nothing to ask any more
    let next = Spec { opening: joint.closing(), opening_date: ymd(2026, 9, 30), period: (ymd(2026, 10, 1), ymd(2026, 10, 31)), ..Spec::new(vec![entry(ymd(2026, 10, 2), &["VIR SEPA ACME", "Salaire"], "100.00")]) };
    let joint_next = Spec { holder: joint_holder(), address: joint_address(), ..next };
    let doc2 = crate::testkit::drop_file(&core, "octobre.pdf", &boursobank_statement_pdf(&joint_next));
    assert!(!core.review_document(doc2).unwrap().unwrap().plans[0].ask_holder);
}

#[test]
fn the_account_form_saves_the_people_and_clears_them() {
    let core = core();
    let id = core.save_account(None, &form(&[("name", "Joint"), ("account_type", "checking"), ("holder_0_civility", "M"), ("holder_0_last_0", "DUPONT"), ("holder_0_first_0", "JEAN"), ("holder_1_civility", "MME"), ("holder_1_last_0", "LEROY"), ("holder_1_first_0", "CLAIRE")])).unwrap();
    db(&core, |c| {
        let a = Account::get(c, id)?.unwrap();
        assert_eq!(a.holder_name.as_deref(), Some("M DUPONT JEAN OU MME LEROY CLAIRE"));
        assert_eq!(holders::of_account(&a).len(), 2);
        Ok(())
    });
    // a form of the older kind (one line) still works
    core.save_account(Some(id), &form(&[("name", "Joint"), ("holder_name", "M DUPONT JEAN")])).unwrap();
    db(&core, |c| {
        let a = Account::get(c, id)?.unwrap();
        assert_eq!(a.holder_name.as_deref(), Some("M DUPONT JEAN"));
        assert_eq!(holders::of_account(&a).len(), 2); // the people are still the ones typed before
        Ok(())
    });
    // blank boxes: nobody
    core.save_account(Some(id), &form(&[("name", "Joint"), ("holder_0_last_0", ""), ("holder_0_first_0", "")])).unwrap();
    db(&core, |c| {
        let a = Account::get(c, id)?.unwrap();
        assert_eq!((a.holder_name, a.holders), (None, None));
        Ok(())
    });
}

#[test]
fn the_account_page_lists_each_person_with_copy_buttons() {
    let mut client = client();
    let csrf = client.login();
    let created = client.post(
        "/accounts/new",
        &[
            ("csrf", &csrf), ("name", "Compte joint"), ("account_type", "checking"), ("balance", "10"),
            ("holder_0_civility", "M"), ("holder_0_last_0", "DUPONT"), ("holder_0_birth_0", "MARTIN"), ("holder_0_first_0", "JEAN"), ("holder_0_first_1", "PAUL"),
            ("holder_1_civility", "MME"), ("holder_1_last_0", "LEROY"), ("holder_1_first_0", "CLAIRE"),
        ],
    );
    let page = created.text();
    assert!(page.contains("Titulaire 1") && page.contains("Titulaire 2"));
    assert!(page.contains("(né(e) MARTIN)") && page.contains("data-copy=\"MARTIN\"") && page.contains("data-copy=\"JEAN PAUL\"") && page.contains("data-copy=\"DUPONT\""));
    assert!(page.contains("M DUPONT JEAN PAUL OU MME LEROY CLAIRE"));
    let _ = upload;
}
