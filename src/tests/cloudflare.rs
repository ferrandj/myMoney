//! python/tests/test_cloudflare.py: Cloudflare Access, the signed token names the visitor; nothing else does.

use std::sync::{Arc, Mutex, OnceLock};

use serde_json::json;

use crate::app::MyMoney;
use crate::testkit::cloudflare::*;
use crate::testkit::env::{csrf_of, settings, Client, Dir, ADMIN_EMAIL, PASSWORD};
use crate::testkit::form;
use crate::web::app::Web;
use crate::web::cloudflare::{team_host, CloudflareAccess};

fn signer() -> &'static Signer {
    static SIGNER: OnceLock<Signer> = OnceLock::new();
    SIGNER.get_or_init(|| Signer::new("key-1"))
}

fn access() -> CloudflareAccess {
    let jwk = signer().jwk();
    CloudflareAccess::with_fetcher(Some(TEAM), Some(&aud()), Box::new(move |_| Ok(vec![jwk.clone()])))
}

// ------------------------------------------------------------------------------------- the verification
#[test]
fn test_team_name_forms_are_equivalent() {
    for value in ["casa", "Casa.cloudflareaccess.com", "https://casa.cloudflareaccess.com/", " casa "] {
        assert_eq!(team_host(value), "casa.cloudflareaccess.com", "{value}");
    }
    let off = |team: Option<&str>, aud: Option<&str>| !CloudflareAccess::new(team, aud).enabled();
    assert!(off(None, Some(&aud())) && off(Some(TEAM), None) && off(Some(""), Some("")));
    assert!(CloudflareAccess::new(Some(TEAM), Some(&aud())).enabled());
}

#[test]
fn test_a_valid_token_names_the_visitor() {
    let cf = access();
    assert_eq!(cf.verify(Some(&signer().token("Admin@Example.com"))), Some("admin@example.com".into()));
    assert_eq!(cf.email_of(&[("cf-access-jwt-assertion".into(), signer().token(BOB))]), Some(BOB.into()));
}

#[test]
fn test_everything_else_is_refused() {
    let cf = access();
    let s = signer();
    let other = Signer::new("key-1"); // same key id, another key pair: a forged signature
    let with = |f: &dyn Fn(&mut Opts)| {
        let mut o = Opts::new("admin@example.com");
        f(&mut o);
        s.token_with(o)
    };
    let tokens: Vec<Option<String>> = vec![
        None,
        Some("".into()),
        Some("garbage".into()),
        Some("a.b.c".into()),
        Some(with(&|o| o.aud = "b".repeat(64))), // a token for another Access application
        Some(with(&|o| o.iss = "https://evil.cloudflareaccess.com".into())), // another team
        Some(with(&|o| o.exp = -3600)), // expired
        Some(other.token("admin@example.com")), // not signed by Cloudflare's key
        Some(with(&|o| o.email = None)), // a service token: no person behind it
        Some(with(&|o| o.email = Some("not-an-email"))),
        Some(with(&|o| o.kid = Some("unknown-key"))),
    ];
    for token in tokens {
        assert_eq!(cf.verify(token.as_deref()), None, "{token:?}");
    }
    let enc = |v: serde_json::Value| b64(&serde_json::to_vec(&v).unwrap());
    let claims = json!({"aud": [aud()], "iss": iss(), "exp": chrono::Utc::now().timestamp() + 300, "email": ADMIN_EMAIL});
    // The classic trick: sign with HMAC using the public key as the secret, hoping the server accepts "HS256".
    let forged = format!("{}.{}.{}", enc(json!({"alg": "HS256", "typ": "JWT", "kid": "key-1"})), enc(claims.clone()), b64(b"not a real mac"));
    assert_eq!(cf.verify(Some(&forged)), None);
    // And an unsigned token.
    let unsigned = format!("{}.{}.", enc(json!({"alg": "none", "typ": "JWT", "kid": "key-1"})), enc(claims));
    assert_eq!(cf.verify(Some(&unsigned)), None);
}

#[test]
fn test_disabled_means_every_token_is_ignored() {
    let jwk = signer().jwk();
    let cf = CloudflareAccess::with_fetcher(None, None, Box::new(move |_| Ok(vec![jwk.clone()])));
    assert_eq!(cf.verify(Some(&signer().token("admin@example.com"))), None);
}

