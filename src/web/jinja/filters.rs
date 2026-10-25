//! Jinja2's built-in filters and tests that the templates use, as Jinja2 3.1 writes them.

use super::eval::{getitem, Environment};
use super::value::*;
use crate::py::{self, PyDec};

fn kwarg<'a>(kwargs: &'a [(String, Value)], name: &str) -> Option<&'a Value> {
    kwargs.iter().find(|(k, _)| k == name).map(|(_, v)| v)
}

fn arg<'a>(args: &'a [Value], kwargs: &'a [(String, Value)], i: usize, name: &str) -> Option<&'a Value> {
    args.get(i).or_else(|| kwarg(kwargs, name))
}

/// `make_attrgetter`: "a.b.0" looked up part by part (digits are indexes).
pub fn attr_path(item: &Value, path: &str) -> Result<Value> {
    let mut v = item.clone();
    for part in path.split('.') {
        let key = match part.parse::<i64>() {
            Ok(i) => Value::Int(i),
            Err(_) => Value::str(part),
        };
        v = getitem(&v, &key)?;
    }
    Ok(v)
}

fn escape_value(v: &Value) -> Value {
    match v {
        Value::Markup(_) => v.clone(),
        other => Value::markup(py::html_escape(&other.to_str())),
    }
}

/// `str % args` for the conversions the templates use (`%s`, `%d`, `%i`, `%f`, `%%`, flags, width, precision).
pub fn percent_format(fmt: &Value, args: &Value) -> Result<Value> {
    let f = fmt.as_str().unwrap_or_default().to_string();
    let values: Vec<Value> = match args {
        Value::Tuple(t) => t.as_ref().clone(),
        other => vec![other.clone()],
    };
    let markup = fmt.is_markup();
    let mut out = String::new();
    let chars: Vec<char> = f.chars().collect();
    let mut i = 0;
    let mut next = 0;
    while i < chars.len() {
        let c = chars[i];
        if c != '%' {
            out.push(c);
            i += 1;
            continue;
        }
        i += 1;
        let mut flags = String::new();
        while i < chars.len() && "-+ #0".contains(chars[i]) {
            flags.push(chars[i]);
            i += 1;
        }
        let mut width = String::new();
        while i < chars.len() && chars[i].is_ascii_digit() {
            width.push(chars[i]);
            i += 1;
        }
        let mut prec: Option<usize> = None;
        if i < chars.len() && chars[i] == '.' {
            i += 1;
            let mut p = String::new();
            while i < chars.len() && chars[i].is_ascii_digit() {
                p.push(chars[i]);
                i += 1;
            }
            prec = Some(p.parse().unwrap_or(0));
        }
        let conv = *chars.get(i).ok_or_else(|| Error("incomplete format".into()))?;
        i += 1;
        if conv == '%' {
            out.push('%');
            continue;
        }
        let v = values.get(next).cloned().ok_or_else(|| Error("not enough arguments for format string".into()))?;
        next += 1;
        let body = match conv {
            's' => {
                let s = if markup { escape_value(&v).to_str() } else { v.to_str() };
                match prec {
                    Some(p) => s.chars().take(p).collect(),
                    None => s,
                }
            }
            'r' => v.repr(),
            'd' | 'i' => {
                let n = match &v {
                    Value::Int(n) => n.to_string(),
                    Value::Bool(b) => (*b as i64).to_string(),
                    Value::Float(x) => (x.trunc() as i64).to_string(),
                    Value::Dec(d) => d.trunc().to_string(),
                    other => return err(format!("%{conv} format: a real number is required, not {}", other.type_name())),
                };
                if flags.contains('+') && !n.starts_with('-') {
                    format!("+{n}")
                } else {
                    n
                }
            }
            'f' | 'F' => {
                let x = to_float(&v).map_err(|_| Error(format!("must be real number, not {}", v.type_name())))?;
                py::float_fixed(x, prec.unwrap_or(6), flags.contains('+'), false)
            }
            other => return err(format!("unsupported format character '{other}'")),
        };
        let w: usize = width.parse().unwrap_or(0);
        let len = body.chars().count();
        if len < w {
            let pad = w - len;
            if flags.contains('-') {
                out.push_str(&body);
                out.push_str(&" ".repeat(pad));
            } else if flags.contains('0') && conv != 's' {
                let (sign, digits) = if body.starts_with(['-', '+']) { body.split_at(1) } else { ("", body.as_str()) };
                out.push_str(sign);
                out.push_str(&"0".repeat(pad));
                out.push_str(digits);
            } else {
                out.push_str(&" ".repeat(pad));
                out.push_str(&body);
            }
        } else {
            out.push_str(&body);
        }
    }
    if next < values.len() && !matches!(args, Value::Dict(_)) {
        return err("not all arguments converted during string formatting");
    }
    Ok(if markup { Value::markup(out) } else { Value::str(out) })
}

