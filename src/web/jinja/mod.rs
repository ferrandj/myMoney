//! A template engine that renders the Jinja2 templates of the Python version to the same bytes.

pub mod eval;
pub mod filters;
pub mod parser;
pub mod value;

pub use eval::{getattr, getitem, Environment};
pub use value::{equals, Error, Key, Obj, Result, Value};

/// Tagged JSON (see tools/jinja_cases.py) to a template value.
pub fn decode(v: &serde_json::Value) -> Value {
    use serde_json::Value as J;
    match v {
        J::Array(a) => Value::list(a.iter().map(decode).collect()),
        J::Object(m) if m.len() == 1 => {
            let (k, x) = m.iter().next().unwrap();
            let s = x.as_str().unwrap_or_default();
            match k.as_str() {
                "$dec" => Value::pydec(crate::py::PyDec::parse(s).unwrap()),
                "$date" => Value::Date(chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()),
                "$dt" => Value::DateTime(crate::models::parse_datetime(s).unwrap()),
                "$markup" => Value::markup(s),
                "$tuple" => Value::tuple(x.as_array().unwrap().iter().map(decode).collect()),
                "$float" => Value::Float(x.as_f64().unwrap()),
                "$obj" => Value::Obj(std::rc::Rc::new(Obj {
                    class: "SimpleNamespace",
                    attrs: x.as_object().unwrap().iter().map(|(a, b)| (a.clone(), decode(b))).collect(),
                })),
                "$intdict" => Value::dict(x.as_array().unwrap().iter().map(|p| (decode(&p[0]), decode(&p[1]))).collect()),
                _ => Value::dict(m.iter().map(|(a, b)| (Value::str(a), decode(b))).collect()),
            }
        }
        J::Object(m) => Value::dict(m.iter().map(|(a, b)| (Value::str(a), decode(b))).collect()),
        other => Value::from(other),
    }
}

/// `mymoney jinja-cases <cases.json>`: render the cases the way tools/jinja_cases.py does with Jinja2.
pub fn run_cases(path: &str) -> String {
    let data: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let extra: Vec<(String, String)> = data["extra"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string())).collect();
    let mut out = vec![];
    for case in data["cases"].as_array().unwrap() {
        let name = case[0].as_str().unwrap();
        let mut sources: Vec<(&str, &str)> = extra.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        sources.push(("__case__", case[1].as_str().unwrap()));
        let rendered = Environment::new(&sources).and_then(|env| {
            let vars = case[2].as_object().unwrap().iter().map(|(k, v)| (k.clone(), decode(v))).collect();
            env.render("__case__", Default::default(), vars)
        });
        out.push(serde_json::json!([name, rendered.unwrap_or_else(|e| format!("ERROR {e}"))]));
    }
    let sources: Vec<(&str, &str)> = extra.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let env = Environment::new(&sources).unwrap();
    for case in data["templates"].as_array().unwrap() {
        let name = case[0].as_str().unwrap();
        let vars = case[1].as_object().unwrap().iter().map(|(k, v)| (k.clone(), decode(v))).collect();
        out.push(serde_json::json!([name, env.render(name, Default::default(), vars).unwrap_or_else(|e| format!("ERROR {e}"))]));
    }
    serde_json::to_string(&out).unwrap()
}
