//! python/tests/test_analytics.py: history, series, flows, spending, repartition and the SVG charts.

use std::collections::HashMap;

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde_json::json;

use crate::models::{Account, Counterparty, Snapshot, Transaction};
use crate::services::analytics::{self, History, MonthFlow, Series};
use crate::services::valuation::AccountValue;
use crate::testkit::env::{client, core, csrf_of, Upload};
use crate::testkit::fixtures::*;
use crate::testkit::db;
use crate::web::charts;

fn d(s: &str) -> Decimal {
    dec(s)
}

fn today() -> NaiveDate {
    ymd(2026, 10, 15)
}

#[derive(Default, Clone)]
struct Tx {
    day: Option<NaiveDate>,
    amount: Decimal,
    kind: Option<&'static str>,
    counterparty_name: Option<&'static str>,
    counterparty_id: Option<i64>,
    internal: bool,
}

fn tx(day: NaiveDate, amount: &str) -> Tx {
    Tx { day: Some(day), amount: d(amount), ..Default::default() }
}

fn add_account(c: &rusqlite::Connection, name: &str, balance: &str, balance_day: NaiveDate, account_type: &str, txs: &[Tx]) -> Account {
    let mut a = Account::default();
    a.name = name.into();
    a.account_type = account_type.into();
    a.balance = d(balance);
    a.balance_date = Some(balance_day.and_hms_opt(0, 0, 0).unwrap());
    a.insert(c).unwrap();
    for (i, t) in txs.iter().enumerate() {
        let mut row = Transaction::default();
        row.account_id = a.id;
        row.fingerprint = format!("{}-{}", a.id, i);
        row.date = t.day.unwrap();
        row.label = "op".into();
        row.amount = t.amount;
        row.kind = t.kind.map(|k| k.to_string());
        row.counterparty_name = t.counterparty_name.map(|k| k.to_string());
        row.counterparty_id = t.counterparty_id;
        if t.internal {
            row.details = Some(json!({"internal": true}));
        }
        row.insert(c).unwrap();
    }
    a
}

fn history(c: &rusqlite::Connection, a: &Account, current: &str) -> History {
    analytics::account_history(c, a, &[], d(current), today()).unwrap()
}

#[test]
fn test_history_is_rebuilt_from_the_balance_and_the_operations() {
    let core = core();
    let h = db(&core, |c| {
        let a = add_account(c, "CC", "1000", ymd(2026, 9, 30), "checking", &[tx(ymd(2026, 9, 10), "-100"), tx(ymd(2026, 9, 20), "50"), tx(ymd(2026, 9, 30), "-10")]);
        Ok(history(c, &a, "1000"))
    });
    assert_eq!(h[&ymd(2026, 9, 30)], d("1000")); // end of the anchor day: includes that day's operation
    assert!(h[&ymd(2026, 9, 29)] == d("1010") && h[&ymd(2026, 9, 20)] == d("1010"));
    assert!(h[&ymd(2026, 9, 19)] == d("960") && h[&ymd(2026, 9, 10)] == d("960"));
    assert_eq!(h[&ymd(2026, 9, 9)], d("1060")); // balance before the first operation
    assert_eq!(*h.keys().next().unwrap(), ymd(2026, 9, 9));
    assert_eq!(h[&today()], d("1000")); // carried to today
}

#[test]
fn test_operations_after_the_known_balance_move_it_forward() {
    let core = core();
    let h = db(&core, |c| {
        let a = add_account(c, "CC", "500", ymd(2026, 9, 30), "checking", &[tx(ymd(2026, 9, 5), "-5"), tx(ymd(2026, 10, 3), "-20")]);
        Ok(history(c, &a, "500"))
    });
    assert_eq!(h[&ymd(2026, 10, 2)], d("500"));
    assert_eq!(h[&ymd(2026, 10, 3)], d("480"));
    assert_eq!(h[&today()], d("480"));
}

