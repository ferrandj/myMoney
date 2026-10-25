//! The values a template handles, with Python's semantics (the templates were written for Jinja2 on Python): truth,
//! equality and ordering, `str()`, arithmetic on ints, floats and decimals, markup that is never escaped twice.

use std::cmp::Ordering;
use std::fmt::Write as _;
use std::rc::Rc;

use chrono::{Datelike, NaiveDate, NaiveDateTime, Timelike};
use indexmap::IndexMap;
use rust_decimal::Decimal;

use crate::py::{self, PyDec};

#[derive(Debug, Clone)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn err<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error(msg.into()))
}

pub type Args = Vec<Value>;
pub type Kwargs = Vec<(String, Value)>;
pub type NativeFn = dyn Fn(Args, Kwargs) -> Result<Value>;

/// A Python object: named attributes (a dataclass, an ORM row, a request...).
pub struct Obj {
    pub class: &'static str,
    pub attrs: IndexMap<String, Value>,
}

/// A key of a dict, compared the way Python hashes (1, 1.0, True and Decimal(1) are one key).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    None,
    Int(i64),
    Num(String),
    Str(String),
    Date(NaiveDate),
    DateTime(NaiveDateTime),
    Tuple(Vec<Key>),
    Other(usize),
}

pub struct Dict {
    pub items: IndexMap<Key, (Value, Value)>,
}

pub struct MacroRef {
    pub def: std::sync::Arc<super::parser::MacroDef>,
    pub closure: Rc<super::eval::Closure>,
}

#[derive(Clone)]
pub enum Value {
    Undefined(Rc<str>),
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    Dec(Rc<PyDec>),
    Str(Rc<str>),
    Markup(Rc<str>),
    List(Rc<Vec<Value>>),
    Tuple(Rc<Vec<Value>>),
    Dict(Rc<Dict>),
    Date(NaiveDate),
    DateTime(NaiveDateTime),
    Obj(Rc<Obj>),
    Func(Rc<NativeFn>),
    Macro(Rc<MacroRef>),
}

impl std::fmt::Debug for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.repr())
    }
}

// ------------------------------------------------------------------------------------------ building

impl Value {
    pub fn str(s: impl AsRef<str>) -> Value {
        Value::Str(Rc::from(s.as_ref()))
    }
    pub fn markup(s: impl AsRef<str>) -> Value {
        Value::Markup(Rc::from(s.as_ref()))
    }
    pub fn undefined(hint: impl AsRef<str>) -> Value {
        Value::Undefined(Rc::from(hint.as_ref()))
    }
    pub fn list(items: Vec<Value>) -> Value {
        Value::List(Rc::new(items))
    }
    pub fn tuple(items: Vec<Value>) -> Value {
        Value::Tuple(Rc::new(items))
    }
    pub fn dec(d: &Decimal) -> Value {
        Value::Dec(Rc::new(PyDec::from(d)))
    }
    pub fn pydec(d: PyDec) -> Value {
        Value::Dec(Rc::new(d))
    }
    pub fn func(f: impl Fn(Args, Kwargs) -> Result<Value> + 'static) -> Value {
        Value::Func(Rc::new(f))
    }
    pub fn obj(class: &'static str, attrs: Vec<(&str, Value)>) -> Value {
        Value::Obj(Rc::new(Obj { class, attrs: attrs.into_iter().map(|(k, v)| (k.to_string(), v)).collect() }))
    }
    pub fn dict(pairs: Vec<(Value, Value)>) -> Value {
        let mut items = IndexMap::new();
        for (k, v) in pairs {
            items.insert(k.key(), (k, v));
        }
        Value::Dict(Rc::new(Dict { items }))
    }
    /// A dict with string keys.
    pub fn sdict(pairs: Vec<(&str, Value)>) -> Value {
        Value::dict(pairs.into_iter().map(|(k, v)| (Value::str(k), v)).collect())
    }

    pub fn opt<T>(v: Option<T>, f: impl FnOnce(T) -> Value) -> Value {
        match v {
            Some(x) => f(x),
            None => Value::None,
        }
    }

