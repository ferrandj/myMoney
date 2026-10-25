//! `mymoney scenario <data dir> <scenario.json>`: the Rust side of tools/scenario.py. The same operations, the same
//! results and the same database dump, to compare the two implementations.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::NaiveDate;
use rusqlite::types::ValueRef;
use rust_decimal::Decimal;
use serde_json::{json, Map, Value};

use crate::app::MyMoney;
use crate::config::Settings;
use crate::error::{AppError, AppResult};
use crate::models::{Account, Loan, Transaction};
use crate::py;
use crate::services::{access, analytics, categories, counterparties as contacts, coverage, loans, statements, users as user_service};

const SKIP: &[&str] = &["created_at", "updated_at", "imported_at", "uploaded_at", "last_login", "fetched_at", "blob_name"];

fn d(v: &Decimal) -> Value {
    Value::String(py::dec_str(v))
}

fn od(v: &Option<Decimal>) -> Value {
    v.as_ref().map(d).unwrap_or(Value::Null)
}

fn day(v: &NaiveDate) -> Value {
    Value::String(v.format("%Y-%m-%d").to_string())
}

fn oday(v: &Option<NaiveDate>) -> Value {
    v.as_ref().map(day).unwrap_or(Value::Null)
}

fn date_arg(op: &Value, k: &str) -> NaiveDate {
    NaiveDate::parse_from_str(op[k].as_str().unwrap_or(""), "%Y-%m-%d").unwrap()
}

fn str_map(v: &Value) -> HashMap<String, String> {
    v.as_object().map(|m| m.iter().map(|(k, v)| (k.clone(), v.as_str().map(|s| s.to_string()).unwrap_or_else(|| v.to_string()))).collect()).unwrap_or_default()
}

fn message(e: &AppError) -> String {
    match e {
        AppError::Locked => String::new(),
        AppError::InvalidPassword => String::new(),
        AppError::Denied(m) | AppError::Value(m) | AppError::Throttled(m) | AppError::Other(m) => m.clone(),
    }
}

fn opt_map(m: Option<Map<String, Value>>) -> Value {
    m.map(Value::Object).unwrap_or(Value::Null)
}

fn all_ids(c: &rusqlite::Connection) -> AppResult<Vec<i64>> {
    Ok(Account::query(c, "ORDER BY id", [])?.into_iter().map(|a| a.id).collect())
}

