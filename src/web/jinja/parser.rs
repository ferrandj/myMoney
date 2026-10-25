//! Jinja2's lexer and parser, for the syntax the templates use: `{{ }}`, `{% %}`, `{# #}` with the `-` whitespace
//! control, and Jinja2's operator precedence (filters and tests bind tighter than arithmetic).

use std::sync::Arc;

use super::value::{err, BinOp, Error, Result};

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Data(String),
    VarBegin,
    VarEnd,
    BlockBegin,
    BlockEnd,
    Name(String),
    Str(String),
    Int(i64),
    Float(f64),
    Op(&'static str),
    Eof,
}

const OPS: &[&str] = &["//", "**", "==", "!=", ">=", "<=", "+", "-", "/", "*", "%", "~", "[", "]", "(", ")", "{", "}", ">", "<", "=", ".", ":", "|", ",", ";"];

fn is_ws(c: char) -> bool {
    c.is_whitespace()
}

fn unescape(raw: &str) -> Result<String> {
    let mut out = String::new();
    let mut it = raw.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            Some('\\') => out.push('\\'),
            Some('\'') => out.push('\''),
            Some('"') => out.push('"'),
            Some('a') => out.push('\x07'),
            Some('b') => out.push('\x08'),
            Some('f') => out.push('\x0c'),
            Some('v') => out.push('\x0b'),
            Some('\n') => {}
            Some(k @ ('x' | 'u' | 'U')) => {
                let n = match k {
                    'x' => 2,
                    'u' => 4,
                    _ => 8,
                };
                let hex: String = (0..n).filter_map(|_| it.next()).collect();
                let code = u32::from_str_radix(&hex, 16).map_err(|_| Error(format!("bad escape \\{k}{hex}")))?;
                out.push(char::from_u32(code).ok_or_else(|| Error("bad escape".into()))?);
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    Ok(out)
}

fn lex(source: &str) -> Result<Vec<Tok>> {
    // keep_trailing_newline=False: one final newline is dropped; newlines are normalized to "\n".
    let mut src = source.replace("\r\n", "\n").replace('\r', "\n");
    if src.ends_with('\n') {
        src.pop();
    }
    let s: Vec<char> = src.chars().collect();
    let mut toks = vec![];
    let mut i = 0;
    let mut strip_next = false;
    let starts = |i: usize, pat: &str| -> bool {
        let p: Vec<char> = pat.chars().collect();
        i + p.len() <= s.len() && s[i..i + p.len()] == p[..]
    };
    while i < s.len() {
        // data up to the next tag
        let mut j = i;
        while j < s.len() && !(s[j] == '{' && j + 1 < s.len() && matches!(s[j + 1], '{' | '%' | '#')) {
            j += 1;
        }
        let mut data: String = s[i..j].iter().collect();
        if strip_next {
            data = data.trim_start_matches(is_ws).to_string();
            strip_next = false;
        }
        if j >= s.len() {
            if !data.is_empty() {
                toks.push(Tok::Data(data));
            }
            break;
        }
        let kind = s[j + 1];
        let mut k = j + 2;
        if k < s.len() && s[k] == '-' {
            data = data.trim_end_matches(is_ws).to_string();
            k += 1;
        } else if k < s.len() && s[k] == '+' {
            k += 1;
        }
        if !data.is_empty() {
            toks.push(Tok::Data(data));
        }
        if kind == '#' {
            // comment: up to "#}" (or "-#}")
            let mut m = k;
            while m + 1 < s.len() && !(s[m] == '#' && s[m + 1] == '}') {
                m += 1;
            }
            if m + 1 >= s.len() {
                return err("missing end of comment tag");
            }
            if m > k && s[m - 1] == '-' {
                strip_next = true;
            }
            i = m + 2;
            continue;
        }
        let (begin, end, close) = if kind == '{' { (Tok::VarBegin, Tok::VarEnd, "}}") } else { (Tok::BlockBegin, Tok::BlockEnd, "%}") };
        toks.push(begin);
        let mut depth = 0usize;
        let mut m = k;
        loop {
            while m < s.len() && is_ws(s[m]) {
                m += 1;
            }
            if m >= s.len() {
                return err(format!("unexpected end of template, expected '{close}'"));
            }
            if depth == 0 {
                if starts(m, &format!("-{close}")) {
                    strip_next = true;
                    m += 3;
                    break;
                }
                if starts(m, close) {
                    m += 2;
                    break;
                }
            }
            let c = s[m];
            if c == '\'' || c == '"' {
                let mut n = m + 1;
                while n < s.len() && s[n] != c {
                    if s[n] == '\\' {
                        n += 1;
                    }
                    n += 1;
                }
                if n >= s.len() {
                    return err("unterminated string");
                }
                let raw: String = s[m + 1..n].iter().collect();
                toks.push(Tok::Str(unescape(&raw)?));
                m = n + 1;
                continue;
            }
            if c.is_ascii_digit() {
                let mut n = m;
                while n < s.len() && (s[n].is_ascii_digit() || s[n] == '_') {
                    n += 1;
                }
                let mut is_float = false;
                if n + 1 < s.len() && s[n] == '.' && s[n + 1].is_ascii_digit() {
                    is_float = true;
                    n += 1;
                    while n < s.len() && (s[n].is_ascii_digit() || s[n] == '_') {
                        n += 1;
                    }
                }
                if n < s.len() && (s[n] == 'e' || s[n] == 'E') {
                    let mut q = n + 1;
                    if q < s.len() && (s[q] == '+' || s[q] == '-') {
                        q += 1;
                    }
                    if q < s.len() && s[q].is_ascii_digit() {
                        is_float = true;
                        n = q;
                        while n < s.len() && s[n].is_ascii_digit() {
                            n += 1;
                        }
                    }
                }
                let text: String = s[m..n].iter().filter(|c| **c != '_').collect();
                toks.push(if is_float {
                    Tok::Float(text.parse().map_err(|_| Error("bad float".into()))?)
                } else {
                    Tok::Int(text.parse().map_err(|_| Error("bad integer".into()))?)
                });
                m = n;
                continue;
            }
            if c.is_alphabetic() || c == '_' {
                let mut n = m;
                while n < s.len() && (s[n].is_alphanumeric() || s[n] == '_') {
                    n += 1;
                }
                toks.push(Tok::Name(s[m..n].iter().collect()));
                m = n;
                continue;
            }
            let op = OPS.iter().find(|op| starts(m, op)).ok_or_else(|| Error(format!("unexpected char {c:?}")))?;
            match *op {
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" => depth = depth.saturating_sub(1),
                _ => {}
            }
            toks.push(Tok::Op(op));
            m += op.chars().count();
        }
        toks.push(end);
        i = m;
    }
    toks.push(Tok::Eof);
    Ok(toks)
}

// --------------------------------------------------------------------------------------------- AST

#[derive(Clone, Debug)]
pub enum Const {
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    In,
    NotIn,
}

#[derive(Clone, Debug)]
pub enum Expr {
    Const(Const),
    Name(String),
    Attr(Box<Expr>, String),
    Item(Box<Expr>, Box<Expr>),
    Slice(Box<Expr>, Option<Box<Expr>>, Option<Box<Expr>>, Option<Box<Expr>>),
    Call(Box<Expr>, Vec<Expr>, Vec<(String, Expr)>),
    Filter(Box<Expr>, String, Vec<Expr>, Vec<(String, Expr)>),
    Test(Box<Expr>, String, Vec<Expr>, bool),
    Bin(BinOp, Box<Expr>, Box<Expr>),
    Concat(Vec<Expr>),
    Neg(Box<Expr>),
    Pos(Box<Expr>),
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Compare(Box<Expr>, Vec<(CmpOp, Expr)>),
    Cond(Box<Expr>, Box<Expr>, Option<Box<Expr>>),
    List(Vec<Expr>),
    Tuple(Vec<Expr>),
    Dict(Vec<(Expr, Expr)>),
}

#[derive(Clone, Debug)]
pub enum Target {
    Name(String),
    Tuple(Vec<Target>),
}

#[derive(Debug)]
pub struct MacroDef {
    pub name: String,
    pub args: Vec<(String, Option<Expr>)>,
    pub body: Vec<Node>,
}

#[derive(Debug)]
pub enum Node {
    Data(String),
    Output(Expr),
    If(Vec<(Expr, Vec<Node>)>, Vec<Node>),
    For { target: Target, iter: Expr, filter: Option<Expr>, body: Vec<Node>, else_: Vec<Node> },
    Set(Target, Expr),
    Block(String),
    Extends(Expr),
    Include(Expr),
    FromImport { template: Expr, names: Vec<(String, String)>, with_context: bool },
    Macro(Arc<MacroDef>),
}

#[derive(Debug)]
pub struct Template {
    pub name: String,
    pub body: Vec<Node>,
    pub blocks: Vec<(String, Vec<Node>)>,
}

impl Template {
    pub fn block(&self, name: &str) -> Option<&Vec<Node>> {
        self.blocks.iter().find(|(n, _)| n == name).map(|(_, b)| b)
    }
}

// ------------------------------------------------------------------------------------------ parser

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    blocks: Vec<(String, Vec<Node>)>,
}

