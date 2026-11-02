//! Budget mode: every operation one by one, sortable and filterable, each with a category that can be changed and is
//! remembered for the operations that read the same.

use rust_decimal::Decimal;
use serde_json::json;

use crate::app::MyMoney;
use crate::models::{Counterparty, Transaction};
use crate::services::access;
use crate::services::analytics;
use crate::services::categories;
use crate::testkit::env::{client, csrf_of, Client, ADMIN_EMAIL, PASSWORD};
use crate::testkit::fixtures::*;
use crate::testkit::{db, form};

fn sign_up(client: &mut Client) -> String {
    let token = client.core.setup_token.clone();
    client.post("/setup", &[("setup_token", &token), ("email", ADMIN_EMAIL), ("name", "Alice"), ("password", PASSWORD), ("confirm", PASSWORD)]);
    csrf_of(&client.get("/documents").text())
}

fn account(core: &MyMoney, name: &str) -> i64 {
    let ident = core.identity_for_email(Some(ADMIN_EMAIL)).unwrap().unwrap();
    access::as_user(Some(ident), || core.save_account(None, &form(&[("name", name), ("account_type", "checking"), ("balance", "0")])).unwrap())
}

fn add(core: &MyMoney, account_id: i64, day: chrono::NaiveDate, label: &str, amount: &str, kind: &str) -> i64 {
    db(core, |c| {
        let n: i64 = c.query_row("SELECT count(*) FROM transactions", [], |r| r.get(0))?;
        let mut t = Transaction::default();
        t.account_id = account_id;
        t.fingerprint = format!("budget-{n}");
        t.date = day;
        t.label = label.to_string();
        t.counterparty_name = Some(label.to_string());
        t.amount = dec(amount);
        t.kind = Some(kind.to_string());
        Ok(t.insert(c)?)
    })
}

fn category_of(core: &MyMoney, id: i64) -> &'static str {
    category_of_with(core, id, None)
}

/// The category as the signed-in user sees it (the rules are personal).
fn category_of_with(core: &MyMoney, id: i64, contact: Option<&str>) -> &'static str {
    let ident = core.identity_for_email(Some(ADMIN_EMAIL)).unwrap().unwrap();
    access::as_user(Some(ident), || {
        db(core, |c| {
            let t = Transaction::get(c, id)?.unwrap();
            let rules = categories::load_rules(c)?;
            Ok(categories::categorize(&t, contact, &rules))
        })
    })
}

#[test]
fn the_budget_lists_every_operation_and_sorts_and_filters_them() {
    let mut web = client();
    sign_up(&mut web);
    let (main, savings) = (account(&web.core, "Compte courant"), account(&web.core, "Livret perso"));
    add(&web.core, main, ymd(2026, 9, 2), "CARTE LIDL", "-60", "card");
    add(&web.core, main, ymd(2026, 9, 9), "CARTE MYSTERY SHOP", "-15", "card");
    add(&web.core, main, ymd(2026, 9, 25), "ACME SAS Salaire", "2000", "transfer");
    add(&web.core, savings, ymd(2026, 9, 12), "PRLV EDF", "-80", "direct_debit");
    add(&web.core, savings, ymd(2026, 9, 30), "INTERETS", "3", "interest");

    let page = web.get("/budget").text();
    for expected in ["Budget", "CARTE LIDL", "CARTE MYSTERY SHOP", "ACME SAS Salaire", "PRLV EDF", "INTERETS", "Compte courant", "Livret perso", "Dépenses par catégorie", "Alimentation", "Énergie et eau"] {
        assert!(page.contains(expected), "{expected}");
    }
    assert!(page.contains("<span class=\"tag\">5</span>"), "the count of operations");
    // by default the newest first; by amount the smallest (the biggest expense) first
    let order = |p: &str, labels: &[&str]| -> Vec<usize> { labels.iter().map(|l| p.find(l).unwrap_or_else(|| panic!("{l}"))).collect() };
    let at = order(&page, &["INTERETS", "ACME SAS", "PRLV EDF", "MYSTERY", "LIDL"]);
    assert!(at.windows(2).all(|w| w[0] < w[1]), "{at:?}");
    let by_amount = web.get("/budget?tri=montant&ordre=asc").text();
    let at = order(&by_amount, &["PRLV EDF", "CARTE LIDL", "MYSTERY SHOP", "INTERETS", "ACME SAS"]);
    assert!(at.windows(2).all(|w| w[0] < w[1]), "{at:?}");
    // filters: one account, one category, only expenses, a text, dates
    let one = web.get(&format!("/budget?compte={savings}")).text();
    assert!(one.contains("PRLV EDF") && one.contains("INTERETS") && !one.contains("CARTE LIDL"));
    let food = web.get("/budget?cat=courses").text();
    assert!(food.contains("CARTE LIDL") && !food.contains("PRLV EDF") && !food.contains("MYSTERY SHOP"));
    assert!(food.contains("Dépenses par catégorie"), "the totals still show every category");
    let out = web.get("/budget?sens=depenses").text();
    assert!(out.contains("CARTE LIDL") && !out.contains("ACME SAS"));
    let income = web.get("/budget?sens=entrees").text();
    assert!(income.contains("ACME SAS") && income.contains("INTERETS") && !income.contains("CARTE LIDL"));
    let text = web.get("/budget?q=mystery").text();
    assert!(text.contains("MYSTERY SHOP") && !text.contains("CARTE LIDL"));
    let dates = web.get("/budget?du=2026-09-10&au=2026-09-26").text();
    assert!(dates.contains("PRLV EDF") && dates.contains("ACME SAS") && !dates.contains("CARTE LIDL") && !dates.contains("INTERETS"));
    assert_eq!(web.get("/budget?cat=nonsense&tri=nonsense&compte=999&sens=x").status, 200, "bad filters are ignored");
    // the totals: 155 spent, 2 003 received
    assert!(page.contains("155,00") && page.contains("2 003,00"), "{page}");
}

