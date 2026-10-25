//! python/tests/test_browse_and_spending.py: documents tree and grouped download, spending by category, and
//! "already known" contacts.

use chrono::NaiveDate;
use regex::Regex;
use rust_decimal::Decimal;
use serde_json::json;

use crate::app::MyMoney;
use crate::error::AppError;
use crate::models::{Account, CategoryRule, Document, Transaction};
use crate::services::access;
use crate::services::analytics::{self, SpendingReport};
use crate::services::categories;
use crate::services::counterparties as contact_service;
use crate::services::users as user_service;
use crate::testkit::env::{bare_core, client, csrf_of, Client, Core, Upload, ADMIN_EMAIL, PASSWORD};
use crate::testkit::fixtures::*;
use crate::testkit::fortuneo::{fortuneo_fees_pdf, fortuneo_info, fortuneo_september, fortuneo_statement_pdf, FeesSpec};
use crate::testkit::{db, form, unzip};
use crate::web::charts;
use crate::web::jinja::eval::getitem;
use crate::web::jinja::Value;
use crate::web::views::{document_tree, zip_documents};

fn d(s: &str) -> Decimal {
    dec(s)
}

fn s(v: &str) -> Option<String> {
    Some(v.to_string())
}

fn tx(label: &str, amount: i64, kind: &str, internal: bool) -> Transaction {
    let mut t = Transaction::default();
    t.label = label.into();
    t.amount = Decimal::from(amount);
    t.kind = s(kind);
    t.date = ymd(2026, 9, 5);
    if internal {
        t.details = Some(json!({"internal": true}));
    }
    t
}

// ------------------------------------------------------------------------------------------ categories
fn guess(label: &str, amount: i64, kind: &str, expected: &str) {
    assert_eq!(categories::categorize(&tx(label, amount, kind, false), None, &Default::default()), expected, "{label}");
}

macro_rules! guesses {
    ($($name:ident: ($label:expr, $amount:expr, $kind:expr, $expected:expr);)*) => {
        $(
            #[test]
            fn $name() {
                guess($label, $amount, $kind, $expected);
            }
        )*
    };
}

guesses! {
    test_the_automatic_guess_carrefour: ("CARTE CARREFOUR CITY", -10, "card", "courses");
    test_the_automatic_guess_lidl: ("LIDL 1234", -20, "card", "courses");
    test_the_automatic_guess_boulangerie: ("Boulangerie du coin", -3, "card", "restaurants");
    test_the_automatic_guess_uber_eats: ("UBER *EATS", -18, "card", "restaurants");
    test_the_automatic_guess_uber_trip: ("UBER TRIP", -12, "card", "transport");
    test_the_automatic_guess_trains: ("TRAINS DE FRANCE", -71, "card", "transport");
    test_the_automatic_guess_eaux: ("PRLV SEPA EAUX DU NORD", -28, "direct_debit", "energie");
    test_the_automatic_guess_edf: ("PRLV EDF CLIENTS", -62, "direct_debit", "energie");
    test_the_automatic_guess_impots: ("PRLV SEPA DIRECTION GENERALE DES FINANCES", -1396, "direct_debit", "impots");
    test_the_automatic_guess_netflix: ("NETFLIX.COM", -13, "card", "abonnements");
    test_the_automatic_guess_orange: ("ORANGE SA", -20, "direct_debit", "telecom");
    test_the_automatic_guess_pharmacie: ("PHARMACIE DU CENTRE", -12, "card", "sante");
    test_the_automatic_guess_amazon: ("AMAZON PAYMENTS", -30, "card", "shopping");
    test_the_automatic_guess_google_play: ("Google Play Apps", -1, "card", "loisirs");
    test_the_automatic_guess_maif: ("ASSURANCE HABITATION MAIF", -25, "direct_debit", "assurances");
    test_the_automatic_guess_assurance_vie: ("ASSURANCE VIE VERSEMENT PERIODIQUE", -80, "direct_debit", "epargne");
    test_the_automatic_guess_sushi: ("SUSHI SHOP", -25, "card", "restaurants");
    test_the_automatic_guess_pret: ("ECH PRET 123", -166, "loan_payment", "credit");
    test_the_automatic_guess_cotis: ("COTIS CARTE", -2, "fee", "frais");
    test_the_automatic_guess_distributeur: ("DISTRIBUTEUR", -40, "cash", "retraits");
    test_the_automatic_guess_proches: ("MARTIN Pauline cadeau", -50, "transfer", "proches");
    test_the_automatic_guess_prlv_inconnu: ("PRLV INCONNU", -9, "direct_debit", "prelevements");
    test_the_automatic_guess_mystery: ("MYSTERY SHOP", -9, "card", "autres");
    test_the_automatic_guess_salaire: ("ACME SAS Salaire septembre", 1500, "transfer", "salaire");
    test_the_automatic_guess_caf: ("CAF PRESTATION", 120, "transfer", "aides");
    test_the_automatic_guess_doctolib: ("Remboursement Doctolib", 20, "transfer", "remboursements");
    test_the_automatic_guess_recus: ("VIREMENT DE JEAN", 100, "transfer", "recus");
    test_the_automatic_guess_interets: ("INTERETS", 1, "interest", "interets");
    test_the_automatic_guess_retour: ("Retour achat", 15, "card", "autres_entrees");
}