fn run(core: &Arc<MyMoney>, op: &Value) -> AppResult<Value> {
    let kind = op["op"].as_str().unwrap_or("");
    let doc = op["doc"].as_i64().unwrap_or(0);
    let index = op["index"].as_u64().unwrap_or(0) as usize;
    match kind {
        "upload" => {
            let path = std::path::Path::new(op["file"].as_str().unwrap());
            let data = std::fs::read(path)?;
            core.purge_quarantine()?;
            let name = op["name"].as_str().map(|s| s.to_string()).unwrap_or_else(|| path.file_name().unwrap().to_string_lossy().into_owned());
            let (doc_id, existing) = core.store_document(&name, &data)?;
            if existing {
                return Ok(json!({"doc": doc_id, "state": "existing"}));
            }
            let status = core.analyze_document(doc_id, None, Some(data))?;
            let verdict = if status == "parsed" { core.check_duplicate(doc_id)? } else { None };
            let imported = if status == "parsed" && verdict.is_none() { core.auto_import(doc_id)? } else { None };
            let state = match (verdict, &imported) {
                (Some("identical"), _) => "duplicate".to_string(),
                (Some(_), _) => "differs".to_string(),
                (None, Some(_)) => "imported".to_string(),
                _ => status,
            };
            return Ok(json!({"doc": doc_id, "state": state, "imported": opt_map(imported)}));
        }
        "analyze" => return Ok(core.analyze_document(doc, op["adapter"].as_str(), None)?.into()),
        "import_default" => return Ok(Value::Object(core.import_default(doc, index, op["force"].as_bool().unwrap_or(false))?)),
        "import" => return Ok(Value::Object(core.import_statement(doc, index, &str_map(&op["form"]))?)),
        "check_obvious" => return Ok(core.check_obvious(doc, op["apply"].as_bool().unwrap_or(false))?.map(Value::String).unwrap_or(Value::Null)),
        "pending" => {
            let reasons: Map<String, Value> = core.pending_reasons()?.into_iter().map(|(i, r)| (i.to_string(), Value::String(r))).collect();
            let (next, n) = core.next_pending(op["after"].as_i64())?;
            return Ok(json!({"reasons": reasons, "next": [next, n]}));
        }
        "invalidate" => {
            let ids: Vec<i64> = op["docs"].as_array().map(|a| a.iter().filter_map(|x| x.as_i64()).collect()).unwrap_or_default();
            return Ok(core.invalidate_documents(&ids)?.into());
        }
        "undo" => return Ok(core.undo_import(doc)?.into()),
        "replace" => return Ok(Value::Object(core.replace_document(doc, index)?)),
        "keep" => {
            core.keep_duplicate(doc)?;
            return Ok(Value::Null);
        }
        "rebuild" => return Ok(Value::Object(core.rebuild_imports()?)),
        "save_account" => return Ok(core.save_account(op["id"].as_i64(), &str_map(&op["data"]))?.into()),
        "delete_account" => {
            core.delete_account(op["id"].as_i64().unwrap_or(0))?;
            return Ok(Value::Null);
        }
        "delete_document" => {
            core.delete_document(doc)?;
            return Ok(Value::Null);
        }
        "portfolio" => {
            let p = core.portfolio(true)?;
            let hist: Vec<Value> = p.history.iter().map(|(dd, v)| json!([day(dd), d(v)])).collect();
            let start = hist.len().saturating_sub(3);
            return Ok(json!({
                "total": d(&p.total), "assets": d(&p.assets), "debts": d(&p.debts),
                "by_category": p.by_category.iter().map(|(c, l, t)| json!([c, l, d(t)])).collect::<Vec<_>>(),
                "accounts": p.accounts.iter().map(|v| json!([v.account.id, d(&v.cash_eur), d(&v.total()), v.warnings])).collect::<Vec<_>>(),
                "history": hist[start..].to_vec(),
            }));
        }
        "review" => {
            let Some(review) = core.review_document(doc)? else { return Ok(json!([])) };
            let out: Vec<Value> = review
                .plans
                .iter()
                .map(|p| {
                    json!({
                        "account": p.account.as_ref().map(|a| a.id), "candidates": p.candidates.iter().map(|a| a.id).collect::<Vec<_>>(),
                        "statuses": p.statuses(), "issues": p.issues, "clean": p.clean(), "overlaps": p.overlaps.iter().map(|o| o.id).collect::<Vec<_>>(),
                        "already": p.already_imported.as_ref().map(|s| s.id), "ask_holder": p.ask_holder, "newer_balance": p.newer_balance,
                        "loan_existing": p.loan_existing, "loan_unverified": p.loan_unverified,
                        "dedup": p.dedup.as_ref().map(|x| json!([x.identical(), x.extra.len(), x.missing.iter().map(|t| t.id).collect::<Vec<_>>(),
                            x.opening.map(|(a, b)| json!([d(&a), d(&b)])), x.closing.map(|(a, b)| json!([d(&a), d(&b)])), x.new_count, x.probable_count])),
                    })
                })
                .collect();
            return Ok(Value::Array(out));
        }
        _ => {}
    }
    core.session(|c| match kind {
        "spending_report" => {
            let ids = all_ids(c)?;
            let r = analytics::spending_report(c, &ids, date_arg(op, "start"), date_arg(op, "end"), op["internal"].as_bool().unwrap_or(false), op["direction"].as_str().unwrap_or("out"), op["category"].as_str())?;
            Ok(json!({
                "total": d(&r.total), "out": d(&r.total_out), "in": d(&r.total_in), "saved": d(&r.saved()),
                "categories": r.categories.iter().map(|x| json!([x.id, x.label, d(&x.total), x.count, x.share.py_str()])).collect::<Vec<_>>(),
                "months": r.months.iter().map(|(m, v)| json!([day(m), d(v)])).collect::<Vec<_>>(),
                "category": r.category.as_ref().map(|x| json!([x.id, d(&x.total)])),
                "merchants": r.merchants.iter().map(|m| json!([m.key, m.label, m.contact_id, d(&m.total), m.count, m.category, m.ruled])).collect::<Vec<_>>(),
                "operations": r.operations.iter().map(|(t, n)| json!([t.id, n])).collect::<Vec<_>>(),
                "top": r.top_merchants.iter().map(|m| json!([m.key, m.label, d(&m.total), m.count, m.category, m.ruled])).collect::<Vec<_>>(),
                "fees": [d(&r.fees.operations_total), r.fees.operations_count, d(&r.fees.summaries_total()), d(&r.fees.interest_paid()), d(&r.fees.refunded()),
                    r.fees.summaries.iter().map(|f| json!([f.account_id, day(&f.period_start), day(&f.period_end), d(&f.charged), d(&f.refunded), d(&f.net), d(&f.operations), f.operations_count, d(&f.gap())])).collect::<Vec<_>>()],
            }))
        }
        "flows" => {
            let ids = all_ids(c)?;
            let (start, end, internal) = (date_arg(op, "start"), date_arg(op, "end"), op["internal"].as_bool().unwrap_or(false));
            let flows = analytics::monthly_flows(c, &ids, start, end, internal)?;
            let sp = analytics::spending(c, &ids, start, end, internal, 12)?;
            let rank = |rs: &[analytics::Ranking]| rs.iter().map(|r| json!([r.label, d(&r.total), r.count, r.contact_id])).collect::<Vec<_>>();
            Ok(json!({"flows": flows.iter().map(|f| json!([day(&f.month), d(&f.income), d(&f.expense), d(&f.net())])).collect::<Vec<_>>(),
                "kinds": rank(&sp.by_kind), "out": rank(&sp.top_out), "in": rank(&sp.top_in), "total": [d(&sp.total_out), d(&sp.total_in)]}))
        }
        "history" => {
            let today = date_arg(op, "today");
            let mut out = Map::new();
            for a in Account::query(c, "ORDER BY id", [])? {
                let holdings = crate::models::Holding::query(c, "WHERE account_id = ?", [a.id])?;
                let h = analytics::account_history(c, &a, &holdings, a.balance, today)?;
                let mut items: Vec<Value> = h.iter().map(|(dd, v)| json!([day(dd), d(v)])).collect();
                let start = items.len().saturating_sub(5);
                items = items[start..].to_vec();
                items.push(h.len().into());
                out.insert(a.id.to_string(), Value::Array(items));
            }
            Ok(Value::Object(out))
        }
        "loan" => {
            let Some(loan) = Loan::query_one(c, "WHERE account_id = ?", [op["account"].as_i64().unwrap_or(0)])? else { return Ok(Value::Null) };
            let plan = loans::build(c, &loan, Some(date_arg(op, "today")))?;
            let mut sm = Map::new();
            if let Some(s) = &plan.summary {
                sm.insert("as_of".into(), day(&s.as_of));
                sm.insert("owed".into(), od(&s.owed));
                sm.insert("end_date".into(), oday(&s.end_date));
                sm.insert("paid_count".into(), s.paid_count.into());
                sm.insert("remaining_count".into(), s.remaining_count.into());
                sm.insert("total_count".into(), s.total_count.into());
                sm.insert("next".into(), s.next.as_ref().map(|r| json!([oday(&r.date), d(&r.total)])).unwrap_or(Value::Null));
                sm.insert("dated".into(), s.dated.into());
                sm.insert("future_interest".into(), d(&s.future_interest));
                sm.insert("future_insurance".into(), d(&s.future_insurance));
                sm.insert("future_total".into(), d(&s.future_total));
                sm.insert("paid_total".into(), d(&s.paid_total));
                sm.insert("paid_interest".into(), d(&s.paid_interest));
                sm.insert("lifetime_interest".into(), d(&s.lifetime_interest));
                sm.insert("repaid_early".into(), d(&s.repaid_early));
                if let Some(sv) = &s.saved {
                    sm.insert("saved_interest".into(), d(&sv.interest));
                    sm.insert("saved_insurance".into(), d(&sv.insurance));
                    sm.insert("saved_months".into(), sv.months.into());
                }
            }
            Ok(json!({
                "rows": plan.rows.iter().map(|r| json!([r.kind, oday(&r.date), r.number, d(&r.capital), d(&r.interest), d(&r.insurance), d(&r.total), d(&r.remaining), r.source, r.status, od(&r.confirmed), r.note])).collect::<Vec<_>>(),
                "checks": plan.checks.iter().map(|x| json!([x.status, x.text])).collect::<Vec<_>>(),
                "summary": sm,
            }))
        }
        "gaps" => Ok(Value::Array(coverage::all_gaps(c, true)?.iter().map(|g| json!([g.account_id, day(&g.start), day(&g.end), g.ack_id, g.label(), g.days()])).collect())),
        "acknowledge" => {
            coverage::acknowledge(c, op["account"].as_i64().unwrap_or(0), date_arg(op, "start"), date_arg(op, "end"), op["note"].as_str())?;
            Ok(Value::Null)
        }
        "coverage" => {
            let (rows, broken) = statements::coverage(c, op["account"].as_i64().unwrap_or(0))?;
            Ok(json!([rows.iter().map(|r| r.id).collect::<Vec<_>>(), broken]))
        }
        "unresolved" => Ok(Value::Array(
            contacts::unresolved(c, op["shops"].as_bool().unwrap_or(false), None, None)?
                .iter()
                .map(|u| json!([u.kind, u.key, u.name, u.iban, u.count, d(&u.total_in), d(&u.total_out), day(&u.last), u.accounts.iter().collect::<Vec<_>>()]))
                .collect(),
        )),
        "identify" => Ok(contacts::identify(c, &str_map(&op["data"]))?.id.into()),
        "contacts" => Ok(Value::Array(
            contacts::summaries(c)?
                .iter()
                .map(|s| json!([s.contact.id, s.contact.name, s.count, d(&s.total_in), d(&s.total_out), oday(&s.last), s.identifiers.iter().map(|i| i.value.clone()).collect::<Vec<_>>()]))
                .collect(),
        )),
        "set_rule" => {
            categories::set_rule(c, op["key"].as_str().unwrap_or(""), op["category"].as_str().unwrap_or(""))?;
            Ok(Value::Null)
        }
        "categorize" => {
            let rules = categories::load_rules(c)?;
            let names = analytics::contact_names(c)?;
            Ok(Value::Array(
                Transaction::query(c, "ORDER BY id", [])?
                    .iter()
                    .map(|t| json!([t.id, categories::categorize(t, t.counterparty_id.and_then(|i| names.get(&i)).map(|s| s.as_str()), &rules)]))
                    .collect(),
            ))
        }
        other => Err(AppError::Value(format!("opération inconnue : {other}"))),
    })
}