fn do_format(v: &Value, args: &[Value], _: &[(String, Value)]) -> Result<Value> {
    let fmt = v.soft_str();
    let a = if args.len() == 1 { args[0].clone() } else { Value::tuple(args.to_vec()) };
    percent_format(&fmt, &a)
}

fn do_replace(v: &Value, args: &[Value], _: &[(String, Value)]) -> Result<Value> {
    let old = args.first().cloned().unwrap_or(Value::str(""));
    let new = args.get(1).cloned().unwrap_or(Value::str(""));
    let count = args.get(2).and_then(|c| c.as_int());
    let s = if old.is_markup() || (new.is_markup() && !v.is_markup()) { escape_value(v) } else { v.soft_str() };
    let replace = |text: &str, o: &str, n: &str| -> String {
        match count {
            Some(c) if c >= 0 => text.replacen(o, n, c as usize),
            _ => text.replace(o, n),
        }
    };
    Ok(match &s {
        Value::Markup(text) => Value::markup(replace(text, &old.to_str(), &escape_value(&new).to_str())),
        other => Value::str(replace(&other.to_str(), &old.to_str(), &new.to_str())),
    })
}

fn do_length(v: &Value, _: &[Value], _: &[(String, Value)]) -> Result<Value> {
    Ok(Value::Int(v.len()? as i64))
}

fn do_safe(v: &Value, _: &[Value], _: &[(String, Value)]) -> Result<Value> {
    Ok(match v {
        Value::Markup(_) => v.clone(),
        other => Value::markup(other.to_str()),
    })
}

fn do_escape(v: &Value, _: &[Value], _: &[(String, Value)]) -> Result<Value> {
    Ok(escape_value(v))
}

/// `int(value)`, else `int(float(value))`, else the default (0).
fn do_int(v: &Value, args: &[Value], kwargs: &[(String, Value)]) -> Result<Value> {
    let default = arg(args, kwargs, 0, "default").cloned().unwrap_or(Value::Int(0));
    Ok(match v {
        Value::Int(_) => v.clone(),
        Value::Bool(b) => Value::Int(*b as i64),
        Value::Float(f) if f.is_finite() => Value::Int(f.trunc() as i64),
        Value::Dec(d) => i64::try_from(d.trunc()).map(Value::Int).unwrap_or(default),
        Value::Str(s) | Value::Markup(s) => match py::py_int(s) {
            Ok(i) => Value::Int(i),
            Err(_) => match to_float(v) {
                Ok(f) if f.is_finite() => Value::Int(f.trunc() as i64),
                _ => default,
            },
        },
        _ => default,
    })
}

/// `round(value, precision)` (method "common"): half to even, on the exact value.
fn do_round(v: &Value, args: &[Value], kwargs: &[(String, Value)]) -> Result<Value> {
    let precision = arg(args, kwargs, 0, "precision").and_then(|p| p.as_int()).unwrap_or(0);
    let method = arg(args, kwargs, 1, "method").map(|m| m.to_str()).unwrap_or_else(|| "common".into());
    if method != "common" {
        let x = to_float(v)?;
        let m = 10f64.powi(precision as i32);
        let r = if method == "ceil" { (x * m).ceil() / m } else { (x * m).floor() / m };
        return Ok(Value::Float(r));
    }
    Ok(match v {
        Value::Int(i) => {
            if precision >= 0 {
                Value::Int(*i)
            } else {
                let r = PyDec::int(*i).rescale(-precision);
                Value::Int(i64::try_from(r.trunc()).unwrap_or(*i))
            }
        }
        Value::Bool(b) => Value::Int(*b as i64),
        Value::Float(f) => {
            if precision >= 0 {
                Value::Float(format!("{:.*}", precision as usize, f).parse().unwrap_or(*f))
            } else {
                let m = 10f64.powi((-precision) as i32);
                Value::Float((f / m).round_ties_even() * m)
            }
        }
        Value::Dec(d) => Value::pydec(d.rescale(-precision)),
        other => return err(format!("type {} doesn't define __round__ method", other.type_name())),
    })
}