    pub fn key(&self) -> Key {
        match self {
            Value::None => Key::None,
            Value::Bool(b) => Key::Int(*b as i64),
            Value::Int(i) => Key::Int(*i),
            Value::Float(f) if f.fract() == 0.0 && f.abs() < 9e15 => Key::Int(*f as i64),
            Value::Float(f) => Key::Num(PyDec::from_f64(*f).norm_key()),
            Value::Dec(d) => match i64::try_from(d.trunc()) {
                Ok(i) if d.cmp(&PyDec::int(i)) == Ordering::Equal => Key::Int(i),
                _ => Key::Num(d.norm_key()),
            },
            Value::Str(s) | Value::Markup(s) => Key::Str(s.to_string()),
            Value::Date(d) => Key::Date(*d),
            Value::DateTime(d) => Key::DateTime(*d),
            Value::Tuple(t) => Key::Tuple(t.iter().map(|v| v.key()).collect()),
            Value::Obj(o) => Key::Other(Rc::as_ptr(o) as *const u8 as usize),
            _ => Key::Other(0),
        }
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Value {
        Value::str(s)
    }
}
impl From<String> for Value {
    fn from(s: String) -> Value {
        Value::str(s)
    }
}
impl From<i64> for Value {
    fn from(i: i64) -> Value {
        Value::Int(i)
    }
}
impl From<usize> for Value {
    fn from(i: usize) -> Value {
        Value::Int(i as i64)
    }
}
impl From<bool> for Value {
    fn from(b: bool) -> Value {
        Value::Bool(b)
    }
}
impl From<f64> for Value {
    fn from(f: f64) -> Value {
        Value::Float(f)
    }
}
impl From<Decimal> for Value {
    fn from(d: Decimal) -> Value {
        Value::dec(&d)
    }
}
impl From<PyDec> for Value {
    fn from(d: PyDec) -> Value {
        Value::pydec(d)
    }
}
impl From<NaiveDate> for Value {
    fn from(d: NaiveDate) -> Value {
        Value::Date(d)
    }
}
impl From<NaiveDateTime> for Value {
    fn from(d: NaiveDateTime) -> Value {
        Value::DateTime(d)
    }
}
impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Value {
        match v {
            Some(x) => x.into(),
            None => Value::None,
        }
    }
}
impl<T: Into<Value>> From<Vec<T>> for Value {
    fn from(v: Vec<T>) -> Value {
        Value::list(v.into_iter().map(Into::into).collect())
    }
}

impl From<serde_json::Value> for Value {
    fn from(v: serde_json::Value) -> Value {
        Value::from(&v)
    }
}

/// JSON as `json.loads` gives it.
impl From<&serde_json::Value> for Value {
    fn from(v: &serde_json::Value) -> Value {
        match v {
            serde_json::Value::Null => Value::None,
            serde_json::Value::Bool(b) => Value::Bool(*b),
            serde_json::Value::Number(n) => match n.as_i64() {
                Some(i) => Value::Int(i),
                None => Value::Float(n.as_f64().unwrap_or(0.0)),
            },
            serde_json::Value::String(s) => Value::str(s),
            serde_json::Value::Array(a) => Value::list(a.iter().map(Value::from).collect()),
            serde_json::Value::Object(m) => Value::dict(m.iter().map(|(k, v)| (Value::str(k), Value::from(v))).collect()),
        }
    }
}

// ----------------------------------------------------------------------------------------- reading

