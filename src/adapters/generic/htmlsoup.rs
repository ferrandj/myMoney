//! `BeautifulSoup(text, "html.parser")` as `ofxparse` uses it: CPython 3.13's `html.parser` tokenizer (with
//! `convert_charrefs=False`, as bs4 sets it) feeding bs4's tree builder. Only what decides the tree is kept: tag
//! names, nesting and the text of every string node (comments, CDATA and the like are strings too).

use fancy_regex::Regex as Fancy;
use regex::Regex;
use std::collections::HashMap;
use std::sync::OnceLock;

use super::ofx_tables::{HTML_ENTITIES, NONCHARACTERS, WINDOWS_1252};
use crate::py;

pub const ROOT: usize = 0;

#[derive(Debug, Clone)]
pub enum Node {
    Tag { name: String, children: Vec<usize> },
    Text(String),
}

#[derive(Debug, Default)]
pub struct Soup {
    pub nodes: Vec<Node>,
}

impl Soup {
    pub fn children(&self, id: usize) -> &[usize] {
        match &self.nodes[id] {
            Node::Tag { children, .. } => children,
            Node::Text(_) => &[],
        }
    }

    pub fn name(&self, id: usize) -> Option<&str> {
        match &self.nodes[id] {
            Node::Tag { name, .. } => Some(name),
            Node::Text(_) => None,
        }
    }

    /// `tag.find(name)`: the first descendant tag with that name, in document order.
    pub fn find(&self, id: usize, name: &str) -> Option<usize> {
        let mut stack: Vec<usize> = self.children(id).iter().rev().copied().collect();
        while let Some(n) = stack.pop() {
            if self.name(n) == Some(name) {
                return Some(n);
            }
            stack.extend(self.children(n).iter().rev());
        }
        None
    }

    /// `tag.findAll(name)`.
    pub fn find_all(&self, id: usize, name: &str) -> Vec<usize> {
        let mut out = vec![];
        let mut stack: Vec<usize> = self.children(id).iter().rev().copied().collect();
        while let Some(n) = stack.pop() {
            if self.name(n) == Some(name) {
                out.push(n);
            }
            stack.extend(self.children(n).iter().rev());
        }
        out
    }
}

// ----------------------------------------------------------------------------------------- tree builder

const EMPTY_ELEMENTS: &[&str] = &[
    "area", "base", "basefont", "bgsound", "br", "col", "command", "embed", "frame", "hr", "image", "img", "input", "isindex", "keygen", "link",
    "menuitem", "meta", "nextid", "param", "source", "spacer", "track", "wbr",
];
const PRESERVE_WHITESPACE: &[&str] = &["pre", "textarea"];

struct Builder {
    soup: Soup,
    stack: Vec<usize>,
    current_data: Vec<String>,
    open_tag_counter: HashMap<String, i64>,
    preserve_stack: Vec<usize>,
    already_closed_empty_element: Vec<String>,
}

impl Builder {
    fn new() -> Self {
        let soup = Soup { nodes: vec![Node::Tag { name: "[document]".into(), children: vec![] }] };
        Builder { soup, stack: vec![ROOT], current_data: vec![], open_tag_counter: HashMap::new(), preserve_stack: vec![], already_closed_empty_element: vec![] }
    }

    fn current(&self) -> usize {
        *self.stack.last().unwrap()
    }

    fn append(&mut self, node: Node) -> usize {
        let id = self.soup.nodes.len();
        self.soup.nodes.push(node);
        let cur = self.current();
        if let Node::Tag { children, .. } = &mut self.soup.nodes[cur] {
            children.push(id);
        }
        id
    }

    fn end_data(&mut self) {
        if self.current_data.is_empty() {
            return;
        }
        let mut data = self.current_data.concat();
        if self.preserve_stack.is_empty() && data.chars().all(|c| " \n\t\x0c\r".contains(c)) {
            data = if data.contains('\n') { "\n".into() } else { " ".into() };
        }
        self.current_data.clear();
        self.append(Node::Text(data));
    }

    fn handle_data(&mut self, data: &str) {
        self.current_data.push(data.to_string());
    }

