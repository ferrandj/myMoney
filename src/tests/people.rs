//! People's cards: typed once, linked to several (joint) accounts, followed by their accounts and files.

use crate::models::{Account, Document, PersonCard};
use crate::services::{holders, people};
use crate::testkit::env::{client, core, Upload};
use crate::testkit::fixtures::*;
use crate::testkit::{db, drop_file, form, num};

fn joint(core: &crate::app::MyMoney, name: &str, extra: &[(&str, &str)]) -> i64 {
    let mut fields = vec![
        ("name", name), ("account_type", "checking"),
        ("holder_0_civility", "M"), ("holder_0_last_0", "DUPONT"), ("holder_0_first_0", "JEAN"),
        ("holder_1_civility", "MME"), ("holder_1_last_0", "LEROY"), ("holder_1_first_0", "CLAIRE"),
    ];
    fields.extend_from_slice(extra);
    core.save_account(None, &form(&fields)).unwrap()
}

#[test]
fn typing_the_holders_of_two_joint_accounts_makes_two_cards_not_four() {
    let core = core();
    let first = joint(&core, "Compte joint", &[]);
    let second = joint(&core, "Livret commun", &[]);
    db(&core, |c| {
        let cards = PersonCard::query(c, "ORDER BY id", [])?;
        assert_eq!(cards.len(), 2, "the same names reuse the same cards");
        for id in [first, second] {
            let ids: Vec<Option<i64>> = holders::of_account(&Account::get(c, id)?.unwrap()).iter().map(|p| p.id).collect();
            assert_eq!(ids, vec![Some(cards[0].id), Some(cards[1].id)]);
        }
        assert_eq!(people::accounts_of(c, cards[0].id)?.len(), 2);
        Ok(())
    });
}

#[test]
fn a_card_picked_by_its_number_brings_its_names() {
    let core = core();
    joint(&core, "Compte joint", &[]);
    let id = core.save_account(None, &form(&[("name", "Compte perso"), ("account_type", "checking"), ("holder_0_person", "1"), ("holder_0_last_0", "ignoré")])).unwrap();
    db(&core, |c| {
        let a = Account::get(c, id)?.unwrap();
        assert_eq!(a.holder_name.as_deref(), Some("M DUPONT JEAN"), "the card's names win over what the boxes say");
        assert_eq!(people::accounts_of(c, 1)?.len(), 2);
        Ok(())
    });
}

#[test]
fn changing_a_card_updates_its_accounts_and_renames_their_files() {
    let core = core();
    let spec = Spec { holder: vec!["M DUPONT JEAN".into()], ..Spec::new(sample_entries()[..3].to_vec()) };
    let doc_id = drop_file(&core, "releve.pdf", &boursobank_statement_pdf(&spec));
    let result = core.import_statement(doc_id, 0, &form(&[("account_id", "new"), ("name", "Compte"), ("import_transactions", "on"), ("holder_0_civility", "M"), ("holder_0_last_0", "DUPONT"), ("holder_0_first_0", "JEAN")])).unwrap();
    let account_id = num(&result, "account_id");
    db(&core, |c| {
        assert!(Document::get(c, doc_id)?.unwrap().filename.contains("_DUPONT_JEAN_"));
        let mut card = PersonCard::get(c, 1)?.unwrap();
        let before = card.clone();
        people::apply(&mut card, &holders::Person { id: Some(1), civility: "M".into(), last_names: vec!["DURAND".into()], birth_names: vec![], first_names: vec!["JEAN".into(), "PAUL".into()] });
        card.save(c, &before)?;
        people::sync(c, 1)?;
        let a = Account::get(c, account_id)?.unwrap();
        assert_eq!(a.holder_name.as_deref(), Some("M DURAND JEAN PAUL"));
        assert!(Document::get(c, doc_id)?.unwrap().filename.contains("_DURAND_JEAN_"), "{}", Document::get(c, doc_id)?.unwrap().filename);
        Ok(())
    });
}

#[test]
fn deleting_a_card_keeps_the_names_on_the_accounts_without_the_link() {
    let core = core();
    let id = joint(&core, "Compte joint", &[]);
    db(&core, |c| {
        people::delete(c, 1)?;
        assert!(PersonCard::get(c, 1)?.is_none());
        let a = Account::get(c, id)?.unwrap();
        assert_eq!(a.holder_name.as_deref(), Some("M DUPONT JEAN OU MME LEROY CLAIRE"));
        assert_eq!(holders::of_account(&a).iter().map(|p| p.id).collect::<Vec<_>>(), vec![None, Some(2)]);
        Ok(())
    });
}

#[test]
fn a_printed_name_that_is_a_known_card_is_offered_with_it() {
    let cards = vec![PersonCard { id: 7, civility: "M".into(), last_names: Some(serde_json::json!(["DUPONT"])), first_names: Some(serde_json::json!(["JEAN", "PAUL"])), ..Default::default() }];
    let guessed = holders::parse_printed("M DUPONT JEAN OU MME LEROY CLAIRE");
    let matched = people::match_guess(&cards, &guessed);
    assert_eq!(matched[0].id, Some(7));
    assert_eq!(matched[0].first_names, vec!["JEAN", "PAUL"], "the card's full names replace the guess");
    assert_eq!(matched[1].id, None);
}

