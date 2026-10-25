//! The standard security handler, as pdfminer applies it with an empty password: RC4 (revisions 2 and 3), the crypt
//! filters of revision 4 (RC4 or AES-128) and AES-256 (revisions 5 and 6). A bank statement that is only protected
//! against changes (an owner password, no password to open it) reads like any other PDF.

use std::collections::HashMap;

use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use md5::{Digest, Md5};
use sha2::{Sha256, Sha384, Sha512};

use super::lexer::Name;
use super::objects::{Dict, Obj, Resolver};

const PASSWORD_PADDING: &[u8; 32] = b"(\xbfN^Nu\x8aAd\x00NV\xff\xfa\x01\x08..\x00\xb6\xd0h>\x80/\x0c\xa9\xfedSiz";

/// Why the document cannot be read: no password opens it (pdfminer's PDFPasswordIncorrect, which pdfplumber reports
/// with an empty message), or the encryption is not one pdfminer handles (its message).
#[derive(Debug)]
pub enum CryptError {
    Password,
    Other(String),
}

#[derive(Clone, Copy, Debug)]
enum Method {
    Rc4,
    Aes128,
    Aes256,
    Identity,
}

#[derive(Debug)]
pub struct Security {
    v: i64,
    key: Vec<u8>,
    encrypt_metadata: bool,
    strf: String,
    cfm: HashMap<String, Method>,
}

fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut s: Vec<u8> = (0..=255).collect();
    let mut j: usize = 0;
    for i in 0..256 {
        j = (j + s[i] as usize + key[i % key.len()] as usize) % 256;
        s.swap(i, j);
    }
    let (mut i, mut j) = (0usize, 0usize);
    data.iter()
        .map(|b| {
            i = (i + 1) % 256;
            j = (j + s[i] as usize) % 256;
            s.swap(i, j);
            b ^ s[(s[i] as usize + s[j] as usize) % 256]
        })
        .collect()
}

fn md5(parts: &[&[u8]]) -> Vec<u8> {
    let mut h = Md5::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().to_vec()
}

/// AES-CBC decryption of the whole blocks (`decryptor().update()`), no padding removed.
fn aes_cbc_decrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Vec<u8> {
    let blocks = data.len() / 16;
    let mut out = Vec::with_capacity(blocks * 16);
    let mut prev = [0u8; 16];
    prev.copy_from_slice(&iv[..16]);
    macro_rules! run {
        ($cipher:expr) => {{
            let c = $cipher;
            for chunk in data[..blocks * 16].chunks(16) {
                let mut block = aes::Block::clone_from_slice(chunk);
                c.decrypt_block(&mut block);
                for k in 0..16 {
                    out.push(block[k] ^ prev[k]);
                }
                prev.copy_from_slice(chunk);
            }
        }};
    }
    match key.len() {
        16 => run!(aes::Aes128::new_from_slice(key).unwrap()),
        24 => run!(aes::Aes192::new_from_slice(key).unwrap()),
        _ => run!(aes::Aes256::new_from_slice(&key[..32]).unwrap()),
    }
    out
}

fn aes128_cbc_encrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Vec<u8> {
    let c = aes::Aes128::new_from_slice(key).unwrap();
    let mut out = Vec::with_capacity(data.len());
    let mut prev = [0u8; 16];
    prev.copy_from_slice(&iv[..16]);
    for chunk in data.chunks(16) {
        let mut block = aes::Block::default();
        for k in 0..16 {
            block[k] = chunk[k] ^ prev[k];
        }
        c.encrypt_block(&mut block);
        prev.copy_from_slice(&block);
        out.extend_from_slice(&block);
    }
    out
}

/// pdfminer's `unpad_aes`.
fn unpad_aes(padded: Vec<u8>) -> Vec<u8> {
    let Some(&padding) = padded.last() else { return padded };
    let p = padding as usize;
    if p > 16 || p > padded.len() {
        return padded;
    }
    if padded[padded.len() - p..].iter().all(|&x| x == padding) {
        return padded[..padded.len() - p].to_vec();
    }
    padded
}

