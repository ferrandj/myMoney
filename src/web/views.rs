//! What the templates see: the rows and results of the services as the objects the Python templates were written
//! for (same attribute names, same types), and the formatting filters of the Python version.

use std::collections::HashMap;

use rusqlite::Connection;

use super::jinja::value::{to_decimal, Result};
use super::jinja::Value;
use crate::adapters::base::{HoldingData, ParsedStatement, TransactionData};
use crate::error::AppResult;
use crate::models::{Account, Counterparty, CounterpartyIdentifier, Document, FeeSummary, Holding, Loan, LoanEvent, LoanInstallment, Statement, Transaction, User};
use crate::py::{self, PyDec};
use crate::services::access::Identity;
use crate::services::coverage::Gap;
use crate::services::holders::{self, Person};
use crate::services::loans;
use crate::services::people;
use crate::services::statements::ImportPlan;

pub fn obj(class: &'static str, mut fields: Vec<(&'static str, Value)>, extra: Vec<(&'static str, Value)>) -> Value {
    for (k, v) in extra {
        match fields.iter_mut().find(|(f, _)| *f == k) {
            Some(slot) => slot.1 = v,
            None => fields.push((k, v)),
        }
    }
    Value::obj(class, fields)
}

pub fn opt_str(s: &Option<String>) -> Value {
    s.clone().into()
}

// ------------------------------------------------------------------------------------------ models

pub fn user(u: &User) -> Value {
    obj("User", u.fields(), vec![("label", u.label().into())])
}

pub fn identity(i: &Identity) -> Value {
    Value::obj(
        "Identity",
        vec![("id", i.id.into()), ("email", i.email.clone().into()), ("name", opt_str(&i.name)), ("is_admin", i.is_admin.into()), ("label", i.label().into())],
    )
}

pub fn holding(h: &Holding) -> Value {
    obj("Holding", h.fields(), vec![])
}

/// An account with what the templates read through its relationships (owner, holdings).
pub fn account(c: &Connection, a: &Account) -> AppResult<Value> {
    let owner = match a.owner_id {
        Some(id) => User::get(c, id)?.map(|u| user(&u)).unwrap_or(Value::None),
        None => Value::None,
    };
    let holdings = Holding::query(c, "WHERE account_id = ? ORDER BY id", [a.id])?;
    Ok(account_with(a, owner, holdings.iter().map(holding).collect()))
}

/// A card as the "existing person" list of the forms and the pages show it.
pub fn person_card(card: &crate::models::PersonCard) -> Value {
    let p = people::person_of(card);
    let strs = |v: &[String]| Value::list(v.iter().map(|s| s.clone().into()).collect());
    let mut f = card.fields();
    f.push(("label", people::label(card).into()));
    f.push(("last", strs(&p.last_names)));
    f.push(("birth", strs(&p.birth_names)));
    f.push(("first", strs(&p.first_names)));
    Value::obj("PersonCard", f)
}

fn person(p: &Person) -> Value {
    Value::sdict(vec![
        ("id", p.id.map(Value::Int).unwrap_or(Value::None)),
        ("civility", p.civility.clone().into()),
        ("last_names", Value::list(p.last_names.iter().map(|s| s.clone().into()).collect())),
        ("birth_names", Value::list(p.birth_names.iter().map(|s| s.clone().into()).collect())),
        ("first_names", Value::list(p.first_names.iter().map(|s| s.clone().into()).collect())),
    ])
}

/// "Compte courant - Joint": the type, with "Joint" when more than one person is behind the account.
pub fn type_label_of(a: &Account) -> String {
    if holders::of_account(a).len() > 1 {
        format!("{} - Joint", a.type_label())
    } else {
        a.type_label().to_string()
    }
}

pub fn account_with(a: &Account, owner: Value, holdings: Vec<Value>) -> Value {
    let people = holders::of_account(a);
    obj(
        "Account",
        a.fields(),
        vec![
            ("people", Value::list(people.iter().map(person).collect())),
            ("holder_blocks", Value::from(&holders::form_blocks(&people, 2))),
            ("holder_shown", people.len().max(1).into()),
            ("type_label", type_label_of(a).into()),
            ("joint", (people.len() > 1).into()),
            ("category", a.category().into()),
            ("rib", a.rib().into()),
            ("owner", owner),
            ("holdings", Value::list(holdings)),
        ],
    )
}