#[test]
fn test_debts_and_foreign_accounts_keep_their_sign_and_rate() {
    let core = core();
    let h = db(&core, |c| {
        let loan = add_account(c, "Prêt", "1000", ymd(2026, 9, 30), "mortgage", &[tx(ymd(2026, 9, 15), "-100")]);
        Ok(history(c, &loan, "-1000")) // portfolio value of a debt is negative
    });
    assert_eq!(h[&ymd(2026, 9, 30)], d("-1000"));
    assert_eq!(h[&ymd(2026, 9, 14)], d("-1100"));
}

#[test]
fn test_accounts_without_operations_use_snapshots() {
    let core = core();
    let h = db(&core, |c| {
        let a = add_account(c, "Livret", "100", ymd(2026, 10, 15), "livret_a", &[]);
        for (day, value) in [(ymd(2026, 10, 1), "90"), (ymd(2026, 10, 10), "95")] {
            let mut sn = Snapshot::default();
            sn.account_id = a.id;
            sn.day = day;
            sn.value = d(value);
            sn.insert(c)?;
        }
        Ok(history(c, &a, "100"))
    });
    assert!(h[&ymd(2026, 10, 1)] == d("90") && h[&ymd(2026, 10, 9)] == d("90"));
    assert!(h[&ymd(2026, 10, 10)] == d("95") && h[&today()] == d("100"));
}

#[test]
fn test_series_total_per_account_and_per_category() {
    let core = core();
    let (cc, liv, histories) = db(&core, |c| {
        let cc = add_account(c, "CC", "1000", ymd(2026, 10, 15), "checking", &[tx(ymd(2026, 10, 1), "100")]);
        let liv = add_account(c, "Livret", "200", ymd(2026, 10, 15), "livret_a", &[tx(ymd(2026, 10, 1), "10")]);
        let mut histories: HashMap<i64, History> = HashMap::new();
        histories.insert(cc.id, history(c, &cc, "1000"));
        histories.insert(liv.id, history(c, &liv, "200"));
        Ok((cc, liv, histories))
    });
    let accounts: HashMap<i64, Account> = [(cc.id, cc.clone()), (liv.id, liv.clone())].into_iter().collect();
    let ids = [cc.id, liv.id];
    let total = &analytics::build_series(&histories, &accounts, &ids, "total", ymd(2026, 9, 30), today())[0];
    let points: HashMap<NaiveDate, Decimal> = total.points.iter().cloned().collect();
    assert_eq!(points[&ymd(2026, 10, 1)], d("1200"));
    assert_eq!(points[&ymd(2026, 9, 30)], d("1090")); // 900 + 190 before
    assert_eq!(analytics::build_series(&histories, &accounts, &ids, "compte", ymd(2026, 9, 30), today()).iter().map(|s| s.label.clone()).collect::<Vec<_>>(), vec!["CC", "Livret"]);
    assert_eq!(analytics::build_series(&histories, &accounts, &ids, "categorie", ymd(2026, 9, 30), today()).iter().map(|s| s.key.clone()).collect::<Vec<_>>(), vec!["liquidites", "epargne"]);
    let only = &analytics::build_series(&histories, &accounts, &[liv.id], "total", ymd(2026, 9, 30), today())[0];
    assert_eq!(only.points.last().unwrap().1, d("200"));
    let stats = analytics::stats(&total.points).unwrap();
    assert_eq!((stats.first, stats.last, stats.delta()), (d("1090"), d("1200"), d("110")));
    assert_eq!(stats.pct().unwrap().format_fixed(2, false, false), "10.09");
}

#[test]
fn test_period_start_and_sampling() {
    assert_eq!(analytics::period_start("1m", today(), ymd(2020, 1, 1)), ymd(2026, 9, 15));
    assert_eq!(analytics::period_start("1m", today(), ymd(2026, 10, 1)), ymd(2026, 10, 1)); // never before the first data
    assert_eq!(analytics::period_start("tout", today(), ymd(2020, 1, 1)), ymd(2020, 1, 1));
    let days = analytics::days(ymd(2016, 1, 1), today());
    assert!(days.len() as i64 <= analytics::MAX_POINTS + 1 && *days.last().unwrap() == today());
}