/// Python's `repr()` of a pdfminer object, for the error messages that quote the encryption dictionary.
pub fn pyrepr(o: &Obj) -> String {
    match o {
        Obj::Null => "None".into(),
        Obj::Bool(b) => if *b { "True" } else { "False" }.into(),
        Obj::Int(i) => i.to_string(),
        Obj::Real(f) => crate::py::float_repr(*f),
        Obj::Name(Name::Text(s)) => format!("/{}", crate::py::repr_str(s)),
        Obj::Name(Name::Bytes(b)) => format!("/{}", bytes_repr(b)),
        Obj::Str(b) => bytes_repr(b),
        Obj::Array(a) => format!("[{}]", a.iter().map(pyrepr).collect::<Vec<_>>().join(", ")),
        Obj::Dict(d) => dict_repr(d),
        Obj::Stream(s) => format!("<PDFStream({}): raw={}, {}>", s.objid.map(|i| i.to_string()).unwrap_or_else(|| "None".into()), s.raw.len(), dict_repr(&s.dict)),
        Obj::Ref(id) => format!("<PDFObjRef:{id}>"),
        Obj::Keyword(k) => format!("/{}", bytes_repr(k)),
    }
}

fn dict_repr(d: &Dict) -> String {
    format!("{{{}}}", d.iter().map(|(k, v)| format!("{}: {}", crate::py::repr_str(k), pyrepr(v))).collect::<Vec<_>>().join(", "))
}