pub fn counterparty(cp: &Counterparty, identifiers: &[CounterpartyIdentifier]) -> Value {
    obj("Counterparty", cp.fields(), vec![("identifiers", Value::list(identifiers.iter().map(|i| obj("CounterpartyIdentifier", i.fields(), vec![])).collect()))])
}

pub fn transaction(t: &Transaction, counterparty: Value) -> Value {
    obj("Transaction", t.fields(), vec![("internal", t.internal().into()), ("counterparty", counterparty)])
}

pub fn document(d: &Document) -> Value {
    obj("Document", d.fields(), vec![])
}

pub fn statement(s: &Statement) -> Value {
    obj("Statement", s.fields(), vec![])
}

pub fn fee_summary(f: &FeeSummary) -> Value {
    obj("FeeSummary", f.fields(), vec![])
}

pub fn loan(l: &Loan, installments: &[LoanInstallment], events: &[LoanEvent]) -> Value {
    obj(
        "Loan",
        l.fields(),
        vec![
            ("installments", Value::list(installments.iter().map(|i| obj("LoanInstallment", i.fields(), vec![])).collect())),
            ("events", Value::list(events.iter().map(|e| obj("LoanEvent", e.fields(), vec![])).collect())),
        ],
    )
}

pub fn gap(g: &Gap) -> Value {
    Value::obj(
        "Gap",
        vec![
            ("account_id", g.account_id.into()),
            ("account_name", g.account_name.clone().into()),
            ("start", g.start.into()),
            ("end", g.end.into()),
            ("ack_id", g.ack_id.into()),
            ("acknowledged", g.acknowledged().into()),
            ("days", g.days().into()),
            ("label", g.label().into()),
        ],
    )
}

// ---------------------------------------------------------------------------------- read documents

fn json_map(m: &Option<serde_json::Map<String, serde_json::Value>>) -> Value {
    match m {
        Some(m) => Value::from(&serde_json::Value::Object(m.clone())),
        None => Value::None,
    }
}

pub fn transaction_data(t: &TransactionData) -> Value {
    Value::obj(
        "TransactionData",
        vec![
            ("date", t.date.into()),
            ("label", t.label.clone().into()),
            ("amount", t.amount.into()),
            ("currency", t.currency.clone().into()),
            ("external_id", opt_str(&t.external_id)),
            ("value_date", t.value_date.into()),
            ("kind", opt_str(&t.kind)),
            ("counterparty", opt_str(&t.counterparty)),
            ("counterparty_iban", opt_str(&t.counterparty_iban)),
            ("reference", opt_str(&t.reference)),
            ("memo", opt_str(&t.memo)),
            ("raw", opt_str(&t.raw)),
            ("details", json_map(&t.details)),
        ],
    )
}

pub fn holding_data(h: &HoldingData) -> Value {
    Value::obj(
        "HoldingData",
        vec![
            ("symbol", opt_str(&h.symbol)),
            ("quantity", h.quantity.into()),
            ("name", opt_str(&h.name)),
            ("isin", opt_str(&h.isin)),
            ("asset_class", h.asset_class.clone().into()),
            ("unit_price", h.unit_price.into()),
            ("value", h.value.into()),
            ("value_currency", opt_str(&h.value_currency)),
            ("cost_basis", h.cost_basis.into()),
            ("extra", json_map(&h.extra)),
        ],
    )
}

