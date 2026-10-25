//! The rates of the regulated savings accounts read online, and the rate asked for the other savings.

use std::cell::RefCell;
use std::collections::BTreeMap;

use rust_decimal::Decimal;

use crate::models::Account;
use crate::services::public_rates::{self as pr, State};
use crate::services::rates::{self, effective_rate_with, needs_rate};
use crate::testkit::env::{client, Dir};
use crate::testkit::fixtures::*;
use crate::testkit::{db, form};

fn d(s: &str) -> Decimal {
    dec(s)
}

const LIVRET_A: &str = "<html><head><script>var x = 'taux d\\'intérêt annuel du livret A est de 9 %';</script></head><body><h2>Quel est le taux de r&eacute;mun&eacute;ration du livret A ?</h2><p>Le taux d&#39;intérêt annuel du livret A est de&nbsp;<strong>1,7&nbsp;%</strong>.</p></body></html>";
const LDDS: &str = "<main><h2>Quel est le taux de rémunération du LDDS ?</h2><h3>Taux</h3><p>Le taux d’intérêt annuel est de <b>1,7 %</b></p><h3>Calcul des intérêts</h3></main>";
const LEP: &str = "<p>Quel est la rémunération du LEP ?</p><p>Le taux d'intérêt du LEP est de <span>2,50 %</span>. Anciens taux : 2,7 %</p>";
const CEL: &str = "<p>Le taux d'intérêt du CEL, hors prime d'État, est de 1,25 %.</p><p>le taux du prêt est de 2,75 %</p>";

fn page(url: &str) -> Result<String, String> {
    Ok(match url {
        u if u.ends_with("F2365") => LIVRET_A,
        u if u.ends_with("F2368") => LDDS,
        u if u.ends_with("F2367") => LEP,
        u if u.ends_with("F16136") => CEL,
        _ => return Err("page inconnue".into()),
    }
    .to_string())
}

#[test]
fn each_official_page_gives_its_rate_whatever_the_markup() {
    let by_kind: BTreeMap<&str, &str> = pr::PAGES.iter().map(|(k, _, pattern)| (*k, *pattern)).collect();
    assert_eq!(pr::parse_rate(LIVRET_A, by_kind["livret_a"]), Some(d("1.7")), "scripts are ignored, entities and non-breaking spaces are read");
    assert_eq!(pr::parse_rate(LDDS, by_kind["ldds"]), Some(d("1.7")));
    assert_eq!(pr::parse_rate(LEP, by_kind["lep"]), Some(d("2.50")), "the first sentence wins over the old rates");
    assert_eq!(pr::parse_rate(CEL, by_kind["cel"]), Some(d("1.25")), "the loan rate of the same page is not the rate");
    assert_eq!(pr::parse_rate("<p>rien ici</p>", by_kind["livret_a"]), None);
    assert_eq!(pr::parse_rate("<p>Le taux d'intérêt annuel du livret A est de 45 %</p>", by_kind["livret_a"]), None, "an implausible rate is refused");
}

#[test]
fn a_page_that_fails_keeps_the_previous_rate_and_nothing_readable_is_an_error() {
    let all = pr::fetch_all(&page, &BTreeMap::new()).unwrap();
    assert_eq!(all.len(), 4);
    let previous: BTreeMap<String, Decimal> = all.clone();
    let only_a = |url: &str| if url.ends_with("F2365") { Ok(LIVRET_A.to_string()) } else { Err("hors ligne".to_string()) };
    assert_eq!(pr::fetch_all(&only_a, &previous).unwrap(), previous);
    let none = |_: &str| -> Result<String, String> { Err("hors ligne".into()) };
    let e = pr::fetch_all(&none, &BTreeMap::new()).unwrap_err();
    assert!(e.contains("livret_a") && e.contains("hors ligne"));
}

#[test]
fn the_rates_are_asked_once_a_day_and_an_hour_after_a_failure() {
    let dir = Dir::new();
    let calls = RefCell::new(0);
    let counting = |url: &str| {
        *calls.borrow_mut() += 1;
        page(url)
    };
    let t0 = 1_800_000_000.0;
    let first = pr::refresh_state(&dir.0, false, t0, &counting);
    assert_eq!((*calls.borrow(), first.rates.len(), first.error.clone()), (4, 4, None));
    pr::refresh_state(&dir.0, false, t0 + 3600.0 * 23.0, &counting);
    assert_eq!(*calls.borrow(), 4, "not again within 24 hours");
    pr::refresh_state(&dir.0, false, t0 + 3600.0 * 25.0, &counting);
    assert_eq!(*calls.borrow(), 8, "again after 24 hours");
    pr::refresh_state(&dir.0, true, t0 + 3600.0 * 25.0 + 1.0, &counting);
    assert_eq!(*calls.borrow(), 12, "and when asked (start-up, the button)");

    // what was read survives a restart, and a failure keeps it
    assert_eq!(pr::load(&dir.0).rates.len(), 4);
    let offline = |_: &str| -> Result<String, String> { Err("hors ligne".into()) };
    let failed = pr::refresh_state(&dir.0, true, t0 + 90_000.0, &offline);
    assert!(failed.error.is_some() && failed.rates.len() == 4 && failed.read_at == t0 + 3600.0 * 25.0 + 1.0, "the old reading stays");
    let retried = RefCell::new(0);
    let again = |u: &str| {
        *retried.borrow_mut() += 1;
        page(u)
    };
    pr::refresh_state(&dir.0, false, t0 + 90_000.0 + 1800.0, &again);
    assert_eq!(*retried.borrow(), 0, "after a failure it waits an hour");
    pr::refresh_state(&dir.0, false, t0 + 90_000.0 + 3700.0, &again);
    assert_eq!(*retried.borrow(), 4);
    assert!(pr::load(&dir.0).error.is_none());
}

