//! A small HTTP/1.1 server: a pool of threads, keep-alive, Content-Length and chunked bodies. It answers the way
//! uvicorn did for the Python version (no `server` header, a `date` header, the path percent-decoded).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

pub struct Request {
    pub method: String,
    /// Percent-decoded, as Starlette's `request.url.path`.
    pub path: String,
    pub query: String,
    /// Names in lower case, values decoded as latin-1 (like Starlette's headers).
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn new(status: u16, content_type: &str, body: impl Into<Vec<u8>>) -> Response {
        let mut r = Response { status, headers: vec![], body: body.into() };
        if !content_type.is_empty() {
            r.headers.push(("content-type".into(), content_type.into()));
        }
        r
    }
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
    /// Add a header unless the response already has it (`headers.setdefault`).
    pub fn set_default(&mut self, name: &str, value: &str) {
        if self.header(name).is_none() {
            self.headers.push((name.to_lowercase(), value.to_string()));
        }
    }
    pub fn add(&mut self, name: &str, value: impl Into<String>) {
        self.headers.push((name.to_lowercase(), value.into()));
    }
}

pub fn reason(status: u16) -> &'static str {
    match status {
        100 => "Continue",
        200 => "OK",
        204 => "No Content",
        206 => "Partial Content",
        303 => "See Other",
        304 => "Not Modified",
        307 => "Temporary Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        413 => "Request Entity Too Large",
        416 => "Requested Range Not Satisfiable",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "",
    }
}

/// `email.utils.formatdate(usegmt=True)`.
pub fn http_date(t: chrono::DateTime<chrono::Utc>) -> String {
    t.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

/// `urllib.parse.unquote` of a latin-1 string: percent escapes are UTF-8 (invalid bytes replaced), the rest is kept.
pub fn unquote(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let mut out = String::new();
    let mut pending: Vec<u8> = vec![];
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '%' && i + 2 < chars.len() {
            let hex: String = chars[i + 1..i + 3].iter().collect();
            if hex.chars().all(|c| c.is_ascii_hexdigit()) {
                pending.push(u8::from_str_radix(&hex, 16).unwrap());
                i += 3;
                continue;
            }
        }
        if !pending.is_empty() {
            out.push_str(&String::from_utf8_lossy(&pending));
            pending.clear();
        }
        out.push(chars[i]);
        i += 1;
    }
    if !pending.is_empty() {
        out.push_str(&String::from_utf8_lossy(&pending));
    }
    out
}

pub fn latin1(b: &[u8]) -> String {
    b.iter().map(|&c| c as char).collect()
}

enum ReadError {
    Closed,
    Bad(u16, &'static str),
}

fn read_line(r: &mut BufReader<TcpStream>, limit: usize) -> Result<Vec<u8>, ReadError> {
    let mut line = vec![];
    loop {
        let buf = r.fill_buf().map_err(|_| ReadError::Closed)?;
        if buf.is_empty() {
            return if line.is_empty() { Err(ReadError::Closed) } else { Err(ReadError::Bad(400, "Bad Request")) };
        }
        match buf.iter().position(|&b| b == b'\n') {
            Some(p) => {
                line.extend_from_slice(&buf[..p]);
                r.consume(p + 1);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(line);
            }
            None => {
                let n = buf.len();
                line.extend_from_slice(buf);
                r.consume(n);
                if line.len() > limit {
                    return Err(ReadError::Bad(431, "Request Header Fields Too Large"));
                }
            }
        }
    }
}

struct Limits {
    max_body: usize,
}

fn read_request(r: &mut BufReader<TcpStream>, limits: &Limits, first: bool) -> Result<(Request, bool), ReadError> {
    let _ = r.get_ref().set_read_timeout(Some(Duration::from_secs(if first { 30 } else { 5 })));
    let mut line = read_line(r, 65536)?;
    while line.is_empty() {
        line = read_line(r, 65536)?; // tolerate blank lines between requests
    }
    let _ = r.get_ref().set_read_timeout(Some(Duration::from_secs(60)));
    let text = latin1(&line);
    let mut parts = text.split(' ');
    let (method, target, version) = match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(m), Some(t), Some(v), None) if !m.is_empty() && !t.is_empty() => (m.to_string(), t.to_string(), v.to_string()),
        _ => return Err(ReadError::Bad(400, "Bad Request")),
    };
    if version != "HTTP/1.1" && version != "HTTP/1.0" {
        return Err(ReadError::Bad(400, "Bad Request"));
    }
    let mut headers = vec![];
    let mut total = 0;
    loop {
        let l = read_line(r, 65536)?;
        if l.is_empty() {
            break;
        }
        total += l.len();
        if total > 65536 || headers.len() > 200 {
            return Err(ReadError::Bad(431, "Request Header Fields Too Large"));
        }
        let h = latin1(&l);
        let (k, v) = h.split_once(':').ok_or(ReadError::Bad(400, "Bad Request"))?;
        if k.is_empty() || k.contains(' ') {
            return Err(ReadError::Bad(400, "Bad Request"));
        }
        headers.push((k.to_lowercase(), v.trim_matches(|c| c == ' ' || c == '\t').to_string()));
    }
    let get = |name: &str| headers.iter().find(|(k, _)| k == name).map(|(_, v): &(String, String)| v.clone());
    let connection = get("connection").unwrap_or_default().to_lowercase();
    let keep_alive = if version == "HTTP/1.0" { connection.contains("keep-alive") } else { !connection.contains("close") };
    if get("expect").map(|e| e.eq_ignore_ascii_case("100-continue")).unwrap_or(false) {
        let _ = r.get_mut().write_all(b"HTTP/1.1 100 Continue\r\n\r\n");
    }
    let mut body = vec![];
    if get("transfer-encoding").map(|t| t.to_lowercase().contains("chunked")).unwrap_or(false) {
        loop {
            let size_line = latin1(&read_line(r, 1024)?);
            let size = usize::from_str_radix(size_line.split(';').next().unwrap_or("").trim(), 16).map_err(|_| ReadError::Bad(400, "Bad Request"))?;
            if size == 0 {
                while !read_line(r, 65536)?.is_empty() {}
                break;
            }
            if body.len() + size > limits.max_body {
                return Err(ReadError::Bad(413, "Request Entity Too Large"));
            }
            let start = body.len();
            body.resize(start + size, 0);
            r.read_exact(&mut body[start..]).map_err(|_| ReadError::Closed)?;
            read_line(r, 16)?;
        }
    } else if let Some(len) = get("content-length") {
        let n: usize = len.trim().parse().map_err(|_| ReadError::Bad(400, "Bad Request"))?;
        if n > limits.max_body {
            return Err(ReadError::Bad(413, "Request Entity Too Large"));
        }
        body.resize(n, 0);
        r.read_exact(&mut body).map_err(|_| ReadError::Closed)?;
    }
    // origin-form, or absolute-form ("http://host/path")
    let target = match target.find("://") {
        Some(p) if !target.starts_with('/') => target[p + 3..].find('/').map(|q| target[p + 3 + q..].to_string()).unwrap_or_else(|| "/".into()),
        _ => target,
    };
    // the fragment is not part of the request (httptools drops it)
    let target = target.split('#').next().unwrap_or("").to_string();
    let (raw_path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.clone(), String::new()),
    };
    Ok((Request { method, path: unquote(&raw_path), query, headers, body }, keep_alive))
}