    fn pop_tag(&mut self) {
        if self.stack.len() <= 1 {
            return;
        }
        let tag = self.stack.pop().unwrap();
        let name = self.soup.name(tag).unwrap_or("").to_string();
        if let Some(c) = self.open_tag_counter.get_mut(&name) {
            *c -= 1;
        }
        if self.preserve_stack.last() == Some(&tag) {
            self.preserve_stack.pop();
        }
    }

    fn pop_to_tag(&mut self, name: &str) {
        if name == "[document]" {
            return;
        }
        let size = self.stack.len();
        for i in (1..size).rev() {
            if self.open_tag_counter.get(name).copied().unwrap_or(0) == 0 {
                break;
            }
            let t = self.stack[i];
            if self.soup.name(t) == Some(name) {
                self.pop_tag();
                break;
            }
            self.pop_tag();
        }
    }

    fn soup_starttag(&mut self, name: &str) {
        self.end_data();
        let id = self.append(Node::Tag { name: name.to_string(), children: vec![] });
        self.stack.push(id);
        *self.open_tag_counter.entry(name.to_string()).or_insert(0) += 1;
        if PRESERVE_WHITESPACE.contains(&name) {
            self.preserve_stack.push(id);
        }
    }

    fn soup_endtag(&mut self, name: &str) {
        self.end_data();
        self.pop_to_tag(name);
    }

    // BeautifulSoupHTMLParser's handlers

    fn handle_starttag(&mut self, tag: &str, handle_empty_element: bool) {
        self.soup_starttag(tag);
        if EMPTY_ELEMENTS.contains(&tag) && handle_empty_element {
            self.handle_endtag(tag, false);
            self.already_closed_empty_element.push(tag.to_string());
        }
    }

    fn handle_startendtag(&mut self, tag: &str) {
        self.handle_starttag(tag, false);
        self.handle_endtag(tag, false);
    }

    fn handle_endtag(&mut self, tag: &str, check_already_closed: bool) {
        if check_already_closed {
            if let Some(p) = self.already_closed_empty_element.iter().position(|t| t == tag) {
                self.already_closed_empty_element.remove(p);
                return;
            }
        }
        self.soup_endtag(tag);
    }

    fn handle_charref(&mut self, name: &str) {
        let (dereferenced, extra) = dereference_numeric(name);
        self.handle_data(&dereferenced);
        self.handle_data(&extra);
    }

    fn handle_entityref(&mut self, name: &str) {
        let data = match HTML_ENTITIES.binary_search_by(|(k, _)| (*k).cmp(name)) {
            Ok(i) => HTML_ENTITIES[i].1.to_string(),
            Err(_) => format!("&{name}"),
        };
        self.handle_data(&data);
    }

    fn handle_special(&mut self, data: &str) {
        // comment, declaration, CDATA, processing instruction: a string node of its own
        self.end_data();
        self.handle_data(data);
        self.end_data();
    }

    fn handle_decl(&mut self, decl: &str) {
        let rest: String = decl.chars().skip("DOCTYPE ".len()).collect();
        self.handle_special(&rest);
    }

    fn unknown_decl(&mut self, data: &str) {
        let d = if py::upper(data).starts_with("CDATA[") { data.chars().skip(6).collect() } else { data.to_string() };
        self.handle_special(&d);
    }
}

