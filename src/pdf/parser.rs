//! pdfminer's `PSStackParser` and its subclasses: the object parser of the file, the parser of object
//! streams, and the content-stream parser that feeds the page interpreter.

use std::collections::VecDeque;
use std::rc::Rc;

use super::lexer::{Lexer, Name, Token};
use super::objects::{literal_name, Dict, Obj, Stream};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Array,
    Dict,
    Proc,
    Inline,
}

/// The stack of `PSStackParser`: objects being built inside arrays, dictionaries and procedures.
#[derive(Default)]
pub struct StackCore {
    context: Vec<(usize, Option<Kind>, Vec<(usize, Obj)>)>,
    curtype: Option<Kind>,
    pub curstack: Vec<(usize, Obj)>,
    pub results: VecDeque<(usize, Obj)>,
}

pub enum Step {
    /// A keyword the subclass must handle (`do_keyword`).
    Keyword(usize, Vec<u8>),
    Done,
}

impl StackCore {
    pub fn reset(&mut self) {
        self.context.clear();
        self.curtype = None;
        self.curstack.clear();
        self.results.clear();
    }
    pub fn push(&mut self, pos: usize, o: Obj) {
        self.curstack.push((pos, o));
    }
    pub fn pop(&mut self, n: usize) -> Vec<(usize, Obj)> {
        if n == 0 {
            // Python's `curstack[-0:]` is the whole list.
            return std::mem::take(&mut self.curstack);
        }
        let at = self.curstack.len().saturating_sub(n);
        self.curstack.split_off(at)
    }
    pub fn popall(&mut self) -> Vec<(usize, Obj)> {
        std::mem::take(&mut self.curstack)
    }
    pub fn add_results(&mut self, objs: Vec<(usize, Obj)>) {
        self.results.extend(objs);
    }
    pub fn in_context(&self) -> bool {
        !self.context.is_empty()
    }
    pub fn start_type(&mut self, pos: usize, kind: Kind) {
        let stack = std::mem::take(&mut self.curstack);
        self.context.push((pos, self.curtype, stack));
        self.curtype = Some(kind);
    }
    pub fn end_type(&mut self, kind: Kind) -> Option<(usize, Vec<Obj>)> {
        if self.curtype != Some(kind) {
            return None;
        }
        let objs: Vec<Obj> = std::mem::take(&mut self.curstack).into_iter().map(|(_, o)| o).collect();
        let (pos, t, stack) = self.context.pop().unwrap();
        self.curtype = t;
        self.curstack = stack;
        Some((pos, objs))
    }

    /// One token through `nextobject`'s dispatch; keywords other than the structural ones go to the caller.
    pub fn feed(&mut self, pos: usize, tok: Token) -> Step {
        match tok {
            Token::Int(i) => self.push(pos, Obj::Int(i)),
            Token::Real(f) => self.push(pos, Obj::Real(f)),
            Token::Bool(b) => self.push(pos, Obj::Bool(b)),
            Token::Str(s) => self.push(pos, Obj::Str(s)),
            Token::Literal(n) => self.push(pos, Obj::Name(n)),
            Token::Keyword(k) => match k.as_slice() {
                b"[" => self.start_type(pos, Kind::Array),
                b"]" => {
                    if let Some((p, objs)) = self.end_type(Kind::Array) {
                        self.push(p, Obj::Array(objs));
                    }
                }
                b"<<" => self.start_type(pos, Kind::Dict),
                b">>" => {
                    if let Some((p, objs)) = self.end_type(Kind::Dict) {
                        if objs.len() % 2 == 0 {
                            let mut d = Dict::new();
                            for pair in objs.chunks(2) {
                                if matches!(pair[1], Obj::Null) {
                                    continue;
                                }
                                d.insert(literal_name(&pair[0]), pair[1].clone());
                            }
                            self.push(p, Obj::Dict(Rc::new(d)));
                        }
                        // An odd dictionary is a syntax error: pdfminer raises, the object is lost.
                    }
                }
                b"{" => self.start_type(pos, Kind::Proc),
                b"}" => {
                    if let Some((p, objs)) = self.end_type(Kind::Proc) {
                        self.push(p, Obj::Array(objs));
                    }
                }
                _ => return Step::Keyword(pos, k),
            },
        }
        Step::Done
    }
}