pub fn parse(name: &str, source: &str) -> Result<Template> {
    let mut p = Parser { toks: lex(source)?, pos: 0, blocks: vec![] };
    let body = p.subparse(&[])?;
    if p.peek() != &Tok::Eof {
        return err(format!("{name}: unexpected tag"));
    }
    Ok(Template { name: name.to_string(), body, blocks: p.blocks })
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.toks[self.pos]
    }
    fn peek_at(&self, n: usize) -> &Tok {
        self.toks.get(self.pos + n).unwrap_or(&Tok::Eof)
    }
    fn next(&mut self) -> Tok {
        let t = self.toks[self.pos].clone();
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }
    fn is_op(&self, op: &str) -> bool {
        matches!(self.peek(), Tok::Op(o) if *o == op)
    }
    fn is_name(&self, name: &str) -> bool {
        matches!(self.peek(), Tok::Name(n) if n == name)
    }
    fn skip_op(&mut self, op: &str) -> bool {
        if self.is_op(op) {
            self.next();
            true
        } else {
            false
        }
    }
    fn skip_name(&mut self, name: &str) -> bool {
        if self.is_name(name) {
            self.next();
            true
        } else {
            false
        }
    }
    fn expect_op(&mut self, op: &str) -> Result<()> {
        if self.skip_op(op) {
            Ok(())
        } else {
            err(format!("expected '{op}', got {:?}", self.peek()))
        }
    }
    fn expect_name(&mut self) -> Result<String> {
        match self.next() {
            Tok::Name(n) => Ok(n),
            other => err(format!("expected a name, got {other:?}")),
        }
    }
    fn expect(&mut self, t: Tok) -> Result<()> {
        let got = self.next();
        if got == t {
            Ok(())
        } else {
            err(format!("expected {t:?}, got {got:?}"))
        }
    }

    /// Nodes up to one of the `{% end %}` keywords (left unconsumed, after the `{%`).
    fn subparse(&mut self, ends: &[&str]) -> Result<Vec<Node>> {
        let mut body = vec![];
        loop {
            match self.peek().clone() {
                Tok::Eof => {
                    if ends.is_empty() {
                        return Ok(body);
                    }
                    return err(format!("unexpected end of template, expected {ends:?}"));
                }
                Tok::Data(d) => {
                    self.next();
                    body.push(Node::Data(d));
                }
                Tok::VarBegin => {
                    self.next();
                    let e = self.parse_tuple(true, false)?;
                    self.expect(Tok::VarEnd)?;
                    body.push(Node::Output(e));
                }
                Tok::BlockBegin => {
                    if let Tok::Name(n) = self.peek_at(1) {
                        if ends.contains(&n.as_str()) {
                            self.next();
                            return Ok(body);
                        }
                    }
                    self.next();
                    body.push(self.parse_statement()?);
                }
                other => return err(format!("unexpected {other:?}")),
            }
        }
    }

    fn end_tag(&mut self) -> Result<()> {
        self.expect(Tok::BlockEnd)
    }

    fn parse_statement(&mut self) -> Result<Node> {
        let name = self.expect_name()?;
        match name.as_str() {
            "if" => {
                let mut branches = vec![];
                let mut test = self.parse_tuple(false, false)?;
                let mut else_ = vec![];
                loop {
                    self.end_tag()?;
                    let body = self.subparse(&["elif", "else", "endif"])?;
                    branches.push((test, body));
                    match self.expect_name()?.as_str() {
                        "elif" => {
                            test = self.parse_tuple(false, false)?;
                        }
                        "else" => {
                            self.end_tag()?;
                            else_ = self.subparse(&["endif"])?;
                            self.expect_name()?;
                            self.end_tag()?;
                            break;
                        }
                        _ => {
                            self.end_tag()?;
                            break;
                        }
                    }
                }
                Ok(Node::If(branches, else_))
            }
            "for" => {
                let target = self.parse_target(true)?;
                if !self.skip_name("in") {
                    return err("expected 'in'");
                }
                let iter = self.parse_tuple(false, true)?;
                let filter = if self.skip_name("if") { Some(self.parse_expression(true)?) } else { None };
                self.end_tag()?;
                let body = self.subparse(&["endfor", "else"])?;
                let else_ = if self.expect_name()? == "else" {
                    self.end_tag()?;
                    let e = self.subparse(&["endfor"])?;
                    self.expect_name()?;
                    e
                } else {
                    vec![]
                };
                self.end_tag()?;
                Ok(Node::For { target, iter, filter, body, else_ })
            }
            "set" => {
                let target = self.parse_target(true)?;
                self.expect_op("=")?;
                let value = self.parse_tuple(true, false)?;
                self.end_tag()?;
                Ok(Node::Set(target, value))
            }
            "block" => {
                let name = self.expect_name()?;
                self.end_tag()?;
                let body = self.subparse(&["endblock"])?;
                self.expect_name()?;
                if let Tok::Name(_) = self.peek() {
                    self.next();
                }
                self.end_tag()?;
                self.blocks.push((name.clone(), body));
                Ok(Node::Block(name))
            }
            "extends" => {
                let e = self.parse_expression(true)?;
                self.end_tag()?;
                Ok(Node::Extends(e))
            }
            "include" => {
                let e = self.parse_expression(true)?;
                if self.skip_name("with") || self.skip_name("without") {
                    self.skip_name("context");
                }
                self.end_tag()?;
                Ok(Node::Include(e))
            }
            "from" => {
                let template = self.parse_expression(true)?;
                if !self.skip_name("import") {
                    return err("expected 'import'");
                }
                let mut names = vec![];
                let mut with_context = false;
                loop {
                    if self.is_name("with") || self.is_name("without") {
                        if let Tok::Name(n) = self.peek_at(1) {
                            if n == "context" {
                                with_context = self.expect_name()? == "with";
                                self.next();
                                break;
                            }
                        }
                    }
                    if !names.is_empty() && !self.skip_op(",") {
                        break;
                    }
                    if matches!(self.peek(), Tok::BlockEnd) {
                        break;
                    }
                    let n = self.expect_name()?;
                    let alias = if self.skip_name("as") { self.expect_name()? } else { n.clone() };
                    names.push((n, alias));
                }
                self.end_tag()?;
                Ok(Node::FromImport { template, names, with_context })
            }
            "macro" => {
                let name = self.expect_name()?;
                self.expect_op("(")?;
                let mut args = vec![];
                while !self.skip_op(")") {
                    if !args.is_empty() {
                        self.expect_op(",")?;
                        if self.skip_op(")") {
                            break;
                        }
                    }
                    let a = self.expect_name()?;
                    let default = if self.skip_op("=") { Some(self.parse_expression(true)?) } else { None };
                    args.push((a, default));
                }
                self.end_tag()?;
                let body = self.subparse(&["endmacro"])?;
                self.expect_name()?;
                self.end_tag()?;
                Ok(Node::Macro(Arc::new(MacroDef { name, args, body })))
            }
            other => err(format!("unknown tag '{other}'")),
        }
    }

    fn parse_target(&mut self, allow_tuple: bool) -> Result<Target> {
        let mut items = vec![];
        loop {
            let t = if self.skip_op("(") {
                let inner = self.parse_target(true)?;
                self.expect_op(")")?;
                inner
            } else {
                Target::Name(self.expect_name()?)
            };
            items.push(t);
            if allow_tuple && self.skip_op(",") {
                if self.is_name("in") || self.is_op("=") || self.is_op(")") {
                    break;
                }
                continue;
            }
            break;
        }
        if items.len() == 1 && !allow_tuple {
            return Ok(items.pop().unwrap());
        }
        Ok(if items.len() == 1 { items.pop().unwrap() } else { Target::Tuple(items) })
    }

    fn is_tuple_end(&self, extra_end: bool) -> bool {
        match self.peek() {
            Tok::VarEnd | Tok::BlockEnd | Tok::Eof => true,
            Tok::Op(")") | Tok::Op("]") | Tok::Op("}") => true,
            Tok::Name(n) if extra_end && n == "recursive" => true,
            _ => false,
        }
    }

    /// `parse_tuple`: one expression, or several separated by commas (a tuple without parentheses).
    fn parse_tuple(&mut self, with_condexpr: bool, extra_end: bool) -> Result<Expr> {
        let mut args = vec![];
        let mut is_tuple = false;
        loop {
            if !args.is_empty() {
                self.expect_op(",")?;
            }
            if self.is_tuple_end(extra_end) {
                break;
            }
            args.push(self.parse_expression(with_condexpr)?);
            if self.is_op(",") {
                is_tuple = true;
            } else {
                break;
            }
        }
        if !is_tuple {
            if let Some(e) = args.pop() {
                return Ok(e);
            }
            return err("expected an expression");
        }
        Ok(Expr::Tuple(args))
    }

    fn parse_expression(&mut self, with_condexpr: bool) -> Result<Expr> {
        if with_condexpr {
            self.parse_condexpr()
        } else {
            self.parse_or()
        }
    }

    fn parse_condexpr(&mut self) -> Result<Expr> {
        let mut expr1 = self.parse_or()?;
        while self.skip_name("if") {
            let expr2 = self.parse_or()?;
            let expr3 = if self.skip_name("else") { Some(Box::new(self.parse_condexpr()?)) } else { None };
            expr1 = Expr::Cond(Box::new(expr2), Box::new(expr1), expr3);
        }
        Ok(expr1)
    }

    fn parse_or(&mut self) -> Result<Expr> {
        let mut left = self.parse_and()?;
        while self.skip_name("or") {
            let right = self.parse_and()?;
            left = Expr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr> {
        let mut left = self.parse_not()?;
        while self.skip_name("and") {
            let right = self.parse_not()?;
            left = Expr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_not(&mut self) -> Result<Expr> {
        if self.skip_name("not") {
            return Ok(Expr::Not(Box::new(self.parse_not()?)));
        }
        self.parse_compare()
    }

    fn parse_compare(&mut self) -> Result<Expr> {
        let expr = self.parse_math1()?;
        let mut ops = vec![];
        loop {
            let op = match self.peek() {
                Tok::Op("==") => CmpOp::Eq,
                Tok::Op("!=") => CmpOp::Ne,
                Tok::Op("<") => CmpOp::Lt,
                Tok::Op("<=") => CmpOp::Le,
                Tok::Op(">") => CmpOp::Gt,
                Tok::Op(">=") => CmpOp::Ge,
                Tok::Name(n) if n == "in" => CmpOp::In,
                Tok::Name(n) if n == "not" && matches!(self.peek_at(1), Tok::Name(m) if m == "in") => {
                    self.next();
                    CmpOp::NotIn
                }
                _ => break,
            };
            self.next();
            ops.push((op, self.parse_math1()?));
        }
        if ops.is_empty() {
            Ok(expr)
        } else {
            Ok(Expr::Compare(Box::new(expr), ops))
        }
    }

    fn parse_math1(&mut self) -> Result<Expr> {
        let mut left = self.parse_concat()?;
        loop {
            let op = if self.is_op("+") {
                BinOp::Add
            } else if self.is_op("-") {
                BinOp::Sub
            } else {
                break;
            };
            self.next();
            let right = self.parse_concat()?;
            left = Expr::Bin(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_concat(&mut self) -> Result<Expr> {
        let mut args = vec![self.parse_math2()?];
        while self.skip_op("~") {
            args.push(self.parse_math2()?);
        }
        if args.len() == 1 {
            Ok(args.pop().unwrap())
        } else {
            Ok(Expr::Concat(args))
        }
    }

    fn parse_math2(&mut self) -> Result<Expr> {
        let mut left = self.parse_pow()?;
        loop {
            let op = match self.peek() {
                Tok::Op("*") => BinOp::Mul,
                Tok::Op("/") => BinOp::Div,
                Tok::Op("//") => BinOp::FloorDiv,
                Tok::Op("%") => BinOp::Mod,
                _ => break,
            };
            self.next();
            let right = self.parse_pow()?;
            left = Expr::Bin(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_pow(&mut self) -> Result<Expr> {
        let mut left = self.parse_unary(true)?;
        while self.skip_op("**") {
            let right = self.parse_unary(true)?;
            left = Expr::Bin(BinOp::Pow, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_unary(&mut self, with_filter: bool) -> Result<Expr> {
        let node = if self.skip_op("-") {
            Expr::Neg(Box::new(self.parse_unary(false)?))
        } else if self.skip_op("+") {
            Expr::Pos(Box::new(self.parse_unary(false)?))
        } else {
            self.parse_primary()?
        };
        let node = self.parse_postfix(node)?;
        if with_filter {
            self.parse_filter_expr(node)
        } else {
            Ok(node)
        }
    }

    fn parse_primary(&mut self) -> Result<Expr> {
        match self.next() {
            Tok::Name(n) => Ok(match n.as_str() {
                "true" | "True" => Expr::Const(Const::Bool(true)),
                "false" | "False" => Expr::Const(Const::Bool(false)),
                "none" | "None" => Expr::Const(Const::None),
                _ => Expr::Name(n),
            }),
            Tok::Str(s) => {
                let mut s = s;
                while let Tok::Str(more) = self.peek().clone() {
                    self.next();
                    s.push_str(&more);
                }
                Ok(Expr::Const(Const::Str(s)))
            }
            Tok::Int(i) => Ok(Expr::Const(Const::Int(i))),
            Tok::Float(f) => Ok(Expr::Const(Const::Float(f))),
            Tok::Op("(") => {
                if self.skip_op(")") {
                    return Ok(Expr::Tuple(vec![]));
                }
                let mut args = vec![];
                let mut is_tuple = false;
                loop {
                    if !args.is_empty() {
                        self.expect_op(",")?;
                    }
                    if self.is_op(")") {
                        break;
                    }
                    args.push(self.parse_expression(true)?);
                    if self.is_op(",") {
                        is_tuple = true;
                    } else {
                        break;
                    }
                }
                self.expect_op(")")?;
                if is_tuple {
                    Ok(Expr::Tuple(args))
                } else {
                    Ok(args.pop().unwrap())
                }
            }
            Tok::Op("[") => {
                let mut items = vec![];
                while !self.is_op("]") {
                    if !items.is_empty() {
                        self.expect_op(",")?;
                        if self.is_op("]") {
                            break;
                        }
                    }
                    items.push(self.parse_expression(true)?);
                }
                self.expect_op("]")?;
                Ok(Expr::List(items))
            }
            Tok::Op("{") => {
                let mut items = vec![];
                while !self.is_op("}") {
                    if !items.is_empty() {
                        self.expect_op(",")?;
                        if self.is_op("}") {
                            break;
                        }
                    }
                    let k = self.parse_expression(true)?;
                    self.expect_op(":")?;
                    let v = self.parse_expression(true)?;
                    items.push((k, v));
                }
                self.expect_op("}")?;
                Ok(Expr::Dict(items))
            }
            other => err(format!("unexpected {other:?}")),
        }
    }

    fn parse_postfix(&mut self, mut node: Expr) -> Result<Expr> {
        loop {
            if self.is_op(".") || self.is_op("[") {
                node = self.parse_subscript(node)?;
            } else if self.is_op("(") {
                node = self.parse_call(node)?;
            } else {
                return Ok(node);
            }
        }
    }

    fn parse_filter_expr(&mut self, mut node: Expr) -> Result<Expr> {
        loop {
            if self.is_op("|") {
                node = self.parse_filter(node)?;
            } else if self.is_name("is") {
                node = self.parse_test(node)?;
            } else if self.is_op("(") {
                node = self.parse_call(node)?;
            } else {
                return Ok(node);
            }
        }
    }

    fn parse_subscript(&mut self, node: Expr) -> Result<Expr> {
        if self.skip_op(".") {
            return match self.next() {
                Tok::Name(n) => Ok(Expr::Attr(Box::new(node), n)),
                Tok::Int(i) => Ok(Expr::Item(Box::new(node), Box::new(Expr::Const(Const::Int(i))))),
                other => err(format!("expected a name after '.', got {other:?}")),
            };
        }
        self.expect_op("[")?;
        let mut parts: Vec<Option<Box<Expr>>> = vec![];
        let mut current: Option<Box<Expr>> = None;
        let mut slice = false;
        loop {
            if self.is_op("]") {
                parts.push(current.take());
                break;
            }
            if self.skip_op(":") {
                slice = true;
                parts.push(current.take());
                continue;
            }
            current = Some(Box::new(self.parse_expression(true)?));
        }
        self.expect_op("]")?;
        if !slice {
            let idx = parts.pop().flatten().ok_or_else(|| Error("empty subscript".into()))?;
            return Ok(Expr::Item(Box::new(node), idx));
        }
        let mut it = parts.into_iter();
        let (a, b, c) = (it.next().flatten(), it.next().flatten(), it.next().flatten());
        Ok(Expr::Slice(Box::new(node), a, b, c))
    }

    fn parse_args(&mut self) -> Result<(Vec<Expr>, Vec<(String, Expr)>)> {
        self.expect_op("(")?;
        let mut args = vec![];
        let mut kwargs = vec![];
        while !self.is_op(")") {
            if !args.is_empty() || !kwargs.is_empty() {
                self.expect_op(",")?;
                if self.is_op(")") {
                    break;
                }
            }
            if let (Tok::Name(n), Tok::Op("=")) = (self.peek().clone(), self.peek_at(1).clone()) {
                self.next();
                self.next();
                kwargs.push((n, self.parse_expression(true)?));
            } else {
                args.push(self.parse_expression(true)?);
            }
        }
        self.expect_op(")")?;
        Ok((args, kwargs))
    }

    fn parse_call(&mut self, node: Expr) -> Result<Expr> {
        let (args, kwargs) = self.parse_args()?;
        Ok(Expr::Call(Box::new(node), args, kwargs))
    }

    fn parse_filter(&mut self, mut node: Expr) -> Result<Expr> {
        while self.skip_op("|") {
            let mut name = self.expect_name()?;
            while self.skip_op(".") {
                name = format!("{name}.{}", self.expect_name()?);
            }
            let (args, kwargs) = if self.is_op("(") { self.parse_args()? } else { (vec![], vec![]) };
            node = Expr::Filter(Box::new(node), name, args, kwargs);
        }
        Ok(node)
    }

    fn parse_test(&mut self, node: Expr) -> Result<Expr> {
        self.next(); // "is"
        let negated = self.skip_name("not");
        let mut name = self.expect_name()?;
        while self.skip_op(".") {
            name = format!("{name}.{}", self.expect_name()?);
        }
        let args = if self.is_op("(") {
            self.parse_args()?.0
        } else {
            let starts_arg = match self.peek() {
                Tok::Name(n) => !matches!(n.as_str(), "else" | "or" | "and" | "is"),
                Tok::Str(_) | Tok::Int(_) | Tok::Float(_) | Tok::Op("(") | Tok::Op("[") | Tok::Op("{") => true,
                _ => false,
            };
            if starts_arg {
                let a = self.parse_primary()?;
                vec![self.parse_postfix(a)?]
            } else {
                vec![]
            }
        };
        Ok(Expr::Test(Box::new(node), name, args, negated))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitespace_control() {
        let t = lex("a  {%- if x -%}\n  b {%+ endif %}  c\n").unwrap();
        assert_eq!(t[0], Tok::Data("a".into()));
        assert!(t.contains(&Tok::Data("b ".into())));
        assert_eq!(t[t.len() - 2], Tok::Data("  c".into()));
    }

    #[test]
    fn precedence() {
        let t = parse("t", "{{ items|length - limit }}{{ -x|abs }}{{ a if b else c }}").unwrap();
        match &t.body[0] {
            Node::Output(Expr::Bin(BinOp::Sub, l, _)) => assert!(matches!(**l, Expr::Filter(..))),
            other => panic!("{other:?}"),
        }
    }
}
