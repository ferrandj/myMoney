//! Rendering, with Jinja2's rules: `extends` and blocks, macros imported `with context`, includes that see the local
//! variables, a scope per loop and per macro, autoescaping of everything that is not markup.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use super::filters;
use super::parser::{CmpOp, Const, Expr, MacroDef, Node, Target, Template};
use super::value::*;

pub type FilterFn = fn(&Value, &[Value], &[(String, Value)]) -> Result<Value>;

pub struct Environment {
    templates: HashMap<String, Arc<Template>>,
    filters: HashMap<String, FilterFn>,
}

type Frame = HashMap<String, Value>;

pub struct Ctx {
    globals: Rc<HashMap<String, Value>>,
    vars: RefCell<HashMap<String, Value>>,
    blocks: RefCell<HashMap<String, Vec<Arc<Template>>>>,
}

impl Ctx {
    fn new(globals: Rc<HashMap<String, Value>>, vars: HashMap<String, Value>) -> Rc<Ctx> {
        Rc::new(Ctx { globals, vars: RefCell::new(vars), blocks: RefCell::new(HashMap::new()) })
    }

    /// `context.get_all()` plus the local variables (what an include or an import with context sees).
    fn all(&self, frames: &[Frame]) -> HashMap<String, Value> {
        let mut all = self.vars.borrow().clone();
        for f in frames {
            for (k, v) in f {
                all.insert(k.clone(), v.clone());
            }
        }
        all
    }
}

/// What a macro sees when it runs: the context it was defined in (and the local variables around its definition).
pub struct Closure {
    ctx: Rc<Ctx>,
    frames: Vec<Frame>,
}

impl Environment {
    pub fn new(sources: &[(&str, &str)]) -> Result<Environment> {
        let mut templates = HashMap::new();
        for (name, source) in sources {
            let t = super::parser::parse(name, source).map_err(|e| Error(format!("{name}: {e}")))?;
            templates.insert(name.to_string(), Arc::new(t));
        }
        let mut env = Environment { templates, filters: HashMap::new() };
        filters::register_builtins(&mut env);
        Ok(env)
    }

    pub fn add_filter(&mut self, name: &str, f: FilterFn) {
        self.filters.insert(name.to_string(), f);
    }

    fn template(&self, name: &str) -> Result<Arc<Template>> {
        self.templates.get(name).cloned().ok_or_else(|| Error(format!("TemplateNotFound: {name}")))
    }

    /// Render `name` with the globals and the variables of the call.
    pub fn render(&self, name: &str, globals: HashMap<String, Value>, vars: HashMap<String, Value>) -> Result<String> {
        let tpl = self.template(name)?;
        let ctx = Ctx::new(Rc::new(globals), vars);
        let mut out = String::new();
        let r = Renderer { env: self };
        r.root(&tpl, &ctx, &mut out, true)?;
        Ok(out)
    }
}

struct Renderer<'e> {
    env: &'e Environment,
}

fn lookup(name: &str, ctx: &Ctx, frames: &[Frame]) -> Value {
    for f in frames.iter().rev() {
        if let Some(v) = f.get(name) {
            return v.clone();
        }
    }
    if let Some(v) = ctx.vars.borrow().get(name) {
        return v.clone();
    }
    if let Some(v) = ctx.globals.get(name) {
        return v.clone();
    }
    Value::undefined(format!("'{name}'"))
}

fn assign(name: &str, value: Value, ctx: &Ctx, frames: &mut [Frame]) {
    match frames.last_mut() {
        Some(f) => {
            f.insert(name.to_string(), value);
        }
        None => {
            ctx.vars.borrow_mut().insert(name.to_string(), value);
        }
    }
}

fn unpack(target: &Target, value: Value, ctx: &Ctx, frames: &mut [Frame]) -> Result<()> {
    match target {
        Target::Name(n) => {
            assign(n, value, ctx, frames);
            Ok(())
        }
        Target::Tuple(parts) => {
            let items = value.iter()?;
            if items.len() != parts.len() {
                return err(if items.len() > parts.len() {
                    format!("too many values to unpack (expected {})", parts.len())
                } else {
                    format!("not enough values to unpack (expected {}, got {})", parts.len(), items.len())
                });
            }
            for (t, v) in parts.iter().zip(items) {
                unpack(t, v, ctx, frames)?;
            }
            Ok(())
        }
    }
}