#[test]
fn test_keys_are_cached_refreshed_for_a_new_key_and_survive_an_outage() {
    let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
    let new = Signer::new("key-2");
    let published = Arc::new(Mutex::new(vec![signer().jwk()]));
    let make = |calls: Arc<Mutex<Vec<String>>>, published: Arc<Mutex<Vec<serde_json::Value>>>| -> crate::web::cloudflare::Fetcher {
        Box::new(move |url| {
            calls.lock().unwrap().push(url.to_string());
            if url.starts_with("https://down") {
                return Err("unreachable".into());
            }
            Ok(published.lock().unwrap().clone())
        })
    };
    let cf = CloudflareAccess::with_fetcher(Some(TEAM), Some(&aud()), make(calls.clone(), published.clone()));
    assert!(cf.verify(Some(&signer().token("admin@example.com"))).is_some() && cf.verify(Some(&signer().token("admin@example.com"))).is_some());
    assert_eq!(calls.lock().unwrap().len(), 1); // cached
    assert_eq!(calls.lock().unwrap()[0], format!("{}/cdn-cgi/access/certs", iss()));
    assert_eq!(cf.verify(Some(&new.token("admin@example.com"))), None); // key-2 is not published yet
    published.lock().unwrap().push(new.jwk());
    cf.age_cache(60); // (the look-up for unknown keys is rate-limited)
    assert_eq!(cf.verify(Some(&new.token("admin@example.com"))), Some(ADMIN_EMAIL.into()));
    assert_eq!(calls.lock().unwrap().len(), 2);
    let down = CloudflareAccess::with_fetcher(Some("down"), Some(&aud()), make(calls.clone(), published));
    let mut o = Opts::new("admin@example.com");
    o.iss = "https://down.cloudflareaccess.com".into();
    assert_eq!(down.verify(Some(&signer().token_with(o))), None); // no crash: the password still works
}

// ------------------------------------------------------------------------------------------------- web
struct Web2 {
    web: Arc<Web>,
    core: Arc<MyMoney>,
    dir: Arc<Dir>,
}

fn web_app(cf_on: bool, trusted_origins: &str) -> Web2 {
    let dir = Dir::new();
    let mut st = settings(&dir);
    st.trusted_origins = trusted_origins.into();
    if cf_on {
        st.cf_team_domain = Some(TEAM.into());
        st.cf_audience = Some(aud());
    }
    let core = MyMoney::new(st.clone()).unwrap();
    let jwk = signer().jwk();
    let cf = CloudflareAccess::with_fetcher(st.cf_team_domain.as_deref(), st.cf_audience.as_deref(), Box::new(move |_| Ok(vec![jwk.clone()])));
    Web2 { web: Web::with_cloudflare(core.clone(), cf).unwrap(), core, dir: Arc::new(dir) }
}

impl Web2 {
    /// A browser of its own, with the headers it adds to every request.
    fn browser(&self, headers: &[(&str, &str)]) -> Client {
        Client { web: self.web.clone(), core: self.core.clone(), dir: self.dir.clone(), cookie: None, headers: headers.iter().map(|(k, v)| (k.to_lowercase(), v.to_string())).collect() }
    }

    /// A browser coming through Cloudflare: its own cookies, and the token Cloudflare adds to every request.
    fn via_cloudflare(&self, email: &str, extra: &[(&str, &str)]) -> Client {
        let token = signer().token(email);
        let mut headers = vec![("cf-access-jwt-assertion", token.as_str())];
        headers.extend_from_slice(extra);
        self.browser(&headers)
    }
}

fn setup_vault(app: &Web2) {
    let mut client = app.browser(&[]);
    let token = app.core.setup_token.clone();
    client.post("/setup", &[("setup_token", &token), ("email", ADMIN_EMAIL), ("name", "Alice"), ("password", PASSWORD), ("confirm", PASSWORD)]);
    let csrf = csrf_of(&client.get("/settings").text());
    client.post("/settings/users", &[("csrf", &csrf), ("email", BOB), ("name", "Bob"), ("password", BOB_PASSWORD)]);
}