impl Value {
    pub fn is_undefined(&self) -> bool {
        matches!(self, Value::Undefined(_))
    }
    pub fn is_none(&self) -> bool {
        matches!(self, Value::None)
    }
    pub fn is_markup(&self) -> bool {
        matches!(self, Value::Markup(_))
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Undefined(_) => "Undefined",
            Value::None => "NoneType",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Dec(_) => "decimal.Decimal",
            Value::Str(_) => "str",
            Value::Markup(_) => "Markup",
            Value::List(_) => "list",
            Value::Tuple(_) => "tuple",
            Value::Dict(_) => "dict",
            Value::Date(_) => "datetime.date",
            Value::DateTime(_) => "datetime.datetime",
            Value::Obj(o) => o.class,
            Value::Func(_) => "function",
            Value::Macro(_) => "Macro",
        }
    }

    /// Python's truth value.
    pub fn truthy(&self) -> bool {
        match self {
            Value::Undefined(_) | Value::None => false,
            Value::Bool(b) => *b,
            Value::Int(i) => *i != 0,
            Value::Float(f) => *f != 0.0,
            Value::Dec(d) => !d.is_zero(),
            Value::Str(s) | Value::Markup(s) => !s.is_empty(),
            Value::List(l) | Value::Tuple(l) => !l.is_empty(),
            Value::Dict(d) => !d.items.is_empty(),
            _ => true,
        }
    }

    /// `str(value)` (Undefined prints nothing).
    pub fn to_str(&self) -> String {
        match self {
            Value::Undefined(_) => String::new(),
            Value::None => "None".into(),
            Value::Bool(b) => if *b { "True" } else { "False" }.into(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => py::float_repr(*f),
            Value::Dec(d) => d.py_str(),
            Value::Str(s) | Value::Markup(s) => s.to_string(),
            Value::Date(d) => d.format("%Y-%m-%d").to_string(),
            Value::DateTime(d) => datetime_str(d, ' '),
            _ => self.repr(),
        }
    }

    /// `repr(value)`.
    pub fn repr(&self) -> String {
        match self {
            Value::Undefined(_) => String::new(),
            Value::Str(s) => py::repr_str(s),
            Value::Markup(s) => format!("Markup({})", py::repr_str(s)),
            Value::Dec(d) => format!("Decimal('{}')", d.py_str()),
            Value::Date(d) => format!("datetime.date({}, {}, {})", d.year(), d.month(), d.day()),
            Value::DateTime(d) => {
                let mut s = format!("datetime.datetime({}, {}, {}, {}, {}", d.year(), d.month(), d.day(), d.hour(), d.minute());
                let us = d.and_utc().timestamp_subsec_micros();
                if d.second() != 0 || us != 0 {
                    let _ = write!(s, ", {}", d.second());
                }
                if us != 0 {
                    let _ = write!(s, ", {us}");
                }
                s + ")"
            }
            Value::List(l) => format!("[{}]", l.iter().map(|v| v.repr()).collect::<Vec<_>>().join(", ")),
            Value::Tuple(l) if l.len() == 1 => format!("({},)", l[0].repr()),
            Value::Tuple(l) => format!("({})", l.iter().map(|v| v.repr()).collect::<Vec<_>>().join(", ")),
            Value::Dict(d) => {
                format!("{{{}}}", d.items.values().map(|(k, v)| format!("{}: {}", k.repr(), v.repr())).collect::<Vec<_>>().join(", "))
            }
            Value::Obj(o) => format!("<{} object>", o.class),
            Value::Func(_) => "<function>".into(),
            Value::Macro(m) => format!("<Macro '{}'>", m.def.name),
            other => other.to_str(),
        }
    }

    /// The text of a value in an autoescaped page.
    pub fn to_html(&self) -> String {
        match self {
            Value::Markup(s) => s.to_string(),
            other => py::html_escape(&other.to_str()),
        }
    }

    /// `soft_str`: markup stays markup, anything else becomes text.
    pub fn soft_str(&self) -> Value {
        match self {
            Value::Markup(_) | Value::Str(_) => self.clone(),
            other => Value::str(other.to_str()),
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) | Value::Markup(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            Value::Bool(b) => Some(*b as i64),
            _ => None,
        }
    }

    /// Iterate like Python's `iter()`: a list, the keys of a dict, the characters of a string; nothing for Undefined.
    pub fn iter(&self) -> Result<Vec<Value>> {
        match self {
            Value::Undefined(_) => Ok(vec![]),
            Value::List(l) | Value::Tuple(l) => Ok(l.as_ref().clone()),
            Value::Dict(d) => Ok(d.items.values().map(|(k, _)| k.clone()).collect()),
            Value::Str(s) => Ok(s.chars().map(|c| Value::str(c.to_string())).collect()),
            Value::Markup(s) => Ok(s.chars().map(|c| Value::markup(c.to_string())).collect()),
            other => err(format!("'{}' object is not iterable", other.type_name())),
        }
    }

    pub fn len(&self) -> Result<usize> {
        match self {
            Value::Undefined(_) => Ok(0),
            Value::List(l) | Value::Tuple(l) => Ok(l.len()),
            Value::Dict(d) => Ok(d.items.len()),
            Value::Str(s) | Value::Markup(s) => Ok(s.chars().count()),
            other => err(format!("object of type '{}' has no len()", other.type_name())),
        }
    }

    pub fn dict_get(&self, key: &Value) -> Option<Value> {
        match self {
            Value::Dict(d) => d.items.get(&key.key()).map(|(_, v)| v.clone()),
            _ => None,
        }
    }

    pub fn attr(&self, name: &str) -> Option<Value> {
        match self {
            Value::Obj(o) => o.attrs.get(name).cloned(),
            _ => None,
        }
    }
}