fn write_response(stream: &mut TcpStream, resp: &Response, head: bool, close: bool) -> std::io::Result<()> {
    let mut out = format!("HTTP/1.1 {} {}\r\n", resp.status, reason(resp.status));
    out.push_str(&format!("date: {}\r\n", http_date(chrono::Utc::now())));
    for (k, v) in &resp.headers {
        if k != "content-length" {
            out.push_str(&format!("{k}: {v}\r\n"));
        }
    }
    let length = resp.header("content-length").map(|s| s.to_string()).unwrap_or_else(|| resp.body.len().to_string());
    if resp.status != 304 && !(100..200).contains(&resp.status) && resp.status != 204 {
        out.push_str(&format!("content-length: {length}\r\n"));
    }
    if close {
        out.push_str("connection: close\r\n");
    }
    out.push_str("\r\n");
    // header values are latin-1 on the wire
    let mut bytes: Vec<u8> = out.chars().map(|c| if (c as u32) < 256 { c as u8 } else { b'?' }).collect();
    if !head && resp.status != 304 {
        bytes.extend_from_slice(&resp.body);
    }
    stream.write_all(&bytes)?;
    stream.flush()
}

fn handle(stream: TcpStream, handler: &(dyn Fn(Request) -> Response + Send + Sync), limits: &Limits) {
    let _ = stream.set_nodelay(true);
    let _ = stream.set_write_timeout(Some(Duration::from_secs(60)));
    let mut writer = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut reader = BufReader::with_capacity(65536, stream);
    let mut first = true;
    loop {
        match read_request(&mut reader, limits, first) {
            Ok((req, keep_alive)) => {
                let head = req.method == "HEAD";
                let resp = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(req))) {
                    Ok(r) => r,
                    Err(_) => Response::new(500, "text/plain; charset=utf-8", "Internal Server Error"),
                };
                if write_response(&mut writer, &resp, head, !keep_alive).is_err() || !keep_alive {
                    return;
                }
            }
            Err(ReadError::Closed) => return,
            Err(ReadError::Bad(status, text)) => {
                let resp = Response::new(status, "text/plain; charset=utf-8", text);
                let _ = write_response(&mut writer, &resp, false, true);
                return;
            }
        }
        first = false;
    }
}

/// Serve forever on `addr` with `workers` threads.
pub fn serve(addr: &str, workers: usize, max_body: usize, handler: Arc<dyn Fn(Request) -> Response + Send + Sync>) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr)?;
    let (tx, rx) = mpsc::channel::<TcpStream>();
    let rx = Arc::new(Mutex::new(rx));
    let limits = Arc::new(Limits { max_body });
    for i in 0..workers {
        let rx = rx.clone();
        let handler = handler.clone();
        let limits = limits.clone();
        std::thread::Builder::new()
            .name(format!("http-{i}"))
            .stack_size(8 * 1024 * 1024)
            .spawn(move || loop {
                let stream = match rx.lock().unwrap().recv() {
                    Ok(s) => s,
                    Err(_) => return,
                };
                handle(stream, handler.as_ref(), &limits);
            })?;
    }
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let _ = tx.send(s);
            }
            Err(e) => log::warn!("connexion refusée : {e}"),
        }
    }
    Ok(())
}