impl<'e> Renderer<'e> {
    /// A template's top level. With `extends`, its own output is dropped and the parent renders its blocks.
    fn root(&self, tpl: &Arc<Template>, ctx: &Rc<Ctx>, out: &mut String, register_own: bool) -> Result<()> {
        if register_own {
            let mut blocks = ctx.blocks.borrow_mut();
            for (name, _) in &tpl.blocks {
                blocks.entry(name.clone()).or_default().push(tpl.clone());
            }
        }
        let mut parent: Option<Arc<Template>> = None;
        let mut frames: Vec<Frame> = vec![];
        for node in &tpl.body {
            match node {
                Node::Extends(e) => {
                    let name = self.eval(e, ctx, &frames)?.to_str();
                    let p = self.env.template(&name)?;
                    {
                        let mut blocks = ctx.blocks.borrow_mut();
                        for (bname, _) in &p.blocks {
                            blocks.entry(bname.clone()).or_default().push(p.clone());
                        }
                    }
                    parent = Some(p);
                }
                Node::Data(_) | Node::Output(_) | Node::Block(_) if parent.is_some() => {}
                other => self.exec_node(other, ctx, &mut frames, out)?,
            }
        }
        if let Some(p) = parent {
            self.root(&p, ctx, out, false)?;
        }
        Ok(())
    }

    fn exec(&self, nodes: &[Node], ctx: &Rc<Ctx>, frames: &mut Vec<Frame>, out: &mut String) -> Result<()> {
        for n in nodes {
            self.exec_node(n, ctx, frames, out)?;
        }
        Ok(())
    }

    fn exec_node(&self, node: &Node, ctx: &Rc<Ctx>, frames: &mut Vec<Frame>, out: &mut String) -> Result<()> {
        match node {
            Node::Data(d) => out.push_str(d),
            Node::Output(e) => {
                let v = self.eval(e, ctx, frames)?;
                out.push_str(&v.to_html());
            }
            Node::If(branches, else_) => {
                for (test, body) in branches {
                    if self.eval(test, ctx, frames)?.truthy() {
                        return self.exec(body, ctx, frames, out);
                    }
                }
                self.exec(else_, ctx, frames, out)?;
            }
            Node::For { target, iter, filter, body, else_ } => {
                let items = self.eval(iter, ctx, frames)?.iter()?;
                let items = match filter {
                    None => items,
                    Some(f) => {
                        let mut kept = vec![];
                        for item in items {
                            frames.push(Frame::new());
                            unpack(target, item.clone(), ctx, frames)?;
                            let keep = self.eval(f, ctx, frames);
                            frames.pop();
                            if keep?.truthy() {
                                kept.push(item);
                            }
                        }
                        kept
                    }
                };
                if items.is_empty() {
                    return self.exec(else_, ctx, frames, out);
                }
                let n = items.len();
                for (i, item) in items.iter().enumerate() {
                    let mut f = Frame::new();
                    f.insert(
                        "loop".into(),
                        Value::obj(
                            "LoopContext",
                            vec![
                                ("index", Value::Int(i as i64 + 1)),
                                ("index0", Value::Int(i as i64)),
                                ("revindex", Value::Int((n - i) as i64)),
                                ("revindex0", Value::Int((n - i - 1) as i64)),
                                ("first", Value::Bool(i == 0)),
                                ("last", Value::Bool(i + 1 == n)),
                                ("length", Value::Int(n as i64)),
                                ("previtem", if i > 0 { items[i - 1].clone() } else { Value::undefined("there is no previous item") }),
                                ("nextitem", if i + 1 < n { items[i + 1].clone() } else { Value::undefined("there is no next item") }),
                            ],
                        ),
                    );
                    frames.push(f);
                    let r = unpack(target, item.clone(), ctx, frames).and_then(|_| self.exec(body, ctx, frames, out));
                    frames.pop();
                    r?;
                }
            }
            Node::Set(target, e) => {
                let v = self.eval(e, ctx, frames)?;
                unpack(target, v, ctx, frames)?;
            }
            Node::Block(name) => {
                let tpl = ctx.blocks.borrow().get(name).and_then(|v| v.first().cloned()).ok_or_else(|| Error(format!("block {name}")))?;
                let body = tpl.block(name).ok_or_else(|| Error(format!("block {name}")))?;
                let mut block_frames = vec![Frame::new()];
                self.exec(body, ctx, &mut block_frames, out)?;
            }
            Node::Extends(_) => return err("extends must be at the top level"),
            Node::Include(e) => {
                let name = self.eval(e, ctx, frames)?.to_str();
                let tpl = self.env.template(&name)?;
                let sub = Ctx::new(ctx.globals.clone(), ctx.all(frames));
                self.root(&tpl, &sub, out, true)?;
            }
            Node::FromImport { template, names, with_context } => {
                let name = self.eval(template, ctx, frames)?.to_str();
                let tpl = self.env.template(&name)?;
                let module = Ctx::new(ctx.globals.clone(), if *with_context { ctx.all(frames) } else { HashMap::new() });
                let mut discard = String::new();
                self.root(&tpl, &module, &mut discard, true)?;
                for (n, alias) in names {
                    let v = module.vars.borrow().get(n).cloned();
                    let v = v.ok_or_else(|| Error(format!("the template {name:?} does not export the requested name {n:?}")))?;
                    assign(alias, v, ctx, frames);
                }
            }
            Node::Macro(def) => {
                let m = Value::Macro(Rc::new(MacroRef { def: def.clone(), closure: Rc::new(Closure { ctx: ctx.clone(), frames: frames.clone() }) }));
                assign(&def.name, m, ctx, frames);
            }
        }
        Ok(())
    }

