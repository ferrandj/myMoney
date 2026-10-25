//! The Cloudflare Access scenarios of the Python version's tests (tests/test_cloudflare.py), through the whole web
//! application: a signed token signs its user in, nothing else does.

use std::sync::Arc;

use base64::Engine;
use rsa::traits::PublicKeyParts;
use rsa::{Pkcs1v15Sign, RsaPrivateKey};
use sha2::{Digest, Sha256};

use super::app::Web;
use super::cloudflare::CloudflareAccess;
use super::http::{Request, Response};
use crate::app::MyMoney;
use crate::config::Settings;

const TEAM: &str = "casa";
const ADMIN: &str = "admin@example.com";
const PASSWORD: &str = "Correct-Horse-Battery-9";
const BOB: &str = "bob@example.com";
const BOB_PASSWORD: &str = "Another-Strong-Pass-7";

fn aud() -> String {
    "a".repeat(64)
}

struct Signer {
    key: RsaPrivateKey,
    kid: &'static str,
}

fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

impl Signer {
    fn new(kid: &'static str) -> Signer {
        Signer { key: RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap(), kid }
    }
    fn jwk(&self) -> serde_json::Value {
        let p = self.key.to_public_key();
        serde_json::json!({"kid": self.kid, "kty": "RSA", "alg": "RS256", "use": "sig", "n": b64(&p.n().to_bytes_be()), "e": b64(&p.e().to_bytes_be())})
    }
    fn token_with(&self, email: Option<&str>, aud: &str, exp: i64) -> String {
        let now = chrono::Utc::now().timestamp();
        let mut claims = serde_json::json!({"aud": [aud], "iss": format!("https://{TEAM}.cloudflareaccess.com"), "iat": now, "exp": now + exp, "type": "app"});
        if let Some(e) = email {
            claims["email"] = e.into();
        }
        let header = serde_json::json!({"alg": "RS256", "typ": "JWT", "kid": self.kid});
        let input = format!("{}.{}", b64(&serde_json::to_vec(&header).unwrap()), b64(&serde_json::to_vec(&claims).unwrap()));
        let sig = self.key.sign(Pkcs1v15Sign::new::<Sha256>(), &Sha256::digest(input.as_bytes())).unwrap();
        format!("{input}.{}", b64(&sig))
    }
    fn token(&self, email: &str) -> String {
        self.token_with(Some(email), &aud(), 300)
    }
}

struct App {
    web: Arc<Web>,
    _dir: std::path::PathBuf,
}

fn app(signer: &Signer, cf_on: bool, trusted_origins: &str) -> App {
    let dir = std::env::temp_dir().join(format!("mymoney-cf-{}", crate::app::token_hex(8)));
    let settings = Settings {
        data_dir: dir.clone(),
        cookie_secure: false,
        auto_lock_minutes: 15,
        unlock_password_file: None,
        unlock_email: None,
        cf_team_domain: cf_on.then(|| TEAM.to_string()),
        cf_audience: cf_on.then(aud),
        trusted_origins: trusted_origins.into(),
        max_upload_mb: 25,
        argon2_memory_kib: 8192,
        argon2_time_cost: 1,
    };
    let core = MyMoney::new(settings.clone()).unwrap();
    let jwk = signer.jwk();
    let cf = CloudflareAccess::with_fetcher(settings.cf_team_domain.as_deref(), settings.cf_audience.as_deref(), Box::new(move |_| Ok(vec![jwk.clone()])));
    App { web: Web::with_cloudflare(core, cf).unwrap(), _dir: dir }
}

impl Drop for App {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self._dir);
    }
}

/// A browser: its own cookie, and the headers it adds to every request (Cloudflare's token).
struct Browser<'a> {
    app: &'a App,
    cookie: Option<String>,
    headers: Vec<(String, String)>,
}