#[test]
fn test_a_visitor_vouched_for_by_cloudflare_enters_without_a_password() {
    let app = web_app(true, "");
    setup_vault(&app);
    let mut alice = app.via_cloudflare(ADMIN_EMAIL, &[]);
    let home = alice.get_raw("/");
    assert!(home.status == 200 && home.text().contains("Alice") && home.set_cookies().contains("mm_session"));
    assert_eq!(alice.get_raw("/login").header("location"), Some("/")); // nothing to type
    let mut bob = app.via_cloudflare(BOB, &[]);
    assert_eq!(bob.get("/settings").status, 200);
    assert!(!bob.get("/settings").text().contains("Ajouter une personne") && bob.get("/").text().contains("Bob")); // signed in as Bob, not as Alice
}

#[test]
fn test_the_identity_follows_the_token_not_an_old_session() {
    let app = web_app(true, "");
    setup_vault(&app);
    let mut browser = app.via_cloudflare(ADMIN_EMAIL, &[]);
    assert!(browser.get("/settings").text().contains("Ajouter une personne"));
    browser.headers = vec![("cf-access-jwt-assertion".into(), signer().token(BOB))]; // someone else signs in on the same browser
    assert!(!browser.get("/settings").text().contains("Ajouter une personne"));
}

#[test]
fn test_an_email_without_an_account_is_turned_away() {
    let app = web_app(true, "");
    setup_vault(&app);
    let mut stranger = app.via_cloudflare("stranger@example.com", &[]);
    let denied = stranger.get_raw("/");
    assert!(denied.status == 403 && denied.text().contains("stranger@example.com") && denied.text().contains("pas de compte"));
    assert!(!denied.set_cookies().contains("mm_session"));
}

#[test]
fn test_a_forged_header_or_a_wrong_token_signs_nobody_in() {
    let app = web_app(true, "");
    setup_vault(&app);
    let mut forged = app.browser(&[("cf-access-authenticated-user-email", ADMIN_EMAIL)]); // what anyone on the LAN can send
    assert_eq!(forged.get_raw("/").header("location"), Some("/login"));
    let mut wrong = Opts::new(ADMIN_EMAIL);
    wrong.aud = "b".repeat(64);
    let mut wrong_app = app.browser(&[("cf-access-jwt-assertion", &signer().token_with(wrong))]);
    assert_eq!(wrong_app.get_raw("/").header("location"), Some("/login"));
    let mut old = Opts::new(ADMIN_EMAIL);
    old.exp = -100;
    let mut expired = app.browser(&[("cf-access-jwt-assertion", &signer().token_with(old))]);
    assert_eq!(expired.get_raw("/").header("location"), Some("/login"));
}

#[test]
fn test_direct_access_on_the_lan_still_works_with_the_password() {
    let app = web_app(true, "");
    setup_vault(&app);
    let mut direct = app.browser(&[]);
    assert_eq!(direct.post_raw("/login", &[("email", ADMIN_EMAIL), ("password", PASSWORD)]).status, 303);
    assert_eq!(direct.get("/").status, 200);
}

#[test]
fn test_with_the_vault_locked_cloudflare_cannot_open_it_but_prefills_the_login() {
    let app = web_app(true, "");
    setup_vault(&app);
    app.core.vault.lock();
    let mut alice = app.via_cloudflare(ADMIN_EMAIL, &[]);
    assert_eq!(alice.get_raw("/").header("location"), Some("/login"));
    let page = alice.get("/login").text();
    assert!(page.contains("Cloudflare te reconnaît") && page.contains(&format!("value=\"{ADMIN_EMAIL}\"")) && page.contains("Coffre verrouillé"));
    assert_eq!(alice.post_raw("/login", &[("email", ADMIN_EMAIL), ("password", PASSWORD)]).status, 303);
    assert!(app.core.vault.unlocked());
    // Once open, the others come in by themselves.
    assert_eq!(app.via_cloudflare(BOB, &[]).get("/").status, 200);
}

#[test]
fn test_logout_through_cloudflare_ends_the_access_session_too() {
    let app = web_app(true, "");
    setup_vault(&app);
    let mut bob = app.via_cloudflare(BOB, &[]);
    let csrf = csrf_of(&bob.get("/settings").text());
    let out = bob.post_raw("/logout", &[("csrf", &csrf)]);
    assert!(out.status == 303 && out.header("location") == Some("/cdn-cgi/access/logout"));
    let mut direct = app.browser(&[]); // not through Cloudflare: back to the login page
    direct.post("/login", &[("email", BOB), ("password", BOB_PASSWORD)]);
    let csrf = csrf_of(&direct.get("/settings").text());
    assert!(direct.post_raw("/logout", &[("csrf", &csrf)]).header("location").unwrap().starts_with("/login"));
}