pub fn datetime_str(d: &NaiveDateTime, sep: char) -> String {
    let us = d.and_utc().timestamp_subsec_micros();
    if us != 0 {
        format!("{}{sep}{}.{:06}", d.format("%Y-%m-%d"), d.format("%H:%M:%S"), us)
    } else {
        format!("{}{sep}{}", d.format("%Y-%m-%d"), d.format("%H:%M:%S"))
    }
}

// --------------------------------------------------------------------------------------- comparing

enum Num {
    Int(i64),
    Float(f64),
    Dec(PyDec),
}

fn num(v: &Value) -> Option<Num> {
    match v {
        Value::Bool(b) => Some(Num::Int(*b as i64)),
        Value::Int(i) => Some(Num::Int(*i)),
        Value::Float(f) => Some(Num::Float(*f)),
        Value::Dec(d) => Some(Num::Dec(d.as_ref().clone())),
        _ => None,
    }
}

fn num_cmp(a: &Num, b: &Num) -> Option<Ordering> {
    match (a, b) {
        (Num::Int(x), Num::Int(y)) => Some(x.cmp(y)),
        (Num::Float(x), Num::Float(y)) => x.partial_cmp(y),
        (Num::Int(x), Num::Float(y)) => exact_cmp(&PyDec::int(*x), *y),
        (Num::Float(x), Num::Int(y)) => exact_cmp(&PyDec::int(*y), *x).map(|o| o.reverse()),
        (Num::Dec(x), Num::Dec(y)) => Some(x.cmp(y)),
        (Num::Dec(x), Num::Int(y)) => Some(x.cmp(&PyDec::int(*y))),
        (Num::Int(x), Num::Dec(y)) => Some(PyDec::int(*x).cmp(y)),
        (Num::Dec(x), Num::Float(y)) => exact_cmp(x, *y),
        (Num::Float(x), Num::Dec(y)) => exact_cmp(y, *x).map(|o| o.reverse()),
    }
}

fn exact_cmp(d: &PyDec, f: f64) -> Option<Ordering> {
    if f.is_nan() {
        return None;
    }
    if f.is_infinite() {
        return Some(if f > 0.0 { Ordering::Less } else { Ordering::Greater });
    }
    Some(d.cmp(&PyDec::from_f64(f)))
}

/// `a == b`.
pub fn equals(a: &Value, b: &Value) -> bool {
    if let (Some(x), Some(y)) = (num(a), num(b)) {
        return num_cmp(&x, &y) == Some(Ordering::Equal);
    }
    match (a, b) {
        (Value::Str(x) | Value::Markup(x), Value::Str(y) | Value::Markup(y)) => x == y,
        (Value::None, Value::None) => true,
        (Value::Undefined(_), Value::Undefined(_)) => true,
        (Value::Date(x), Value::Date(y)) => x == y,
        (Value::DateTime(x), Value::DateTime(y)) => x == y,
        (Value::List(x), Value::List(y)) | (Value::Tuple(x), Value::Tuple(y)) => x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| equals(p, q)),
        (Value::Dict(x), Value::Dict(y)) => {
            x.items.len() == y.items.len() && x.items.iter().all(|(k, (_, v))| y.items.get(k).map(|(_, w)| equals(v, w)).unwrap_or(false))
        }
        (Value::Obj(x), Value::Obj(y)) => Rc::ptr_eq(x, y),
        (Value::Func(x), Value::Func(y)) => Rc::ptr_eq(x, y),
        _ => false,
    }
}

fn undefined_error(v: &Value) -> Error {
    match v {
        Value::Undefined(h) => Error(format!("{h} is undefined")),
        _ => Error("undefined".into()),
    }
}

/// `a < b` and friends: numbers, strings, dates and sequences; anything else is Python's TypeError.
pub fn compare(a: &Value, b: &Value, op: &str) -> Result<Ordering> {
    if a.is_undefined() {
        return Err(undefined_error(a));
    }
    if b.is_undefined() {
        return Err(undefined_error(b));
    }
    if let (Some(x), Some(y)) = (num(a), num(b)) {
        return num_cmp(&x, &y).ok_or_else(|| Error("comparison with NaN".into()));
    }
    match (a, b) {
        (Value::Str(x) | Value::Markup(x), Value::Str(y) | Value::Markup(y)) => Ok(x.as_ref().cmp(y.as_ref())),
        (Value::Date(x), Value::Date(y)) => Ok(x.cmp(y)),
        (Value::DateTime(x), Value::DateTime(y)) => Ok(x.cmp(y)),
        (Value::List(x), Value::List(y)) | (Value::Tuple(x), Value::Tuple(y)) => {
            for (p, q) in x.iter().zip(y.iter()) {
                if !equals(p, q) {
                    return compare(p, q, op);
                }
            }
            Ok(x.len().cmp(&y.len()))
        }
        _ => err(format!("'{op}' not supported between instances of '{}' and '{}'", a.type_name(), b.type_name())),
    }
}