#[test]
fn test_a_word_is_a_whole_word() {
    guess("BPCE PRELEVEMENT", -9, "direct_debit", "prelevements"); // "bp" is not "bpce"
    guess("BP STATION", -50, "card", "transport");
    guess("FREE MOBILE", -9, "direct_debit", "telecom");
    guess("Transport en commun", -2, "card", "autres"); // "sport*" starts a word, "transport" does not
}

#[test]
fn test_the_users_rule_wins_over_everything_and_internal_transfers_stay_apart() {
    let t = tx("CARREFOUR CITY", -10, "card", false);
    let key = categories::merchant(&t, None).0;
    let rules = |k: &str, v: &str| [(k.to_string(), v.to_string())].into_iter().collect::<std::collections::HashMap<_, _>>();
    assert_eq!(categories::categorize(&t, None, &rules(&key, "loisirs")), "loisirs");
    let loan = tx("ECH PRET 1", -100, "loan_payment", false);
    let loan_key = categories::merchant(&tx("ECH PRET 1", -100, "card", false), None).0;
    assert_eq!(categories::categorize(&loan, None, &rules(&loan_key, "courses")), "courses");
    assert_eq!(categories::categorize(&t, None, &rules(&key, "nonsense")), "courses"); // a stale rule for a category that no longer exists is ignored
    assert_eq!(categories::categorize(&tx("VIR M DUPONT", -60, "transfer", true), None, &Default::default()), "interne");
    let out: Vec<&str> = categories::options("out").iter().map(|o| o.id).collect();
    assert!(["courses", "autres", "interne"].iter().all(|i| out.contains(i)) && !out.contains(&"salaire"));
    let inc: Vec<&str> = categories::options("in").iter().map(|o| o.id).collect();
    assert!(["salaire", "recus"].iter().all(|i| inc.contains(i)));
}

fn browse_core() -> Core {
    let core = bare_core();
    core.setup(ADMIN_EMAIL, Some("Alice"), PASSWORD).unwrap();
    db(&core, |c| {
        user_service::add_with_password(c, &core.vault, "bob@example.com", Some("Bob"), "Another-Strong-Pass-7", false)?;
        Ok(())
    });
    core
}

fn add_ops(core: &MyMoney) -> i64 {
    let ident = core.identity_for_email(Some(ADMIN_EMAIL)).unwrap().unwrap();
    let account_id = access::as_user(Some(ident), || core.save_account(None, &form(&[("name", "Courant"), ("account_type", "checking"), ("balance", "0")])).unwrap());
    let rows = [
        (ymd(2026, 8, 3), "CARTE CARREFOUR CITY", "-40", "card"),
        (ymd(2026, 9, 2), "CARTE LIDL", "-60", "card"),
        (ymd(2026, 9, 9), "CARTE MYSTERY SHOP", "-15", "card"),
        (ymd(2026, 9, 10), "CARTE MYSTERY SHOP", "-5", "card"),
        (ymd(2026, 9, 12), "PRLV EDF", "-80", "direct_debit"),
        (ymd(2026, 9, 25), "ACME SAS Salaire", "2000", "transfer"),
        (ymd(2026, 9, 26), "VIR M DUPONT", "-300", "transfer"),
    ];
    db(core, |c| {
        for (i, (day, label, amount, kind)) in rows.iter().enumerate() {
            let mut t = Transaction::default();
            t.account_id = account_id;
            t.fingerprint = format!("{account_id}-{i}");
            t.date = *day;
            t.label = label.to_string();
            t.amount = d(amount);
            t.kind = s(kind);
            if label.starts_with("VIR M") {
                t.details = Some(json!({"internal": true}));
            }
            t.counterparty_name = Some(label.replace("CARTE ", ""));
            t.insert(c)?;
        }
        Ok(())
    });
    account_id
}

