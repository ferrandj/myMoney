//! PostScript / PDF content tokenizer, written to behave exactly like pdfminer's `PSBaseParser`
//! (same token boundaries, same number and string rules, same quirks), so that the characters read from a
//! page are the ones the Python version read.

/// A literal name: text when it is valid UTF-8 (pdfminer keeps the raw bytes otherwise).
#[derive(Clone, Debug, PartialEq)]
pub enum Name {
    Text(String),
    Bytes(Vec<u8>),
}

impl Name {
    pub fn from_bytes(b: Vec<u8>) -> Name {
        match String::from_utf8(b) {
            Ok(s) => Name::Text(s),
            Err(e) => Name::Bytes(e.into_bytes()),
        }
    }
    /// `literal_name()`: the text, or the Python `str()` of the bytes when they are not UTF-8.
    pub fn as_str(&self) -> String {
        match self {
            Name::Text(s) => s.clone(),
            Name::Bytes(b) => python_bytes_repr(b),
        }
    }
}

pub fn python_bytes_repr(b: &[u8]) -> String {
    let mut s = String::from("b'");
    for &c in b {
        match c {
            b'\\' => s.push_str("\\\\"),
            b'\'' => s.push_str("\\'"),
            b'\t' => s.push_str("\\t"),
            b'\n' => s.push_str("\\n"),
            b'\r' => s.push_str("\\r"),
            0x20..=0x7e => s.push(c as char),
            _ => s.push_str(&format!("\\x{:02x}", c)),
        }
    }
    s.push('\'');
    s
}

#[derive(Clone, Debug, PartialEq)]
pub enum Token {
    Int(i64),
    Real(f64),
    Bool(bool),
    Literal(Name),
    Keyword(Vec<u8>),
    Str(Vec<u8>),
}

#[derive(Clone, Copy, PartialEq)]
enum State {
    Main,
    Comment,
    Literal,
    LiteralHex,
    Number,
    Float,
    Keyword,
    Str,
    Str1,
    WOpen,
    WClose,
    HexString,
}

/// `\s` of Python's bytes regular expressions.
pub fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

fn end_literal(c: u8) -> bool {
    matches!(c, b'#' | b'/' | b'%' | b'[' | b']' | b'(' | b')' | b'<' | b'>' | b'{' | b'}') || is_space(c)
}

fn is_hex(c: u8) -> bool {
    c.is_ascii_hexdigit()
}

pub struct Lexer<'a> {
    pub data: &'a [u8],
    pub pos: usize,
    state: State,
    cur: Vec<u8>,
    cur_pos: usize,
    paren: i32,
    hex: Vec<u8>,
    oct: Vec<u8>,
    tokens: std::collections::VecDeque<(usize, Token)>,
    pub eof: bool,
}