#[test]
fn the_online_rate_wins_for_today_only_and_only_while_it_is_recent() {
    let at = 1_800_000_000.0;
    let state = State { rates: BTreeMap::from([("livret_a".to_string(), d("9.9")), ("lep".to_string(), d("4.4"))]), read_at: at - 3600.0, checked_at: at - 3600.0, error: None };
    let (rate, why) = effective_rate_with(Some(&state), at, "livret_a", None, None);
    assert_eq!(rate, Some(d("9.9")));
    assert!(why.contains("taux officiel") && why.contains("service-public.gouv.fr"), "{why}");
    assert_eq!(effective_rate_with(Some(&state), at, "livret_jeune", None, None).0, Some(d("9.9")), "the Livret Jeune follows the Livret A");
    assert_eq!(effective_rate_with(Some(&state), at, "livret_a", Some(d("3")), None).1, "saisi manuellement", "a typed rate always wins");
    let past = effective_rate_with(Some(&state), at, "livret_a", None, Some(ymd(2024, 3, 1)));
    assert_eq!(past.0, Some(d("3.0")), "the rate of a past day comes from the table");
    let old = State { read_at: at - 46.0 * 86400.0, ..state.clone() };
    assert_eq!(effective_rate_with(Some(&old), at, "livret_a", None, None).0, rates::effective_rate("livret_a", None, None).0, "an old reading falls back to the table");
    assert_eq!(effective_rate_with(None, at, "ldds", None, None).0, rates::effective_rate("ldds", None, None).0);
    assert_eq!(effective_rate_with(Some(&state), at, "ldds", None, None).1.contains("taux réglementé depuis"), true, "a rate not read online uses the table");
}

#[test]
fn only_the_products_without_a_public_rate_are_asked() {
    for t in ["savings", "pel", "term_deposit", "life_insurance", "crowdfunding"] {
        assert!(needs_rate(t, None), "{t}");
        assert!(!needs_rate(t, Some(d("2"))), "{t} once typed");
    }
    for t in ["livret_a", "ldds", "lep", "cel", "livret_jeune", "checking", "pea", "mortgage", "crypto_wallet"] {
        assert!(!needs_rate(t, None), "{t}");
    }
}

#[test]
fn test_web_the_rate_of_a_savings_account_is_asked_typed_and_changed() {
    let mut client = client();
    let csrf = client.login();
    let id = client.core.save_account(None, &form(&[("name", "Livret banque"), ("account_type", "savings"), ("balance", "100")])).unwrap();
    let other = client.core.save_account(None, &form(&[("name", "Livret A"), ("account_type", "livret_a"), ("balance", "100")])).unwrap();

    let home = client.get("/").text();
    assert!(home.contains("Taux à renseigner") && home.contains("Livret banque") && home.contains(&format!("/accounts/{id}/rate")));
    assert!(!home.contains(&format!("/accounts/{other}/rate")), "a regulated livret has its rate by itself");
    assert!(client.get(&format!("/accounts/{id}")).text().contains("À renseigner"));

    let bad = client.post(&format!("/accounts/{id}/rate"), &[("csrf", &csrf), ("rate", "beaucoup")]);
    assert!(bad.text().contains("Taux invalide"));
    let done = client.post(&format!("/accounts/{id}/rate"), &[("csrf", &csrf), ("rate", "2,75 %"), ("next", "/")]);
    assert!(done.text().contains("Taux enregistré : 2,75 %") && !done.text().contains("Taux à renseigner"));
    db(&client.core, |c| {
        assert_eq!(Account::get(c, id)?.unwrap().interest_rate, Some(d("2.75")));
        Ok(())
    });
    let page = client.get(&format!("/accounts/{id}")).text();
    assert!(page.contains("2,75 %") && page.contains("saisi manuellement") && page.contains("Modifier le taux"));
    client.post(&format!("/accounts/{id}/rate"), &[("csrf", &csrf), ("rate", "3")]);
    db(&client.core, |c| {
        assert_eq!(Account::get(c, id)?.unwrap().interest_rate, Some(d("3")));
        Ok(())
    });

    let none = client.core.save_account(None, &form(&[("name", "Livret sans rien"), ("account_type", "savings")])).unwrap();
    client.post(&format!("/accounts/{none}/rate"), &[("csrf", &csrf), ("none", "1")]);
    db(&client.core, |c| {
        assert_eq!(Account::get(c, none)?.unwrap().interest_rate, Some(Decimal::ZERO));
        Ok(())
    });
    assert!(!client.get("/").text().contains("Taux à renseigner"), "nothing left to ask");
    assert_eq!(client.post(&format!("/accounts/{id}/rate"), &[("rate", "9")]).status, 403); // CSRF
    let settings = client.get("/settings").text();
    assert!(settings.contains("Actualiser maintenant") && settings.contains("service-public.gouv.fr"));
}