struct Args {
    start: NaiveDate,
    internal: bool,
    direction: &'static str,
    category: Option<&'static str>,
}

impl Default for Args {
    fn default() -> Args {
        Args { start: ymd(2026, 8, 1), internal: false, direction: "out", category: None }
    }
}

fn report(core: &MyMoney, account_id: i64, a: Args) -> SpendingReport {
    db(core, |c| analytics::spending_report(c, &[account_id], a.start, ymd(2026, 9, 30), a.internal, a.direction, a.category))
}

#[test]
fn test_the_report_adds_up_by_category_and_leaves_out_the_internal_transfers() {
    let core = browse_core();
    let a = add_ops(&core);
    let r = report(&core, a, Args::default());
    assert_eq!((r.total, r.total_in, r.saved()), (d("200"), d("2000"), d("1800"))); // the 300 between own accounts counts for nobody
    let by = |id: &str| r.categories.iter().find(|c| c.id == id).unwrap();
    assert_eq!((by("courses").total, by("courses").count, by("energie").total, by("autres").total), (d("100"), 2, d("80"), d("20")));
    assert_eq!(r.categories.iter().map(|c| c.total).sum::<Decimal>(), r.total);
    assert!((r.categories.iter().map(|c| c.share.to_f64()).sum::<f64>() - 100.0).abs() < 1e-6);
    assert_eq!(r.categories.iter().map(|c| c.id).collect::<Vec<_>>(), vec!["courses", "energie", "autres"]); // biggest first
    assert_eq!(r.months.iter().map(|(m, _)| *m).collect::<Vec<_>>(), vec![ymd(2026, 8, 1), ymd(2026, 9, 1)]); // one entry per month of the period, even an empty one
    let with = report(&core, a, Args { internal: true, ..Args::default() });
    assert_eq!(with.total, d("500"));
    assert!(with.categories.iter().any(|c| c.id == "interne"));
}

#[test]
fn test_the_report_by_month_and_for_income() {
    let core = browse_core();
    let a = add_ops(&core);
    let sept = report(&core, a, Args { start: ymd(2026, 9, 1), ..Args::default() });
    assert_eq!(sept.total, d("160"));
    assert_eq!(sept.months, vec![(ymd(2026, 9, 1), d("160"))]);
    let whole = report(&core, a, Args::default());
    assert_eq!(whole.months, vec![(ymd(2026, 8, 1), d("40")), (ymd(2026, 9, 1), d("160"))]);
    let income = report(&core, a, Args { direction: "in", ..Args::default() });
    assert_eq!(income.total, d("2000"));
    assert_eq!(income.categories.iter().map(|c| c.id).collect::<Vec<_>>(), vec!["salaire"]);
}

#[test]
fn test_one_category_gives_its_merchants_operations_and_months() {
    let core = browse_core();
    let a = add_ops(&core);
    let r = report(&core, a, Args { category: Some("courses"), ..Args::default() });
    assert_eq!(r.category.as_ref().unwrap().total, d("100"));
    assert_eq!(r.merchants.iter().map(|m| m.label.clone()).collect::<Vec<_>>(), vec!["LIDL", "CARREFOUR CITY"]);
    assert_eq!(r.operations.iter().map(|(t, name)| (t.label.clone(), name.clone())).collect::<Vec<_>>(), vec![("CARTE LIDL".to_string(), "Courant".to_string()), ("CARTE CARREFOUR CITY".to_string(), "Courant".to_string())]); // newest first
    assert_eq!(r.months, vec![(ymd(2026, 8, 1), d("40")), (ymd(2026, 9, 1), d("60"))]); // the months of this category only
    let other = report(&core, a, Args { category: Some("autres"), ..Args::default() });
    assert_eq!(other.merchants.iter().map(|m| (m.label.clone(), m.count)).collect::<Vec<_>>(), vec![("MYSTERY SHOP".to_string(), 2)]);
    assert!(!other.merchants[0].ruled);
}

