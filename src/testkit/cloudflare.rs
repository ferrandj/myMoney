//! What Cloudflare is, for the tests: a key pair, the published keys, and tokens signed with it.

use base64::Engine;
use rsa::traits::PublicKeyParts;
use rsa::{Pkcs1v15Sign, RsaPrivateKey};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const TEAM: &str = "casa";
pub const BOB: &str = "bob@example.com";
pub const BOB_PASSWORD: &str = "Another-Strong-Pass-7";

pub fn aud() -> String {
    "a".repeat(64)
}

pub fn iss() -> String {
    format!("https://{TEAM}.cloudflareaccess.com")
}

pub struct Signer {
    key: RsaPrivateKey,
    pub kid: String,
}

pub fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// The options of a token (the Python `Signer.token` keyword arguments).
pub struct Opts<'a> {
    pub email: Option<&'a str>,
    pub aud: String,
    pub iss: String,
    pub exp: i64,
    pub kid: Option<&'a str>,
}

impl<'a> Opts<'a> {
    pub fn new(email: &'a str) -> Opts<'a> {
        Opts { email: Some(email), aud: aud(), iss: iss(), exp: 300, kid: None }
    }
}

impl Signer {
    pub fn new(kid: &str) -> Signer {
        Signer { key: RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap(), kid: kid.into() }
    }

    pub fn jwk(&self) -> Value {
        let p = self.key.to_public_key();
        json!({"kid": self.kid, "kty": "RSA", "alg": "RS256", "use": "sig", "n": b64(&p.n().to_bytes_be()), "e": b64(&p.e().to_bytes_be())})
    }

    pub fn token_with(&self, o: Opts) -> String {
        let now = chrono::Utc::now().timestamp();
        let mut claims = json!({"aud": [o.aud], "iss": o.iss, "iat": now, "exp": now + o.exp, "type": "app"});
        if let Some(e) = o.email {
            claims["email"] = e.into();
        }
        let header = json!({"alg": "RS256", "typ": "JWT", "kid": o.kid.unwrap_or(&self.kid)});
        let input = format!("{}.{}", b64(&serde_json::to_vec(&header).unwrap()), b64(&serde_json::to_vec(&claims).unwrap()));
        let sig = self.key.sign(Pkcs1v15Sign::new::<Sha256>(), &Sha256::digest(input.as_bytes())).unwrap();
        format!("{input}.{}", b64(&sig))
    }

    pub fn token(&self, email: &str) -> String {
        self.token_with(Opts::new(email))
    }
}