/// `int(name, base)` for what html.parser hands over: whitespace around, a sign, underscores, Unicode digits
/// and, in base 16, an optional "0x".
fn py_int_base(s: &str, base: u32) -> Option<u64> {
    let t = py::strip(s);
    let t = t.strip_prefix('+').unwrap_or(t);
    if t.starts_with('-') {
        return None; // a negative code point is no use: treat it like an unparsable one
    }
    let t = if base == 16 { t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")).map(|r| r.strip_prefix('_').unwrap_or(r)).unwrap_or(t) } else { t };
    if t.is_empty() || t.starts_with('_') || t.ends_with('_') || t.contains("__") {
        return None;
    }
    let mut v: u64 = 0;
    for c in t.chars() {
        if c == '_' {
            continue;
        }
        let d = if py::is_decimal_char(c) {
            c.to_digit(10).unwrap_or_else(|| crate::adapters::common::unicode_digit(c))
        } else if base == 16 && c.is_ascii_hexdigit() {
            c.to_digit(16).unwrap()
        } else {
            return None;
        };
        v = v.saturating_mul(base as u64).saturating_add(d as u64);
    }
    Some(v)
}

fn numeric_character_reference(n: u64) -> String {
    let replacement = "\u{fffd}".to_string();
    if n == 0 || n > 0x10ffff || (0xd800..=0xdfff).contains(&n) {
        return replacement;
    }
    if (0xfdd0..=0xfdef).contains(&n) || NONCHARACTERS.contains(&(n as u32)) {
        return char::from_u32(n as u32).map(|c| c.to_string()).unwrap_or(replacement);
    }
    if (0x80..=0x9f).contains(&n) {
        if let Some((_, s)) = WINDOWS_1252.iter().find(|(k, _)| *k as u64 == n) {
            return s.to_string();
        }
    }
    char::from_u32(n as u32).map(|c| c.to_string()).unwrap_or(replacement)
}

/// bs4's `_dereference_numeric_character_reference`: (text, data that follows it).
fn dereference_numeric(name: &str) -> (String, String) {
    let (body, base) = if name.starts_with('x') || name.starts_with('X') { (&name[1..], 16) } else { (name, 10) };
    let mut extra = String::new();
    let mut real: Option<u64> = py_int_base(body, base);
    if real.is_none() {
        let pattern = if base == 16 { r"^([0-9a-f]+)(.*)" } else { r"^([0-9]+)(.*)" };
        if let Some(m) = crate::adapters::re::re(pattern).captures(body) {
            real = u64::from_str_radix(&m[1], base).ok().or(Some(u64::MAX));
            extra = m[2].to_string();
        }
    }
    match real {
        None => (String::new(), body.to_string()),
        Some(n) => (numeric_character_reference(n), extra),
    }
}

// ------------------------------------------------------------------------------------------- tokenizer

struct Res {
    entityref: Regex,
    charref: Regex,
    incomplete_charref: Regex,
    incomplete: Regex,
    starttagopen: Regex,
    endtagopen: Regex,
    commentclose: Regex,
    commentabruptclose: Regex,
    tagfind: Fancy,
    attrfind: Fancy,
    locatetagend: Fancy,
}

fn res() -> &'static Res {
    static R: OnceLock<Res> = OnceLock::new();
    R.get_or_init(|| Res {
        entityref: Regex::new(r"\A&([a-zA-Z][-.a-zA-Z0-9]*)[^a-zA-Z0-9]").unwrap(),
        charref: Regex::new(r"\A&#(?:[0-9]+|[xX][0-9a-fA-F]+)[^0-9a-fA-F]").unwrap(),
        incomplete_charref: Regex::new(r"\A&#(?:[0-9]|[xX][0-9a-fA-F])").unwrap(),
        incomplete: Regex::new(r"\A&[a-zA-Z#]").unwrap(),
        starttagopen: Regex::new(r"\A<[a-zA-Z]").unwrap(),
        endtagopen: Regex::new(r"\A</[a-zA-Z]").unwrap(),
        commentclose: Regex::new(r"--!?>").unwrap(),
        commentabruptclose: Regex::new(r"\A-?>").unwrap(),
        tagfind: Fancy::new(r"\A([a-zA-Z][^\t\n\r\x0c />]*)(?:[\t\n\r\x0c ]|/(?!>))*").unwrap(),
        // the attribute name looks at the character before it: the text handed over starts one character early
        attrfind: Fancy::new(
            r#"\A(?s:.)((?<=['"\t\n\r\x0c /])[^\t\n\r\x0c />][^\t\n\r\x0c /=>]*)([\t\n\r\x0c ]*=[\t\n\r\x0c ]*('[^']*'|"[^"]*"|(?!['"])[^>\t\n\r\x0c ]*))?(?:[\t\n\r\x0c ]|/(?!>))*"#,
        )
        .unwrap(),
        locatetagend: Fancy::new(
            r#"\A[a-zA-Z][^\t\n\r\x0c />]*[\t\n\r\x0c /]*(?:(?<=['"\t\n\r\x0c /])[^\t\n\r\x0c />][^\t\n\r\x0c /=>]*(?:[\t\n\r\x0c ]*=[\t\n\r\x0c ]*(?:'[^']*'|"[^"]*"|(?!['"])[^>\t\n\r\x0c ]*))?[\t\n\r\x0c /]*)*>?"#,
        )
        .unwrap(),
    })
}