    fn call_macro(&self, m: &MacroRef, args: Vec<Value>, kwargs: Vec<(String, Value)>) -> Result<Value> {
        let def: &MacroDef = &m.def;
        if args.len() > def.args.len() {
            return err(format!("macro '{}' takes not more than {} argument(s)", def.name, def.args.len()));
        }
        let mut frames = m.closure.frames.clone();
        frames.push(Frame::new());
        for (i, (name, default)) in def.args.iter().enumerate() {
            let v = if i < args.len() {
                args[i].clone()
            } else if let Some((_, v)) = kwargs.iter().find(|(k, _)| k == name) {
                v.clone()
            } else if let Some(d) = default {
                self.eval(d, &m.closure.ctx, &frames)?
            } else {
                Value::undefined(format!("parameter '{name}' was not provided"))
            };
            frames.last_mut().unwrap().insert(name.clone(), v);
        }
        if let Some((k, _)) = kwargs.iter().find(|(k, _)| !def.args.iter().any(|(a, _)| a == k)) {
            return err(format!("macro '{}' takes no keyword argument '{k}'", def.name));
        }
        let mut out = String::new();
        self.exec(&def.body, &m.closure.ctx, &mut frames, &mut out)?;
        Ok(Value::markup(out))
    }

    fn call(&self, f: &Value, args: Vec<Value>, kwargs: Vec<(String, Value)>) -> Result<Value> {
        match f {
            Value::Func(func) => func(args, kwargs),
            Value::Macro(m) => self.call_macro(m, args, kwargs),
            Value::Undefined(h) => err(format!("{h} is undefined")),
            other => err(format!("'{}' object is not callable", other.type_name())),
        }
    }

    fn eval_args(&self, args: &[Expr], kwargs: &[(String, Expr)], ctx: &Rc<Ctx>, frames: &[Frame]) -> Result<(Vec<Value>, Vec<(String, Value)>)> {
        let a = args.iter().map(|e| self.eval(e, ctx, frames)).collect::<Result<Vec<_>>>()?;
        let k = kwargs.iter().map(|(n, e)| Ok((n.clone(), self.eval(e, ctx, frames)?))).collect::<Result<Vec<_>>>()?;
        Ok((a, k))
    }