impl<'a> Lexer<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Lexer {
            data,
            pos: 0,
            state: State::Main,
            cur: Vec::new(),
            cur_pos: 0,
            paren: 0,
            hex: Vec::new(),
            oct: Vec::new(),
            tokens: Default::default(),
            eof: false,
        }
    }

    /// Restart reading at `pos` (pdfminer's `seek`): the pending token is dropped.
    pub fn seek(&mut self, pos: usize) {
        self.pos = pos;
        self.state = State::Main;
        self.cur.clear();
        self.tokens.clear();
        self.eof = false;
    }

    fn add(&mut self, t: Token) {
        self.tokens.push_back((self.cur_pos, t));
    }

    /// One step of the state machine over `s` from `i`: returns the next index (as pdfminer's `_parse_*`).
    fn step(&mut self, s: &[u8], i: usize, base: usize) -> usize {
        match self.state {
            State::Main => {
                let mut j = i;
                while j < s.len() && is_space(s[j]) {
                    j += 1;
                }
                if j >= s.len() {
                    return s.len();
                }
                let c = s[j];
                self.cur_pos = base + j;
                match c {
                    b'%' => {
                        self.cur = vec![b'%'];
                        self.state = State::Comment;
                    }
                    b'/' => {
                        self.cur.clear();
                        self.state = State::Literal;
                    }
                    b'-' | b'+' | b'0'..=b'9' => {
                        self.cur = vec![c];
                        self.state = State::Number;
                    }
                    b'.' => {
                        self.cur = vec![c];
                        self.state = State::Float;
                    }
                    b'a'..=b'z' | b'A'..=b'Z' => {
                        self.cur = vec![c];
                        self.state = State::Keyword;
                    }
                    b'(' => {
                        self.cur.clear();
                        self.paren = 1;
                        self.state = State::Str;
                    }
                    b'<' => {
                        self.cur.clear();
                        self.state = State::WOpen;
                    }
                    b'>' => {
                        self.cur.clear();
                        self.state = State::WClose;
                    }
                    0 => {}
                    _ => self.add(Token::Keyword(vec![c])),
                }
                j + 1
            }
            State::Comment => {
                let mut j = i;
                while j < s.len() && s[j] != b'\r' && s[j] != b'\n' {
                    j += 1;
                }
                self.cur.extend_from_slice(&s[i..j]);
                if j >= s.len() {
                    return s.len();
                }
                self.state = State::Main;
                j
            }
            State::Literal => {
                let mut j = i;
                while j < s.len() && !end_literal(s[j]) {
                    j += 1;
                }
                self.cur.extend_from_slice(&s[i..j]);
                if j >= s.len() {
                    return s.len();
                }
                if s[j] == b'#' {
                    self.hex.clear();
                    self.state = State::LiteralHex;
                    return j + 1;
                }
                let name = Name::from_bytes(std::mem::take(&mut self.cur));
                self.add(Token::Literal(name));
                self.state = State::Main;
                j
            }
            State::LiteralHex => {
                if i < s.len() && is_hex(s[i]) && self.hex.len() < 2 {
                    self.hex.push(s[i]);
                    return i + 1;
                }
                if !self.hex.is_empty() {
                    let v = u8::from_str_radix(std::str::from_utf8(&self.hex).unwrap(), 16).unwrap();
                    self.cur.push(v);
                }
                self.state = State::Literal;
                i
            }
            State::Number => {
                let mut j = i;
                while j < s.len() && s[j].is_ascii_digit() {
                    j += 1;
                }
                self.cur.extend_from_slice(&s[i..j]);
                if j >= s.len() {
                    return s.len();
                }
                if s[j] == b'.' {
                    self.cur.push(b'.');
                    self.state = State::Float;
                    return j + 1;
                }
                if let Some(v) = parse_python_int(&self.cur) {
                    self.add(Token::Int(v));
                }
                self.state = State::Main;
                j
            }
            State::Float => {
                let mut j = i;
                while j < s.len() && s[j].is_ascii_digit() {
                    j += 1;
                }
                self.cur.extend_from_slice(&s[i..j]);
                if j >= s.len() {
                    return s.len();
                }
                if let Some(v) = parse_python_float(&self.cur) {
                    self.add(Token::Real(v));
                }
                self.state = State::Main;
                j
            }
            State::Keyword => {
                let mut j = i;
                while j < s.len() && !end_literal(s[j]) {
                    j += 1;
                }
                if j >= s.len() {
                    self.cur.extend_from_slice(&s[i..]);
                    return s.len();
                }
                self.cur.extend_from_slice(&s[i..j]);
                let tok = match self.cur.as_slice() {
                    b"true" => Token::Bool(true),
                    b"false" => Token::Bool(false),
                    _ => Token::Keyword(self.cur.clone()),
                };
                self.add(tok);
                self.state = State::Main;
                j
            }
            State::Str => {
                let mut j = i;
                while j < s.len() && !matches!(s[j], b'(' | b')' | b'\\') {
                    j += 1;
                }
                self.cur.extend_from_slice(&s[i..j]);
                if j >= s.len() {
                    return s.len();
                }
                let c = s[j];
                if c == b'\\' {
                    self.oct.clear();
                    self.state = State::Str1;
                    return j + 1;
                }
                if c == b'(' {
                    self.paren += 1;
                    self.cur.push(c);
                    return j + 1;
                }
                // ')'
                self.paren -= 1;
                if self.paren != 0 {
                    self.cur.push(c);
                    return j + 1;
                }
                let v = std::mem::take(&mut self.cur);
                self.add(Token::Str(v));
                self.state = State::Main;
                j + 1
            }
            State::Str1 => {
                let c = if i < s.len() { Some(s[i]) } else { None };
                if let Some(c) = c {
                    if (b'0'..=b'7').contains(&c) && self.oct.len() < 3 {
                        self.oct.push(c);
                        return i + 1;
                    }
                }
                if !self.oct.is_empty() {
                    let code = u32::from_str_radix(std::str::from_utf8(&self.oct).unwrap(), 8).unwrap();
                    // pdfminer asserts < 256; keep the low byte rather than stop reading the page.
                    self.cur.push((code & 0xff) as u8);
                    self.state = State::Str;
                    return i;
                }
                let mut i = i;
                match c {
                    Some(b'b') => self.cur.push(8),
                    Some(b't') => self.cur.push(9),
                    Some(b'n') => self.cur.push(10),
                    Some(b'f') => self.cur.push(12),
                    Some(b'r') => self.cur.push(13),
                    Some(b'(') => self.cur.push(40),
                    Some(b')') => self.cur.push(41),
                    Some(b'\\') => self.cur.push(92),
                    Some(b'\r') if i + 1 < s.len() && s[i + 1] == b'\n' => i += 1,
                    _ => {}
                }
                self.state = State::Str;
                i + 1
            }
            State::WOpen => {
                if i < s.len() && s[i] == b'<' {
                    self.add(Token::Keyword(b"<<".to_vec()));
                    self.state = State::Main;
                    return i + 1;
                }
                self.state = State::HexString;
                i
            }
            State::WClose => {
                let mut i = i;
                if i < s.len() && s[i] == b'>' {
                    self.add(Token::Keyword(b">>".to_vec()));
                    i += 1;
                }
                self.state = State::Main;
                i
            }
            State::HexString => {
                let mut j = i;
                while j < s.len() && (is_space(s[j]) || is_hex(s[j])) {
                    j += 1;
                }
                self.cur.extend_from_slice(&s[i..j]);
                if j >= s.len() {
                    return s.len();
                }
                let digits: Vec<u8> = self.cur.iter().copied().filter(|c| !is_space(*c)).collect();
                let mut out = Vec::new();
                let mut k = 0;
                while k < digits.len() {
                    if k + 1 < digits.len() {
                        out.push(u8::from_str_radix(std::str::from_utf8(&digits[k..k + 2]).unwrap(), 16).unwrap());
                        k += 2;
                    } else {
                        // HEX_PAIR's "." alternative: a lone digit is its own value (int("a", 16) == 10).
                        out.push(u8::from_str_radix(std::str::from_utf8(&digits[k..k + 1]).unwrap(), 16).unwrap());
                        k += 1;
                    }
                }
                self.cur.clear();
                self.add(Token::Str(out));
                self.state = State::Main;
                j
            }
        }
    }

    /// The next token with its position, or None at the end of the data.
    pub fn next_token(&mut self) -> Option<(usize, Token)> {
        if self.eof && self.tokens.is_empty() {
            return None;
        }
        while self.tokens.is_empty() {
            if self.pos >= self.data.len() {
                // End of data in the middle of a token: finish it as if a newline followed.
                let st = self.state;
                if st != State::Main {
                    let s = b"\n";
                    self.step(s, 0, self.data.len());
                    // A string or comment stays open: nothing more will come.
                }
                self.eof = true;
                if self.tokens.is_empty() {
                    return None;
                }
                break;
            }
            let data = self.data;
            let p = self.pos;
            self.pos = self.step(data, p, 0);
        }
        self.tokens.pop_front()
    }

    /// The raw bytes of an inline image, as pdfminer's `get_inline_data` reads them.
    pub fn inline_data(&mut self, pos: usize, target: &[u8]) -> Vec<u8> {
        self.seek(pos.min(self.data.len()));
        let s = self.data;
        let mut i = 0usize;
        let mut data: Vec<u8> = Vec::new();
        let mut p = self.pos;
        while i <= target.len() {
            if p >= s.len() {
                break;
            }
            if i > 0 {
                let c = s[p];
                data.push(c);
                p += 1;
                if (target.len() <= i && is_space(c)) || (i < target.len() && c == target[i]) {
                    i += 1;
                } else {
                    i = 0;
                }
            } else {
                match s[p..].iter().position(|&c| c == target[0]) {
                    Some(k) => {
                        data.extend_from_slice(&s[p..p + k + 1]);
                        p += k + 1;
                        i = 1;
                    }
                    None => {
                        data.extend_from_slice(&s[p..]);
                        p = s.len();
                    }
                }
            }
        }
        self.pos = p;
        let cut = target.len() + 1;
        if data.len() >= cut {
            data.truncate(data.len() - cut);
        } else {
            data.clear();
        }
        if data.ends_with(b"\r\n") {
            data.truncate(data.len() - 2);
        } else if data.ends_with(b"\r") || data.ends_with(b"\n") {
            data.truncate(data.len() - 1);
        }
        data
    }
}