const CDATA_CONTENT_ELEMENTS: &[&str] = &["script", "style", "xmp", "iframe", "noembed", "noframes"];
const RCDATA_CONTENT_ELEMENTS: &[&str] = &["textarea", "title"];

struct Parser {
    b: Builder,
    rawdata: String,
    cdata_elem: Option<String>,
    interesting: Fancy,
}

fn interesting_normal() -> Fancy {
    Fancy::new("[&<]").unwrap()
}

/// The end of a match of `re` anchored at `pos` (`\A` in the pattern).
fn match_end(re: &Fancy, text: &str, pos: usize) -> Option<usize> {
    re.find(&text[pos..]).ok().flatten().map(|m| pos + m.end())
}

fn prev_char_start(text: &str, pos: usize) -> usize {
    text[..pos].char_indices().next_back().map(|(i, _)| i).unwrap_or(0)
}

fn chars_from(text: &str, pos: usize, n: usize) -> String {
    text[pos..].chars().take(n).collect()
}

impl Parser {
    fn set_cdata_mode(&mut self, elem: &str, escapable: bool) {
        let elem = py::lower(elem);
        // re.IGNORECASE | re.ASCII: only ASCII letters match either case
        let ci: String = elem
            .chars()
            .map(|c| if c.is_ascii_alphabetic() { format!("[{}{}]", c.to_ascii_lowercase(), c.to_ascii_uppercase()) } else { fancy_regex::escape(&c.to_string()).into_owned() })
            .collect();
        self.interesting = if elem == "plaintext" {
            Fancy::new(r"\z").unwrap()
        } else if escapable {
            Fancy::new(&format!(r"&|</{ci}(?=[\t\n\r\x0c />])")).unwrap()
        } else {
            Fancy::new(&format!(r"</{ci}(?=[\t\n\r\x0c />])")).unwrap()
        };
        self.cdata_elem = Some(elem);
    }

    fn clear_cdata_mode(&mut self) {
        self.interesting = interesting_normal();
        self.cdata_elem = None;
    }