fn do_join(v: &Value, args: &[Value], kwargs: &[(String, Value)]) -> Result<Value> {
    let sep = arg(args, kwargs, 0, "d").cloned().unwrap_or(Value::str(""));
    let mut items = v.iter()?;
    if let Some(a) = arg(args, kwargs, 1, "attribute") {
        let path = a.to_str();
        items = items.iter().map(|i| attr_path(i, &path)).collect::<Result<_>>()?;
    }
    if sep.is_markup() || items.iter().any(|i| i.is_markup()) {
        let parts: Vec<String> = items.iter().map(|i| escape_value(i).to_str()).collect();
        return Ok(Value::markup(parts.join(&escape_value(&sep).to_str())));
    }
    Ok(Value::str(items.iter().map(|i| i.to_str()).collect::<Vec<_>>().join(&sep.to_str())))
}

fn do_select(v: &Value, args: &[Value], _: &[(String, Value)]) -> Result<Value> {
    select_items(v, args, None, true)
}

fn do_selectattr(v: &Value, args: &[Value], _: &[(String, Value)]) -> Result<Value> {
    let path = args.first().map(|a| a.to_str()).ok_or_else(|| Error("Missing parameter for attribute name".into()))?;
    select_items(v, &args[1..], Some(&path), true)
}

fn do_reject(v: &Value, args: &[Value], _: &[(String, Value)]) -> Result<Value> {
    select_items(v, args, None, false)
}

fn select_items(v: &Value, args: &[Value], path: Option<&str>, keep: bool) -> Result<Value> {
    let mut out = vec![];
    for item in v.iter()? {
        let x = match path {
            Some(p) => attr_path(&item, p)?,
            None => item.clone(),
        };
        let ok = match args.first() {
            None => x.truthy(),
            Some(name) => test(&name.to_str(), &x, &args[1..])?,
        };
        if ok == keep {
            out.push(item);
        }
    }
    Ok(Value::list(out))
}

fn do_map(v: &Value, args: &[Value], kwargs: &[(String, Value)]) -> Result<Value> {
    let items = v.iter()?;
    if let Some(a) = kwarg(kwargs, "attribute") {
        let path = a.to_str();
        let default = kwarg(kwargs, "default").cloned();
        return Ok(Value::list(
            items
                .iter()
                .map(|i| {
                    let x = attr_path(i, &path)?;
                    Ok(match (&x, &default) {
                        (Value::Undefined(_), Some(d)) => d.clone(),
                        _ => x,
                    })
                })
                .collect::<Result<_>>()?,
        ));
    }
    let name = args.first().map(|a| a.to_str()).ok_or_else(|| Error("map requires a filter".into()))?;
    let _ = name;
    err("map with a filter name is not supported")
}

fn do_first(v: &Value, _: &[Value], _: &[(String, Value)]) -> Result<Value> {
    Ok(v.iter()?.into_iter().next().unwrap_or_else(|| Value::undefined("No first item, sequence was empty.")))
}

fn do_last(v: &Value, _: &[Value], _: &[(String, Value)]) -> Result<Value> {
    Ok(v.iter()?.into_iter().last().unwrap_or_else(|| Value::undefined("No last item, sequence was empty.")))
}

fn ignore_case(v: &Value) -> Value {
    match v {
        Value::Str(s) => Value::str(py::lower(s)),
        Value::Markup(s) => Value::markup(py::lower(s)),
        other => other.clone(),
    }
}