    pub fn eval(&self, e: &Expr, ctx: &Rc<Ctx>, frames: &[Frame]) -> Result<Value> {
        Ok(match e {
            Expr::Const(c) => match c {
                Const::None => Value::None,
                Const::Bool(b) => Value::Bool(*b),
                Const::Int(i) => Value::Int(*i),
                Const::Float(f) => Value::Float(*f),
                Const::Str(s) => Value::str(s),
            },
            Expr::Name(n) => lookup(n, ctx, frames),
            Expr::Attr(obj, name) => getattr(&self.eval(obj, ctx, frames)?, name)?,
            Expr::Item(obj, idx) => getitem(&self.eval(obj, ctx, frames)?, &self.eval(idx, ctx, frames)?)?,
            Expr::Slice(obj, a, b, c) => {
                let v = self.eval(obj, ctx, frames)?;
                let bound = |x: &Option<Box<Expr>>| -> Result<Option<i64>> {
                    match x {
                        None => Ok(None),
                        Some(e) => match self.eval(e, ctx, frames)? {
                            Value::None => Ok(None),
                            other => other.as_int().map(Some).ok_or_else(|| Error("slice indices must be integers or None".into())),
                        },
                    }
                };
                slice(&v, bound(a)?, bound(b)?, bound(c)?)?
            }
            Expr::Call(f, args, kwargs) => {
                let callee = self.eval(f, ctx, frames)?;
                let (a, k) = self.eval_args(args, kwargs, ctx, frames)?;
                self.call(&callee, a, k)?
            }
            Expr::Filter(value, name, args, kwargs) => {
                let v = self.eval(value, ctx, frames)?;
                let (a, k) = self.eval_args(args, kwargs, ctx, frames)?;
                let f = self.env.filters.get(name).ok_or_else(|| Error(format!("No filter named '{name}'.")))?;
                f(&v, &a, &k)?
            }
            Expr::Test(value, name, args, negated) => {
                let v = self.eval(value, ctx, frames)?;
                let a = args.iter().map(|e| self.eval(e, ctx, frames)).collect::<Result<Vec<_>>>()?;
                Value::Bool(filters::test(name, &v, &a)? != *negated)
            }
            Expr::Bin(op, a, b) => binary(*op, &self.eval(a, ctx, frames)?, &self.eval(b, ctx, frames)?)?,
            Expr::Concat(parts) => {
                let vals = parts.iter().map(|p| self.eval(p, ctx, frames)).collect::<Result<Vec<_>>>()?;
                if vals.iter().any(|v| v.is_markup()) {
                    Value::markup(vals.iter().map(|v| v.to_html()).collect::<String>())
                } else {
                    Value::str(vals.iter().map(|v| v.to_str()).collect::<String>())
                }
            }
            Expr::Neg(x) => negate(&self.eval(x, ctx, frames)?)?,
            Expr::Pos(x) => self.eval(x, ctx, frames)?,
            Expr::Not(x) => Value::Bool(!self.eval(x, ctx, frames)?.truthy()),
            Expr::And(a, b) => {
                let l = self.eval(a, ctx, frames)?;
                if !l.truthy() {
                    l
                } else {
                    self.eval(b, ctx, frames)?
                }
            }
            Expr::Or(a, b) => {
                let l = self.eval(a, ctx, frames)?;
                if l.truthy() {
                    l
                } else {
                    self.eval(b, ctx, frames)?
                }
            }
            Expr::Compare(first, ops) => {
                let mut left = self.eval(first, ctx, frames)?;
                for (op, rexpr) in ops {
                    let right = self.eval(rexpr, ctx, frames)?;
                    let ok = match op {
                        CmpOp::Eq => equals(&left, &right),
                        CmpOp::Ne => !equals(&left, &right),
                        CmpOp::Lt => compare(&left, &right, "<")?.is_lt(),
                        CmpOp::Le => compare(&left, &right, "<=")?.is_le(),
                        CmpOp::Gt => compare(&left, &right, ">")?.is_gt(),
                        CmpOp::Ge => compare(&left, &right, ">=")?.is_ge(),
                        CmpOp::In => contains(&right, &left)?,
                        CmpOp::NotIn => !contains(&right, &left)?,
                    };
                    if !ok {
                        return Ok(Value::Bool(false));
                    }
                    left = right;
                }
                Value::Bool(true)
            }
            Expr::Cond(test, then, else_) => {
                if self.eval(test, ctx, frames)?.truthy() {
                    self.eval(then, ctx, frames)?
                } else {
                    match else_ {
                        Some(e) => self.eval(e, ctx, frames)?,
                        None => Value::undefined("the inline if-expression evaluated to false and no else section was defined."),
                    }
                }
            }
            Expr::List(items) => Value::list(items.iter().map(|i| self.eval(i, ctx, frames)).collect::<Result<_>>()?),
            Expr::Tuple(items) => Value::tuple(items.iter().map(|i| self.eval(i, ctx, frames)).collect::<Result<_>>()?),
            Expr::Dict(items) => {
                let mut pairs = vec![];
                for (k, v) in items {
                    pairs.push((self.eval(k, ctx, frames)?, self.eval(v, ctx, frames)?));
                }
                Value::dict(pairs)
            }
        })
    }
}

