//! Query strings, forms (urlencoded and multipart) and cookies, read the way Starlette and python-multipart read them.

use std::collections::HashMap;

use super::http::{latin1, unquote, Request};

/// `unquote_plus` of a latin-1 string.
pub fn unquote_plus(s: &str) -> String {
    unquote(&s.replace('+', " "))
}

/// `parse_qsl(query, keep_blank_values=True)` (also how the urlencoded form parser splits fields).
pub fn parse_qsl(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|chunk| !chunk.is_empty())
        .map(|chunk| match chunk.split_once('=') {
            Some((k, v)) => (unquote_plus(k), unquote_plus(v)),
            None => (unquote_plus(chunk), String::new()),
        })
        .collect()
}

pub struct Upload {
    pub filename: String,
    pub data: Vec<u8>,
}

pub enum Field {
    Text(String),
    File(Upload),
}

impl Field {
    pub fn text(&self) -> Option<&str> {
        match self {
            Field::Text(t) => Some(t),
            Field::File(_) => None,
        }
    }
}

/// Starlette's FormData: an ordered multi-dict whose `get` is the last value of a key.
#[derive(Default)]
pub struct FormData {
    pub items: Vec<(String, Field)>,
}

impl FormData {
    pub fn get(&self, key: &str) -> Option<&Field> {
        self.items.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v)
    }
    /// `form.get(key)` when it is a string.
    pub fn text(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(|f| f.text())
    }
    pub fn getlist(&self, key: &str) -> Vec<&Field> {
        self.items.iter().filter(|(k, _)| k == key).map(|(_, v)| v).collect()
    }
    /// `{k: v for k, v in form.items() if isinstance(v, str)}` (the last value of each key).
    pub fn strings(&self) -> HashMap<String, String> {
        let mut last: Vec<(&str, &Field)> = vec![];
        for (k, v) in &self.items {
            match last.iter_mut().find(|(lk, _)| *lk == k.as_str()) {
                Some(slot) => slot.1 = v,
                None => last.push((k.as_str(), v)),
            }
        }
        last.into_iter().filter_map(|(k, v)| v.text().map(|t| (k.to_string(), t.to_string()))).collect()
    }
}

/// cgi-style `_parseparam`: ";"-separated segments, quotes respected.
fn parse_params(s: &str) -> Vec<String> {
    let mut out = vec![];
    let mut rest = format!(";{s}");
    while rest.starts_with(';') {
        rest = rest[1..].to_string();
        let mut end = rest.find(';').map(|e| e as i64).unwrap_or(-1);
        while end > 0 {
            let head = &rest[..end as usize];
            let quotes = head.matches('"').count() as i64 - head.matches("\\\"").count() as i64;
            if quotes % 2 == 0 {
                break;
            }
            end = rest[end as usize + 1..].find(';').map(|e| e as i64 + end + 1).unwrap_or(-1);
        }
        let end = if end < 0 { rest.len() } else { end as usize };
        out.push(rest[..end].trim().to_string());
        rest = rest[end..].to_string();
    }
    out
}

/// python-multipart's `parse_options_header` (latin-1 text in, values as latin-1 text).
pub fn parse_options_header(value: &str) -> (String, HashMap<String, String>) {
    if value.is_empty() {
        return (String::new(), HashMap::new());
    }
    if !value.contains(';') {
        return (value.to_lowercase().trim().to_string(), HashMap::new());
    }
    let mut segments = parse_params(value).into_iter();
    let ctype = segments.next().unwrap_or_default();
    let mut options = HashMap::new();
    for segment in segments {
        let (key, val) = match segment.split_once('=') {
            Some((k, v)) => (k.to_string(), v.to_string()),
            None => (segment.clone(), String::new()),
        };
        if key.contains('*') {
            continue;
        }
        let mut val = val;
        if val.chars().count() >= 2 && val.starts_with('"') && val.ends_with('"') {
            val = val[1..val.len() - 1].replace("\\\\", "\\").replace("\\\"", "\"");
        }
        if key == "filename" {
            let chars: Vec<char> = val.chars().collect();
            let drive = chars.len() >= 3 && chars[1] == ':' && chars[2] == '\\';
            if drive || val.starts_with("\\\\") {
                val = val.rsplit('\\').next().unwrap_or("").to_string();
            }
        }
        options.insert(key, val);
    }
    (ctype, options)
}

/// Starlette's `_user_safe_decode`: the charset, else latin-1.
fn safe_decode(latin: &str) -> String {
    let bytes: Vec<u8> = latin.chars().map(|c| c as u32 as u8).collect();
    match String::from_utf8(bytes.clone()) {
        Ok(s) => s,
        Err(_) => latin1(&bytes),
    }
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|&i| &hay[i..i + needle.len()] == needle)
}

