//! A throwaway data directory with a core on it, and a browser-like client on the web application: the Rust side of
//! python/tests/conftest.py (`settings`, `core`) and of FastAPI's TestClient.

use std::sync::Arc;

use crate::app::MyMoney;
use crate::config::Settings;
use crate::web::app::Web;
use crate::web::http::{Request, Response};

pub const PASSWORD: &str = "Correct-Horse-9!";
pub const ADMIN_EMAIL: &str = "admin@example.com";

/// A temporary data directory, removed when dropped.
pub struct Dir(pub std::path::PathBuf);

impl Dir {
    pub fn new() -> Dir {
        let dir = std::env::temp_dir().join(format!("mymoney-test-{}", crate::app::token_hex(8)));
        std::fs::create_dir_all(&dir).unwrap();
        Dir(dir)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The `settings` fixture: a fresh directory, cheap Argon2 parameters to keep the tests fast.
pub fn settings(dir: &Dir) -> Settings {
    Settings {
        data_dir: dir.0.clone(),
        cookie_secure: false,
        auto_lock_minutes: 15,
        unlock_password_file: None,
        unlock_email: None,
        cf_team_domain: None,
        cf_audience: None,
        trusted_origins: String::new(),
        max_upload_mb: 25,
        argon2_memory_kib: 8192,
        argon2_time_cost: 1,
    }
}

/// The `core` fixture: a core whose vault is set up with a single master password (no user yet).
pub struct Core {
    pub core: Arc<MyMoney>,
    pub dir: Dir,
}

impl std::ops::Deref for Core {
    type Target = MyMoney;
    fn deref(&self) -> &MyMoney {
        &self.core
    }
}

pub fn core() -> Core {
    let dir = Dir::new();
    let core = MyMoney::new(settings(&dir)).unwrap();
    core.vault.setup(PASSWORD, None).unwrap();
    Core { core, dir }
}

/// A core on a fresh directory whose vault is not set up yet.
pub fn bare_core() -> Core {
    let dir = Dir::new();
    let core = MyMoney::new(settings(&dir)).unwrap();
    Core { core, dir }
}

pub struct Page {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Path and query of the last request (after the redirects).
    pub url: String,
}

impl Page {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
    pub fn set_cookies(&self) -> String {
        self.headers.iter().filter(|(k, _)| k == "set-cookie").map(|(_, v)| v.clone()).collect::<Vec<_>>().join("\n")
    }
    /// The first capture group of a regular expression over the body.
    pub fn find(&self, pattern: &str) -> Option<String> {
        regex::Regex::new(pattern).unwrap().captures(&self.text()).map(|c| c[1].to_string())
    }
}

/// The `client` fixture: the whole web application, with a cookie jar like a browser.
pub struct Client {
    pub web: Arc<Web>,
    pub core: Arc<MyMoney>,
    pub dir: Arc<Dir>,
    pub cookie: Option<String>,
    pub headers: Vec<(String, String)>,
}

pub fn client() -> Client {
    let dir = Dir::new();
    client_with(dir, |s| s)
}

pub fn client_with(dir: Dir, tweak: impl FnOnce(Settings) -> Settings) -> Client {
    let settings = tweak(settings(&dir));
    let core = MyMoney::new(settings).unwrap();
    let web = Web::new(core.clone()).unwrap();
    Client { web, core, dir: Arc::new(dir), cookie: None, headers: vec![] }
}

pub struct Upload<'a> {
    pub field: &'a str,
    pub filename: &'a str,
    pub data: &'a [u8],
    pub content_type: &'a str,
}

fn split_url(url: &str) -> (String, String) {
    let rest = url.strip_prefix("http://testserver").unwrap_or(url);
    let rest = rest.split('#').next().unwrap();
    match rest.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (rest.to_string(), String::new()),
    }
}

impl Client {
    /// Another browser on the same application (its own cookie).
    pub fn another(&self) -> Client {
        Client { web: self.web.clone(), core: self.core.clone(), dir: self.dir.clone(), cookie: None, headers: vec![] }
    }