pub fn parsed_statement(st: &ParsedStatement) -> Value {
    Value::obj(
        "ParsedStatement",
        vec![
            ("source_format", st.source_format.clone().into()),
            ("institution", opt_str(&st.institution)),
            ("adapter_id", opt_str(&st.adapter_id)),
            ("document_kind", opt_str(&st.document_kind)),
            ("account_label", opt_str(&st.account_label)),
            ("account_type", opt_str(&st.account_type)),
            ("currency", st.currency.clone().into()),
            ("holder_name", opt_str(&st.holder_name)),
            ("holder_address", opt_str(&st.holder_address)),
            ("iban", opt_str(&st.iban)),
            ("bic", opt_str(&st.bic)),
            ("bank_code", opt_str(&st.bank_code)),
            ("branch_code", opt_str(&st.branch_code)),
            ("account_number", opt_str(&st.account_number)),
            ("rib_key", opt_str(&st.rib_key)),
            ("balance", st.balance.into()),
            ("balance_date", st.balance_date.into()),
            ("opening_balance", st.opening_balance.into()),
            ("opening_date", st.opening_date.into()),
            ("period_start", st.period_start.into()),
            ("period_end", st.period_end.into()),
            ("transactions", Value::list(st.transactions.iter().map(transaction_data).collect())),
            ("holdings", match &st.holdings {
                Some(h) => Value::list(h.iter().map(holding_data).collect()),
                None => Value::None,
            }),
            ("candidate_balances", Value::list(st.candidate_balances.iter().map(|(l, a)| Value::tuple(vec![l.clone().into(), (*a).into()])).collect())),
            ("warnings", Value::list(st.warnings.iter().map(|w| w.clone().into()).collect())),
            ("confidence", st.confidence.clone().into()),
            ("extra", Value::from(&serde_json::Value::Object(st.extra.clone()))),
        ],
    )
}

pub fn plan(c: &Connection, p: &ImportPlan) -> AppResult<Value> {
    let st = &p.statement;
    let tx = |i: usize| st.transactions.get(i).map(transaction_data).unwrap_or(Value::None);
    let classified = Value::obj(
        "Classified",
        vec![
            ("new", Value::list(p.classified.new.iter().map(|(i, _)| tx(*i)).collect())),
            ("exact", Value::list(p.classified.exact.iter().map(|i| tx(*i)).collect())),
            ("probable", Value::list(p.classified.probable.iter().map(|(i, _)| tx(*i)).collect())),
        ],
    );
    let dedup = match &p.dedup {
        None => Value::None,
        Some(d) => Value::obj(
            "DedupReport",
            vec![
                ("previous", statement(&d.previous)),
                ("document", d.document.as_ref().map(document).unwrap_or(Value::None)),
                ("extra", Value::list(d.extra.iter().map(|i| tx(*i)).collect())),
                ("missing", Value::list(d.missing.iter().map(|t| transaction(t, Value::None)).collect())),
                ("opening", d.opening.map(|(a, b)| Value::tuple(vec![a.into(), b.into()])).unwrap_or(Value::None)),
                ("closing", d.closing.map(|(a, b)| Value::tuple(vec![a.into(), b.into()])).unwrap_or(Value::None)),
                ("new_count", d.new_count.into()),
                ("probable_count", d.probable_count.into()),
                ("identical", d.identical().into()),
                ("can_replace", d.can_replace().into()),
            ],
        ),
    };
    let statuses: Vec<Value> = p.statuses().into_iter().map(Value::from).collect();
    let statuses = Value::list(statuses);
    let chosen = match &p.account {
        Some(a) => account(c, a)?,
        None => Value::None,
    };
    let candidates = p.candidates.iter().map(|a| account(c, a)).collect::<AppResult<Vec<_>>>()?;
    // The holders the form offers: the account's own, else a reading of the line the bank prints.
    let known = p.account.as_ref().map(holders::of_account).unwrap_or_default();
    // a printed name that is a known card's is offered with that card, so the names are not typed again
    let people = if known.is_empty() { people::match_guess(&people::visible(c)?, &holders::parse_printed(st.holder_name.as_deref().unwrap_or(""))) } else { known };
    let shown = if p.ask_holder || people.len() >= 2 { people.len().max(2) } else { 1 };
    Ok(Value::obj(
        "ImportPlan",
        vec![
            ("holder_blocks", Value::from(&holders::form_blocks(&people, 2))),
            ("holder_shown", shown.into()),
            ("statement", parsed_statement(st)),
            ("account", chosen),
            ("candidates", Value::list(candidates)),
            ("classified", classified),
            ("already_imported", p.already_imported.as_ref().map(statement).unwrap_or(Value::None)),
            ("overlaps", Value::list(p.overlaps.iter().map(statement).collect())),
            ("issues", Value::list(p.issues.iter().map(|i| i.clone().into()).collect())),
            ("newer_balance", p.newer_balance.into()),
            ("ask_holder", p.ask_holder.into()),
            ("dedup", dedup),
            ("fee_existing", p.fee_existing.as_ref().map(fee_summary).unwrap_or(Value::None)),
            ("loan_existing", opt_str(&p.loan_existing)),
            ("loan_unverified", opt_str(&p.loan_unverified)),
            ("statuses", Value::func(move |_, _| Ok(statuses.clone()))),
        ],
    ))
}

