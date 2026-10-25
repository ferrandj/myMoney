//! Python `re` patterns in Rust: `\s` and `\S` translated to Python's whitespace set (which also has \x1c-\x1f),
//! and the helpers for `re.match` (anchored at the start), `re.fullmatch`, `re.search`, `re.sub`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use regex::Regex;

pub const PY_SPACE: &str = r"\t\n\x0B\x0C\r\x1C-\x1F \x{85}\x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}";

/// Translate a Python pattern: `\s` / `\S` become Python's whitespace class, inside or outside brackets.
pub fn translate(p: &str) -> String {
    let mut out = String::with_capacity(p.len() + 16);
    let chars: Vec<char> = p.chars().collect();
    let mut i = 0;
    let mut in_class = false;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            let n = chars[i + 1];
            match (n, in_class) {
                ('s', false) => out.push_str(&format!("[{PY_SPACE}]")),
                ('s', true) => out.push_str(PY_SPACE),
                ('S', false) => out.push_str(&format!("[^{PY_SPACE}]")),
                _ => {
                    out.push(c);
                    out.push(n);
                }
            }
            i += 2;
            continue;
        }
        if c == '[' && !in_class {
            in_class = true;
            out.push(c);
            // a "]" right after "[" or "[^" is a literal
            if i + 1 < chars.len() && chars[i + 1] == '^' {
                out.push('^');
                i += 1;
            }
            if i + 1 < chars.len() && chars[i + 1] == ']' {
                out.push_str("\\]");
                i += 1;
            }
        } else if c == ']' && in_class {
            in_class = false;
            out.push(c);
        } else if c == '[' && in_class {
            out.push_str("\\[");
        } else {
            out.push(c);
        }
        i += 1;
    }
    out
}

fn cache() -> &'static Mutex<HashMap<String, Regex>> {
    static C: OnceLock<Mutex<HashMap<String, Regex>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// A compiled Python pattern (cached).
pub fn re(p: &str) -> Regex {
    let mut c = cache().lock().unwrap();
    if let Some(r) = c.get(p) {
        return r.clone();
    }
    let r = Regex::new(&translate(p)).unwrap_or_else(|e| panic!("regex {p}: {e}"));
    c.insert(p.to_string(), r.clone());
    r
}

/// `re.match(p, s)`: a match that starts at the beginning.
pub fn py_match<'a>(p: &str, s: &'a str) -> Option<regex::Captures<'a>> {
    re(&format!(r"\A(?:{p})")).captures(s)
}

/// `re.fullmatch(p, s)`.
pub fn fullmatch<'a>(p: &str, s: &'a str) -> Option<regex::Captures<'a>> {
    re(&format!(r"\A(?:{p})\z")).captures(s)
}

/// `re.search(p, s)`.
pub fn search<'a>(p: &str, s: &'a str) -> Option<regex::Captures<'a>> {
    re(p).captures(s)
}

/// `re.sub(p, repl, s)` with a literal replacement.
pub fn sub(p: &str, repl: &str, s: &str) -> String {
    re(p).replace_all(s, regex::NoExpand(repl)).into_owned()
}

/// `re.findall(p, s)` for a pattern without groups (or the first group).
pub fn findall(p: &str, s: &str) -> Vec<String> {
    let r = re(p);
    r.captures_iter(s).map(|c| c.get(1).or_else(|| c.get(0)).map(|m| m.as_str().to_string()).unwrap_or_default()).collect()
}

/// A group's text, or None when it did not take part in the match (Python's `m.group(n)` → None).
pub fn g<'a>(c: &regex::Captures<'a>, i: usize) -> Option<&'a str> {
    c.get(i).map(|m| m.as_str())
}

/// A group's text, "" when it did not take part.
pub fn gs<'a>(c: &regex::Captures<'a>, i: usize) -> &'a str {
    c.get(i).map(|m| m.as_str()).unwrap_or("")
}
