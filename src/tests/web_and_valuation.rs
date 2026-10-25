//! python/tests/test_web_and_valuation.py

use std::collections::HashMap;

use regex::Regex;
use rust_decimal::Decimal;

use crate::models::{Account, Holding};
use crate::services::prices::PriceError;
use crate::services::valuation::{PriceSource, Valuator};
use crate::testkit::env::{client, ADMIN_EMAIL, PASSWORD};
use crate::testkit::fixtures::*;

fn d(s: &str) -> Decimal {
    dec(s)
}

struct FakePrices;

impl PriceSource for FakePrices {
    fn convert(&self, amount: Decimal, from: &str, _to: &str) -> Result<Decimal, PriceError> {
        let rate = match from.to_uppercase().as_str() {
            "EUR" => d("1"),
            "USD" => d("0.9"),
            other => panic!("no rate for {other}"),
        };
        Ok(amount * rate)
    }
    fn crypto_prices(&self, symbols: &[String], _currency: &str, _ids: &HashMap<String, String>) -> HashMap<String, Decimal> {
        symbols.iter().filter_map(|s| match s.to_uppercase().as_str() {
            "BTC" => Some(("BTC".to_string(), d("50000"))),
            "ETH" => Some(("ETH".to_string(), d("2000"))),
            _ => None,
        }).collect()
    }
    fn security_price(&self, isin: Option<&str>, _symbol: Option<&str>) -> Option<(Decimal, String)> {
        (isin == Some("IE00B4L5Y983")).then(|| (d("100"), "EUR".to_string()))
    }
}

#[test]
fn test_valuation() {
    let mut acc = Account::default();
    acc.name = "Mix".into();
    acc.account_type = "crypto_exchange".into();
    acc.currency = "EUR".into();
    let mut btc = Holding::default();
    btc.symbol = Some("BTC".into());
    btc.quantity = d("0.1");
    btc.asset_class = "crypto".into();
    let mut usdc = Holding::default();
    usdc.symbol = Some("USDC".into());
    usdc.quantity = d("10");
    usdc.asset_class = "crypto".into();
    usdc.value = Some(d("10"));
    usdc.value_currency = Some("USD".into());
    let mut etf = Holding::default();
    etf.isin = Some("IE00B4L5Y983".into());
    etf.quantity = d("3");
    etf.asset_class = "etf".into();
    let holdings = vec![btc, usdc, etf];
    let prices = FakePrices;
    let mut v = Valuator::new(&prices);
    v.prefetch(&[(acc.clone(), holdings.clone())]);
    assert_eq!(v.account_value(&acc, &holdings).total(), d("5000") + d("9") + d("300"));
    let mut loan = Account::default();
    loan.name = "Prêt".into();
    loan.account_type = "mortgage".into();
    loan.currency = "EUR".into();
    loan.balance = d("1000");
    assert_eq!(v.account_value(&loan, &[]).total(), d("-1000"));
}

fn csrf(html: &str) -> String {
    Regex::new(r#"name="csrf" value="([^"]+)""#).unwrap().captures(html).unwrap()[1].to_string()
}

#[test]
fn test_web_setup_requires_token_and_sets_secure_headers() {
    let mut client = client();
    let r = client.get_raw("/");
    assert_eq!(r.header("location"), Some("/setup"));
    let r = client.post("/setup", &[("setup_token", "nope"), ("email", ADMIN_EMAIL), ("password", PASSWORD), ("confirm", PASSWORD)]);
    assert_eq!(r.status, 403);
    let token = client.core.setup_token.clone();
    let r = client.post("/setup", &[("setup_token", &token), ("email", ADMIN_EMAIL), ("password", PASSWORD), ("confirm", PASSWORD)]);
    assert!(r.status == 200 && r.text().to_lowercase().contains("code de secours"));
    let cookie = r.set_cookies().to_lowercase();
    assert!(cookie.contains("httponly") && cookie.contains("samesite=strict"));
    let r = client.get("/");
    assert!(r.header("content-security-policy").unwrap().contains("default-src 'none'"));
    assert!(r.header("x-frame-options") == Some("DENY") && r.header("cache-control") == Some("no-store"));
}

#[test]
fn test_web_requires_login_and_csrf() {
    let mut client = client();
    let core = client.core.clone();
    let token = core.setup_token.clone();
    client.post("/setup", &[("setup_token", &token), ("email", ADMIN_EMAIL), ("password", PASSWORD), ("confirm", PASSWORD)]);
    let page = client.get("/accounts/new");
    // Missing / wrong CSRF token
    assert_eq!(client.post("/accounts/new", &[("name", "X"), ("account_type", "checking")]).status, 403);
    assert_eq!(client.post("/accounts/new", &[("csrf", "bad"), ("name", "X")]).status, 403);
    // Cross-origin POST refused
    let r = client.post_with("/accounts/new", &[("csrf", &csrf(&page.text())), ("name", "X")], &[("origin", "https://evil.example")]);
    assert_eq!(r.status, 403);
    let r = client.post("/accounts/new", &[("csrf", &csrf(&page.text())), ("name", "Livret"), ("account_type", "livret_a"), ("balance", "100")]);
    assert!(r.status == 200 && r.text().contains("1,70"));

    // Locking wipes the key: every page goes back to the login screen.
    client.post("/logout", &[("csrf", &csrf(&r.text())), ("lock", "1")]);
    assert!(!core.vault.unlocked());
    assert_eq!(client.get_raw("/").header("location"), Some("/login"));
    assert_eq!(client.post("/login", &[("email", ADMIN_EMAIL), ("password", "Wrong-Password-1!")]).status, 401);
    assert_eq!(client.post("/login", &[("email", ADMIN_EMAIL), ("password", PASSWORD)]).status, 200);
    assert!(core.vault.unlocked());
}