// ------------------------------------------------------------------------------------------- loans

fn loan_row(r: &loans::Row) -> Value {
    Value::obj(
        "Row",
        vec![
            ("kind", r.kind.into()),
            ("date", r.date.into()),
            ("number", r.number.into()),
            ("capital", r.capital.into()),
            ("interest", r.interest.into()),
            ("insurance", r.insurance.into()),
            ("total", r.total.into()),
            ("remaining", r.remaining.into()),
            ("source", r.source.into()),
            ("status", r.status.into()),
            ("confirmed", r.confirmed.into()),
            ("note", r.note.clone().into()),
        ],
    )
}

pub fn loan_plan(p: &loans::Plan) -> Value {
    let summary = match &p.summary {
        None => Value::dict(vec![]),
        Some(s) => {
            let mut pairs: Vec<(&str, Value)> = vec![
                ("as_of", s.as_of.into()),
                ("owed", s.owed.into()),
                ("end_date", s.end_date.into()),
                ("paid_count", s.paid_count.into()),
                ("remaining_count", s.remaining_count.into()),
                ("total_count", s.total_count.into()),
                ("next", s.next.as_ref().map(loan_row).unwrap_or(Value::None)),
                ("dated", s.dated.into()),
                ("future_interest", s.future_interest.into()),
                ("future_insurance", s.future_insurance.into()),
                ("future_total", s.future_total.into()),
                ("paid_total", s.paid_total.into()),
                ("paid_interest", s.paid_interest.into()),
                ("lifetime_interest", s.lifetime_interest.into()),
                ("repaid_early", s.repaid_early.into()),
            ];
            if let Some(saved) = &s.saved {
                pairs.push(("saved_interest", saved.interest.into()));
                pairs.push(("saved_insurance", saved.insurance.into()));
                pairs.push(("saved_months", saved.months.into()));
            }
            Value::sdict(pairs)
        }
    };
    let check = |c: &loans::Check| Value::obj("Check", vec![("status", c.status.into()), ("text", c.text.clone().into())]);
    Value::obj(
        "Plan",
        vec![
            ("rows", Value::list(p.rows.iter().map(loan_row).collect())),
            ("checks", Value::list(p.checks.iter().map(check).collect())),
            ("summary", summary),
            ("installments", Value::list(p.installments().into_iter().map(loan_row).collect())),
            ("problems", Value::list(p.problems().into_iter().map(check).collect())),
        ],
    )
}

// -------------------------------------------------------------------------------------- formatting

fn currency_symbol(currency: &Value) -> String {
    match currency.as_str() {
        Some("EUR") => "€".into(),
        Some("USD") => "$".into(),
        Some("GBP") => "£".into(),
        _ => currency.to_str(),
    }
}

/// An amount as plain text (for attributes and messages).
pub fn money_text(value: &Value, currency: &Value, decimals: usize) -> Result<String> {
    if value.is_none() {
        return Ok("—".into());
    }
    let d = to_decimal(value)?;
    let sign = if d.cmp(&PyDec::int(0)).is_lt() { "-" } else { "" };
    let text = d.abs().format_fixed(decimals, false, true).replace(',', " ").replace('.', ",");
    Ok(format!("{sign}{text} {}", currency_symbol(currency)))
}

fn filter_args(args: &[Value], kwargs: &[(String, Value)]) -> (Value, usize) {
    let get = |i: usize, name: &str| args.get(i).cloned().or_else(|| kwargs.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()));
    let currency = get(0, "currency").unwrap_or_else(|| Value::str("EUR"));
    let decimals = get(1, "decimals").and_then(|d| d.as_int()).unwrap_or(2).max(0) as usize;
    (currency, decimals)
}