fn dump(core: &MyMoney) -> AppResult<Value> {
    let reader = crate::adapters::registry::reader_version();
    core.session(|c| {
        let mut st = c.prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")?;
        let tables: Vec<String> = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        let mut out = Map::new();
        for t in tables {
            let mut st = c.prepare(&format!("PRAGMA table_info(\"{t}\")"))?;
            let cols: Vec<String> = st.query_map([], |r| r.get(1))?.collect::<Result<_, _>>()?;
            let keep: Vec<String> = cols.into_iter().filter(|c| !SKIP.contains(&c.as_str())).collect();
            let sql = format!("SELECT {} FROM \"{t}\" ORDER BY rowid", keep.iter().map(|c| format!("\"{c}\"")).collect::<Vec<_>>().join(", "));
            let mut st = c.prepare(&sql)?;
            let mut rows = st.query([])?;
            let mut list = vec![];
            while let Some(r) = rows.next()? {
                let mut m = Map::new();
                for (i, col) in keep.iter().enumerate() {
                    let v = match r.get_ref(i)? {
                        ValueRef::Null => Value::Null,
                        ValueRef::Integer(n) => n.into(),
                        ValueRef::Real(f) => Value::from(f),
                        ValueRef::Text(t) => Value::String(String::from_utf8_lossy(t).replace(reader, "READER")),
                        ValueRef::Blob(b) => Value::String(hex::encode(b)),
                    };
                    m.insert(col.clone(), v);
                }
                list.push(Value::Object(m));
            }
            out.insert(t, Value::Array(list));
        }
        Ok(Value::Object(out))
    })
}

pub fn main(data_dir: &str, script: &str) -> i32 {
    let ops: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(script).unwrap()).unwrap();
    let settings = Settings { data_dir: data_dir.into(), argon2_memory_kib: 8192, argon2_time_cost: 1, ..Settings::from_env() };
    let core = MyMoney::new(settings).unwrap();
    core.setup("moi@example.org", Some("Moi"), "un mot de passe assez long").unwrap();
    let me = core.session(|c| Ok(user_service::get_by_email(c, Some("moi@example.org"))?.map(|u| user_service::identity(&u)))).unwrap();
    let results: Vec<Value> = access::as_user(me, || {
        ops.iter()
            .map(|op| match run(&core, op) {
                Ok(v) => json!({"op": op["op"], "ok": v}),
                Err(e) => json!({"op": op["op"], "error": message(&e)}),
            })
            .collect()
    });
    let db = dump(&core).unwrap();
    println!("{}", serde_json::to_string(&json!({"results": results, "db": db})).unwrap().replace(crate::adapters::registry::reader_version(), "READER"));
    0
}