#[test]
fn test_cookie_is_secure_behind_an_https_tunnel_and_the_public_origin_is_accepted() {
    let app = web_app(true, "https://mymoney.example.com");
    setup_vault(&app);
    // The tunnel talks plain http to the app but tells it the browser used https.
    let mut tunnel = app.via_cloudflare(BOB, &[("x-forwarded-proto", "https")]);
    let first = tunnel.get_raw("/");
    let cookie = first.set_cookies().to_lowercase();
    assert!(cookie.contains("secure") && cookie.contains("httponly") && cookie.contains("samesite=strict"));
    assert!(tunnel.get_raw("/").header("strict-transport-security").is_some());
    let mut plain = app.via_cloudflare(BOB, &[]); // plain http on the LAN: no Secure flag, or the browser would drop the cookie
    assert!(!plain.get_raw("/").set_cookies().to_lowercase().contains("secure"));
    // A form posted from the public address is accepted even though the Host the app sees is another one.
    let mut browser = app.via_cloudflare(BOB, &[]);
    let csrf = csrf_of(&browser.get("/settings").text());
    let fields = [("csrf", csrf.as_str()), ("current", BOB_PASSWORD), ("new", BOB_PASSWORD), ("confirm", BOB_PASSWORD)];
    assert_eq!(browser.post_with("/settings/password", &fields, &[("origin", "https://mymoney.example.com")]).status, 200);
    assert_eq!(browser.post_with("/settings/password", &fields, &[("origin", "https://evil.example")]).status, 403);
}

#[test]
fn test_without_cloudflare_settings_tokens_are_ignored() {
    let app = web_app(false, "");
    let mut client = app.browser(&[]);
    let token = app.core.setup_token.clone();
    client.post("/setup", &[("setup_token", &token), ("email", ADMIN_EMAIL), ("password", PASSWORD), ("confirm", PASSWORD)]);
    let mut stranger = app.via_cloudflare(ADMIN_EMAIL, &[]);
    assert_eq!(stranger.get_raw("/").header("location"), Some("/login"));
    assert!(client.get("/settings").text().contains("Désactivé"));
}

#[test]
fn test_a_vault_from_before_users_can_be_upgraded_through_cloudflare() {
    let dir = Dir::new();
    let mut st = settings(&dir);
    let old = MyMoney::new(st.clone()).unwrap();
    old.vault.setup(PASSWORD, None).unwrap();
    old.save_account(None, &form(&[("name", "Ancien compte"), ("account_type", "checking"), ("balance", "42")])).unwrap();
    old.vault.lock();
    drop(old);
    st.cf_team_domain = Some(TEAM.into());
    st.cf_audience = Some(aud());
    let core = MyMoney::new(st.clone()).unwrap();
    let jwk = signer().jwk();
    let cf = CloudflareAccess::with_fetcher(st.cf_team_domain.as_deref(), st.cf_audience.as_deref(), Box::new(move |_| Ok(vec![jwk.clone()])));
    let app = Web2 { web: Web::with_cloudflare(core.clone(), cf).unwrap(), core, dir: Arc::new(dir) };
    let mut browser = app.via_cloudflare(ADMIN_EMAIL, &[]);
    assert!(browser.get("/login").text().contains("Mot de passe maître"));
    assert_eq!(browser.post_raw("/login", &[("password", PASSWORD)]).header("location"), Some("/first-user"));
    let page = browser.get("/first-user"); // not turned away: nobody has an account yet, and Cloudflare's e-mail is offered
    assert!(page.status == 200 && page.text().contains(&format!("value=\"{ADMIN_EMAIL}\"")));
    let done = browser.post("/first-user", &[("csrf", &csrf_of(&page.text())), ("email", ADMIN_EMAIL), ("name", "Alice")]);
    assert!(done.text().contains("Ancien compte"));
    assert_eq!(app.via_cloudflare(ADMIN_EMAIL, &[]).get("/").status, 200); // and from now on, Cloudflare is enough
}