// ----------------------------------------------------------------------------------- content streams

/// `PDFContentParser`: the operands and operators of the page's content streams, one by one.
pub struct ContentParser<'a> {
    streams: Vec<&'a [u8]>,
    istream: usize,
    lexer: Option<Lexer<'a>>,
    core: StackCore,
}

impl<'a> ContentParser<'a> {
    pub fn new(streams: Vec<&'a [u8]>) -> Self {
        ContentParser { streams, istream: 0, lexer: None, core: StackCore::default() }
    }

    fn next_token(&mut self) -> Option<(usize, Token)> {
        loop {
            if self.lexer.is_none() {
                if self.istream >= self.streams.len() {
                    return None;
                }
                self.lexer = Some(Lexer::new(self.streams[self.istream]));
                self.istream += 1;
            }
            match self.lexer.as_mut().unwrap().next_token() {
                Some(t) => return Some(t),
                None => self.lexer = None,
            }
        }
    }

    pub fn next_object(&mut self) -> Option<Obj> {
        while self.core.results.is_empty() {
            let (pos, tok) = self.next_token()?;
            if let Step::Keyword(p, k) = self.core.feed(pos, tok) {
                self.do_keyword(p, k);
            }
            if !self.core.in_context() {
                let all = self.core.popall();
                self.core.add_results(all);
            }
        }
        self.core.results.pop_front().map(|(_, o)| o)
    }

    fn do_keyword(&mut self, pos: usize, k: Vec<u8>) {
        match k.as_slice() {
            b"BI" => self.core.start_type(pos, Kind::Inline),
            b"ID" => {
                if let Some((_, objs)) = self.core.end_type(Kind::Inline) {
                    if objs.len() % 2 != 0 {
                        return;
                    }
                    let mut d = Dict::new();
                    for pair in objs.chunks(2) {
                        d.insert(literal_name(&pair[0]), pair[1].clone());
                    }
                    let mut eos: &[u8] = b"EI";
                    if let Some(f) = d.get("F") {
                        let first = match f {
                            Obj::Array(a) => a.first().cloned(),
                            other => Some(other.clone()),
                        };
                        if let Some(Obj::Name(n)) = first {
                            if n == Name::Text("A85".into()) || n == Name::Text("ASCII85Decode".into()) {
                                eos = b"~>";
                            }
                        }
                    }
                    if let Some(lexer) = self.lexer.as_mut() {
                        let mut data = lexer.inline_data(pos + 3, eos);
                        if eos != b"EI" {
                            data.extend_from_slice(eos);
                        }
                        self.core.push(pos, Obj::Stream(Rc::new(Stream::new(d, data))));
                        if eos == b"EI" {
                            self.core.push(pos, Obj::Keyword(b"EI".to_vec()));
                        }
                    }
                }
            }
            _ => self.core.push(pos, Obj::Keyword(k)),
        }
    }
}

// ------------------------------------------------------------------------------------- file objects

/// `PDFParser`: objects of the file, read from any position.
pub struct FileParser<'a> {
    pub data: &'a [u8],
    lexer: Lexer<'a>,
    pub core: StackCore,
    pub fallback: bool,
    /// Resolves an indirect /Length (the document reads that object with its own parser).
    pub resolve: Option<&'a dyn Fn(u32) -> Obj>,
}