#[test]
fn test_web_the_people_pages_list_cards_with_their_accounts_and_edit_them() {
    let mut client = client();
    let csrf = client.login();
    let id = joint(&client.core, "Compte joint", &[("balance", "250")]);
    joint(&client.core, "Livret commun", &[("balance", "40")]);
    let page = client.get("/people").text();
    assert!(page.contains("M DUPONT JEAN") && page.contains("MME LEROY CLAIRE") && page.contains("Compte joint") && page.contains("Livret commun") && page.contains("joint"), "{page}");
    assert!(page.replace('\u{202f}', " ").replace('\u{a0}', " ").contains("290,00 €") && page.contains("/analytics?periode=tout&c="));
    assert!(client.get("/accounts").text().contains("href=\"/people/1\""), "the accounts page links to the cards");

    let detail = client.get("/people/1");
    assert!(detail.status == 200 && detail.text().contains("Comptes de cette personne") && detail.text().contains("MME LEROY CLAIRE"));
    assert_eq!(client.get("/people/999").status, 404);

    let done = client.post("/people/1", &[("csrf", &csrf), ("holder_0_civility", "M"), ("holder_0_last_0", "DURAND"), ("holder_0_first_0", "JEAN"), ("notes", "né à Lyon")]);
    assert!(done.text().contains("Fiche mise à jour") && done.text().contains("M DURAND JEAN"));
    db(&client.core, |c| {
        assert_eq!(Account::get(c, id)?.unwrap().holder_name.as_deref(), Some("M DURAND JEAN OU MME LEROY CLAIRE"));
        assert_eq!(PersonCard::get(c, 1)?.unwrap().notes.as_deref(), Some("né à Lyon"));
        Ok(())
    });
    assert!(client.post("/people/1", &[("csrf", &csrf), ("notes", "x")]).text().contains("au moins un nom"));

    let created = client.post("/people/new", &[("csrf", &csrf), ("holder_0_civility", "MME"), ("holder_0_last_0", "MARTIN"), ("holder_0_birth_0", "BERNARD"), ("holder_0_first_0", "ANNE")]);
    assert!(created.text().contains("Personne enregistrée") && created.text().contains("MME MARTIN ANNE") && created.text().contains("seule") == false);
    let deleted = client.post("/people/3/delete", &[("csrf", &csrf)]);
    assert!(deleted.text().contains("Fiche supprimée"));
    assert_eq!(client.get("/people/3").status, 404);
    assert_eq!(client.post("/people/1/delete", &[]).status, 403); // CSRF
}

#[test]
fn test_web_the_forms_offer_the_registered_people() {
    let mut client = client();
    client.login();
    joint(&client.core, "Compte joint", &[]);
    let form_page = client.get("/accounts/new").text();
    assert!(form_page.contains("person-pick") && form_page.contains("data-last=\"DUPONT\"") && form_page.contains("name=\"holder_0_person\""), "{form_page}");

    // an imported joint account: the printed line is recognised as the two cards
    let csrf = client.login();
    let spec = Spec { holder: vec!["M DUPONT JEAN OU MME LEROY CLAIRE".into()], address: joint_address(), account: "00077777777".into(), ..Spec::new(sample_entries()[..3].to_vec()) };
    let page = client.post_files("/documents", &[("csrf", &csrf)], &[Upload { field: "files", filename: "releve.pdf", data: &boursobank_statement_pdf(&spec), content_type: "application/pdf" }]);
    let text = page.text();
    assert!(text.contains("name=\"holder_0_person\" value=\"1\"") && text.contains("name=\"holder_1_person\" value=\"2\""), "{text}");
}

#[test]
fn accounts_that_predate_the_cards_get_theirs_once() {
    let core = core();
    // as the version before the cards stored them: names in `holders` without a card, and a clear printed name
    let id = core.save_account(None, &form(&[("name", "Ancien"), ("account_type", "checking"), ("holder_name", "M DUPONT JEAN")])).unwrap();
    db(&core, |c| {
        let mut a = Account::get(c, id)?.unwrap();
        let before = a.clone();
        a.holders = Some(holders::to_json(&[holders::Person { id: None, civility: "MME".into(), last_names: vec!["LEROY".into()], birth_names: vec![], first_names: vec!["CLAIRE".into()] }]));
        a.save(c, &before)?;
        assert_eq!(PersonCard::query(c, "", [])?.len(), 0);
        assert_eq!(people::backfill(c)?, 1);
        assert_eq!(people::backfill(c)?, 0, "nothing more to do");
        let cards = PersonCard::query(c, "", [])?;
        assert_eq!(cards.len(), 1);
        assert_eq!(holders::of_account(&Account::get(c, id)?.unwrap())[0].id, Some(cards[0].id));
        Ok(())
    });
}