#[test]
fn test_flows_leave_out_transfers_between_own_accounts_unless_asked() {
    let core = core();
    let (flows, with_internal) = db(&core, |c| {
        let a = add_account(
            c,
            "CC",
            "0",
            ymd(2026, 10, 15),
            "checking",
            &[tx(ymd(2026, 9, 5), "2000"), tx(ymd(2026, 9, 6), "-300"), Tx { internal: true, ..tx(ymd(2026, 9, 7), "-500") }, tx(ymd(2026, 10, 2), "-40")],
        );
        Ok((analytics::monthly_flows(c, &[a.id], ymd(2026, 9, 1), today(), false)?, analytics::monthly_flows(c, &[a.id], ymd(2026, 9, 1), today(), true)?))
    });
    assert_eq!(
        flows.iter().map(|f| (f.month, f.income, f.expense, f.net())).collect::<Vec<_>>(),
        vec![(ymd(2026, 9, 1), d("2000"), d("300"), d("1700")), (ymd(2026, 10, 1), Decimal::ZERO, d("40"), d("-40"))]
    );
    assert_eq!(with_internal[0].expense, d("800"));
}

#[test]
fn test_spending_groups_by_kind_and_by_contact() {
    let core = core();
    let (result, cp_id) = db(&core, |c| {
        let mut cp = Counterparty::default();
        cp.name = "Pauline".into();
        cp.insert(c)?;
        let a = add_account(
            c,
            "CC",
            "0",
            ymd(2026, 10, 15),
            "checking",
            &[
                Tx { kind: Some("transfer"), counterparty_name: Some("MARTIN Pauline"), counterparty_id: Some(cp.id), ..tx(ymd(2026, 9, 5), "-30") },
                Tx { kind: Some("transfer"), counterparty_name: Some("Pauline MARTIN"), counterparty_id: Some(cp.id), ..tx(ymd(2026, 9, 6), "-20") },
                Tx { kind: Some("card"), counterparty_name: Some("UBER"), ..tx(ymd(2026, 9, 7), "-15") },
                Tx { kind: Some("card"), counterparty_name: Some("UBER"), ..tx(ymd(2026, 9, 8), "-5") },
                Tx { kind: Some("transfer"), counterparty_name: Some("ACME"), ..tx(ymd(2026, 9, 9), "2400") },
            ],
        );
        Ok((analytics::spending(c, &[a.id], ymd(2026, 9, 1), today(), false, 12)?, cp.id))
    });
    assert_eq!((result.total_out, result.total_in), (d("70"), d("2400")));
    assert_eq!(result.by_kind.iter().map(|r| (r.label.clone(), r.total)).collect::<Vec<_>>(), vec![("Virements".to_string(), d("50")), ("Cartes".to_string(), d("20"))]);
    assert_eq!(result.top_out.iter().map(|r| (r.label.clone(), r.total, r.count)).collect::<Vec<_>>(), vec![("Pauline".to_string(), d("50"), 2), ("UBER".to_string(), d("20"), 2)]);
    assert_eq!(result.top_out[0].contact_id, Some(cp_id));
    assert_eq!(result.top_in[0].label, "ACME");
}

#[test]
fn test_repartition_by_category_and_by_account() {
    let value = |id: i64, name: &str, kind: &str, total: &str| {
        let mut account = Account::default();
        account.id = id;
        account.name = name.into();
        account.account_type = kind.into();
        AccountValue { account, cash_eur: d(total), holdings: vec![], warnings: vec![] }
    };
    let values = vec![value(1, "CC", "checking", "100"), value(2, "Livret", "livret_a", "300"), value(4, "Livret 2", "ldds", "100"), value(3, "Prêt", "loan", "-50")];
    assert_eq!(analytics::repartition(&values, "categorie").into_iter().map(|(k, _, t)| (k, t)).collect::<Vec<_>>(), vec![("epargne".to_string(), d("400")), ("liquidites".to_string(), d("100"))]);
    assert_eq!(analytics::repartition(&values, "compte").into_iter().map(|(_, l, _)| l).collect::<Vec<_>>(), vec!["Livret", "CC", "Livret 2"]);
}