#[test]
fn test_choosing_a_category_for_a_merchant_moves_all_its_operations_and_is_personal() {
    let core = browse_core();
    let a = add_ops(&core);
    let alice = core.identity_for_email(Some(ADMIN_EMAIL)).unwrap().unwrap();
    let bob = core.identity_for_email(Some("bob@example.com")).unwrap().unwrap();
    let key = report(&core, a, Args { category: Some("autres"), ..Args::default() }).merchants[0].key.clone();
    access::as_user(Some(alice.clone()), || db(&core, |c| categories::set_rule(c, &key, "loisirs")));
    access::as_user(Some(alice.clone()), || {
        let r = report(&core, a, Args::default());
        assert_eq!(r.categories.iter().find(|c| c.id == "loisirs").unwrap().total, d("20"));
        assert!(!r.categories.iter().any(|c| c.id == "autres"));
        assert!(report(&core, a, Args { category: Some("loisirs"), ..Args::default() }).merchants[0].ruled);
    });
    db(&core, |c| {
        // Bob's own view of the same data (nothing is shared with him: use the rule table directly)
        assert_eq!(CategoryRule::query(c, "", [])?.iter().map(|r| r.owner_id).collect::<Vec<_>>(), vec![Some(alice.id)]);
        Ok(())
    });
    access::as_user(Some(bob), || {
        db(&core, |c| {
            assert!(categories::load_rules(c)?.is_empty()); // Alice's choice is not Bob's
            Ok(())
        });
    });
    access::as_user(Some(alice), || {
        db(&core, |c| {
            categories::set_rule(c, &key, "auto")?; // forget it
            assert!(categories::load_rules(c)?.is_empty());
            match categories::set_rule(c, &key, "nonsense") {
                Err(AppError::Value(m)) => assert!(m.contains("inconnue"), "{m}"),
                other => panic!("{other:?}"),
            }
            Ok(())
        });
    });
}

#[test]
fn test_month_bars_are_drawn_with_a_tooltip() {
    let svg = charts::month_bars(&[(ymd(2026, 8, 1), d("40")), (ymd(2026, 9, 1), d("160"))], "Par mois", "neg");
    assert!(svg.starts_with("<svg") && svg.matches("class=\"bar-neg\"").count() == 2 && svg.contains("class=\"pop\"") && svg.contains("09/2026") && svg.contains("160 €"));
    assert_eq!(charts::month_bars(&[(ymd(2026, 8, 1), d("0"))], "Par mois", "neg"), "");
    assert_eq!(charts::month_bars(&[], "Par mois", "neg"), "");
}

// ------------------------------------------------------------------------------------------------- web
fn sign_up(client: &mut Client) -> String {
    let token = client.core.setup_token.clone();
    client.post("/setup", &[("setup_token", &token), ("email", ADMIN_EMAIL), ("name", "Alice"), ("password", PASSWORD), ("confirm", PASSWORD)]);
    csrf_of(&client.get("/documents").text())
}

#[test]
fn test_web_spending_by_category_with_drill_down_and_recategorising() {
    let mut web = client();
    let csrf = sign_up(&mut web);
    add_ops(&web.core);
    let page = web.get("/analytics?vue=depenses&periode=tout").text();
    for expected in ["Où est parti", "par catégorie", "Courses", "Énergie et eau", "Autres dépenses", "cat=courses", "Plus gros bénéficiaires", "Mois par mois", "Revenus", "Plus grosses sources", "Solde de la période"] {
        assert!(page.contains(expected), "{expected}");
    }
    // Inside a category: who, when, and a way to change it.
    let drill = web.get("/analytics?vue=depenses&periode=tout&cat=autres").text();
    for expected in ["Toutes les catégories", "MYSTERY SHOP", "name=\"category\"", "Changer"] {
        assert!(drill.contains(expected), "{expected}");
    }
    let mut shop = Transaction::default();
    shop.label = "MYSTERY SHOP".into();
    shop.counterparty_name = s("MYSTERY SHOP");
    let key = categories::merchant(&shop, None).0;
    let done = web.post("/analytics/categorize", &[("csrf", &csrf), ("key", &key), ("label", "MYSTERY SHOP"), ("category", "loisirs"), ("next", "/analytics?vue=depenses&periode=tout")]).text();
    assert!(done.contains("classé dans") && done.contains("Loisirs et culture") && !done.contains("cat=autres"));
    let again = web.get("/analytics?vue=depenses&periode=tout&cat=loisirs").text();
    assert!(again.contains("à ta façon") && again.contains("↺ Automatique"));
    let bad = web.post("/analytics/categorize", &[("csrf", &csrf), ("key", "x"), ("category", "nonsense"), ("next", "//evil.example")]);
    assert!(bad.text().contains("inconnue") && !bad.url.contains("evil.example"));
    assert_eq!(web.post("/analytics/categorize", &[("key", "x"), ("category", "loisirs")]).status, 403); // CSRF
}

