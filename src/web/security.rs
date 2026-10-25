//! Web sessions, CSRF protection and security headers.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use crate::app::token_urlsafe;
use crate::services::access::Identity;

pub const COOKIE: &str = "mm_session";

pub const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'self'; manifest-src 'self'; worker-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'";

pub const SECURITY_HEADERS: &[(&str, &str)] = &[
    ("Content-Security-Policy", CSP),
    ("X-Content-Type-Options", "nosniff"),
    ("X-Frame-Options", "DENY"),
    ("Referrer-Policy", "no-referrer"),
    ("Permissions-Policy", "camera=(), microphone=(), geolocation=(), payment=(), usb=()"),
    ("Cross-Origin-Opener-Policy", "same-origin"),
    ("Cross-Origin-Resource-Policy", "same-origin"),
    ("Cache-Control", "no-store"),
];

#[derive(Clone, Debug)]
pub struct WebSession {
    pub csrf: String,
    last_seen: Instant,
    /// Who is signed in (None only in the short step where a pre-users vault creates its first user).
    pub identity: Option<Identity>,
}

pub struct Sessions {
    idle: f64,
    items: Mutex<HashMap<String, WebSession>>,
}

impl Sessions {
    pub fn new(idle_minutes: i64) -> Sessions {
        Sessions { idle: (idle_minutes * 60) as f64, items: Mutex::new(HashMap::new()) }
    }

    pub fn create(&self, identity: Option<Identity>) -> String {
        let token = token_urlsafe(32);
        let session = WebSession { csrf: token_urlsafe(32), last_seen: Instant::now(), identity };
        self.items.lock().unwrap().insert(token.clone(), session);
        token
    }

    /// Sign a user out everywhere (their account was removed or changed).
    pub fn drop_user(&self, user_id: i64) {
        self.items.lock().unwrap().retain(|_, s| s.identity.as_ref().map(|i| i.id != user_id).unwrap_or(true));
    }

    pub fn update_identity(&self, identity: &Identity) {
        for s in self.items.lock().unwrap().values_mut() {
            if s.identity.as_ref().map(|i| i.id == identity.id).unwrap_or(false) {
                s.identity = Some(identity.clone());
            }
        }
    }

    pub fn set_identity(&self, token: &str, identity: Option<Identity>) {
        if let Some(s) = self.items.lock().unwrap().get_mut(token) {
            s.identity = identity;
        }
    }

    pub fn get(&self, token: Option<&str>) -> Option<WebSession> {
        let token = token.filter(|t| !t.is_empty())?;
        let mut items = self.items.lock().unwrap();
        let session = items.get_mut(token)?;
        if session.last_seen.elapsed().as_secs_f64() > self.idle {
            items.remove(token);
            return None;
        }
        session.last_seen = Instant::now();
        Some(session.clone())
    }

    pub fn discard(&self, token: Option<&str>) {
        self.items.lock().unwrap().remove(token.unwrap_or(""));
    }

    pub fn clear(&self) {
        self.items.lock().unwrap().clear();
    }

    pub fn active(&self) -> bool {
        let mut items = self.items.lock().unwrap();
        let idle = self.idle;
        items.retain(|_, s| s.last_seen.elapsed().as_secs_f64() <= idle);
        !items.is_empty()
    }
}

/// `hmac.compare_digest` on text.
pub fn same(a: &str, b: &str) -> bool {
    use subtle_eq::ct_eq;
    ct_eq(a.as_bytes(), b.as_bytes())
}

mod subtle_eq {
    pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
        if a.len() != b.len() {
            return false;
        }
        a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }
}