#[test]
fn test_charts_are_plain_svg_with_tooltips() {
    let series = vec![Series { key: "a".into(), label: "Compte <A>".into(), points: (1..29).map(|day| (ymd(2026, 9, day), d(&(1000 + day * 10).to_string()))).collect() }];
    let svg = charts::line_chart(&series, "Évolution");
    assert!(svg.starts_with("<svg") && svg.contains("<polyline") && svg.contains("<polygon") && svg.contains("Compte &lt;A&gt;") && !svg.contains("<script"));
    assert!(svg.contains("class=\"line s0\"") && !svg.contains("style=")); // colours come from classes: the CSP forbids inline styles
    assert!(svg.contains("class=\"pop\"") && svg.contains("01/09/2026") && !svg.contains("<title")); // an instant tooltip, not the delayed native one
    assert_eq!(charts::line_chart(&[], "Évolution"), "");
    assert_eq!(charts::line_chart(&[Series { key: "a".into(), label: "A".into(), points: vec![(ymd(2026, 9, 1), d("1"))] }], "Évolution"), "");
    let flows = vec![MonthFlow { month: ymd(2026, 8, 1), income: d("100"), expense: d("40") }, MonthFlow { month: ymd(2026, 9, 1), income: d("0"), expense: d("0") }];
    assert_eq!(charts::flow_chart(&flows, "Entrées et sorties par mois").matches("bar-").count(), 4);
    assert_eq!(charts::flow_chart(&[], "Entrées et sorties par mois"), "");
    let ring = charts::donut(&[("a".into(), "Épargne".into(), d("300")), ("b".into(), "Liquidités".into(), d("100"))], "Répartition");
    assert!(ring.contains("75.000 25.000") && ring.contains("Épargne"));
    assert_eq!(charts::donut(&[], "Répartition"), "");
    assert_eq!(charts::euro(-1234.5, 2), "−1 234,50 €");
    // The last tick always reaches the highest value (a chart whose axis stops early would clip its bars).
    for (low, high) in [(0.0, 2460.0), (0.0, 1.0), (-2000.0, 1700.0), (1000.0, 1003.0), (5.0, 5.0), (0.0, 99_999.0)] {
        let ticks = charts::nice_ticks(low, high, 5.0);
        assert!(ticks[0] <= low && *ticks.last().unwrap() >= high, "{low} {high} {ticks:?}");
    }
    let dot = charts::line_chart(
        &[
            Series { key: "a".into(), label: "A".into(), points: vec![(ymd(2026, 9, 1), d("1000")), (ymd(2026, 9, 20), d("1100"))] },
            Series { key: "b".into(), label: "Livret".into(), points: vec![(ymd(2026, 9, 20), d("500"))] },
        ],
        "Évolution",
    );
    assert!(dot.contains("<circle") && dot.contains("class=\"dot s1\""));
}