// --------------------------------------------------------------------------- attributes and items

fn method(this: &Value, f: impl Fn(&Value, Args, Kwargs) -> Result<Value> + 'static) -> Value {
    let this = this.clone();
    Value::func(move |a, k| f(&this, a, k))
}

fn strftime(fmt: &str, d: &chrono::NaiveDateTime) -> Result<String> {
    use std::fmt::Write;
    let mut s = String::new();
    write!(s, "{}", d.format(fmt)).map_err(|_| Error(format!("bad strftime format {fmt:?}")))?;
    Ok(s)
}

fn arg_str(a: &[Value], i: usize) -> Result<String> {
    match a.get(i) {
        Some(v) => v.as_str().map(|s| s.to_string()).ok_or_else(|| Error("expected a string argument".into())),
        None => err("missing argument"),
    }
}

/// `environment.getattr(obj, name)`: the attribute, else the item of that name, else Undefined.
pub fn getattr(v: &Value, name: &str) -> Result<Value> {
    let missing = || Value::undefined(format!("'{} object' has no attribute '{}'", v.type_name(), name));
    Ok(match v {
        Value::Undefined(h) => return err(format!("{h} is undefined")),
        Value::Obj(o) => o.attrs.get(name).cloned().unwrap_or_else(missing),
        Value::Dict(_) => match name {
            "get" => method(v, |this, a, _| {
                let key = a.first().cloned().unwrap_or(Value::None);
                Ok(this.dict_get(&key).unwrap_or_else(|| a.get(1).cloned().unwrap_or(Value::None)))
            }),
            "items" => method(v, |this, _, _| match this {
                Value::Dict(d) => Ok(Value::list(d.items.values().map(|(k, v)| Value::tuple(vec![k.clone(), v.clone()])).collect())),
                _ => unreachable!(),
            }),
            "keys" => method(v, |this, _, _| this.iter().map(Value::list)),
            "values" => method(v, |this, _, _| match this {
                Value::Dict(d) => Ok(Value::list(d.items.values().map(|(_, v)| v.clone()).collect())),
                _ => unreachable!(),
            }),
            _ => v.dict_get(&Value::str(name)).unwrap_or_else(missing),
        },
        Value::Str(_) | Value::Markup(_) => {
            let wrap = |this: &Value, s: String| if this.is_markup() { Value::markup(s) } else { Value::str(s) };
            match name {
                "startswith" | "endswith" => {
                    let ends = name == "endswith";
                    method(v, move |this, a, _| {
                        let s = this.as_str().unwrap_or_default();
                        let prefixes: Vec<String> = match a.first() {
                            Some(Value::Tuple(t)) => t.iter().map(|x| x.to_str()).collect(),
                            _ => vec![arg_str(&a, 0)?],
                        };
                        Ok(Value::Bool(prefixes.iter().any(|p| if ends { s.ends_with(p.as_str()) } else { s.starts_with(p.as_str()) })))
                    })
                }
                "lower" => method(v, move |this, _, _| Ok(wrap(this, crate::py::lower(this.as_str().unwrap_or_default())))),
                "upper" => method(v, move |this, _, _| Ok(wrap(this, crate::py::upper(this.as_str().unwrap_or_default())))),
                "strip" => method(v, move |this, _, _| Ok(wrap(this, crate::py::strip(this.as_str().unwrap_or_default()).to_string()))),
                _ => missing(),
            }
        }
        Value::Date(d) => match name {
            "year" => Value::Int(chrono::Datelike::year(d) as i64),
            "month" => Value::Int(chrono::Datelike::month(d) as i64),
            "day" => Value::Int(chrono::Datelike::day(d) as i64),
            "isoformat" => method(v, |this, _, _| Ok(Value::str(this.to_str()))),
            "strftime" => method(v, |this, a, _| match this {
                Value::Date(d) => Ok(Value::str(strftime(&arg_str(&a, 0)?, &d.and_hms_opt(0, 0, 0).unwrap())?)),
                _ => unreachable!(),
            }),
            _ => missing(),
        },
        Value::DateTime(d) => match name {
            "year" => Value::Int(chrono::Datelike::year(d) as i64),
            "month" => Value::Int(chrono::Datelike::month(d) as i64),
            "day" => Value::Int(chrono::Datelike::day(d) as i64),
            "isoformat" => method(v, |this, _, _| match this {
                Value::DateTime(d) => Ok(Value::str(datetime_str(d, 'T'))),
                _ => unreachable!(),
            }),
            "strftime" => method(v, |this, a, _| match this {
                Value::DateTime(d) => Ok(Value::str(strftime(&arg_str(&a, 0)?, d)?)),
                _ => unreachable!(),
            }),
            "date" => method(v, |this, _, _| match this {
                Value::DateTime(d) => Ok(Value::Date(d.date())),
                _ => unreachable!(),
            }),
            _ => missing(),
        },
        _ => missing(),
    })
}

