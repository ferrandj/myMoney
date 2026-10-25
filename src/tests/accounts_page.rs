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