#[test]
fn test_web_income_view_months_and_links_from_the_flows() {
    let mut web = client();
    sign_up(&mut web);
    add_ops(&web.core);
    let income = web.get("/analytics?vue=depenses&periode=tout&sens=entrees").text();
    assert!(income.contains("il vient, par catégorie") && income.contains("Salaire et revenus") && income.contains("Reçu"));
    let flows = web.get("/analytics?vue=flux&periode=tout").text();
    assert!(flows.contains("vue=depenses") && flows.contains("mois=2026-09") && flows.to_lowercase().contains("où est parti"));
    let month = web.get("/analytics?vue=depenses&periode=tout&mois=2026-09").text();
    assert!(month.contains("Mois : 09/2026") && !month.contains("CARREFOUR") && month.contains("160,00")); // the August purchase is out
    assert!(!web.get("/analytics?vue=depenses&periode=tout&mois=nonsense").text().contains("Mois :"));
    let empty = web.get("/analytics?vue=depenses&periode=tout&mois=2020-01");
    assert!(empty.status == 200 && empty.text().contains("Aucune opération"));
    assert_eq!(web.get("/analytics?vue=depenses&cat=nonsense").status, 200); // an unknown category is just ignored
}

#[test]
fn test_the_account_page_names_the_category_of_each_operation() {
    let mut web = client();
    sign_up(&mut web);
    let a = add_ops(&web.core);
    let page = web.get(&format!("/accounts/{a}")).text();
    assert!(page.contains("Courses") && page.contains("Énergie et eau"));
}

// ----------------------------------------------------------------------------------------- documents
fn doc(id: i64, name: &str, institution: Option<&str>, adapter: Option<&str>, kind: &str, day: Option<NaiveDate>) -> Document {
    let mut d = Document::default();
    d.id = id;
    d.filename = name.into();
    d.adapter_id = adapter.map(|a| a.to_string());
    d.kind = kind.into();
    d.size = 2048;
    d.status = "imported".into();
    d.parse_result = Some(json!({"institution": institution, "statements": day.map(|d| vec![json!({"period_end": d.to_string()})]).unwrap_or_default()}));
    d.uploaded_at = ymd(2026, 10, 1).and_hms_opt(0, 0, 0).unwrap();
    d
}

fn at(v: &Value, key: &str) -> Value {
    getitem(v, &Value::str(key)).unwrap()
}

fn idx(v: &Value, i: i64) -> Value {
    getitem(v, &Value::Int(i)).unwrap()
}

fn items(v: &Value) -> Vec<Value> {
    match v {
        Value::List(l) | Value::Tuple(l) => l.to_vec(),
        other => panic!("not a list: {other:?}"),
    }
}

fn labels(v: &Value) -> Vec<String> {
    items(v).iter().map(|x| at(x, "label").as_str().unwrap().to_string()).collect()
}

fn account_named(id: i64, name: &str, institution: &str, kind: &str) -> Account {
    let mut a = Account::default();
    a.id = id;
    a.name = name.into();
    a.institution = s(institution);
    a.account_type = kind.into();
    a
}