#[test]
fn a_category_changed_by_hand_is_remembered_for_the_operations_that_read_the_same() {
    let mut web = client();
    let csrf = sign_up(&mut web);
    let main = account(&web.core, "Compte courant");
    let first = add(&web.core, main, ymd(2026, 9, 9), "CARTE LA PETITE TABLE", "-15", "card");
    let second = add(&web.core, main, ymd(2026, 9, 20), "CARTE LA PETITE TABLE", "-22", "card");
    let other = add(&web.core, main, ymd(2026, 9, 21), "CARTE AUTRE PLACE", "-9", "card");
    assert_eq!(category_of(&web.core, first), "autres");

    // one row changed: the merchant is remembered, the other operation of it follows, and so does a later one
    let done = web.post("/budget/categorize", &[("csrf", &csrf), ("id", &first.to_string()), ("category", "restaurants"), ("next", "/budget?tri=date")]);
    assert!(done.text().contains("classé dans") && done.text().contains("Restaurants"), "{}", done.text());
    assert_eq!(category_of(&web.core, first), "restaurants");
    assert_eq!(category_of(&web.core, second), "restaurants");
    assert_eq!(category_of(&web.core, other), "autres", "another merchant is left alone");
    let later = add(&web.core, main, ymd(2026, 10, 3), "CARTE LA PETITE TABLE", "-30", "card");
    assert_eq!(category_of(&web.core, later), "restaurants");
    assert!(web.get("/budget").text().contains("retenue"));

    // "1×": only this operation
    web.post("/budget/categorize", &[("csrf", &csrf), ("id", &later.to_string()), ("category", "loisirs"), ("only", "1"), ("next", "/budget")]);
    assert_eq!(category_of(&web.core, later), "loisirs");
    assert_eq!(category_of(&web.core, second), "restaurants");
    assert!(web.get("/budget").text().contains("choisie"));

    // back to automatic: for this operation, then for the merchant
    web.post("/budget/categorize", &[("csrf", &csrf), ("id", &later.to_string()), ("category", "auto"), ("only", "1"), ("next", "/budget")]);
    assert_eq!(category_of(&web.core, later), "restaurants", "it follows the merchant again");
    web.post("/budget/categorize", &[("csrf", &csrf), ("id", &first.to_string()), ("category", "auto"), ("next", "/budget")]);
    assert_eq!(category_of(&web.core, first), "autres");
    assert_eq!(category_of(&web.core, second), "autres");

    // an unknown category, a missing token and a foreign address change nothing
    let bad = web.post("/budget/categorize", &[("csrf", &csrf), ("id", &first.to_string()), ("category", "nonsense"), ("next", "//evil.example")]);
    assert!(bad.text().contains("inconnue") && !bad.url.contains("evil.example"));
    assert_eq!(web.post("/budget/categorize", &[("id", &first.to_string()), ("category", "loisirs")]).status, 403);
    assert_eq!(category_of(&web.core, first), "autres");
}