// ------------------------------------------------------------------------------------------------- web
#[test]
fn test_web_views_filters_and_totals() {
    let mut client = client();
    let token = client.core.setup_token.clone();
    client.post("/setup", &[("setup_token", &token), ("email", crate::testkit::env::ADMIN_EMAIL), ("password", crate::testkit::env::PASSWORD), ("confirm", crate::testkit::env::PASSWORD)]);
    let csrf = csrf_of(&client.get("/documents").text());
    let spec = Spec::new(sample_entries());
    let data = boursobank_statement_pdf(&spec);
    let page = client.post_files("/documents", &[("csrf", &csrf)], &[Upload { field: "files", filename: "r.pdf", data: &data, content_type: "application/pdf" }]);
    let doc_id = page.find(r"/documents/(\d+)/import/0").unwrap();
    client.post(
        &format!("/documents/{doc_id}/import/0"),
        &[("csrf", &csrf), ("account_id", "new"), ("name", "Compte courant"), ("update_balance", "on"), ("balance", &spec.closing().to_string()), ("import_transactions", "on")],
    );
    client.post("/accounts/new", &[("csrf", &csrf), ("name", "Livret A"), ("account_type", "livret_a"), ("balance", "5000")]);

    let evolution = client.get("/analytics?periode=tout");
    let text = evolution.text();
    assert!(evolution.status == 200 && text.contains("<svg") && text.contains("prêts non compris"));
    assert!(text.contains("Total de la sélection") && text.contains("Compte courant") && text.contains("Livret A"));
    let per_account = client.get("/analytics?vue=evolution&par=compte&periode=tout").text();
    assert!(per_account.matches("class=\"line s").count() >= 1 && per_account.contains("legend-label"));
    assert!(text.contains("Tous les comptes"));

    let only_savings = client.get("/analytics?vue=evolution&par=compte&c=2").text();
    assert!(only_savings.contains("1 compte(s) sélectionné(s)") && !only_savings.contains("donut-svg")); // the split by account or category is a grouping of the evolution chart
    assert!(only_savings.replace('\u{202f}', " ").replace('\u{a0}', " ").contains("5 000,00 €")); // thousands separator is a non-breaking space
    assert_eq!(only_savings.matches("Compte courant").count(), 1); // only in the list of accounts to tick, not in the figures

    let flux = client.get("/analytics?vue=flux&periode=tout");
    assert!(flux.status == 200 && flux.text().contains("bar-pos") && flux.text().contains("09/2026") && flux.text().contains("Total"));
    let with_internal = client.get("/analytics?vue=flux&periode=tout&interne=1");
    assert!(with_internal.status == 200 && with_internal.text().contains("checked"));

    let depenses = client.get("/analytics?vue=depenses&periode=tout");
    let text = depenses.text();
    assert!(depenses.status == 200 && text.contains("par catégorie") && text.contains("Courses") && text.contains("Impôts et taxes"));

    // Spending and income share one page, each with its own ring
    assert!(text.matches("donut-svg").count() >= 2 && text.contains("Revenus") && text.contains("Dépenses") && text.contains("Mois par mois"));
    assert!(!text.contains("vue=repartition") && !text.contains("vue=flux"));

    // The calendar: two dates replace the preset periods and bound the figures, the end date values the accounts
    let ranged = client.get("/analytics?vue=depenses&du=2026-09-01&au=2026-09-30");
    let ranged_text = ranged.text();
    assert!(ranged.status == 200 && ranged_text.contains("name=\"du\" value=\"2026-09-01\"") && ranged_text.contains("name=\"au\" value=\"2026-09-30\""));
    assert!(!ranged_text.contains("aria-current=\"true\">1 an")); // no preset is highlighted
    let past = client.get("/analytics?vue=evolution&du=2026-08-01&au=2026-08-31").text();
    assert!(past.contains("Valeur au 31/08/2026") && past.contains("Patrimoine, prêts non compris au 31/08/2026"));
    assert_eq!(client.get("/analytics?vue=evolution&du=nonsense&au=2999-01-01").status, 200); // bad dates are ignored, a future end stops at today

    // The dashboard draws the same wealth line, with axes and a choice of period
    let home = client.get("/?periode=3m").text();
    assert!(home.contains("Évolution du patrimoine") && home.contains("chart-svg") && home.contains("/?periode=1a") && home.contains("aria-current=\"true\">3 mois"));
    assert_eq!(client.get("/?periode=zzz").status, 200);

    // Nonsense parameters fall back to defaults instead of failing.
    let weird = client.get("/analytics?vue=nope&periode=zzz&par=zzz&c=999");
    assert!(weird.status == 200 && weird.text().contains("Analyses"));
    assert!(client.get("/").text().contains("href=\"/analytics\""));
}

#[test]
fn test_web_analytics_needs_login() {
    let mut client = client();
    assert_eq!(client.get_raw("/analytics").status, 303);
}