/// Python's `int(b"...")`: optional sign, digits (underscores are not produced by the tokenizer).
pub fn parse_python_int(b: &[u8]) -> Option<i64> {
    let s = std::str::from_utf8(b).ok()?;
    let t = s.strip_prefix('+').unwrap_or(s);
    if t.is_empty() || t == "-" {
        return None;
    }
    match t.parse::<i64>() {
        Ok(v) => Some(v),
        Err(_) => t.parse::<f64>().ok().map(|f| f as i64),
    }
}

/// Python's `float(b"...")` for what the tokenizer collects: sign, digits and one dot.
pub fn parse_python_float(b: &[u8]) -> Option<f64> {
    let s = std::str::from_utf8(b).ok()?;
    let t = s.strip_prefix('+').unwrap_or(s);
    if t.is_empty() || t == "-" || t == "." || t == "-." {
        return None;
    }
    t.parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(s: &[u8]) -> Vec<Token> {
        let mut l = Lexer::new(s);
        let mut v = vec![];
        while let Some((_, t)) = l.next_token() {
            v.push(t);
        }
        v
    }

    #[test]
    fn numbers_names_strings() {
        assert_eq!(
            all(b"12 -3 .5 4. -.25 /F1 (a\\(b\\)) <414> [ ] << >> BT"),
            vec![
                Token::Int(12),
                Token::Int(-3),
                Token::Real(0.5),
                Token::Real(4.0),
                Token::Real(-0.25),
                Token::Literal(Name::Text("F1".into())),
                Token::Str(b"a(b)".to_vec()),
                Token::Str(vec![0x41, 0x04]),
                Token::Keyword(b"[".to_vec()),
                Token::Keyword(b"]".to_vec()),
                Token::Keyword(b"<<".to_vec()),
                Token::Keyword(b">>".to_vec()),
                Token::Keyword(b"BT".to_vec()),
            ]
        );
        assert_eq!(all(b"(\\351t\\351)"), vec![Token::Str(vec![0xe9, b't', 0xe9])]);
        assert_eq!(all(b"/A#20B"), vec![Token::Literal(Name::Text("A B".into()))]);
        assert_eq!(all(b"Tj"), vec![Token::Keyword(b"Tj".to_vec())]);
    }
}
