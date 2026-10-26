//! The tax table: 2026 figures to start with, edited by the administrator, checked once a month.

use std::collections::BTreeMap;

use rust_decimal::Decimal;

use crate::services::fiscal::{self, TaxTable};
use crate::services::public_rates::State;
use crate::testkit::env::client;
use crate::testkit::fixtures::*;
use crate::testkit::{db, form};

fn d(s: &str) -> Decimal {
    dec(s)
}

const DAY: f64 = 86400.0;

#[test]
fn the_table_starts_with_the_figures_of_2026() {
    let t = TaxTable::default();
    let interets = t.get("interets").unwrap();
    assert_eq!((interets.ir, interets.social, interets.total()), (Some(d("12.8")), d("18.6"), Some(d("31.4"))));
    assert_eq!(t.get("av_moins_8_ans").unwrap().total(), Some(d("30.0")), "life insurance keeps the old social levies");
    assert_eq!(t.get("av_plus_8_ans").unwrap().total(), Some(d("24.7")));
    assert_eq!(t.get("pea_5_ans").unwrap().total(), Some(d("18.6")));
    assert_eq!(t.get("revenus_fonciers").unwrap().total(), None, "the progressive scale: no flat total");
    assert_eq!(t.get("plus_values_immo").unwrap().total(), Some(d("36.2")));
    assert!(t.rows.len() >= 11 && t.year == 2026);
}

#[test]
fn a_product_is_taxed_by_its_own_line() {
    let t = TaxTable::default();
    let pct = |ty: &str| {
        let (_, ir, ps) = t.for_product(ty);
        ir + ps
    };
    for free in ["livret_a", "ldds", "lep", "livret_jeune"] {
        assert_eq!(pct(free), Decimal::ZERO, "{free}");
    }
    assert_eq!(pct("cel"), d("18.6"), "only the social levies");
    assert_eq!(pct("savings"), d("31.4"));
    assert_eq!(pct("term_deposit"), d("31.4"));
    assert_eq!(pct("pel"), d("31.4"));
    assert_eq!(pct("life_insurance"), d("30"), "the line of life insurance before 8 years");
}

#[test]
fn the_reminder_is_due_once_a_month_and_can_be_put_off_a_day() {
    let at = 1_800_000_000.0;
    let mut t = TaxTable::default();
    assert!(t.due(at), "never checked");
    assert_eq!(t.days_since_review(at), None);
    t.reviewed_at = Some(at - 10.0 * DAY);
    assert!(!t.due(at) && t.days_since_review(at) == Some(10));
    t.reviewed_at = Some(at - 31.0 * DAY);
    assert!(t.due(at));
    t.snoozed_until = Some(at + 3600.0);
    assert!(!t.due(at), "put off");
    assert!(t.due(at + 2.0 * 3600.0), "and back after the delay");
}