impl<'a> FileParser<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        FileParser { data, lexer: Lexer::new(data), core: StackCore::default(), fallback: false, resolve: None }
    }

    pub fn seek(&mut self, pos: usize) {
        self.lexer.seek(pos);
        self.core.reset();
    }

    pub fn next_token(&mut self) -> Option<(usize, Token)> {
        self.lexer.next_token()
    }

    /// `nextline()` from the lexer's current position.
    pub fn next_line(&mut self) -> Option<(usize, Vec<u8>)> {
        let d = self.data;
        let start = self.lexer.pos;
        if start >= d.len() {
            return None;
        }
        let mut i = start;
        while i < d.len() && d[i] != b'\r' && d[i] != b'\n' {
            i += 1;
        }
        if i < d.len() {
            if d[i] == b'\r' {
                i += 1;
                if i < d.len() && d[i] == b'\n' {
                    i += 1;
                }
            } else {
                i += 1;
            }
        }
        self.lexer.seek(i);
        Some((start, d[start..i].to_vec()))
    }

    /// `nextobject()`; `None` at the end of the file.
    pub fn next_object(&mut self) -> Option<Obj> {
        while self.core.results.is_empty() {
            let (pos, tok) = self.lexer.next_token()?;
            if let Step::Keyword(p, k) = self.core.feed(pos, tok) {
                self.do_keyword(p, k);
            }
            // PDFParser.flush does nothing: results only come from keywords.
        }
        self.core.results.pop_front().map(|(_, o)| o)
    }

    fn do_keyword(&mut self, pos: usize, k: Vec<u8>) {
        match k.as_slice() {
            b"xref" | b"startxref" => {
                let x = self.core.pop(1);
                self.core.add_results(x);
            }
            b"endobj" => {
                let x = self.core.pop(4);
                self.core.add_results(x);
            }
            b"null" => self.core.push(pos, Obj::Null),
            b"R" => {
                if self.core.curstack.len() >= 2 {
                    let two = self.core.pop(2);
                    if let Some(id) = safe_int(&two[0].1) {
                        if id >= 0 {
                            self.core.push(pos, Obj::Ref(id as u32));
                        }
                    }
                }
            }
            b"stream" => {
                let popped = self.core.pop(1);
                let dic = match popped.into_iter().next() {
                    Some((_, Obj::Dict(d))) => (*d).clone(),
                    Some(_) => Dict::new(),
                    None => return,
                };
                let mut objlen: usize = 0;
                if !self.fallback {
                    let len = match dic.get("Length") {
                        Some(Obj::Ref(id)) => self.resolve.map(|f| f(*id)),
                        other => other.cloned(),
                    };
                    if let Some(Obj::Int(n)) = len {
                        objlen = n.max(0) as usize;
                    }
                }
                self.lexer.seek(pos);
                let Some((_, line)) = self.next_line() else { return };
                let mut pos = pos + line.len();
                let d = self.data;
                let start = pos.min(d.len());
                let end = (pos + objlen).min(d.len());
                let mut data = d[start..end].to_vec();
                self.lexer.seek(pos + objlen);
                loop {
                    let Some((_, line)) = self.next_line() else { break };
                    if let Some(i) = find(&line, b"endstream") {
                        objlen += i;
                        if self.fallback {
                            data.extend_from_slice(&line[..i]);
                        }
                        break;
                    }
                    objlen += line.len();
                    if self.fallback {
                        data.extend_from_slice(&line);
                    }
                }
                self.lexer.seek(pos + objlen);
                pos = start;
                let s = Stream::new(dic, data);
                self.core.push(pos, Obj::Stream(Rc::new(s)));
            }
            _ => self.core.push(pos, Obj::Keyword(k)),
        }
    }
}

pub fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

pub fn safe_int(o: &Obj) -> Option<i64> {
    match o {
        Obj::Int(i) => Some(*i),
        Obj::Real(f) if f.is_finite() => Some(f.trunc() as i64),
        Obj::Bool(b) => Some(*b as i64),
        Obj::Str(s) => std::str::from_utf8(s).ok().and_then(|t| t.trim().parse().ok()),
        _ => None,
    }
}

/// `PDFStreamParser`: the objects packed in an object stream (every object is a result).
pub fn parse_object_stream(data: &[u8]) -> Vec<Obj> {
    let mut lexer = Lexer::new(data);
    let mut core = StackCore::default();
    let mut out = Vec::new();
    loop {
        while core.results.is_empty() {
            let Some((pos, tok)) = lexer.next_token() else {
                return out;
            };
            if let Step::Keyword(p, k) = core.feed(pos, tok) {
                match k.as_slice() {
                    b"R" => {
                        let two = core.pop(2);
                        if let Some((_, o)) = two.first() {
                            if let Some(id) = safe_int(o) {
                                core.push(p, Obj::Ref(id as u32));
                            }
                        }
                    }
                    b"obj" | b"endobj" => {}
                    // PDFStreamParser keeps "null" as a keyword (only PDFParser turns it into None).
                    _ => core.push(p, Obj::Keyword(k)),
                }
            }
            if !core.in_context() {
                let all = core.popall();
                core.add_results(all);
            }
        }
        if let Some((_, o)) = core.results.pop_front() {
            out.push(o);
        }
    }
}