/// `repr(bytes)`.
pub fn bytes_repr(b: &[u8]) -> String {
    let quote = if b.contains(&b'\'') && !b.contains(&b'"') { '"' } else { '\'' };
    let mut out = format!("b{quote}");
    for &c in b {
        match c {
            b'\\' => out.push_str("\\\\"),
            b'\t' => out.push_str("\\t"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            c if c as char == quote => {
                out.push('\\');
                out.push(quote);
            }
            32..=126 => out.push(c as char),
            c => out.push_str(&format!("\\x{c:02x}")),
        }
    }
    out.push(quote);
    out
}

fn literal_name(o: &Obj) -> String {
    match o {
        Obj::Name(Name::Text(s)) => s.clone(),
        Obj::Name(Name::Bytes(b)) => String::from_utf8_lossy(b).into_owned(),
        Obj::Str(b) => String::from_utf8_lossy(b).into_owned(),
        other => pyrepr(other),
    }
}

impl Security {
    /// `PDFDocument._initialize_password("")`.
    pub fn new(docid: &[Vec<u8>], param: &Dict, r: &dyn Resolver) -> Result<Security, CryptError> {
        let err = |m: String| CryptError::Other(m);
        let p_repr = || format!("param={}", dict_repr(param));
        let get = |k: &str| param.get(k).map(|v| r.resolve(v));
        let missing = |k: &str| CryptError::Other(format!("'{k}'"));
        let filter = get("Filter").map(|f| literal_name(&f)).unwrap_or_else(|| "None".into());
        if filter != "Standard" {
            return Err(err(format!("Unknown filter: {}", p_repr())));
        }
        let int = |o: Option<Obj>| match o {
            Some(Obj::Int(i)) => i,
            _ => 0,
        };
        let v = int(get("V"));
        if ![1, 2, 4, 5].contains(&v) {
            return Err(err(format!("Unknown algorithm: {}", p_repr())));
        }
        // init_params (PDFStandardSecurityHandler)
        let rev = int(Some(get("R").ok_or_else(|| missing("R"))?));
        let p_raw = int(Some(get("P").ok_or_else(|| missing("P"))?));
        let p: u64 = if p_raw > 0 { p_raw as u64 } else { (p_raw + (1i64 << 32)) as u64 };
        let bytes = |o: Obj| match o {
            Obj::Str(b) => b,
            _ => vec![],
        };
        let o = bytes(get("O").ok_or_else(|| missing("O"))?);
        let u = bytes(get("U").ok_or_else(|| missing("U"))?);
        let mut length = match get("Length") {
            None => 40,
            other => int(other),
        };
        let mut encrypt_metadata = true;
        let mut strf = String::new();
        let mut cfm: HashMap<String, Method> = HashMap::new();
        if v >= 4 {
            // PDFStandardSecurityHandlerV4.init_params
            length = 128;
            let cf = match get("CF") {
                Some(Obj::Dict(d)) => (*d).clone(),
                _ => Dict::new(),
            };
            let stmf = literal_name(&get("StmF").ok_or_else(|| missing("StmF"))?);
            strf = literal_name(&get("StrF").ok_or_else(|| missing("StrF"))?);
            encrypt_metadata = match get("EncryptMetadata") {
                None => true,
                Some(Obj::Bool(b)) => b,
                Some(Obj::Int(i)) => i != 0,
                Some(Obj::Null) => false,
                Some(_) => true,
            };
            if stmf != strf {
                return Err(err(format!("Unsupported crypt filter: {}", p_repr())));
            }
            for (k, f) in &cf {
                let f = r.resolve(f);
                let name = match &f {
                    Obj::Dict(d) => d.get("CFM").map(|c| literal_name(&r.resolve(c))).ok_or_else(|| missing("CFM"))?,
                    _ => return Err(err("'PDFObjRef' object is not subscriptable".into())),
                };
                let method = match (v, name.as_str()) {
                    (4, "V2") => Some(Method::Rc4),
                    (4, "AESV2") => Some(Method::Aes128),
                    (5, "AESV3") => Some(Method::Aes256),
                    _ => None,
                };
                match method {
                    Some(m) => {
                        cfm.insert(k.clone(), m);
                    }
                    None => return Err(err(format!("Unknown crypt filter method: {}", p_repr()))),
                }
            }
            cfm.insert("Identity".into(), Method::Identity);
            if !cfm.contains_key(&strf) {
                return Err(err(format!("Undefined crypt filter: {}", p_repr())));
            }
        }
        let supported: &[i64] = match v {
            1 | 2 => &[2, 3],
            4 => &[4],
            _ => &[5, 6],
        };
        if v == 5 {
            length = 256;
        }
        let _ = length;
        if !supported.contains(&rev) {
            return Err(err(format!("Unsupported revision: {}", p_repr())));
        }
        if p > u32::MAX as u64 {
            return Err(err("'L' format requires 0 <= number <= 4294967295".into()));
        }
        let key = if v == 5 {
            let oe = bytes(get("OE").ok_or_else(|| missing("OE"))?);
            let ue = bytes(get("UE").ok_or_else(|| missing("UE"))?);
            authenticate_v5(rev, &o, &u, &oe, &ue)
        } else {
            let docid0 = docid.first().cloned().unwrap_or_default();
            let length = if v >= 4 { 128 } else { length };
            authenticate(rev, length, &o, &u, p as u32, &docid0, v >= 4 && !encrypt_metadata)
        };
        let Some(key) = key else { return Err(CryptError::Password) };
        Ok(Security { v, key, encrypt_metadata, strf, cfm })
    }

    /// `decipher(objid, genno, data, attrs)`.
    pub fn decrypt(&self, objid: u32, genno: u32, data: &[u8], attrs: Option<&Dict>) -> Vec<u8> {
        if self.v < 4 {
            return self.rc4(objid, genno, data);
        }
        if !self.encrypt_metadata {
            if let Some(a) = attrs {
                if a.get("Type").map(|t| t.is_name("Metadata")).unwrap_or(false) {
                    return data.to_vec();
                }
            }
        }
        match self.cfm.get(&self.strf).copied().unwrap_or(Method::Identity) {
            Method::Rc4 => self.rc4(objid, genno, data),
            Method::Aes128 => self.aes128(objid, genno, data),
            Method::Aes256 => {
                if data.len() < 16 {
                    return vec![];
                }
                unpad_aes(aes_cbc_decrypt(&self.key, &data[..16], &data[16..]))
            }
            Method::Identity => data.to_vec(),
        }
    }

    fn object_key(&self, objid: u32, genno: u32, salt: &[u8]) -> Vec<u8> {
        let mut key = self.key.clone();
        key.extend_from_slice(&objid.to_le_bytes()[..3]);
        key.extend_from_slice(&genno.to_le_bytes()[..2]);
        key.extend_from_slice(salt);
        let n = key.len().min(16);
        md5(&[&key])[..n].to_vec()
    }

    fn rc4(&self, objid: u32, genno: u32, data: &[u8]) -> Vec<u8> {
        rc4(&self.object_key(objid, genno, b""), data)
    }

    fn aes128(&self, objid: u32, genno: u32, data: &[u8]) -> Vec<u8> {
        if data.len() < 16 {
            return vec![];
        }
        let key = self.object_key(objid, genno, b"sAlT");
        unpad_aes(aes_cbc_decrypt(&key, &data[..16], &data[16..]))
    }
}

/// Algorithm 3.2: the file key from a password.
fn encryption_key(rev: i64, length: i64, password: &[u8], o: &[u8], p: u32, docid0: &[u8], no_metadata: bool) -> Vec<u8> {
    let padded: Vec<u8> = password.iter().chain(PASSWORD_PADDING.iter()).take(32).copied().collect();
    let mut parts: Vec<&[u8]> = vec![&padded, o];
    let pb = p.to_le_bytes();
    parts.push(&pb);
    parts.push(docid0);
    if rev >= 4 && no_metadata {
        parts.push(b"\xff\xff\xff\xff");
    }
    let mut result = md5(&parts);
    let mut n = 5usize;
    if rev >= 3 {
        n = (length / 8).max(0) as usize;
        for _ in 0..50 {
            result = md5(&[&result[..n.min(result.len())]]);
        }
    }
    result[..n.min(result.len())].to_vec()
}

fn compute_u(rev: i64, key: &[u8], docid0: &[u8]) -> Vec<u8> {
    if key.is_empty() {
        return vec![];
    }
    if rev == 2 {
        return rc4(key, PASSWORD_PADDING);
    }
    let hash = md5(&[PASSWORD_PADDING, docid0]);
    let mut result = rc4(key, &hash);
    for i in 1..20u8 {
        let k: Vec<u8> = key.iter().map(|c| c ^ i).collect();
        result = rc4(&k, &result);
    }
    let mut twice = result.clone();
    twice.extend_from_slice(&result);
    twice
}

fn user_key(rev: i64, length: i64, password: &[u8], o: &[u8], u: &[u8], p: u32, docid0: &[u8], no_metadata: bool) -> Option<Vec<u8>> {
    let key = encryption_key(rev, length, password, o, p, docid0, no_metadata);
    let computed = compute_u(rev, &key, docid0);
    let head = |b: &[u8]| b[..b.len().min(16)].to_vec();
    let ok = if rev == 2 { computed == u } else { head(&computed) == head(u) };
    ok.then_some(key)
}

/// `authenticate("")`: the user password first, then the owner one.
fn authenticate(rev: i64, length: i64, o: &[u8], u: &[u8], p: u32, docid0: &[u8], no_metadata: bool) -> Option<Vec<u8>> {
    let password: &[u8] = b"";
    if let Some(k) = user_key(rev, length, password, o, u, p, docid0, no_metadata) {
        return Some(k);
    }
    // Algorithm 3.7
    let padded: Vec<u8> = password.iter().chain(PASSWORD_PADDING.iter()).take(32).copied().collect();
    let mut hash = md5(&[&padded]);
    if rev >= 3 {
        for _ in 0..50 {
            hash = md5(&[&hash]);
        }
    }
    let n = if rev >= 3 { (length / 8).max(0) as usize } else { 5 };
    let key = hash[..n.min(hash.len())].to_vec();
    if key.is_empty() {
        return None;
    }
    let user_password = if rev == 2 {
        rc4(&key, o)
    } else {
        let mut up = o.to_vec();
        for i in (0..20u8).rev() {
            let k: Vec<u8> = key.iter().map(|c| c ^ i).collect();
            up = rc4(&k, &up);
        }
        up
    };
    user_key(rev, length, &user_password, o, u, p, docid0, no_metadata)
}

fn bytes_mod_3(b: &[u8]) -> usize {
    b.iter().map(|x| (*x % 3) as usize).sum::<usize>() % 3
}

fn password_hash(rev: i64, password: &[u8], salt: &[u8], vector: Option<&[u8]>) -> Vec<u8> {
    if rev == 5 {
        let mut h = Sha256::new();
        h.update(password);
        h.update(salt);
        if let Some(v) = vector {
            h.update(v);
        }
        return h.finalize().to_vec();
    }
    // revision 6
    let salt = &salt[..salt.len().min(8)];
    let mut h = Sha256::new();
    h.update(password);
    h.update(salt);
    if let Some(v) = vector {
        h.update(v);
    }
    let mut k = h.finalize().to_vec();
    let (mut round, mut last) = (0i64, 0i64);
    while round < 64 || last > round - 32 {
        let mut unit = password.to_vec();
        unit.extend_from_slice(&k);
        if let Some(v) = vector {
            unit.extend_from_slice(v);
        }
        let k1: Vec<u8> = unit.repeat(64);
        let e = aes128_cbc_encrypt(&k[..16], &k[16..32], &k1);
        k = match bytes_mod_3(&e[..16]) {
            0 => Sha256::digest(&e).to_vec(),
            1 => Sha384::digest(&e).to_vec(),
            _ => Sha512::digest(&e).to_vec(),
        };
        last = *e.last().unwrap() as i64;
        round += 1;
    }
    k[..32].to_vec()
}

fn authenticate_v5(rev: i64, o: &[u8], u: &[u8], oe: &[u8], ue: &[u8]) -> Option<Vec<u8>> {
    let password: &[u8] = b"";
    let slice = |b: &[u8], a: usize, z: usize| b.get(a.min(b.len())..z.min(b.len())).unwrap_or(&[]).to_vec();
    let (o_hash, o_valid, o_key) = (slice(o, 0, 32), slice(o, 32, 40), slice(o, 40, o.len()));
    let (u_hash, u_valid, u_key) = (slice(u, 0, 32), slice(u, 32, 40), slice(u, 40, u.len()));
    let zero = [0u8; 16];
    if password_hash(rev, password, &o_valid, Some(u)) == o_hash {
        let key = password_hash(rev, password, &o_key, Some(u));
        return Some(aes_cbc_decrypt(&key, &zero, oe));
    }
    if password_hash(rev, password, &u_valid, None) == u_hash {
        let key = password_hash(rev, password, &u_key, None);
        return Some(aes_cbc_decrypt(&key, &zero, ue));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rc4_known_vector() {
        assert_eq!(hex::encode(rc4(b"Key", b"Plaintext")), "bbf316e8d940af0ad3");
    }

    #[test]
    fn unpad_like_pdfminer() {
        assert_eq!(unpad_aes(vec![1, 2, 3, 2, 2]), vec![1, 2, 3]);
        assert_eq!(unpad_aes(vec![1, 2, 17]), vec![1, 2, 17]);
        assert_eq!(unpad_aes(vec![1, 2, 3]), vec![1, 2, 3]);
        assert_eq!(bytes_repr(b"a'b\x00"), "b\"a'b\\x00\"");
    }
}