#[test]
fn test_documents_are_grouped_by_bank_then_type_then_year_newest_first() {
    // Rust also groups by account between the bank and the type.
    let mut docs = vec![
        doc(1, "a.pdf", Some("BoursoBank"), Some("x"), "statement", Some(ymd(2026, 9, 30))),
        doc(2, "b.pdf", Some("BoursoBank"), Some("x"), "statement", Some(ymd(2025, 12, 31))),
        doc(3, "c.pdf", Some("BoursoBank"), Some("x"), "savings_statement", Some(ymd(2026, 6, 30))),
        doc(4, "d.pdf", Some("Fortuneo"), Some("x"), "fees_summary", Some(ymd(2023, 12, 31))),
        doc(5, "e.pdf", None, None, "statement", None),
        doc(6, "f.pdf", Some("Fortuneo"), Some("x"), "statement", Some(ymd(2026, 10, 31))),
        doc(7, "g.pdf", Some("BoursoBank"), Some("x"), "statement", Some(ymd(2026, 3, 31))),
        doc(8, "h.pdf", Some("Fortuneo"), None, "statement", None),
    ];
    // two documents of the checking account, one of the savings account, the fees recap of a Fortuneo account
    for (id, account) in [(1, 10), (2, 10), (7, 10), (3, 11), (4, 12)] {
        docs.iter_mut().find(|d| d.id == id).unwrap().account_id = Some(account);
    }
    let accounts: std::collections::HashMap<i64, Account> = [
        account_named(10, "Compte courant", "BoursoBank", "checking"),
        account_named(11, "Mon PEL", "BoursoBank", "pel"),
        account_named(12, "Compte Fortuneo", "Fortuneo", "checking"),
    ]
    .into_iter()
    .map(|a| (a.id, a))
    .collect();
    let tree = document_tree(&docs, &accounts);
    assert_eq!(labels(&tree), vec!["BoursoBank", "Fortuneo", "Autres documents"]);
    assert_eq!(items(&tree).iter().map(|b| at(b, "count").as_int().unwrap()).collect::<Vec<_>>(), vec![4, 3, 1]);
    let bourso = idx(&tree, 0);
    assert_eq!(labels(&at(&bourso, "accounts")), vec!["Compte courant", "Mon PEL"]);
    let checking = idx(&at(&bourso, "accounts"), 0);
    assert_eq!(at(&checking, "subtitle").as_str(), Some("Compte courant"));
    assert_eq!(labels(&at(&checking, "kinds")), vec!["Relevé de compte"]);
    let statements = idx(&at(&checking, "kinds"), 0);
    let years: Vec<(i64, Vec<i64>)> = items(&at(&statements, "years"))
        .iter()
        .map(|y| (idx(y, 0).as_int().unwrap(), items(&idx(y, 1)).iter().map(|row| at(&idx(row, 0), "id").as_int().unwrap()).collect()))
        .collect();
    assert_eq!(years, vec![(2026, vec![1, 7]), (2025, vec![2])]);
    let pel = idx(&at(&bourso, "accounts"), 1);
    assert_eq!(labels(&at(&pel, "kinds")), vec!["Relevé d'épargne"]);
    // Fortuneo: the account first, then what belongs to no account; unrecognised files last
    let fortuneo = idx(&tree, 1);
    assert_eq!(labels(&at(&fortuneo, "accounts")), vec!["Compte Fortuneo", "Sans compte"]);
    assert_eq!(labels(&at(&idx(&at(&fortuneo, "accounts"), 0), "kinds")), vec!["Récapitulatif des frais"]);
    assert_eq!(labels(&at(&idx(&at(&fortuneo, "accounts"), 1), "kinds")), vec!["Relevé de compte", "Non reconnu"]);
    let others = idx(&tree, 2);
    assert_eq!(labels(&at(&others, "accounts")), vec!["Sans compte"]);
    assert_eq!(labels(&at(&idx(&at(&others, "accounts"), 0), "kinds")), vec!["Non reconnu"]);
}

fn drop_documents(web: &mut Client, csrf: &str) -> std::collections::HashMap<String, i64> {
    let mut ids = std::collections::HashMap::new();
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("sept.pdf", boursobank_statement_pdf(&Spec::new(sample_entries()))),
        ("fortuneo.pdf", fortuneo_statement_pdf(&fortuneo_september())),
        ("frais.pdf", fortuneo_fees_pdf(&FeesSpec::new())),
        ("fgdr.pdf", fortuneo_info()),
    ];
    for (name, data) in files {
        web.post_files("/documents", &[("csrf", csrf)], &[Upload { field: "files", filename: name, data: &data, content_type: "application/pdf" }]);
        let id = db(&web.core, |c| Ok(Document::query_one(c, "WHERE filename = ?1 OR original_filename = ?1", [name])?.unwrap().id));
        ids.insert(name.to_string(), id);
        if name == "sept.pdf" || name == "fortuneo.pdf" {
            web.post(&format!("/documents/{id}/import/0"), &[("csrf", csrf), ("account_id", "new"), ("name", name), ("import_transactions", "on")]);
        }
    }
    ids
}

