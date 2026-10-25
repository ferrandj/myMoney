//! Cloudflare Access: who is the person behind this request?
//!
//! With an Access application in front of the app, Cloudflare authenticates the visitor and adds to every request a
//! signed token in the `Cf-Access-Jwt-Assertion` header. Only a token whose RS256 signature checks out against
//! Cloudflare's published keys, for this very application (`aud`), from this team (`iss`), not expired, counts. The keys
//! come from `https://<team>.cloudflareaccess.com/cdn-cgi/access/certs` and are kept for an hour. The checks are those
//! PyJWT made for the Python version (30 s of leeway).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine;
use rsa::{BigUint, Pkcs1v15Sign, RsaPublicKey};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const HEADER: &str = "cf-access-jwt-assertion";
const KEY_LIFETIME: f64 = 3600.0;
const ISSUER_SUFFIX: &str = ".cloudflareaccess.com";
const LEEWAY: i64 = 30;

/// "acme", "acme.cloudflareaccess.com" or "https://acme.cloudflareaccess.com/" all name the same team.
pub fn team_host(team: &str) -> String {
    let v = crate::py::strip(team).to_lowercase();
    let v = v.strip_prefix("https://").unwrap_or(&v);
    let v = v.strip_prefix("http://").unwrap_or(v);
    let v = v.trim_matches('/');
    if v.ends_with(ISSUER_SUFFIX) {
        v.to_string()
    } else {
        format!("{v}{ISSUER_SUFFIX}")
    }
}

pub type Fetcher = Box<dyn Fn(&str) -> Result<Vec<Value>, String> + Send + Sync>;

fn fetch_keys(url: &str) -> Result<Vec<Value>, String> {
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(10)).redirects(0).build();
    let resp = agent.get(url).call().map_err(|e| e.to_string())?;
    let body: Value = resp.into_json().map_err(|e| e.to_string())?;
    Ok(body.get("keys").and_then(|k| k.as_array()).cloned().unwrap_or_default())
}

struct Cache {
    keys: HashMap<String, RsaPublicKey>,
    fetched_at: Option<Instant>,
}

pub struct CloudflareAccess {
    pub audience: String,
    pub host: String,
    fetcher: Fetcher,
    cache: Mutex<Cache>,
}

fn b64(s: &str) -> Result<Vec<u8>, String> {
    let t = s.trim_end_matches('=');
    base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(t).map_err(|e| e.to_string())
}

/// `int(value)` as PyJWT applies it to a time claim.
fn claim_int(v: &Value) -> Result<i64, String> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f.trunc() as i64)).ok_or("int".into()),
        Value::Bool(b) => Ok(*b as i64),
        Value::String(s) => crate::py::py_int(s).map_err(|e| e.to_string()),
        _ => Err("int".into()),
    }
}

fn rsa_key(jwk: &Value) -> Result<RsaPublicKey, String> {
    match jwk.get("kty").and_then(|k| k.as_str()) {
        Some("RSA") => {}
        _ => return Err("Unsupported kty".into()),
    }
    if let Some(alg) = jwk.get("alg").and_then(|a| a.as_str()) {
        if !["RS256", "RS384", "RS512", "PS256", "PS384", "PS512"].contains(&alg) {
            return Err(format!("Unable to find an algorithm for key: {alg}"));
        }
    }
    let n = jwk.get("n").and_then(|v| v.as_str()).ok_or("n")?;
    let e = jwk.get("e").and_then(|v| v.as_str()).ok_or("e")?;
    RsaPublicKey::new(BigUint::from_bytes_be(&b64(n)?), BigUint::from_bytes_be(&b64(e)?)).map_err(|e| e.to_string())
}

impl CloudflareAccess {
    pub fn new(team_domain: Option<&str>, audience: Option<&str>) -> CloudflareAccess {
        Self::with_fetcher(team_domain, audience, Box::new(fetch_keys))
    }

    pub fn with_fetcher(team_domain: Option<&str>, audience: Option<&str>, fetcher: Fetcher) -> CloudflareAccess {
        let host = match team_domain {
            Some(t) if !crate::py::strip(t).is_empty() => team_host(t),
            _ => String::new(),
        };
        CloudflareAccess {
            audience: crate::py::strip(audience.unwrap_or("")).to_string(),
            host,
            fetcher,
            cache: Mutex::new(Cache { keys: HashMap::new(), fetched_at: None }),
        }
    }

    pub fn enabled(&self) -> bool {
        !self.host.is_empty() && !self.audience.is_empty()
    }

    pub fn issuer(&self) -> String {
        format!("https://{}", self.host)
    }