pub fn f_money_text(v: &Value, args: &[Value], kwargs: &[(String, Value)]) -> Result<Value> {
    let (currency, decimals) = filter_args(args, kwargs);
    Ok(Value::str(money_text(v, &currency, decimals)?))
}

/// An amount in the page: marked as sensitive, so the "hide sensitive data" button can blur it.
pub fn f_money(v: &Value, args: &[Value], kwargs: &[(String, Value)]) -> Result<Value> {
    let (currency, decimals) = filter_args(args, kwargs);
    Ok(Value::markup(format!(r#"<span class="sensitive">{}</span>"#, py::html_escape(&money_text(v, &currency, decimals)?))))
}

pub fn f_qty(v: &Value, _: &[Value], _: &[(String, Value)]) -> Result<Value> {
    if v.is_none() {
        return Ok(Value::markup("—"));
    }
    let d = to_decimal(v)?;
    let text = d.format_fixed(8, false, true);
    let text = text.trim_end_matches('0').trim_end_matches('.');
    Ok(Value::markup(format!(r#"<span class="sensitive">{}</span>"#, py::html_escape(&text.replace(',', " ").replace('.', ",")))))
}

pub fn frdate(v: &Value) -> String {
    if !v.truthy() {
        return "—".into();
    }
    match v {
        Value::DateTime(d) => d.format("%d/%m/%Y %H:%M").to_string(),
        Value::Date(d) => d.format("%d/%m/%Y").to_string(),
        Value::Str(s) | Value::Markup(s) => {
            let c: Vec<char> = s.chars().collect();
            let digit = |i: usize| py::is_decimal_char(c[i]);
            if c.len() == 10 && c[4] == '-' && c[7] == '-' && [0, 1, 2, 3, 5, 6, 8, 9].iter().all(|&i| digit(i)) {
                let part = |a: usize, b: usize| c[a..b].iter().collect::<String>();
                format!("{}/{}/{}", part(8, 10), part(5, 7), part(0, 4))
            } else {
                s.to_string()
            }
        }
        other => other.to_str(),
    }
}

pub fn f_frdate(v: &Value, _: &[Value], _: &[(String, Value)]) -> Result<Value> {
    Ok(Value::str(frdate(v)))
}

/// An IBAN in groups of four characters, the way banks print it.
pub fn iban(v: &Value) -> String {
    let text = if v.truthy() { v.to_str() } else { String::new() };
    let compact: String = py::split_ws(&text).concat();
    let chars: Vec<char> = compact.chars().collect();
    chars.chunks(4).map(|c| c.iter().collect::<String>()).collect::<Vec<_>>().join(" ")
}

pub fn f_iban(v: &Value, _: &[Value], _: &[(String, Value)]) -> Result<Value> {
    Ok(Value::str(iban(v)))
}

pub fn f_pct(v: &Value, _: &[Value], _: &[(String, Value)]) -> Result<Value> {
    if v.is_none() {
        return Ok(Value::str("—"));
    }
    let d = to_decimal(v)?;
    Ok(Value::str(format!("{} %", d.format_fixed(1, true, false)).replace('.', ",")))
}

/// What one sends to be paid: holder, IBAN, BIC and RIB, one per line.
pub fn identity_text(account: &Value) -> Result<Value> {
    let get = |k: &str| super::jinja::getattr(account, k);
    let mut lines = vec![];
    let holder = get("holder_name")?;
    if holder.truthy() {
        lines.push(format!("Titulaire : {}", holder.to_str()));
    }
    let ib = get("iban")?;
    if ib.truthy() {
        lines.push(format!("IBAN : {}", iban(&ib)));
    }
    let bic = get("bic")?;
    if bic.truthy() {
        lines.push(format!("BIC : {}", bic.to_str()));
    }
    let rib = get("rib")?;
    if rib.truthy() {
        lines.push(format!("RIB : {}", rib.to_str()));
    }
    Ok(Value::str(lines.join("\n")))
}

const RELIABILITY: [(&str, &str); 4] = [("high", "élevée"), ("medium", "moyenne"), ("low", "faible"), ("none", "non lu")];

fn reliability_label(code: &str) -> &'static str {
    RELIABILITY.iter().find(|(c, _)| *c == code).map(|(_, l)| *l).unwrap_or("")
}

/// How far to trust what was read from a document: the weakest statement decides.
pub fn reliability(doc: &Document) -> Value {
    let result = doc.parse_result.clone().filter(|r| !r.is_null()).unwrap_or(serde_json::json!({}));
    let statements: Vec<serde_json::Value> = result.get("statements").filter(|s| crate::models::truthy(s)).and_then(|s| s.as_array().cloned()).unwrap_or_default();
    if ["uploaded", "needs_choice", "failed"].contains(&doc.status.as_str()) || statements.is_empty() {
        return Value::sdict(vec![("code", "none".into()), ("label", "non lu".into()), ("detail", "Pas encore lu : ouvre-le pour choisir la lecture".into())]);
    }
    let order = ["low", "medium", "high"];
    let mut code = "high";
    for st in &statements {
        let level = st.get("confidence").and_then(|c| c.as_str()).filter(|c| order.contains(c)).unwrap_or("medium");
        if order.iter().position(|o| *o == level) < order.iter().position(|o| *o == code) {
            code = order[order.iter().position(|o| *o == level).unwrap()];
        }
    }
    let warnings: usize = statements.iter().map(|st| st.get("warnings").and_then(|w| w.as_array()).map(|w| w.len()).unwrap_or(0)).sum();
    let mut detail = match code {
        "high" => "Contrôles réussis (soldes et totaux qui concordent)",
        "medium" => "Lecture plausible mais pas entièrement recoupée",
        _ => "Lecture douteuse : à vérifier ligne par ligne",
    }
    .to_string();
    if warnings > 0 {
        detail += &format!(" · {warnings} avertissement(s)");
    }
    let decision = result.get("decision").cloned().unwrap_or(serde_json::Value::Null);
    if doc.status == "parsed" && !(decision.is_null() || decision.as_str() == Some("auto")) {
        detail += " · type de document à confirmer";
    }
    Value::sdict(vec![("code", code.into()), ("label", reliability_label(code).into()), ("detail", detail.into())])
}

// ------------------------------------------------------------------------------------------ helpers

/// The fees recaps of an account, each with what the imported statements say for the same period.
pub fn fee_summaries(c: &Connection, account: &Account) -> AppResult<Vec<Value>> {
    let mut rows = vec![];
    for summary in FeeSummary::query(c, "WHERE account_id = ? ORDER BY period_start DESC", [account.id])? {
        let spent = Transaction::query(
            c,
            "WHERE account_id = ? AND kind = 'fee' AND date >= ? AND date <= ?",
            rusqlite::params![account.id, summary.period_start.format("%Y-%m-%d").to_string(), summary.period_end.format("%Y-%m-%d").to_string()],
        )?;
        let seen = spent.iter().fold(PyDec::int(0), |a, t| a.add(&PyDec::from(&t.amount).neg()));
        let complete = crate::services::coverage::covers(c, account.id, summary.period_start, summary.period_end)?;
        rows.push(Value::sdict(vec![
            ("summary", fee_summary(&summary)),
            ("seen", Value::pydec(seen)),
            ("seen_count", spent.len().into()),
            ("complete", complete.into()),
        ]));
    }
    Ok(rows)
}

pub use crate::services::naming::{document_day, document_group, DOC_KINDS};

/// The institution a document is filed under: its account's, else the one its reading names.
pub fn institution_of(d: &Document, accounts: &HashMap<i64, Account>) -> String {
    d.account_id.and_then(|i| accounts.get(&i)).and_then(|a| a.institution.clone()).filter(|i| !i.is_empty()).unwrap_or_else(|| document_group(d).0)
}

/// The names a document is about, as a line: its account's holders, else the ones its own reading names.
pub fn document_names(d: &Document, account: Option<&Account>) -> String {
    if let Some(a) = account {
        if let Some(h) = a.holder_name.as_deref().filter(|h| !h.is_empty()) {
            return h.to_string();
        }
    }
    parse_statements_json(d).iter().find_map(|st| st.get("holder_name").and_then(|h| h.as_str()).filter(|h| !h.is_empty()).map(|h| h.to_string())).unwrap_or_default()
}

fn parse_statements_json(d: &Document) -> Vec<serde_json::Value> {
    d.parse_result.as_ref().and_then(|r| r.get("statements")).and_then(|s| s.as_array()).cloned().unwrap_or_default()
}

/// Documents grouped for browsing: institution, then account, then type of document, then year, newest first. A
/// document that belongs to no account (a letter, a document still waiting) goes in "Sans compte", last.
pub fn document_tree(documents: &[Document], accounts: &HashMap<i64, Account>) -> Value {
    struct Kind<'a> {
        label: String,
        docs: Vec<&'a Document>,
    }
    struct Acc<'a> {
        id: Option<i64>,
        label: String,
        subtitle: String,
        holder: String,
        kinds: Vec<Kind<'a>>,
    }
    let mut banks: Vec<(String, Vec<Acc>)> = vec![];
    for d in documents {
        let account = d.account_id.and_then(|i| accounts.get(&i));
        let (institution, kind) = (institution_of(d, accounts), document_group(d).1);
        let bank = match banks.iter().position(|(b, _)| *b == institution) {
            Some(i) => i,
            None => {
                banks.push((institution, vec![]));
                banks.len() - 1
            }
        };
        let (id, label, subtitle, holder) = match account {
            Some(a) => (Some(a.id), a.name.clone(), type_label_of(a), a.holder_name.clone().unwrap_or_default()),
            None => (None, "Sans compte".to_string(), String::new(), String::new()),
        };
        let accs = &mut banks[bank].1;
        let slot = match accs.iter().position(|x| x.id == id) {
            Some(i) => i,
            None => {
                accs.push(Acc { id, label, subtitle, holder, kinds: vec![] });
                accs.len() - 1
            }
        };
        let kinds = &mut accs[slot].kinds;
        match kinds.iter_mut().find(|k| k.label == kind) {
            Some(k) => k.docs.push(d),
            None => kinds.push(Kind { label: kind, docs: vec![d] }),
        }
    }
    let plain = |t: &str| py::lower(&crate::adapters::common::strip_accents(t));
    banks.sort_by_key(|(b, _)| (b == "Autres documents", plain(b)));
    let mut tree = vec![];
    for (institution, mut accs) in banks {
        accs.sort_by_key(|a| (a.id.is_none(), plain(&a.label), a.id));
        let (mut bank_count, mut bank_imported) = (0usize, 0usize);
        let mut account_values = vec![];
        for mut acc in accs {
            acc.kinds.sort_by_key(|k| (k.label == "Non reconnu", plain(&k.label)));
            let (mut acc_count, mut acc_imported) = (0usize, 0usize);
            let mut kind_values = vec![];
            for mut kind in acc.kinds {
                kind.docs.sort_by(|a, b| (document_day(b), b.id).cmp(&(document_day(a), a.id)));
                let mut years: Vec<(i32, Vec<Value>)> = vec![];
                for d in &kind.docs {
                    let day = document_day(d);
                    let year = chrono::Datelike::year(&day);
                    if years.last().map(|(y, _)| *y != year).unwrap_or(true) {
                        years.push((year, vec![]));
                    }
                    let names = document_names(d, d.account_id.and_then(|i| accounts.get(&i)));
                    years.last_mut().unwrap().1.push(Value::tuple(vec![document(d), day.into(), names.into()]));
                }
                let imported = kind.docs.iter().filter(|d| d.status == "imported").count();
                acc_count += kind.docs.len();
                acc_imported += imported;
                kind_values.push(Value::sdict(vec![
                    ("label", kind.label.clone().into()),
                    ("count", kind.docs.len().into()),
                    ("imported", imported.into()),
                    ("years", Value::list(years.into_iter().map(|(y, ds)| Value::tuple(vec![(y as i64).into(), Value::list(ds)])).collect())),
                ]));
            }
            bank_count += acc_count;
            bank_imported += acc_imported;
            account_values.push(Value::sdict(vec![
                ("id", acc.id.map(Value::from).unwrap_or(Value::None)),
                ("label", acc.label.into()),
                ("subtitle", acc.subtitle.into()),
                ("holder", acc.holder.into()),
                ("count", acc_count.into()),
                ("imported", acc_imported.into()),
                ("kinds", Value::list(kind_values)),
            ]));
        }
        tree.push(Value::sdict(vec![
            ("label", institution.into()),
            ("count", bank_count.into()),
            ("imported", bank_imported.into()),
            ("accounts", Value::list(account_values)),
        ]));
    }
    Value::list(tree)
}

/// Python's `zipfile` writing (filename, content) pairs with ZIP_DEFLATED; two files with the same name keep both
/// ("releve (2).pdf").
pub fn zip_documents(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    use std::io::Write;
    let now = chrono::Local::now().naive_local();
    let (date, time) = {
        use chrono::{Datelike, Timelike};
        let year = (now.year() - 1980).max(0) as u16;
        (((year << 9) | ((now.month() as u16) << 5) | now.day() as u16), (((now.hour() as u16) << 11) | ((now.minute() as u16) << 5) | (now.second() as u16 / 2)))
    };
    let mut out: Vec<u8> = vec![];
    let mut central: Vec<u8> = vec![];
    let mut used: Vec<String> = vec![];
    let mut count = 0u16;
    for (filename, data) in files {
        let name = filename.replace(['/', '\\'], "_");
        let name = if name.is_empty() { "document".to_string() } else { name };
        let (stem, dot, ext) = match name.rfind('.') {
            Some(p) => (name[..p].to_string(), true, name[p + 1..].to_string()),
            None => (String::new(), false, name.clone()),
        };
        let mut candidate = name.clone();
        let mut n = 1;
        while used.contains(&py::lower(&candidate)) {
            n += 1;
            candidate = if dot { format!("{stem} ({n}).{ext}") } else { format!("{name} ({n})") };
        }
        used.push(py::lower(&candidate));
        let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(6));
        enc.write_all(data).unwrap();
        let compressed = enc.finish().unwrap();
        let crc = crc32(data);
        let fname = candidate.as_bytes();
        let flags: u16 = if candidate.is_ascii() { 0 } else { 0x800 };
        let offset = out.len() as u32;
        let mut local = vec![];
        local.extend_from_slice(&0x04034b50u32.to_le_bytes());
        local.extend_from_slice(&20u16.to_le_bytes());
        local.extend_from_slice(&flags.to_le_bytes());
        local.extend_from_slice(&8u16.to_le_bytes());
        local.extend_from_slice(&time.to_le_bytes());
        local.extend_from_slice(&date.to_le_bytes());
        local.extend_from_slice(&crc.to_le_bytes());
        local.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        local.extend_from_slice(&(data.len() as u32).to_le_bytes());
        local.extend_from_slice(&(fname.len() as u16).to_le_bytes());
        local.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&local);
        out.extend_from_slice(fname);
        out.extend_from_slice(&compressed);
        central.extend_from_slice(&0x02014b50u32.to_le_bytes());
        central.extend_from_slice(&((3u16 << 8) | 20).to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&flags.to_le_bytes());
        central.extend_from_slice(&8u16.to_le_bytes());
        central.extend_from_slice(&time.to_le_bytes());
        central.extend_from_slice(&date.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(fname.len() as u16).to_le_bytes());
        central.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
        central.extend_from_slice(&(0o600u32 << 16).to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(fname);
        count += 1;
    }
    let cd_offset = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x06054b50u32.to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = flate2::Crc::new();
    c.update(data);
    c.sum()
}

pub fn str_map(pairs: &[(&str, &str)]) -> Value {
    Value::dict(pairs.iter().map(|(k, v)| (Value::str(k), Value::str(v))).collect())
}

pub fn int_map<V: Into<Value> + Clone>(m: &HashMap<i64, V>) -> Value {
    Value::dict(m.iter().map(|(k, v)| (Value::Int(*k), v.clone().into())).collect())
}