fn min_max(v: &Value, args: &[Value], kwargs: &[(String, Value)], max: bool) -> Result<Value> {
    let case_sensitive = arg(args, kwargs, 0, "case_sensitive").map(|c| c.truthy()).unwrap_or(false);
    let path = arg(args, kwargs, 1, "attribute").map(|a| a.to_str());
    let items = v.iter()?;
    let mut best: Option<(Value, Value)> = None;
    for item in items {
        let mut k = match &path {
            Some(p) => attr_path(&item, p)?,
            None => item.clone(),
        };
        if !case_sensitive {
            k = ignore_case(&k);
        }
        best = Some(match best {
            None => (k, item),
            Some((bk, bi)) => {
                let o = compare(&k, &bk, if max { ">" } else { "<" })?;
                if (max && o.is_gt()) || (!max && o.is_lt()) {
                    (k, item)
                } else {
                    (bk, bi)
                }
            }
        });
    }
    Ok(best.map(|(_, i)| i).unwrap_or_else(|| Value::undefined("No aggregated item, sequence was empty.")))
}

fn do_min(v: &Value, args: &[Value], kwargs: &[(String, Value)]) -> Result<Value> {
    min_max(v, args, kwargs, false)
}

fn do_max(v: &Value, args: &[Value], kwargs: &[(String, Value)]) -> Result<Value> {
    min_max(v, args, kwargs, true)
}

fn do_sum(v: &Value, args: &[Value], kwargs: &[(String, Value)]) -> Result<Value> {
    let path = kwarg(kwargs, "attribute").or_else(|| args.first()).map(|a| a.to_str());
    let mut total = kwarg(kwargs, "start").cloned().or_else(|| args.get(1).cloned()).unwrap_or(Value::Int(0));
    for item in v.iter()? {
        let x = match &path {
            Some(p) => attr_path(&item, p)?,
            None => item,
        };
        total = binary(BinOp::Add, &total, &x)?;
    }
    Ok(total)
}

fn do_list(v: &Value, _: &[Value], _: &[(String, Value)]) -> Result<Value> {
    Ok(Value::list(v.iter()?))
}

fn do_reverse(v: &Value, _: &[Value], _: &[(String, Value)]) -> Result<Value> {
    Ok(match v {
        Value::Str(s) => Value::str(s.chars().rev().collect::<String>()),
        Value::Markup(s) => Value::markup(s.chars().rev().collect::<String>()),
        other => {
            let mut items = other.iter()?;
            items.reverse();
            Value::list(items)
        }
    })
}

fn do_lower(v: &Value, _: &[Value], _: &[(String, Value)]) -> Result<Value> {
    Ok(match v.soft_str() {
        Value::Markup(s) => Value::markup(py::lower(&s)),
        other => Value::str(py::lower(&other.to_str())),
    })
}

fn do_upper(v: &Value, _: &[Value], _: &[(String, Value)]) -> Result<Value> {
    Ok(match v.soft_str() {
        Value::Markup(s) => Value::markup(py::upper(&s)),
        other => Value::str(py::upper(&other.to_str())),
    })
}

/// Jinja2's `url_quote`: UTF-8, "/" kept outside query strings.
fn url_quote(v: &Value, for_qs: bool) -> String {
    let q = py::quote(&v.to_str(), if for_qs { "" } else { "/" });
    if for_qs {
        q.replace("%20", "+")
    } else {
        q
    }
}

fn do_urlencode(v: &Value, _: &[Value], _: &[(String, Value)]) -> Result<Value> {
    Ok(Value::str(match v {
        Value::Str(_) | Value::Markup(_) => url_quote(v, false),
        Value::Dict(d) => d.items.values().map(|(k, x)| format!("{}={}", url_quote(k, true), url_quote(x, true))).collect::<Vec<_>>().join("&"),
        Value::List(_) | Value::Tuple(_) => v
            .iter()?
            .iter()
            .map(|pair| {
                let kv = pair.iter()?;
                Ok(format!("{}={}", url_quote(&kv[0], true), url_quote(&kv[1], true)))
            })
            .collect::<Result<Vec<_>>>()?
            .join("&"),
        other => url_quote(other, false),
    }))
}

fn do_default(v: &Value, args: &[Value], kwargs: &[(String, Value)]) -> Result<Value> {
    let default = arg(args, kwargs, 0, "default_value").cloned().unwrap_or(Value::str(""));
    let boolean = arg(args, kwargs, 1, "boolean").map(|b| b.truthy()).unwrap_or(false);
    Ok(if v.is_undefined() || (boolean && !v.truthy()) { default } else { v.clone() })
}