    pub fn send(&mut self, method: &str, url: &str, body: Vec<u8>, content_type: Option<&str>, extra: &[(&str, &str)]) -> Page {
        let (path, query) = split_url(url);
        let mut headers = self.headers.clone();
        headers.push(("host".into(), "testserver".into()));
        for (k, v) in extra {
            headers.push((k.to_lowercase(), v.to_string()));
        }
        if let Some(c) = &self.cookie {
            headers.push(("cookie".into(), format!("mm_session={c}")));
        }
        if let Some(ct) = content_type {
            headers.push(("content-type".into(), ct.into()));
        }
        let resp: Response = self.web.handle(Request { peer: None, method: method.into(), path: path.clone(), query: query.clone(), headers, body });
        for (k, v) in &resp.headers {
            if k == "set-cookie" && v.starts_with("mm_session=") {
                let value = v.split(';').next().unwrap()["mm_session=".len()..].to_string();
                self.cookie = if value == "\"\"" || value.is_empty() { None } else { Some(value) };
            }
        }
        let full = if query.is_empty() { path } else { format!("{path}?{query}") };
        Page { status: resp.status, headers: resp.headers, body: resp.body, url: full }
    }

    fn follow(&mut self, mut page: Page, extra: &[(&str, &str)]) -> Page {
        for _ in 0..20 {
            match (page.status, page.header("location").map(|s| s.to_string())) {
                (301 | 302 | 303 | 307 | 308, Some(l)) => page = self.send("GET", &l, vec![], None, extra),
                _ => break,
            }
        }
        page
    }

    /// GET without following redirects.
    pub fn get_raw(&mut self, url: &str) -> Page {
        self.send("GET", url, vec![], None, &[])
    }

    /// GET, following the redirects (TestClient's default).
    pub fn get(&mut self, url: &str) -> Page {
        let page = self.get_raw(url);
        self.follow(page, &[])
    }

    pub fn get_with(&mut self, url: &str, extra: &[(&str, &str)]) -> Page {
        let page = self.send("GET", url, vec![], None, extra);
        self.follow(page, extra)
    }

    fn form_body(form: &[(&str, &str)]) -> Vec<u8> {
        crate::py::urlencode(&form.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Vec<_>>()).into_bytes()
    }

    /// POST a form without following redirects.
    pub fn post_raw(&mut self, url: &str, form: &[(&str, &str)]) -> Page {
        self.send("POST", url, Self::form_body(form), Some("application/x-www-form-urlencoded"), &[])
    }

    /// POST a form, following the redirects.
    pub fn post(&mut self, url: &str, form: &[(&str, &str)]) -> Page {
        let page = self.post_raw(url, form);
        self.follow(page, &[])
    }

    pub fn post_with(&mut self, url: &str, form: &[(&str, &str)], extra: &[(&str, &str)]) -> Page {
        let page = self.send("POST", url, Self::form_body(form), Some("application/x-www-form-urlencoded"), extra);
        self.follow(page, extra)
    }

    pub fn post_files_raw(&mut self, url: &str, form: &[(&str, &str)], files: &[Upload]) -> Page {
        let boundary = "----mymoneytestboundary";
        let mut body: Vec<u8> = vec![];
        for (k, v) in form {
            body.extend(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n").bytes());
        }
        for f in files {
            body.extend(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\nContent-Type: {}\r\n\r\n", f.field, f.filename, f.content_type).bytes());
            body.extend(f.data);
            body.extend(b"\r\n");
        }
        body.extend(format!("--{boundary}--\r\n").bytes());
        self.send("POST", url, body, Some(&format!("multipart/form-data; boundary={boundary}")), &[])
    }

    pub fn post_files(&mut self, url: &str, form: &[(&str, &str)], files: &[Upload]) -> Page {
        let page = self.post_files_raw(url, form, files);
        self.follow(page, &[])
    }

    /// Set up the first administrator through the web form, as the Python tests' `login` helpers do. Returns the CSRF token
    /// of the documents page.
    pub fn login(&mut self) -> String {
        let token = self.core.setup_token.clone();
        self.post("/setup", &[("setup_token", &token), ("email", ADMIN_EMAIL), ("password", PASSWORD), ("confirm", PASSWORD)]);
        csrf_of(&self.get("/documents").text())
    }
}

pub fn csrf_of(html: &str) -> String {
    regex::Regex::new(r#"name="csrf" value="([^"]+)""#).unwrap().captures(html).expect("a csrf field").get(1).unwrap().as_str().to_string()
}