    fn goahead(&mut self, end: bool) {
        let raw = std::mem::take(&mut self.rawdata);
        let n = raw.len();
        let r = res();
        let mut i = 0usize;
        while i < n {
            let j = match self.interesting.find_from_pos(&raw, i).ok().flatten() {
                Some(m) => m.start(),
                None => {
                    if self.cdata_elem.is_some() {
                        break;
                    }
                    n
                }
            };
            if i < j {
                self.b.handle_data(&raw[i..j]);
            }
            i = j;
            if i == n {
                break;
            }
            let rest = &raw[i..];
            if rest.starts_with('<') {
                let mut k: isize = if r.starttagopen.is_match(rest) {
                    self.parse_starttag(&raw, i)
                } else if rest.starts_with("</") {
                    self.parse_endtag(&raw, i)
                } else if rest.starts_with("<!--") {
                    self.parse_comment(&raw, i)
                } else if rest.starts_with("<?") {
                    self.parse_pi(&raw, i)
                } else if rest.starts_with("<!") {
                    self.parse_html_declaration(&raw, i)
                } else if i + 1 < n || end {
                    self.b.handle_data("<");
                    (i + 1) as isize
                } else {
                    break;
                };
                if k < 0 {
                    if !end {
                        break;
                    }
                    if r.starttagopen.is_match(rest) {
                        // an unfinished start tag at the end is dropped
                    } else if rest.starts_with("</") {
                        if i + 2 == n {
                            self.b.handle_data("</");
                        } else if r.endtagopen.is_match(rest) {
                        } else {
                            self.b.handle_special(&raw[i + 2..]);
                        }
                    } else if rest.starts_with("<!--") {
                        let mut j = n;
                        for suffix in ["--!", "--", "-"] {
                            if raw[i + 4..].ends_with(suffix) {
                                j -= suffix.len();
                                break;
                            }
                        }
                        self.b.handle_special(&raw[i + 4..j]);
                    } else if rest.starts_with("<![CDATA[") {
                        self.b.unknown_decl(&raw[i + 3..]);
                    } else if py::lower(&chars_from(&raw, i, 9)) == "<!doctype" {
                        self.b.handle_decl(&raw[i + 2..]);
                    } else if rest.starts_with("<!") {
                        self.b.handle_special(&raw[i + 2..]);
                    } else if rest.starts_with("<?") {
                        self.b.handle_special(&raw[i + 2..]);
                    }
                    k = n as isize;
                }
                i = k as usize;
            } else if rest.starts_with("&#") {
                if let Some(m) = r.charref.find(rest) {
                    let last = m.as_str().chars().next_back().unwrap();
                    let name = &m.as_str()[2..m.as_str().len() - last.len_utf8()];
                    self.b.handle_charref(name);
                    let mut k = i + m.end();
                    if last != ';' {
                        k -= last.len_utf8();
                    }
                    i = k;
                    continue;
                }
                if r.incomplete_charref.is_match(rest) {
                    if end {
                        self.b.handle_charref(&raw[i + 2..]);
                        i = n;
                    }
                    break;
                } else if rest.chars().nth(3).is_some() {
                    // not the end of the buffer, and can't be confused with some other construct
                    self.b.handle_data("&#");
                    i += 2;
                } else {
                    break;
                }
            } else if rest.starts_with('&') {
                if let Some(m) = r.entityref.captures(rest) {
                    let whole = m.get(0).unwrap().as_str();
                    let last = whole.chars().next_back().unwrap();
                    self.b.handle_entityref(&m[1]);
                    let mut k = i + whole.len();
                    if last != ';' {
                        k -= last.len_utf8();
                    }
                    i = k;
                    continue;
                }
                if r.incomplete.is_match(rest) {
                    if end {
                        self.b.handle_entityref(&raw[i + 1..]);
                        i = n;
                    }
                    break;
                } else if i + 1 < n {
                    self.b.handle_data("&");
                    i += 1;
                } else {
                    break;
                }
            } else {
                unreachable!("interesting.search() lied");
            }
        }
        if end && i < n {
            self.b.handle_data(&raw[i..n]);
            i = n;
        }
        self.rawdata = raw[i..].to_string();
    }

    fn parse_html_declaration(&mut self, raw: &str, i: usize) -> isize {
        let rest = &raw[i..];
        if rest.starts_with("<!--") {
            self.parse_comment(raw, i)
        } else if rest.starts_with("<![CDATA[") {
            match raw[i + 9..].find("]]>") {
                None => -1,
                Some(p) => {
                    let j = i + 9 + p;
                    self.b.unknown_decl(&raw[i + 3..j]);
                    (j + 3) as isize
                }
            }
        } else if py::lower(&chars_from(raw, i, 9)) == "<!doctype" {
            let start = i + chars_from(raw, i, 9).len();
            match raw[start..].find('>') {
                None => -1,
                Some(p) => {
                    let gt = start + p;
                    self.b.handle_decl(&raw[i + 2..gt]);
                    (gt + 1) as isize
                }
            }
        } else {
            self.parse_bogus_comment(raw, i)
        }
    }

    fn parse_comment(&mut self, raw: &str, i: usize) -> isize {
        let r = res();
        let (start, end) = match r.commentabruptclose.find(&raw[i + 4..]) {
            Some(m) => (i + 4 + m.start(), i + 4 + m.end()),
            None => match r.commentclose.find(&raw[i + 4..]) {
                Some(m) => (i + 4 + m.start(), i + 4 + m.end()),
                None => return -1,
            },
        };
        self.b.handle_special(&raw[i + 4..start]);
        end as isize
    }

    fn parse_bogus_comment(&mut self, raw: &str, i: usize) -> isize {
        match raw[i + 2..].find('>') {
            None => -1,
            Some(p) => {
                let pos = i + 2 + p;
                self.b.handle_special(&raw[i + 2..pos]);
                (pos + 1) as isize
            }
        }
    }

    fn parse_pi(&mut self, raw: &str, i: usize) -> isize {
        match raw[i + 2..].find('>') {
            None => -1,
            Some(p) => {
                let j = i + 2 + p;
                self.b.handle_special(&raw[i + 2..j]);
                (j + 1) as isize
            }
        }
    }

