//! The "Comptes" tab: every account with its details, and the quick edits.

use crate::models::Account;
use crate::testkit::env::client;
use crate::testkit::{db, form};

#[test]
fn the_accounts_tab_lists_every_account_and_renames_in_place() {
    let mut client = client();
    let csrf = client.login();
    let id = client
        .core
        .save_account(None, &form(&[("name", "Compte joint"), ("institution", "Banque Test"), ("account_type", "checking"), ("balance", "120,50"), ("holder_0_civility", "M"), ("holder_0_last_0", "DUPONT"), ("holder_0_first_0", "JEAN"), ("holder_1_civility", "MME"), ("holder_1_last_0", "LEROY"), ("holder_1_first_0", "CLAIRE"), ("iban", "FR7630001007941234567890185")]))
        .unwrap();
    let archived = client.core.save_account(None, &form(&[("name", "Vieux livret"), ("account_type", "savings"), ("archived", "on")])).unwrap();

    let page = client.get("/accounts").text();
    assert!(page.contains("Compte joint") && page.contains("Banque Test") && page.contains("M DUPONT JEAN") && page.contains("MME LEROY CLAIRE"), "{page}");
    assert!(page.contains("120,50") && page.contains(&format!("/accounts/{id}/edit")) && page.contains("Archivés") && page.contains("Vieux livret"));
    assert!(client.get("/").text().contains("href=\"/accounts\""), "the tab is in the top bar");

    let done = client.post(&format!("/accounts/{id}/quick"), &[("csrf", &csrf), ("name", "Compte du foyer"), ("institution", "Autre Banque"), ("account_type", "savings")]);
    assert!(done.text().contains("Compte mis à jour") && done.text().contains("Compte du foyer"));
    db(&client.core, |c| {
        let a = Account::get(c, id)?.unwrap();
        assert_eq!((a.name.as_str(), a.institution.as_deref(), a.account_type.as_str()), ("Compte du foyer", Some("Autre Banque"), "savings"));
        // what the quick edit does not show is left alone
        assert_eq!((a.iban.as_deref(), a.holder_name.as_deref()), (Some("FR7630001007941234567890185"), Some("M DUPONT JEAN OU MME LEROY CLAIRE")));
        assert!(Account::get(c, archived)?.unwrap().archived);
        Ok(())
    });
    // a bad token changes nothing
    let refused = client.post(&format!("/accounts/{id}/quick"), &[("csrf", "nope"), ("name", "Piraté")]);
    assert!(refused.status >= 400 || !refused.text().contains("Compte mis à jour"));
    db(&client.core, |c| {
        assert_eq!(Account::get(c, id)?.unwrap().name, "Compte du foyer");
        Ok(())
    });
}

#[test]
fn a_joint_account_says_so_next_to_its_type_with_its_holders_and_can_be_filtered() {
    let mut client = client();
    client.login();
    let joint = client.core.save_account(None, &form(&[("name", "Compte du foyer"), ("account_type", "checking"), ("balance", "10"), ("holder_0_civility", "M"), ("holder_0_last_0", "DUPONT"), ("holder_0_first_0", "JEAN"), ("holder_1_civility", "MME"), ("holder_1_last_0", "LEROY"), ("holder_1_first_0", "CLAIRE")])).unwrap();
    client.core.save_account(None, &form(&[("name", "Compte perso"), ("account_type", "checking"), ("balance", "20"), ("holder_0_civility", "M"), ("holder_0_last_0", "DUPONT"), ("holder_0_first_0", "JEAN")])).unwrap();

    let home = client.get("/").text();
    assert!(home.contains("Compte courant - Joint") && home.contains("M DUPONT JEAN OU MME LEROY CLAIRE"), "{home}");
    assert_eq!(home.matches("Compte courant - Joint").count(), 1, "only the account with two people is joint");
    let page = client.get(&format!("/accounts/{joint}")).text();
    assert!(page.contains("Compte courant - Joint") && page.contains("M DUPONT JEAN OU MME LEROY CLAIRE"));

    let only_joint = client.get("/accounts?filtre=joints").text();
    assert!(only_joint.contains("Compte du foyer") && !only_joint.contains("Compte perso"));
    let only_single = client.get("/accounts?filtre=individuels").text();
    assert!(only_single.contains("Compte perso") && !only_single.contains("Compte du foyer"));
    assert!(client.get("/accounts").text().contains("Compte perso"));

    // the analyses table and the people page say it too
    let analytics = client.get("/analytics?vue=evolution&periode=tout").text();
    assert!(analytics.contains("Titulaire(s)") && analytics.contains("Compte courant - Joint"));
    assert!(client.get("/people/1").text().contains("Compte courant - Joint"));
}