impl<'a> Browser<'a> {
    fn new(app: &'a App, headers: &[(&str, &str)]) -> Browser<'a> {
        Browser { app, cookie: None, headers: headers.iter().map(|(k, v)| (k.to_lowercase(), v.to_string())).collect() }
    }
    fn send(&mut self, method: &str, url: &str, form: &[(&str, &str)], extra: &[(&str, &str)]) -> Response {
        let (path, query) = url.split_once('?').map(|(p, q)| (p.to_string(), q.to_string())).unwrap_or((url.to_string(), String::new()));
        let mut headers = self.headers.clone();
        headers.push(("host".into(), "testserver".into()));
        for (k, v) in extra {
            headers.push((k.to_lowercase(), v.to_string()));
        }
        if let Some(c) = &self.cookie {
            headers.push(("cookie".into(), format!("mm_session={c}")));
        }
        let mut body = vec![];
        if method == "POST" {
            headers.push(("content-type".into(), "application/x-www-form-urlencoded".into()));
            body = crate::py::urlencode(&form.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Vec<_>>()).into_bytes();
        }
        let resp = self.app.web.handle(Request { peer: None, method: method.into(), path, query, headers, body });
        for (k, v) in &resp.headers {
            if k == "set-cookie" && v.starts_with("mm_session=") {
                let value = v.split(';').next().unwrap()["mm_session=".len()..].to_string();
                self.cookie = if value == "\"\"" { None } else { Some(value) };
            }
        }
        resp
    }
    fn get(&mut self, url: &str) -> Response {
        self.send("GET", url, &[], &[])
    }
    fn post(&mut self, url: &str, form: &[(&str, &str)]) -> Response {
        self.send("POST", url, form, &[])
    }
    /// Follow redirects like TestClient does by default.
    fn get_page(&mut self, url: &str) -> Response {
        let mut r = self.get(url);
        for _ in 0..5 {
            match (r.status, r.header("location").map(|s| s.to_string())) {
                (303 | 307, Some(l)) => r = self.get(&l),
                _ => break,
            }
        }
        r
    }
}

fn text(r: &Response) -> String {
    String::from_utf8_lossy(&r.body).into_owned()
}

fn csrf_of(page: &str) -> String {
    let i = page.find("name=\"csrf\" value=\"").unwrap() + 19;
    page[i..].split('"').next().unwrap().to_string()
}

fn cookie_header(r: &Response) -> String {
    r.headers.iter().filter(|(k, _)| k == "set-cookie").map(|(_, v)| v.clone()).collect::<Vec<_>>().join("\n")
}

fn setup_vault(app: &App) {
    let mut b = Browser::new(app, &[]);
    let token = app.web.core.setup_token.clone();
    b.post("/setup", &[("setup_token", &token), ("email", ADMIN), ("name", "Alice"), ("password", PASSWORD), ("confirm", PASSWORD)]);
    let csrf = csrf_of(&text(&b.get("/settings")));
    b.post("/settings/users", &[("csrf", &csrf), ("email", BOB), ("name", "Bob"), ("password", BOB_PASSWORD)]);
}

#[test]
fn cloudflare_through_the_web_app() {
    let signer = Signer::new("key-1");
    let t_alice = signer.token(ADMIN);
    let t_bob = signer.token(BOB);

    // a visitor vouched for by Cloudflare enters without a password
    let a = app(&signer, true, "");
    setup_vault(&a);
    let mut alice = Browser::new(&a, &[("cf-access-jwt-assertion", &t_alice)]);
    let home = alice.get("/");
    assert!(home.status == 200 && text(&home).contains("Alice") && cookie_header(&home).contains("mm_session"));
    assert_eq!(alice.get("/login").header("location"), Some("/"));
    let mut bob = Browser::new(&a, &[("cf-access-jwt-assertion", &t_bob)]);
    assert_eq!(bob.get_page("/settings").status, 200);
    assert!(!text(&bob.get_page("/settings")).contains("Ajouter une personne") && text(&bob.get_page("/")).contains("Bob"));

    // the identity follows the token, not an old session
    let mut browser = Browser::new(&a, &[("cf-access-jwt-assertion", &t_alice)]);
    assert!(text(&browser.get_page("/settings")).contains("Ajouter une personne"));
    browser.headers = vec![("cf-access-jwt-assertion".into(), t_bob.clone())];
    assert!(!text(&browser.get_page("/settings")).contains("Ajouter une personne"));

    // an e-mail without an account is turned away
    let t_stranger = signer.token("stranger@example.com");
    let mut stranger = Browser::new(&a, &[("cf-access-jwt-assertion", &t_stranger)]);
    let denied = stranger.get("/");
    assert!(denied.status == 403 && text(&denied).contains("stranger@example.com") && text(&denied).contains("pas de compte"));
    assert!(!cookie_header(&denied).contains("mm_session"));

    // a forged header or a wrong token signs nobody in
    let mut forged = Browser::new(&a, &[("cf-access-authenticated-user-email", ADMIN)]);
    assert_eq!(forged.get("/").header("location"), Some("/login"));
    let wrong = signer.token_with(Some(ADMIN), &"b".repeat(64), 300);
    let mut wrong_app = Browser::new(&a, &[("cf-access-jwt-assertion", &wrong)]);
    assert_eq!(wrong_app.get("/").header("location"), Some("/login"));
    let expired = signer.token_with(Some(ADMIN), &aud(), -100);
    let mut expired_b = Browser::new(&a, &[("cf-access-jwt-assertion", &expired)]);
    assert_eq!(expired_b.get("/").header("location"), Some("/login"));
    let service = signer.token_with(None, &aud(), 300);
    let mut service_b = Browser::new(&a, &[("cf-access-jwt-assertion", &service)]);
    assert_eq!(service_b.get("/").header("location"), Some("/login"));

    // direct access on the LAN still works with the password
    let mut direct = Browser::new(&a, &[]);
    assert_eq!(direct.post("/login", &[("email", ADMIN), ("password", PASSWORD)]).status, 303);
    assert_eq!(direct.get_page("/").status, 200);

    // logout through Cloudflare ends the Access session too
    let mut bob2 = Browser::new(&a, &[("cf-access-jwt-assertion", &t_bob)]);
    let csrf = csrf_of(&text(&bob2.get_page("/settings")));
    let out = bob2.post("/logout", &[("csrf", &csrf)]);
    assert!(out.status == 303 && out.header("location") == Some("/cdn-cgi/access/logout"));
    let mut direct2 = Browser::new(&a, &[]);
    direct2.post("/login", &[("email", BOB), ("password", BOB_PASSWORD)]);
    let csrf = csrf_of(&text(&direct2.get_page("/settings")));
    assert!(direct2.post("/logout", &[("csrf", &csrf)]).header("location").unwrap().starts_with("/login"));

    // the cookie is Secure behind an HTTPS tunnel
    let mut tunnel = Browser::new(&a, &[("cf-access-jwt-assertion", &t_bob), ("x-forwarded-proto", "https")]);
    let r = tunnel.get("/");
    let cookie = cookie_header(&r).to_lowercase();
    assert!(cookie.contains("secure") && cookie.contains("httponly") && cookie.contains("samesite=strict"));
    assert!(r.header("strict-transport-security").is_some());
    let mut plain = Browser::new(&a, &[("cf-access-jwt-assertion", &t_bob)]);
    assert!(!cookie_header(&plain.get("/")).to_lowercase().contains("secure"));

    // with the vault locked Cloudflare cannot open it, but prefills the login
    a.web.core.vault.lock();
    let mut alice3 = Browser::new(&a, &[("cf-access-jwt-assertion", &t_alice)]);
    assert_eq!(alice3.get("/").header("location"), Some("/login"));
    let page = text(&alice3.get("/login"));
    assert!(page.contains("Cloudflare te reconnaît") && page.contains(&format!("value=\"{ADMIN}\"")) && page.contains("Coffre verrouillé"));
    assert_eq!(alice3.post("/login", &[("email", ADMIN), ("password", PASSWORD)]).status, 303);
    assert!(a.web.core.vault.unlocked());
    assert_eq!(Browser::new(&a, &[("cf-access-jwt-assertion", &t_bob)]).get("/").status, 200);
}

#[test]
fn public_origin_and_disabled_cloudflare() {
    let signer = Signer::new("key-1");
    let t_bob = signer.token(BOB);
    let a = app(&signer, true, "https://mymoney.example.com");
    setup_vault(&a);
    let mut browser = Browser::new(&a, &[("cf-access-jwt-assertion", &t_bob)]);
    let csrf = csrf_of(&text(&browser.get_page("/settings")));
    let form = [("csrf", csrf.as_str()), ("current", BOB_PASSWORD), ("new", BOB_PASSWORD), ("confirm", BOB_PASSWORD)];
    let ok = browser.send("POST", "/settings/password", &form, &[("origin", "https://mymoney.example.com")]);
    assert_ne!(ok.status, 403);
    let evil = browser.send("POST", "/settings/password", &form, &[("origin", "https://evil.example")]);
    assert_eq!(evil.status, 403);

    // without Cloudflare settings, tokens are ignored
    let b = app(&signer, false, "");
    let mut client = Browser::new(&b, &[]);
    let token = b.web.core.setup_token.clone();
    client.post("/setup", &[("setup_token", &token), ("email", ADMIN), ("password", PASSWORD), ("confirm", PASSWORD)]);
    let t_admin = signer.token(ADMIN);
    let mut stranger = Browser::new(&b, &[("cf-access-jwt-assertion", &t_admin)]);
    assert_eq!(stranger.get("/").header("location"), Some("/login"));
    assert!(text(&client.get_page("/settings")).contains("Désactivé"));
}

#[test]
fn a_vault_from_before_users_upgraded_through_cloudflare() {
    let signer = Signer::new("key-1");
    let a = app(&signer, true, "");
    // the old layout: one master password, no user
    a.web.core.vault.setup(PASSWORD, None).unwrap();
    a.web.core.save_account(None, &[("name".to_string(), "Ancien compte".to_string()), ("account_type".into(), "checking".into()), ("balance".into(), "42".into())].into_iter().collect()).unwrap();
    a.web.core.vault.lock();
    let t_admin = signer.token(ADMIN);
    let mut browser = Browser::new(&a, &[("cf-access-jwt-assertion", &t_admin)]);
    assert!(text(&browser.get("/login")).contains("Mot de passe maître"));
    assert_eq!(browser.post("/login", &[("password", PASSWORD)]).header("location"), Some("/first-user"));
    let page = browser.get("/first-user");
    assert!(page.status == 200 && text(&page).contains(&format!("value=\"{ADMIN}\"")));
    let csrf = csrf_of(&text(&page));
    let r = browser.post("/first-user", &[("csrf", &csrf), ("email", ADMIN), ("name", "Alice")]);
    let done = browser.get_page(r.header("location").unwrap());
    assert!(text(&done).contains("Ancien compte"));
    assert_eq!(Browser::new(&a, &[("cf-access-jwt-assertion", &t_admin)]).get("/").status, 200);
}
