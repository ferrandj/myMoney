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

const PFU: &str = "<p>Les revenus financiers perçus depuis le 1.1.2018 sont soumis, sauf exceptions, au prélèvement forfaire unique (PFU) de 12,8&nbsp;% (7,5 % pour certains produits d'assurance-vie).</p><p>A compter du 1<sup>er</sup> janvier 2026, le taux des prélèvements sociaux passent à 18,6 % sauf pour les produits suivants qui restent soumis au taux de 17,2 %.</p>";

fn page(url: &str) -> Result<String, String> {
    Ok(match url {
        u if u.ends_with("F2365") => LIVRET_A,
        u if u.ends_with("F2368") => LDDS,
        u if u.ends_with("F2367") => LEP,
        u if u.ends_with("F16136") => CEL,
        u if u.ends_with("les-revenus-mobiliers") => PFU,
        _ => return Err("page inconnue".into()),
    }
    .to_string())
}

#[test]
fn each_official_page_gives_its_rate_whatever_the_markup() {
    let by_kind: BTreeMap<&str, (&str, u32)> = pr::PAGES.iter().map(|(k, _, pattern, max)| (*k, (*pattern, *max))).collect();
    let rate = |html: &str, kind: &str| pr::parse_rate(html, by_kind[kind].0, by_kind[kind].1);
    assert_eq!(rate(LIVRET_A, "livret_a"), Some(d("1.7")), "scripts are ignored, entities and non-breaking spaces are read");
    assert_eq!(rate(LDDS, "ldds"), Some(d("1.7")));
    assert_eq!(rate(LEP, "lep"), Some(d("2.50")), "the first sentence wins over the old rates");
    assert_eq!(rate(CEL, "cel"), Some(d("1.25")), "the loan rate of the same page is not the rate");
    assert_eq!(rate("<p>rien ici</p>", "livret_a"), None);
    assert_eq!(rate("<p>Le taux d'intérêt annuel du livret A est de 45 %</p>", "livret_a"), None, "an implausible rate is refused");
}

#[test]
fn a_page_that_fails_keeps_the_previous_rate_and_nothing_readable_is_an_error() {
    let all = pr::fetch_all(&page, &BTreeMap::new()).unwrap();
    assert_eq!(all.len(), 6);
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
    assert_eq!((*calls.borrow(), first.rates.len(), first.error.clone()), (5, 6, None), "five addresses: the flat tax parts share one page");
    pr::refresh_state(&dir.0, false, t0 + 3600.0 * 23.0, &counting);
    assert_eq!(*calls.borrow(), 5, "not again within 24 hours");
    pr::refresh_state(&dir.0, false, t0 + 3600.0 * 25.0, &counting);
    assert_eq!(*calls.borrow(), 10, "again after 24 hours");
    pr::refresh_state(&dir.0, true, t0 + 3600.0 * 25.0 + 1.0, &counting);
    assert_eq!(*calls.borrow(), 15, "and when asked (start-up, the button)");

    // what was read survives a restart, and a failure keeps it
    assert_eq!(pr::load(&dir.0).rates.len(), 6);
    let offline = |_: &str| -> Result<String, String> { Err("hors ligne".into()) };
    let failed = pr::refresh_state(&dir.0, true, t0 + 90_000.0, &offline);
    assert!(failed.error.is_some() && failed.rates.len() == 6 && failed.read_at == t0 + 3600.0 * 25.0 + 1.0, "the old reading stays");
    let retried = RefCell::new(0);
    let again = |u: &str| {
        *retried.borrow_mut() += 1;
        page(u)
    };
    pr::refresh_state(&dir.0, false, t0 + 90_000.0 + 1800.0, &again);
    assert_eq!(*retried.borrow(), 0, "after a failure it waits an hour");
    pr::refresh_state(&dir.0, false, t0 + 90_000.0 + 3700.0, &again);
    assert_eq!(*retried.borrow(), 5);
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

#[test]
fn gross_and_net_convert_both_ways() {
    use crate::services::rates::gross_and_net;
    assert_eq!(gross_and_net(d("3"), None, d("0.314")), (d("3"), d("2.058")), "a rate is gross unless said otherwise");
    assert_eq!(gross_and_net(d("3"), Some("gross"), d("0.30")), (d("3"), d("2.1")));
    assert_eq!(gross_and_net(d("2.1"), Some("net"), d("0.30")), (d("3"), d("2.1")));
    assert_eq!(gross_and_net(d("1.7"), Some("net"), Decimal::ZERO), (d("1.7"), d("1.7")));
}

#[test]
fn test_web_gross_and_net_on_the_account() {
    let mut client = client();
    let csrf = client.login();
    let id = client.core.save_account(None, &form(&[("name", "Livret banque"), ("account_type", "savings"), ("balance", "100")])).unwrap();
    let home = client.get("/").text();
    assert!(home.contains("name=\"basis\"") && home.contains(">Brut<") && home.contains(">Net<"), "the case sits next to the rate");

    client.post(&format!("/accounts/{id}/rate"), &[("csrf", &csrf), ("rate", "3"), ("basis", "gross")]);
    let page = client.get(&format!("/accounts/{id}")).text();
    assert!(page.contains("3,00 % brut") && page.contains("2,06 % net") && page.contains("flat tax 31,4 %"), "{page}");
    client.post(&format!("/accounts/{id}/rate"), &[("csrf", &csrf), ("rate", "2,5"), ("basis", "net")]);
    let net = client.get(&format!("/accounts/{id}")).text();
    assert!(net.contains("3,64 % brut") && net.contains("2,50 % net"), "{net}");
    db(&client.core, |c| {
        assert_eq!(Account::get(c, id)?.unwrap().interest_basis.as_deref(), Some("net"));
        Ok(())
    });
    // the account form carries the case too
    assert!(client.get(&format!("/accounts/{id}/edit")).text().contains("name=\"interest_basis\""));

    // the tax table sets the percentage: editing the line of the interest changes the net
    let table = client.get("/settings/tax").text();
    assert!(table.contains("name=\"ir_interets\"") && table.contains("legifiscal.fr"));
    client.post("/settings/tax", &[("csrf", &csrf), ("ir_interets", "12,8"), ("social_interets", "17,2")]);
    assert!(client.get(&format!("/accounts/{id}")).text().contains("3,57 % brut"), "30 % in total now");

    // a regulated livret is not taxed: no brut / net
    let a = client.core.save_account(None, &form(&[("name", "Mon livret A"), ("account_type", "livret_a"), ("balance", "100")])).unwrap();
    let livret = client.get(&format!("/accounts/{a}")).text();
    assert!(!livret.contains("% brut") && livret.contains("non imposé : brut = net"), "{livret}");
}