#[test]
fn the_table_is_kept_and_new_lines_appear_by_themselves() {
    let mut client = client();
    client.login();
    db(&client.core, |c| {
        assert_eq!(fiscal::load(c)?.get("interets").unwrap().total(), Some(d("31.4")));
        let mut t = fiscal::load(c)?;
        t.rows.iter_mut().find(|r| r.key == "interets").unwrap().social = d("17.2");
        t.reviewed_at = Some(1_700_000_000.0);
        fiscal::save(c, &t)?;
        let back = fiscal::load(c)?;
        assert_eq!(back.get("interets").unwrap().total(), Some(d("30.0")));
        assert_eq!(back.edited, vec!["interets"]);
        assert_eq!(back.reviewed_at, Some(1_700_000_000.0));
        // a table saved by an older version that had fewer lines: the missing ones come with their default
        crate::models::setting_set(c, fiscal::SETTING, r#"{"year":2026,"reviewed_at":1,"rates":{"interets":{"ir":"12.8","social":"18.6"}}}"#)?;
        let older = fiscal::load(c)?;
        assert_eq!(older.get("dividendes").unwrap().total(), Some(d("31.4")));
        assert!(older.edited.is_empty());
        Ok(())
    });
}

#[test]
fn the_form_is_read_with_commas_and_mistakes_keep_what_was_there() {
    let t = TaxTable::default();
    let mut f: BTreeMap<String, String> = BTreeMap::new();
    f.insert("ir_interets".into(), "12,8".into());
    f.insert("social_interets".into(), "19,6 %".into());
    f.insert("ir_dividendes".into(), "abc".into());
    f.insert("social_dividendes".into(), "-3".into());
    f.insert("ir_revenus_fonciers".into(), "".into());
    f.insert("social_revenus_fonciers".into(), "17,2".into());
    let out = fiscal::from_form(&t, &f);
    assert_eq!(out.get("interets").unwrap().total(), Some(d("32.4")));
    assert_eq!(out.get("dividendes").unwrap().total(), Some(d("31.4")), "an invalid figure changes nothing");
    assert_eq!(out.get("revenus_fonciers").unwrap().ir, None, "a progressive-scale line stays one");
    assert_eq!(out.edited, vec!["interets"]);
    assert_eq!(fiscal::parse_percent(" 7,5 "), Some(d("7.5")));
    assert_eq!(fiscal::parse_percent("101"), None);
}

#[test]
fn the_reminder_says_when_the_tax_office_page_disagrees() {
    let at = 1_800_000_000.0;
    let t = TaxTable::default();
    let read = |ir: &str, ps: &str| State { rates: BTreeMap::from([("pfu_ir".to_string(), d(ir)), ("pfu_ps".to_string(), d(ps))]), read_at: at - 60.0, checked_at: at - 60.0, error: None };
    assert!(fiscal::differences(&t, Some(&read("12.8", "18.6")), at).is_empty());
    let diff = fiscal::differences(&t, Some(&read("12.8", "19.6")), at);
    assert_eq!(diff.len(), 1);
    assert!(diff[0].contains("19,6") && diff[0].contains("18,6") && diff[0].contains("prélèvements sociaux"), "{diff:?}");
    assert!(fiscal::differences(&t, None, at).is_empty(), "nothing read, nothing to compare");
    let old = State { read_at: at - 50.0 * DAY, ..read("12.8", "19.6") };
    assert!(fiscal::differences(&t, Some(&old), at).is_empty(), "an old reading is not trusted");
}

#[test]
fn test_web_the_administrator_is_reminded_and_edits_the_table() {
    let mut client = client();
    let csrf = client.login();
    let home = client.get("/").text();
    assert!(home.contains("id=\"tax-reminder\"") && home.contains("jamais été vérifiés") && home.contains("/settings/tax"), "{home}");
    assert!(!client.get("/settings/tax").text().contains("id=\"tax-reminder\""), "not on the page that answers it");

    let page = client.get("/settings/tax").text();
    assert!(page.contains("legifiscal.fr/reperes-fiscaux/flat-tax-pfu-champ-application-taux-applicables.html") && page.contains("name=\"ir_interets\" inputmode=\"decimal\" value=\"12,80\"") && page.contains("name=\"social_av_moins_8_ans\""), "{page}");

    // putting it off hides it for a day, it is still due afterwards
    let snoozed = client.post("/settings/tax/snooze", &[("csrf", &csrf), ("next", "/")]);
    assert!(!snoozed.text().contains("id=\"tax-reminder\""));
    assert!(!client.get("/accounts").text().contains("id=\"tax-reminder\""));
    db(&client.core, |c| {
        let mut t = fiscal::load(c)?;
        assert!(t.snoozed_until.is_some() && t.reviewed_at.is_none());
        t.snoozed_until = Some(1.0); // the day is over
        fiscal::save(c, &t)?;
        Ok(())
    });
    assert!(client.get("/").text().contains("id=\"tax-reminder\""));

    // changing a figure is a check
    let saved = client.post("/settings/tax", &[("csrf", &csrf), ("ir_interets", "12,8"), ("social_interets", "17,2")]);
    assert!(saved.text().contains("marqués comme vérifiés") && saved.text().contains("modifié"));
    assert!(!client.get("/").text().contains("id=\"tax-reminder\""), "checked just now");
    assert_eq!(client.core.tax_table().get("interets").unwrap().total(), Some(d("30.0")));

    // "it is up to date" confirms without changing
    db(&client.core, |c| {
        let mut t = fiscal::load(c)?;
        t.reviewed_at = Some(1.0);
        fiscal::save(c, &t)?;
        Ok(())
    });
    assert!(client.get("/").text().contains("depuis"));
    let ok = client.post("/settings/tax/reviewed", &[("csrf", &csrf), ("next", "/")]);
    assert!(ok.text().contains("prochain rappel dans un mois") && !ok.text().contains("id=\"tax-reminder\""));

    // the 2026 figures come back, the check stays
    client.post("/settings/tax/reset", &[("csrf", &csrf)]);
    assert_eq!(client.core.tax_table().get("interets").unwrap().total(), Some(d("31.4")));
    assert!(client.core.tax_table().reviewed_at.is_some());
    assert_eq!(client.post("/settings/tax", &[("ir_interets", "1")]).status, 403); // CSRF

    // the net follows the table, and the settings page points to it
    let id = client.core.save_account(None, &form(&[("name", "Livret banque"), ("account_type", "savings"), ("balance", "100"), ("interest_rate", "3")])).unwrap();
    assert!(client.get(&format!("/accounts/{id}")).text().contains("2,06 % net"));
    let settings = client.get("/settings").text();
    assert!(settings.contains("Ouvrir le tableau fiscal") && settings.contains("legifiscal.fr"));
}