    pub fn logout_url(&self) -> &'static str {
        "/cdn-cgi/access/logout"
    }

    fn key_for(&self, kid: Option<&str>) -> Result<Option<RsaPublicKey>, String> {
        let mut cache = self.cache.lock().unwrap();
        let age = cache.fetched_at.map(|t| t.elapsed().as_secs_f64()).unwrap_or(f64::INFINITY);
        let known = kid.map(|k| cache.keys.contains_key(k)).unwrap_or(false);
        if age > KEY_LIFETIME || !known {
            if age > KEY_LIFETIME || age > 30.0 {
                let mut keys = HashMap::new();
                for k in (self.fetcher)(&format!("{}/cdn-cgi/access/certs", self.issuer()))? {
                    if let Some(id) = k.get("kid") {
                        keys.insert(id.as_str().map(|s| s.to_string()).unwrap_or_else(|| id.to_string()), rsa_key(&k)?);
                    }
                }
                cache.keys = keys;
                cache.fetched_at = Some(Instant::now());
            }
        }
        Ok(kid.and_then(|k| cache.keys.get(k).cloned()))
    }

    fn decode(&self, token: &str) -> Result<Option<Value>, String> {
        let (signing_input, signature) = token.rsplit_once('.').ok_or("Not enough segments")?;
        let (header_b64, payload_b64) = signing_input.split_once('.').ok_or("Not enough segments")?;
        let header: Value = serde_json::from_slice(&b64(header_b64)?).map_err(|_| "Invalid header string")?;
        if !header.is_object() {
            return Err("Invalid header string: must be a json object".into());
        }
        let kid = header.get("kid").and_then(|k| k.as_str());
        let Some(key) = self.key_for(kid)? else { return Ok(None) };
        if header.get("alg").and_then(|a| a.as_str()) != Some("RS256") {
            return Err("The specified alg value is not allowed".into());
        }
        let payload_bytes = b64(payload_b64)?;
        let sig = b64(signature)?;
        let digest = Sha256::digest(signing_input.as_bytes());
        key.verify(Pkcs1v15Sign::new::<Sha256>(), &digest, &sig).map_err(|_| "Signature verification failed")?;
        let claims: Value = serde_json::from_slice(&payload_bytes).map_err(|_| "Invalid payload string")?;
        if !claims.is_object() {
            return Err("Invalid payload string: must be a json object".into());
        }
        for required in ["exp", "iss", "aud"] {
            if claims.get(required).map(|v| v.is_null()).unwrap_or(true) {
                return Err(format!("Token is missing the \"{required}\" claim"));
            }
        }
        let now = chrono::Utc::now().timestamp();
        if let Some(sub) = claims.get("sub") {
            if !sub.is_string() {
                return Err("Subject must be a string".into());
            }
        }
        if let Some(jti) = claims.get("jti") {
            if !jti.is_string() {
                return Err("JWT ID must be a string".into());
            }
        }
        if let Some(iat) = claims.get("iat") {
            if claim_int(iat)? > now + LEEWAY {
                return Err("The token is not yet valid (iat)".into());
            }
        }
        if let Some(nbf) = claims.get("nbf") {
            if claim_int(nbf)? > now + LEEWAY {
                return Err("The token is not yet valid (nbf)".into());
            }
        }
        if claim_int(&claims["exp"])? <= now - LEEWAY {
            return Err("Signature has expired".into());
        }
        let aud: Vec<Value> = match &claims["aud"] {
            Value::String(s) => vec![Value::String(s.clone())],
            Value::Array(a) => a.clone(),
            _ => return Err("Invalid claim format in token".into()),
        };
        if aud.iter().any(|a| !a.is_string()) {
            return Err("Invalid claim format in token".into());
        }
        if !aud.iter().any(|a| a.as_str() == Some(self.audience.as_str())) {
            return Err("Audience doesn't match".into());
        }
        if claims["iss"].as_str() != Some(self.issuer().as_str()) {
            return Err("Invalid issuer".into());
        }
        Ok(Some(claims))
    }

    /// The verified e-mail of the visitor (lower case), or None for anything that is not a valid Access token for this
    /// application (no token, bad signature, other application, expired, a service token without e-mail).
    pub fn verify(&self, token: Option<&str>) -> Option<String> {
        let token = token.filter(|t| !t.is_empty())?;
        if !self.enabled() {
            return None;
        }
        match self.decode(token) {
            Ok(Some(claims)) => {
                let email = claims.get("email")?.as_str()?;
                if email.contains('@') {
                    Some(crate::py::strip(email).to_lowercase())
                } else {
                    None
                }
            }
            Ok(None) => None,
            Err(e) => {
                log::info!("Jeton Cloudflare Access refusé : {e}");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::traits::PublicKeyParts;

    fn token(key: &rsa::RsaPrivateKey, header: &Value, claims: &Value) -> String {
        let enc = |v: &Value| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(v).unwrap());
        let input = format!("{}.{}", enc(header), enc(claims));
        let sig = key.sign(Pkcs1v15Sign::new::<Sha256>(), &Sha256::digest(input.as_bytes())).unwrap();
        format!("{input}.{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sig))
    }

    #[test]
    fn verifies_like_pyjwt() {
        let mut rng = rand::thread_rng();
        let key = rsa::RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let public = key.to_public_key();
        let enc = |b: Vec<u8>| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b);
        let jwk = serde_json::json!({"kid": "k1", "kty": "RSA", "n": enc(public.n().to_bytes_be()), "e": enc(public.e().to_bytes_be())});
        let cf = CloudflareAccess::with_fetcher(Some("Acme"), Some("aud1"), Box::new(move |_| Ok(vec![jwk.clone()])));
        let now = chrono::Utc::now().timestamp();
        let header = serde_json::json!({"alg": "RS256", "kid": "k1"});
        let good = serde_json::json!({"email": " Alice@Example.com", "aud": ["aud1"], "iss": "https://acme.cloudflareaccess.com", "exp": now + 60});
        assert_eq!(cf.verify(Some(&token(&key, &header, &good))), Some("alice@example.com".into()));
        let mut expired = good.clone();
        expired["exp"] = (now - 31).into();
        assert_eq!(cf.verify(Some(&token(&key, &header, &expired))), None);
        let mut other = good.clone();
        other["aud"] = "aud2".into();
        assert_eq!(cf.verify(Some(&token(&key, &header, &other))), None);
        let mut tampered = token(&key, &header, &good);
        tampered.push('A');
        assert_eq!(cf.verify(Some(&tampered)), None);
    }
}