    fn check_for_whole_start_tag(&self, raw: &str, i: usize) -> isize {
        let j = match_end(&res().locatetagend, raw, i + 1).unwrap_or(i + 1);
        if !raw[..j].ends_with('>') {
            return -1;
        }
        j as isize
    }

    fn parse_starttag(&mut self, raw: &str, i: usize) -> isize {
        let endpos = self.check_for_whole_start_tag(raw, i);
        if endpos < 0 {
            return endpos;
        }
        let endpos = endpos as usize;
        let r = res();
        let caps = r.tagfind.captures(&raw[i + 1..]).ok().flatten().expect("unexpected call to parse_starttag()");
        let mut k = i + 1 + caps.get(0).unwrap().end();
        let tag = py::lower(caps.get(1).unwrap().as_str());
        while k < endpos {
            let from = prev_char_start(raw, k);
            match r.attrfind.find(&raw[from..]).ok().flatten() {
                Some(m) => k = from + m.end(),
                None => break,
            }
        }
        let end = py::strip(&raw[k..endpos]);
        if end != ">" && end != "/>" {
            self.b.handle_data(&raw[i..endpos]);
            return endpos as isize;
        }
        if end.ends_with("/>") {
            self.b.handle_startendtag(&tag);
        } else {
            self.b.handle_starttag(&tag, true);
            if CDATA_CONTENT_ELEMENTS.contains(&tag.as_str()) || tag == "plaintext" {
                self.set_cdata_mode(&tag, false);
            } else if RCDATA_CONTENT_ELEMENTS.contains(&tag.as_str()) {
                self.set_cdata_mode(&tag, true);
            }
        }
        endpos as isize
    }

    fn parse_endtag(&mut self, raw: &str, i: usize) -> isize {
        let r = res();
        if !raw[i + 2..].contains('>') {
            return -1;
        }
        if !r.endtagopen.is_match(&raw[i..]) {
            if raw[i + 2..].starts_with('>') {
                return (i + 3) as isize; // "</>" is ignored
            }
            return self.parse_bogus_comment(raw, i);
        }
        let j = match_end(&r.locatetagend, raw, i + 2).unwrap_or(i + 2);
        if !raw[..j].ends_with('>') {
            return -1;
        }
        let caps = r.tagfind.captures(&raw[i + 2..]).ok().flatten().expect("parse_endtag");
        let tag = py::lower(caps.get(1).unwrap().as_str());
        self.b.handle_endtag(&tag, true);
        self.clear_cdata_mode();
        j as isize
    }
}

/// `BeautifulSoup(markup, "html.parser")`.
pub fn parse(markup: &str) -> Soup {
    let mut p = Parser { b: Builder::new(), rawdata: String::new(), cdata_elem: None, interesting: interesting_normal() };
    // HTMLParser.feed then close: a first pass that stops at what may continue, then the end of the input.
    if !markup.is_empty() {
        p.rawdata = markup.to_string();
        p.goahead(false);
    }
    p.goahead(true);
    p.b.end_data();
    while p.b.stack.len() > 1 {
        p.b.pop_tag();
    }
    p.b.soup
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(s: &Soup, id: usize) -> Vec<String> {
        s.children(id).iter().map(|c| match &s.nodes[*c] { Node::Text(t) => t.clone(), Node::Tag { name, .. } => format!("<{name}>") }).collect()
    }

    #[test]
    fn quirks_like_bs4() {
        let s = parse("<a>&#12a\nrest</a><b>x</b>");
        let a = s.find(ROOT, "a").unwrap();
        assert_eq!(texts(&s, a), vec!["\x0ca"]);
        assert!(s.find(ROOT, "b").is_none());
        let s = parse("<a>x&amp;y &foo; &eacute &#233; &#x41;</a>");
        assert_eq!(texts(&s, s.find(ROOT, "a").unwrap()), vec!["x&y &foo é é A"]);
        let s = parse("<a></a><a>&#;</a>");
        let all = s.find_all(ROOT, "a");
        assert_eq!(texts(&s, all[1]), vec!["&#;"]);
    }
}