#[test]
fn a_phone_gets_charts_drawn_on_a_narrower_canvas() {
    use crate::web::charts;
    let points: Vec<(NaiveDate, Decimal)> = (0..10).map(|i| (ymd(2026, 1, 1) + chrono::Duration::days(i), d("100") + Decimal::from(i * 10))).collect();
    let series = vec![Series { key: "total".into(), label: "Total".into(), points }];
    let wide = charts::line_chart(&series, "Évolution");
    charts::set_compact(true);
    let narrow = charts::line_chart(&series, "Évolution");
    charts::set_compact(false);
    assert!(wide.contains("viewBox=\"0 0 820 320\"") && narrow.contains("viewBox=\"0 0 430 300\""), "{narrow}");
    assert_eq!(charts::line_chart(&series, "Évolution"), wide, "and back to the usual canvas");
}

#[test]
fn test_web_the_analyses_are_drawn_narrow_for_a_phone_and_wide_otherwise() {
    let mut client = client();
    let token = client.core.setup_token.clone();
    client.post("/setup", &[("setup_token", &token), ("email", crate::testkit::env::ADMIN_EMAIL), ("password", crate::testkit::env::PASSWORD), ("confirm", crate::testkit::env::PASSWORD)]);
    let csrf = csrf_of(&client.get("/documents").text());
    let spec = Spec::new(sample_entries());
    let data = boursobank_statement_pdf(&spec);
    let page = client.post_files("/documents", &[("csrf", &csrf)], &[Upload { field: "files", filename: "r.pdf", data: &data, content_type: "application/pdf" }]);
    let doc_id = page.find(r"/documents/(\d+)/import/0").unwrap();
    client.post(&format!("/documents/{doc_id}/import/0"), &[("csrf", &csrf), ("account_id", "new"), ("name", "Compte courant"), ("update_balance", "on"), ("balance", &spec.closing().to_string()), ("import_transactions", "on")]);
    let phone = "Mozilla/5.0 (Linux; Android 14; Pixel 7) AppleWebKit/537.36 Chrome/120 Mobile Safari/537.36";
    let narrow = client.get_with("/analytics?periode=tout", &[("user-agent", phone)]).text();
    let wide = client.get_with("/analytics?periode=tout", &[("user-agent", "Mozilla/5.0 (X11; Linux x86_64) Chrome/120 Safari/537.36")]).text();
    assert!(narrow.contains("viewBox=\"0 0 430 300\""), "phone");
    assert!(!wide.contains("viewBox=\"0 0 430 300\""), "desktop");
}

#[test]
fn test_web_loans_are_not_counted_in_the_total() {
    let mut client = client();
    let token = client.core.setup_token.clone();
    client.post("/setup", &[("setup_token", &token), ("email", crate::testkit::env::ADMIN_EMAIL), ("password", crate::testkit::env::PASSWORD), ("confirm", crate::testkit::env::PASSWORD)]);
    let csrf = csrf_of(&client.get("/documents").text());
    client.post("/accounts/new", &[("csrf", &csrf), ("name", "Livret A"), ("account_type", "livret_a"), ("balance", "5000")]);
    client.post("/accounts/new", &[("csrf", &csrf), ("name", "Prêt exemple"), ("account_type", "loan"), ("balance", "-3000")]);
    let flat = |s: String| s.replace('\u{202f}', " ").replace('\u{a0}', " ");
    let page = flat(client.get("/analytics?periode=tout").text());
    // the total is what is owned (5 000), the loan has its own tile
    let total = regex::Regex::new(r#"Tous les comptes</span><span class="big small-big"><span class="sensitive">([^<]*)<"#).unwrap().captures(&page).map(|c| c[1].to_string()).unwrap_or_default();
    assert_eq!(total, "5 000,00 €", "{total}");
    let at = page.find("Prêts (à part").unwrap_or(0);
    assert!(page.contains("Prêts (à part, hors total)") && page.contains("-3 000,00 €"), "{}", &page[at..at + 200]);
    let foot = regex::Regex::new(r#"Total de la sélection</th><th class="num"><span class="sensitive">([^<]*)<"#).unwrap().captures(&page).map(|c| c[1].to_string()).unwrap_or_default();
    assert_eq!(foot, "5 000,00 €");
}