#[test]
fn the_new_categories_are_guessed() {
    let guess = |label: &str, amount: &str, kind: &str| {
        let mut t = Transaction::default();
        t.label = label.into();
        t.counterparty_name = Some(label.into());
        t.amount = dec(amount);
        t.kind = Some(kind.into());
        categories::categorize(&t, None, &Default::default())
    };
    assert_eq!(guess("CARTE STEAMGAMES.COM", "-20", "card"), "jeux_video");
    assert_eq!(guess("PLAYSTATION PLUS", "-60", "card"), "jeux_video");
    assert_eq!(guess("CARTE CARREFOUR MARKET", "-40", "card"), "courses");
    assert_eq!(categories::get("courses").unwrap().label, "Alimentation");
    assert_eq!(guess("CARTE TOTAL ACCESS", "-55", "card"), "essence");
    assert_eq!(guess("CARTE STATION ESSO", "-50", "card"), "essence");
    assert_eq!(guess("CARTE NORAUTO", "-120", "card"), "garage");
    assert_eq!(guess("NAVIGO RATP", "-86", "card"), "transport_commun");
    assert_eq!(guess("CARTE UBER TRIP", "-12", "card"), "transport");
    assert_eq!(guess("ECH PRET 0001234", "-900", "loan_payment"), "credit");
    assert_eq!(categories::get("credit").unwrap().label, "Remboursement d'emprunt");
    assert_eq!(guess("CREDIT LOGT 0001234", "50000", "loan_payment"), "emprunt");
    assert_eq!(categories::get("emprunt").unwrap().label, "Emprunt (fonds reçus)");
    assert_eq!(categories::get("interne").unwrap().label, "Transfert compte à compte");
    // each one can be picked by hand, on the right side
    assert!(categories::options("out").iter().any(|c| c.id == "jeux_video") && categories::options("out").iter().any(|c| c.id == "interne"));
    assert!(categories::options("in").iter().any(|c| c.id == "emprunt"));
}

#[test]
fn a_transfer_to_my_own_other_account_is_neither_spent_nor_received() {
    let mut web = client();
    let csrf = sign_up(&mut web);
    let main = account(&web.core, "Compte courant");
    // a contact "Moi (autre compte)": what goes to it is a transfer between accounts, with no rule to write
    let me_id = db(&web.core, |c| Ok(Counterparty { name: "Mon livret ailleurs".into(), kind: "self".into(), ..Default::default() }.insert(c)?));
    let to_me = add(&web.core, main, ymd(2026, 9, 5), "VIR VERS LIVRET", "-200", "transfer");
    db(&web.core, |c| {
        let mut t = Transaction::get(c, to_me)?.unwrap();
        t.counterparty_id = Some(me_id);
        t.update(c)?;
        Ok(())
    });
    let shop = add(&web.core, main, ymd(2026, 9, 6), "CARTE LIDL", "-50", "card");
    assert_eq!(category_of_with(&web.core, to_me, Some("Mon livret ailleurs")), "interne");

    let page = web.get("/budget").text();
    assert!(page.contains("Transfert compte à compte") && page.contains("200,00"), "{page}");
    // the spending report leaves it out unless asked
    let (start, end) = (ymd(2026, 9, 1), ymd(2026, 9, 30));
    let report = db(&web.core, |c| analytics::spending_report(c, &[main], start, end, false, "out", None));
    assert_eq!(report.total, dec("50"));
    let with = db(&web.core, |c| analytics::spending_report(c, &[main], start, end, true, "out", None));
    assert_eq!(with.total, dec("250"));

    // an operation can also be called a transfer between accounts by hand
    web.post("/budget/categorize", &[("csrf", &csrf), ("id", &shop.to_string()), ("category", "interne"), ("only", "1"), ("next", "/budget")]);
    let report = db(&web.core, |c| analytics::spending_report(c, &[main], start, end, false, "out", None));
    assert_eq!(report.total, Decimal::ZERO);
}

#[test]
fn another_user_cannot_change_the_category_of_an_operation_of_mine() {
    let mut web = client();
    let csrf = sign_up(&mut web);
    let main = account(&web.core, "Compte courant");
    let id = add(&web.core, main, ymd(2026, 9, 9), "CARTE LA PETITE TABLE", "-15", "card");
    let err = db(&web.core, |c| {
        let other = access::Identity { id: 9999, email: "other@example.org".into(), name: None, is_admin: false };
        Ok(access::as_user(Some(other), || crate::services::budget::set_category(c, id, "restaurants", false)).err().map(|e| e.to_string()))
    });
    assert!(err.is_some(), "refused");
    assert_eq!(category_of(&web.core, id), "autres");
    let _ = (csrf, json!(null));
}