/// `a in b`.
pub fn contains(container: &Value, item: &Value) -> Result<bool> {
    match container {
        Value::Undefined(_) => Ok(false),
        Value::List(l) | Value::Tuple(l) => Ok(l.iter().any(|v| equals(v, item))),
        Value::Dict(d) => Ok(d.items.contains_key(&item.key())),
        Value::Str(s) | Value::Markup(s) => match item {
            Value::Str(x) | Value::Markup(x) => Ok(s.contains(x.as_ref())),
            other => err(format!("'in <string>' requires string as left operand, not {}", other.type_name())),
        },
        other => err(format!("argument of type '{}' is not iterable", other.type_name())),
    }
}

// --------------------------------------------------------------------------------------- arithmetic

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    FloorDiv,
    Mod,
    Pow,
}

impl BinOp {
    fn symbol(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::FloorDiv => "//",
            BinOp::Mod => "%",
            BinOp::Pow => "**",
        }
    }
}

fn py_floordiv(a: i64, b: i64) -> i64 {
    let q = a / b;
    if (a % b != 0) && ((a < 0) != (b < 0)) {
        q - 1
    } else {
        q
    }
}

fn py_mod(a: i64, b: i64) -> i64 {
    let r = a % b;
    if r != 0 && ((r < 0) != (b < 0)) {
        r + b
    } else {
        r
    }
}

fn float_op(op: BinOp, x: f64, y: f64) -> Result<Value> {
    Ok(Value::Float(match op {
        BinOp::Add => x + y,
        BinOp::Sub => x - y,
        BinOp::Mul => x * y,
        BinOp::Div => {
            if y == 0.0 {
                return err("float division by zero");
            }
            x / y
        }
        BinOp::FloorDiv => {
            if y == 0.0 {
                return err("float floor division by zero");
            }
            (x / y).floor()
        }
        BinOp::Mod => {
            if y == 0.0 {
                return err("float modulo");
            }
            let r = x % y;
            if r != 0.0 && ((r < 0.0) != (y < 0.0)) {
                r + y
            } else {
                r
            }
        }
        BinOp::Pow => x.powf(y),
    }))
}

fn dec_op(op: BinOp, x: &PyDec, y: &PyDec) -> Result<Value> {
    Ok(Value::pydec(match op {
        BinOp::Add => x.add(y),
        BinOp::Sub => x.sub(y),
        BinOp::Mul => x.mul(y),
        BinOp::Div => match x.div(y) {
            Some(q) => q,
            None => return err(if x.is_zero() { "[<class 'decimal.DivisionUndefined'>]" } else { "[<class 'decimal.DivisionByZero'>]" }),
        },
        _ => return err(format!("unsupported decimal operation {}", op.symbol())),
    }))
}