fn parse_multipart(body: &[u8], boundary: &str) -> Result<FormData, String> {
    let invalid = || "Invalid multipart data.".to_string();
    let b: Vec<u8> = boundary.chars().map(|c| c as u32 as u8).collect();
    let delim = [b"--".as_slice(), &b].concat();
    let mut pos = find(body, &delim, 0).ok_or_else(invalid)? + delim.len();
    let mut form = FormData::default();
    loop {
        if body[pos..].starts_with(b"--") {
            return Ok(form);
        }
        // the rest of the boundary line
        let eol = find(body, b"\r\n", pos).ok_or_else(invalid)?;
        pos = eol + 2;
        let head_end = if body[pos..].starts_with(b"\r\n") { pos - 2 } else { find(body, b"\r\n\r\n", pos).ok_or_else(invalid)? };
        let head = &body[pos..head_end];
        let mut disposition = String::new();
        for line in head.split(|&c| c == b'\n') {
            let line = latin1(line.strip_suffix(b"\r").unwrap_or(line));
            if let Some((k, v)) = line.split_once(':') {
                if k.trim().eq_ignore_ascii_case("content-disposition") {
                    disposition = v.trim_start().to_string();
                }
            }
        }
        let data_start = head_end + 4;
        let next = [b"\r\n".as_slice(), &delim].concat();
        let data_end = find(body, &next, data_start).ok_or_else(invalid)?;
        let data = body[data_start..data_end].to_vec();
        let (_, options) = parse_options_header(&disposition);
        let name = options.get("name").ok_or_else(|| "The Content-Disposition header field \"name\" must be provided.".to_string())?;
        let name = safe_decode(name);
        match options.get("filename") {
            Some(f) => form.items.push((name, Field::File(Upload { filename: safe_decode(f), data }))),
            None => {
                if data.len() > 1024 * 1024 {
                    return Err("Part exceeded maximum size of 1024KB.".into());
                }
                let text = String::from_utf8(data.clone()).unwrap_or_else(|_| latin1(&data));
                form.items.push((name, Field::Text(text)));
            }
        }
        pos = data_end + next.len();
    }
}

/// `await request.form()`; an error is Starlette's 400 detail.
pub fn read_form(req: &Request) -> Result<FormData, String> {
    let (ctype, params) = parse_options_header(req.header("content-type").unwrap_or(""));
    if ctype == "multipart/form-data" {
        let boundary = params.get("boundary").ok_or_else(|| "Missing boundary in multipart.".to_string())?;
        return parse_multipart(&req.body, boundary);
    }
    if ctype == "application/x-www-form-urlencoded" {
        let text = latin1(&req.body);
        let items = parse_qsl(&text).into_iter().map(|(k, v)| (k, Field::Text(v))).collect();
        return Ok(FormData { items });
    }
    Ok(FormData::default())
}

/// Starlette's `cookie_parser`.
pub fn cookies(req: &Request) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if let Some(header) = req.header("cookie") {
        for chunk in header.split(';') {
            let (k, v) = match chunk.split_once('=') {
                Some((k, v)) => (k.trim(), v.trim()),
                None => ("", chunk.trim()),
            };
            if !k.is_empty() || !v.is_empty() {
                out.insert(k.to_string(), cookie_unquote(v));
            }
        }
    }
    out
}

fn cookie_unquote(v: &str) -> String {
    if v.len() < 2 || !v.starts_with('"') || !v.ends_with('"') {
        return v.to_string();
    }
    let inner: Vec<char> = v[1..v.len() - 1].chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < inner.len() {
        if inner[i] == '\\' && i + 1 < inner.len() {
            if i + 3 < inner.len() && inner[i + 1..i + 4].iter().all(|c| ('0'..='7').contains(c)) {
                let oct: String = inner[i + 1..i + 4].iter().collect();
                out.push(char::from_u32(u32::from_str_radix(&oct, 8).unwrap()).unwrap_or('?'));
                i += 4;
                continue;
            }
            out.push(inner[i + 1]);
            i += 2;
            continue;
        }
        out.push(inner[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_header() {
        let (c, o) = parse_options_header("form-data; name=\"files\"; filename=\"C:\\\\dir\\\\rel\u{e9}v\u{e9}.pdf\"");
        assert_eq!(c, "form-data");
        assert_eq!(o["name"], "files");
        assert_eq!(o["filename"], "relévé.pdf");
        assert_eq!(parse_qsl("a=1&&b=x+y%C3%A9&c"), vec![("a".into(), "1".into()), ("b".into(), "x yé".into()), ("c".into(), "".into())]);
    }

    #[test]
    fn multipart() {
        let body = b"--XX\r\nContent-Disposition: form-data; name=\"csrf\"\r\n\r\ntok\r\n--XX\r\nContent-Disposition: form-data; name=\"files\"; filename=\"a.csv\"\r\nContent-Type: text/csv\r\n\r\nx;y\r\n1;2\r\n--XX--\r\n";
        let f = parse_multipart(body, "XX").unwrap();
        assert_eq!(f.text("csrf"), Some("tok"));
        match f.get("files").unwrap() {
            Field::File(u) => {
                assert_eq!(u.filename, "a.csv");
                assert_eq!(u.data, b"x;y\r\n1;2");
            }
            _ => panic!(),
        }
    }
}