fn do_string(v: &Value, _: &[Value], _: &[(String, Value)]) -> Result<Value> {
    Ok(v.soft_str())
}

fn do_abs(v: &Value, _: &[Value], _: &[(String, Value)]) -> Result<Value> {
    Ok(match v {
        Value::Int(i) => Value::Int(i.abs()),
        Value::Float(f) => Value::Float(f.abs()),
        Value::Dec(d) => Value::pydec(d.abs()),
        other => return err(format!("bad operand type for abs(): '{}'", other.type_name())),
    })
}

fn do_trim(v: &Value, _: &[Value], _: &[(String, Value)]) -> Result<Value> {
    Ok(match v.soft_str() {
        Value::Markup(s) => Value::markup(py::strip(&s)),
        other => Value::str(py::strip(&other.to_str())),
    })
}

pub fn register_builtins(env: &mut Environment) {
    env.add_filter("format", do_format);
    env.add_filter("replace", do_replace);
    env.add_filter("length", do_length);
    env.add_filter("count", do_length);
    env.add_filter("safe", do_safe);
    env.add_filter("e", do_escape);
    env.add_filter("escape", do_escape);
    env.add_filter("int", do_int);
    env.add_filter("round", do_round);
    env.add_filter("join", do_join);
    env.add_filter("select", do_select);
    env.add_filter("reject", do_reject);
    env.add_filter("selectattr", do_selectattr);
    env.add_filter("map", do_map);
    env.add_filter("first", do_first);
    env.add_filter("last", do_last);
    env.add_filter("min", do_min);
    env.add_filter("max", do_max);
    env.add_filter("sum", do_sum);
    env.add_filter("list", do_list);
    env.add_filter("reverse", do_reverse);
    env.add_filter("lower", do_lower);
    env.add_filter("upper", do_upper);
    env.add_filter("urlencode", do_urlencode);
    env.add_filter("default", do_default);
    env.add_filter("d", do_default);
    env.add_filter("string", do_string);
    env.add_filter("abs", do_abs);
    env.add_filter("trim", do_trim);
}

/// `value is <name>(args)`.
pub fn test(name: &str, v: &Value, args: &[Value]) -> Result<bool> {
    Ok(match name {
        "defined" => !v.is_undefined(),
        "undefined" => v.is_undefined(),
        "none" => v.is_none(),
        "true" => matches!(v, Value::Bool(true)),
        "false" => matches!(v, Value::Bool(false)),
        "string" => matches!(v, Value::Str(_) | Value::Markup(_)),
        "number" => matches!(v, Value::Int(_) | Value::Float(_) | Value::Dec(_) | Value::Bool(_)),
        "mapping" => matches!(v, Value::Dict(_)),
        "sequence" | "iterable" => matches!(v, Value::List(_) | Value::Tuple(_) | Value::Str(_) | Value::Markup(_) | Value::Dict(_)),
        "odd" => v.as_int().map(|i| i % 2 != 0).unwrap_or(false),
        "even" => v.as_int().map(|i| i % 2 == 0).unwrap_or(false),
        "eq" | "equalto" | "==" => equals(v, args.first().unwrap_or(&Value::None)),
        "ne" | "!=" => !equals(v, args.first().unwrap_or(&Value::None)),
        "lt" | "lessthan" | "<" => compare(v, args.first().unwrap_or(&Value::None), "<")?.is_lt(),
        "le" | "<=" => compare(v, args.first().unwrap_or(&Value::None), "<=")?.is_le(),
        "gt" | "greaterthan" | ">" => compare(v, args.first().unwrap_or(&Value::None), ">")?.is_gt(),
        "ge" | ">=" => compare(v, args.first().unwrap_or(&Value::None), ">=")?.is_ge(),
        "in" => contains(args.first().unwrap_or(&Value::None), v)?,
        "sameas" => match (v, args.first()) {
            (Value::None, Some(Value::None)) => true,
            (Value::Bool(a), Some(Value::Bool(b))) => a == b,
            _ => false,
        },
        "callable" => matches!(v, Value::Func(_) | Value::Macro(_)),
        other => return err(format!("No test named '{other}'.")),
    })
}