#[test]
fn test_web_documents_page_is_a_tree_you_can_tick() {
    let mut web = client();
    let csrf = sign_up(&mut web);
    drop_documents(&mut web, &csrf);
    let page = web.get("/documents").text();
    assert!(page.matches("class=\"doc-bank\"").count() == 2 && page.contains("BoursoBank") && page.contains("Fortuneo"));
    assert!(page.contains("Récapitulatif des frais") && page.contains("Information") && page.contains("Relevé de compte"));
    assert!(page.matches("name=\"ids\"").count() == 4 && page.contains("id=\"download-selected\"") && page.contains("data-check-group"));
    assert!(page.contains("2026") && page.contains("action=\"/documents/download\""));
    assert!(Regex::new(r#"<details class="doc-bank" open>"#).unwrap().is_match(&page)); // the first bank is open, the others folded
}

#[test]
fn test_web_download_one_file_as_it_is_or_several_as_a_zip() {
    let mut web = client();
    let csrf = sign_up(&mut web);
    let ids = drop_documents(&mut web, &csrf);
    let (sept, frais) = (ids["sept.pdf"].to_string(), ids["frais.pdf"].to_string());
    let one = web.post("/documents/download", &[("csrf", &csrf), ("ids", &sept)]);
    assert!(one.status == 200 && one.body.starts_with(b"%PDF") && one.header("content-disposition").unwrap().contains("BOURSOBANK_COMPTE-COURANT_RELEVE-DE-COMPTE_DUPONT_JEAN_2026-09-30.pdf"));
    let many = web.post("/documents/download", &[("csrf", &csrf), ("ids", &sept), ("ids", &frais), ("ids", &sept), ("ids", "junk")]);
    assert_eq!(many.header("content-type"), Some("application/zip"));
    assert!(many.header("content-disposition").unwrap().contains(".zip"));
    let mut archive = unzip(&many.body);
    archive.sort();
    assert_eq!(archive.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>(), vec!["BOURSOBANK_COMPTE-COURANT_RELEVE-DE-COMPTE_DUPONT_JEAN_2026-09-30.pdf", "FORTUNEO_COMPTE-COURANT_RECAPITULATIF-DES-FRAIS_MARTIN_PAUL_2023-12-31.pdf"]);
    assert!(archive.iter().all(|(_, data)| data.starts_with(b"%PDF"))); // asked twice, sent once
    assert!(web.post("/documents/download", &[("csrf", &csrf)]).text().contains("au moins un document"));
    assert_eq!(web.post("/documents/download", &[("ids", &sept)]).status, 403); // CSRF
    assert_eq!(web.post("/documents/download", &[("csrf", &csrf), ("ids", "99999")]).status, 404);
}

#[test]
fn test_web_download_refuses_documents_one_may_not_open() {
    let mut alice = client();
    let csrf = sign_up(&mut alice);
    let ids = drop_documents(&mut alice, &csrf);
    alice.post("/settings/users", &[("csrf", &csrf), ("email", "bob@example.com"), ("name", "Bob"), ("password", "Another-Strong-Pass-7")]);
    let mut bob = alice.another();
    bob.post("/login", &[("email", "bob@example.com"), ("password", "Another-Strong-Pass-7")]);
    let bob_csrf = csrf_of(&bob.get("/documents").text());
    let (sept, frais) = (ids["sept.pdf"].to_string(), ids["frais.pdf"].to_string());
    assert_eq!(bob.post("/documents/download", &[("csrf", &bob_csrf), ("ids", &sept)]).status, 404);
    assert_eq!(bob.post("/documents/download", &[("csrf", &bob_csrf), ("ids", &sept), ("ids", &frais)]).status, 404); // nothing partial
}

#[test]
fn test_zip_keeps_two_files_with_the_same_name() {
    let files: Vec<(String, Vec<u8>)> = [("releve.pdf", "1"), ("releve.pdf", "2"), ("RELEVE.PDF", "3"), ("sans-extension", "4"), ("a/b.pdf", "5")].iter().map(|(n, c)| (n.to_string(), c.as_bytes().to_vec())).collect();
    let mut names: Vec<String> = unzip(&zip_documents(&files)).into_iter().map(|(n, _)| n).collect();
    names.sort();
    assert_eq!(names, vec!["RELEVE (3).PDF", "a_b.pdf", "releve (2).pdf", "releve.pdf", "sans-extension"]);
}

// ------------------------------------------------------------------------------------------- contacts
fn make_contact(core: &MyMoney, name: &str) -> i64 {
    db(core, |c| {
        let cp = contact_service::save_contact(c, None, &form(&[("name", name), ("kind", "person")]))?;
        contact_service::add_identifier(c, &cp, "name", Some("MARTIN Pauline"), Some("MARTIN Pauline"))?;
        Ok(cp.id)
    })
}

#[test]
fn test_identify_with_an_existing_contact_adds_the_new_name_and_iban_to_it() {
    let core = browse_core();
    let a = add_ops(&core);
    let iban = fake_iban(3);
    db(&core, |c| {
        for (fp, day, amount) in [("x1", ymd(2026, 9, 20), "-30"), ("x2", ymd(2026, 9, 21), "-10")] {
            let mut t = Transaction::default();
            t.account_id = a;
            t.fingerprint = fp.into();
            t.date = day;
            t.label = "VIR SEPA PAULINE M".into();
            t.amount = d(amount);
            t.kind = s("transfer");
            t.counterparty_name = s("PAULINE M");
            t.counterparty_iban = Some(iban.clone());
            t.insert(c)?;
        }
        Ok(())
    });
    let contact_id = make_contact(&core, "Pauline");
    db(&core, |c| {
        assert!(contact_service::unresolved(c, false, None, None)?.iter().any(|g| g.iban.as_deref() == Some(iban.as_str())));
        let cp = contact_service::identify(c, &form(&[("contact_id", &contact_id.to_string()), ("iban", &iban), ("printed_name", "PAULINE M")]))?;
        assert_eq!(cp.id, contact_id);
        let identifiers = contact_service::identifiers_of(c, cp.id)?;
        let kinds: std::collections::BTreeSet<String> = identifiers.iter().map(|i| i.kind.clone()).collect();
        assert_eq!(kinds, ["iban".to_string(), "name".to_string()].into_iter().collect());
        assert_eq!(identifiers.len(), 3); // her old printed name, the new one, and the IBAN
        assert!(!contact_service::unresolved(c, false, None, None)?.iter().any(|g| g.iban.as_deref() == Some(iban.as_str()))); // both operations are linked now
        assert_eq!(Transaction::query(c, "WHERE counterparty_id = ?", [contact_id])?.len(), 2);
        Ok(())
    });
    db(&core, |c| {
        match contact_service::identify(c, &form(&[("iban", ""), ("printed_name", "SOMEONE"), ("name", "  ")])) {
            Err(AppError::Value(m)) => assert!(m.contains("nom, ou choisis un contact"), "{m}"),
            other => panic!("{other:?}"),
        }
        match contact_service::identify(c, &form(&[("contact_id", "99999"), ("printed_name", "X")])) {
            Err(AppError::Value(m)) => assert!(m.contains("introuvable"), "{m}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(contact_service::choices(c)?.into_iter().map(|(_, n)| n).collect::<Vec<_>>(), vec!["Pauline"]);
        Ok(())
    });
}

#[test]
fn test_web_quick_form_offers_the_contacts_already_known() {
    let mut web = client();
    let csrf = sign_up(&mut web);
    add_ops(&web.core);
    let first = web.get("/contacts").text();
    assert!(!first.contains("data-inline-contact") && !first.contains("Nouveau contact…")); // nobody known yet: only the new-contact fields
    let contact_id = make_contact(&web.core, "Pauline Martin");
    let page = web.get("/contacts").text();
    assert!(page.contains("data-inline-contact") && page.contains(&format!("<option value=\"{contact_id}\">Pauline Martin</option>")) && page.contains("Nouveau contact…"));
    assert!(page.matches("data-inline-contact").count() >= 2);
    // The same person under another name: pick her instead of creating a second contact.
    let done = web.post("/contacts/identify", &[("csrf", &csrf), ("next", "/contacts"), ("contact_id", &contact_id.to_string()), ("iban", ""), ("printed_name", "MYSTERY SHOP")]).text();
    assert!(done.contains("enregistré") && done.contains("Pauline Martin"));
    let detail = web.get(&format!("/contacts/{contact_id}")).text().to_lowercase();
    assert!(detail.contains("mystery shop") || detail.contains("mystery"));
    let nothing = web.post("/contacts/identify", &[("csrf", &csrf), ("next", "/contacts"), ("iban", ""), ("printed_name", "ACME SAS"), ("name", "")]);
    assert!(nothing.text().contains("choisis un contact déjà enregistré"));
}