/// `environment.getitem(obj, key)`: the item, else the attribute of that name, else Undefined.
pub fn getitem(v: &Value, key: &Value) -> Result<Value> {
    let missing = || Value::undefined(format!("'{} object' has no attribute '{}'", v.type_name(), key.to_str()));
    match v {
        Value::Undefined(h) => return err(format!("{h} is undefined")),
        Value::List(l) | Value::Tuple(l) => {
            if let Some(i) = key.as_int() {
                let n = l.len() as i64;
                let j = if i < 0 { i + n } else { i };
                return Ok(if j >= 0 && j < n { l[j as usize].clone() } else { missing() });
            }
        }
        Value::Str(s) | Value::Markup(s) => {
            if let Some(i) = key.as_int() {
                let chars: Vec<char> = s.chars().collect();
                let n = chars.len() as i64;
                let j = if i < 0 { i + n } else { i };
                return Ok(if j >= 0 && j < n { Value::str(chars[j as usize].to_string()) } else { missing() });
            }
        }
        Value::Dict(_) => {
            if let Some(x) = v.dict_get(key) {
                return Ok(x);
            }
        }
        _ => {}
    }
    if let Some(name) = key.as_str() {
        if !matches!(v, Value::Dict(_)) {
            return getattr(v, name);
        }
        // a dict has no attribute of a missing key's name (except its methods)
        if matches!(name, "get" | "items" | "keys" | "values") {
            return getattr(v, name);
        }
    }
    Ok(missing())
}

fn slice(v: &Value, start: Option<i64>, stop: Option<i64>, step: Option<i64>) -> Result<Value> {
    let step = step.unwrap_or(1);
    if step == 0 {
        return err("slice step cannot be zero");
    }
    let pick = |n: usize| -> Vec<usize> {
        let n = n as i64;
        let norm = |x: i64, low: i64, high: i64| -> i64 {
            let x = if x < 0 { x + n } else { x };
            x.clamp(low, high)
        };
        let mut out = vec![];
        if step > 0 {
            let (mut i, end) = (start.map(|s| norm(s, 0, n)).unwrap_or(0), stop.map(|s| norm(s, 0, n)).unwrap_or(n));
            while i < end {
                out.push(i as usize);
                i += step;
            }
        } else {
            let (mut i, end) = (start.map(|s| norm(s, -1, n - 1)).unwrap_or(n - 1), stop.map(|s| norm(s, -1, n - 1)).unwrap_or(-1));
            while i > end {
                out.push(i as usize);
                i += step;
            }
        }
        out
    };
    Ok(match v {
        Value::List(l) => Value::list(pick(l.len()).into_iter().map(|i| l[i].clone()).collect()),
        Value::Tuple(l) => Value::tuple(pick(l.len()).into_iter().map(|i| l[i].clone()).collect()),
        Value::Str(s) | Value::Markup(s) => {
            let chars: Vec<char> = s.chars().collect();
            let t: String = pick(chars.len()).into_iter().map(|i| chars[i]).collect();
            if v.is_markup() {
                Value::markup(t)
            } else {
                Value::str(t)
            }
        }
        Value::Undefined(h) => return err(format!("{h} is undefined")),
        other => return err(format!("'{}' object is not subscriptable", other.type_name())),
    })
}