pub fn binary(op: BinOp, a: &Value, b: &Value) -> Result<Value> {
    if a.is_undefined() {
        return Err(undefined_error(a));
    }
    if b.is_undefined() {
        return Err(undefined_error(b));
    }
    if let (Some(x), Some(y)) = (num(a), num(b)) {
        return match (x, y) {
            (Num::Int(p), Num::Int(q)) => match op {
                BinOp::Add => Ok(Value::Int(p + q)),
                BinOp::Sub => Ok(Value::Int(p - q)),
                BinOp::Mul => Ok(Value::Int(p * q)),
                BinOp::Div => {
                    if q == 0 {
                        return err("division by zero");
                    }
                    Ok(Value::Float(p as f64 / q as f64))
                }
                BinOp::FloorDiv => {
                    if q == 0 {
                        return err("integer division or modulo by zero");
                    }
                    Ok(Value::Int(py_floordiv(p, q)))
                }
                BinOp::Mod => {
                    if q == 0 {
                        return err("integer modulo by zero");
                    }
                    Ok(Value::Int(py_mod(p, q)))
                }
                BinOp::Pow => {
                    if q >= 0 {
                        Ok(Value::Int(p.pow(q as u32)))
                    } else {
                        Ok(Value::Float((p as f64).powf(q as f64)))
                    }
                }
            },
            (Num::Float(p), Num::Float(q)) => float_op(op, p, q),
            (Num::Int(p), Num::Float(q)) => float_op(op, p as f64, q),
            (Num::Float(p), Num::Int(q)) => float_op(op, p, q as f64),
            (Num::Dec(p), Num::Dec(q)) => dec_op(op, &p, &q),
            (Num::Dec(p), Num::Int(q)) => dec_op(op, &p, &PyDec::int(q)),
            (Num::Int(p), Num::Dec(q)) => dec_op(op, &PyDec::int(p), &q),
            _ => err(format!(
                "unsupported operand type(s) for {}: '{}' and '{}'",
                op.symbol(),
                a.type_name(),
                b.type_name()
            )),
        };
    }
    match (op, a, b) {
        (BinOp::Add, Value::Markup(x), other @ (Value::Str(_) | Value::Markup(_))) => Ok(Value::markup(format!("{x}{}", other.to_html()))),
        (BinOp::Add, Value::Str(x), Value::Markup(y)) => Ok(Value::markup(format!("{}{y}", py::html_escape(x)))),
        (BinOp::Add, Value::Str(x), Value::Str(y)) => Ok(Value::str(format!("{x}{y}"))),
        (BinOp::Add, Value::List(x), Value::List(y)) => Ok(Value::list(x.iter().chain(y.iter()).cloned().collect())),
        (BinOp::Add, Value::Tuple(x), Value::Tuple(y)) => Ok(Value::tuple(x.iter().chain(y.iter()).cloned().collect())),
        (BinOp::Mul, Value::Str(x), Value::Int(n)) | (BinOp::Mul, Value::Int(n), Value::Str(x)) => Ok(Value::str(x.repeat((*n).max(0) as usize))),
        (BinOp::Mod, Value::Str(_) | Value::Markup(_), _) => super::filters::percent_format(a, b),
        _ => err(format!("unsupported operand type(s) for {}: '{}' and '{}'", op.symbol(), a.type_name(), b.type_name())),
    }
}

pub fn negate(v: &Value) -> Result<Value> {
    match v {
        Value::Int(i) => Ok(Value::Int(-i)),
        Value::Bool(b) => Ok(Value::Int(-(*b as i64))),
        Value::Float(f) => Ok(Value::Float(-f)),
        Value::Dec(d) => Ok(Value::pydec(d.neg())),
        Value::Undefined(_) => Err(undefined_error(v)),
        other => err(format!("bad operand type for unary -: '{}'", other.type_name())),
    }
}

/// `float(value)`.
pub fn to_float(v: &Value) -> Result<f64> {
    match v {
        Value::Int(i) => Ok(*i as f64),
        Value::Bool(b) => Ok(*b as i64 as f64),
        Value::Float(f) => Ok(*f),
        Value::Dec(d) => Ok(d.to_f64()),
        Value::Str(s) | Value::Markup(s) => {
            let t = py::strip(s).replace('_', "");
            match t.to_lowercase().as_str() {
                "inf" | "+inf" | "infinity" | "+infinity" => Ok(f64::INFINITY),
                "-inf" | "-infinity" => Ok(f64::NEG_INFINITY),
                "nan" | "+nan" | "-nan" => Ok(f64::NAN),
                _ => t.parse::<f64>().map_err(|_| Error(format!("could not convert string to float: {}", py::repr_str(s)))),
            }
        }
        other => err(format!("float() argument must be a string or a real number, not '{}'", other.type_name())),
    }
}

/// `Decimal(value)`.
pub fn to_decimal(v: &Value) -> Result<PyDec> {
    match v {
        Value::Int(i) => Ok(PyDec::int(*i)),
        Value::Bool(b) => Ok(PyDec::int(*b as i64)),
        Value::Float(f) => Ok(PyDec::from_f64(*f)),
        Value::Dec(d) => Ok(d.as_ref().clone()),
        Value::Str(s) | Value::Markup(s) => PyDec::parse(s).ok_or_else(|| Error("[<class 'decimal.ConversionSyntax'>]".into())),
        other => err(format!("conversion from {} to Decimal is not supported", other.type_name())),
    }
}
