//! The tables, with the same columns, types and defaults as the Python models (SQLAlchemy), stored the same way:
//! decimals as text, dates as "YYYY-MM-DD", timestamps as "YYYY-MM-DD HH:MM:SS.ffffff", JSON as Python's
//! `json.dumps`, booleans as 0/1.

use chrono::{NaiveDate, NaiveDateTime};
use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::{params_from_iter, Connection, Row};
use rust_decimal::Decimal;
use serde_json::Value;

use crate::py;

pub type Json = Option<Value>;

pub fn utcnow() -> NaiveDateTime {
    chrono::Utc::now().naive_utc()
}

pub fn fmt_datetime(d: &NaiveDateTime) -> String {
    d.format("%Y-%m-%d %H:%M:%S%.6f").to_string()
}

pub fn parse_datetime(s: &str) -> Option<NaiveDateTime> {
    for f in ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M"] {
        if let Ok(d) = NaiveDateTime::parse_from_str(s, f) {
            return Some(d);
        }
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok().map(|d| d.and_hms_opt(0, 0, 0).unwrap())
}

pub fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.get(..10).unwrap_or(s), "%Y-%m-%d").ok()
}

/// A value that goes in and out of a column.
pub trait Col: Sized {
    fn to_sql(&self) -> SqlValue;
    fn from_sql(v: ValueRef) -> Self;
    /// SQLAlchemy's `compare_values`: an attribute set to an equal value is no change (no UPDATE for it).
    fn same(&self, other: &Self) -> bool {
        self.to_sql() == other.to_sql()
    }
}

fn text_of(v: ValueRef) -> Option<String> {
    match v {
        ValueRef::Null => None,
        ValueRef::Text(t) => Some(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Integer(i) => Some(i.to_string()),
        ValueRef::Real(f) => Some(py::float_repr(f)),
        ValueRef::Blob(b) => Some(String::from_utf8_lossy(b).into_owned()),
    }
}

impl Col for i64 {
    fn to_sql(&self) -> SqlValue {
        SqlValue::Integer(*self)
    }
    fn from_sql(v: ValueRef) -> Self {
        match v {
            ValueRef::Integer(i) => i,
            ValueRef::Real(f) => f as i64,
            other => text_of(other).and_then(|t| t.parse().ok()).unwrap_or(0),
        }
    }
}

impl Col for Option<i64> {
    fn to_sql(&self) -> SqlValue {
        self.map(SqlValue::Integer).unwrap_or(SqlValue::Null)
    }
    fn from_sql(v: ValueRef) -> Self {
        match v {
            ValueRef::Null => None,
            other => Some(i64::from_sql(other)),
        }
    }
}

impl Col for String {
    fn to_sql(&self) -> SqlValue {
        SqlValue::Text(self.clone())
    }
    fn from_sql(v: ValueRef) -> Self {
        text_of(v).unwrap_or_default()
    }
}

impl Col for Option<String> {
    fn to_sql(&self) -> SqlValue {
        self.clone().map(SqlValue::Text).unwrap_or(SqlValue::Null)
    }
    fn from_sql(v: ValueRef) -> Self {
        text_of(v)
    }
}

impl Col for bool {
    fn to_sql(&self) -> SqlValue {
        SqlValue::Integer(*self as i64)
    }
    fn from_sql(v: ValueRef) -> Self {
        i64::from_sql(v) != 0
    }
}

impl Col for Decimal {
    fn to_sql(&self) -> SqlValue {
        SqlValue::Text(py::dec_str(self))
    }
    fn from_sql(v: ValueRef) -> Self {
        text_of(v).and_then(|t| py::dec_parse(&t)).unwrap_or_default()
    }
    fn same(&self, other: &Self) -> bool {
        self == other
    }
}

impl Col for Option<Decimal> {
    fn to_sql(&self) -> SqlValue {
        self.map(|d| SqlValue::Text(py::dec_str(&d))).unwrap_or(SqlValue::Null)
    }
    fn from_sql(v: ValueRef) -> Self {
        text_of(v).and_then(|t| py::dec_parse(&t))
    }
    fn same(&self, other: &Self) -> bool {
        self == other
    }
}

impl Col for NaiveDate {
    fn to_sql(&self) -> SqlValue {
        SqlValue::Text(self.format("%Y-%m-%d").to_string())
    }
    fn from_sql(v: ValueRef) -> Self {
        text_of(v).and_then(|t| parse_date(&t)).unwrap_or_default()
    }
}

impl Col for Option<NaiveDate> {
    fn to_sql(&self) -> SqlValue {
        self.map(|d| d.to_sql()).unwrap_or(SqlValue::Null)
    }
    fn from_sql(v: ValueRef) -> Self {
        text_of(v).and_then(|t| parse_date(&t))
    }
}

impl Col for NaiveDateTime {
    fn to_sql(&self) -> SqlValue {
        SqlValue::Text(fmt_datetime(self))
    }
    fn from_sql(v: ValueRef) -> Self {
        text_of(v).and_then(|t| parse_datetime(&t)).unwrap_or_default()
    }
}

impl Col for Option<NaiveDateTime> {
    fn to_sql(&self) -> SqlValue {
        self.map(|d| d.to_sql()).unwrap_or(SqlValue::Null)
    }
    fn from_sql(v: ValueRef) -> Self {
        text_of(v).and_then(|t| parse_datetime(&t))
    }
}

/// JSON columns have three states, as with SQLAlchemy: never set (`None`, SQL NULL: the column was left out of the
/// INSERT), set to Python None (`Some(Value::Null)`, stored as the JSON text "null"), or a value.
impl Col for Json {
    fn to_sql(&self) -> SqlValue {
        match self {
            None => SqlValue::Null,
            Some(v) => SqlValue::Text(py::json_dumps(v)),
        }
    }
    fn from_sql(v: ValueRef) -> Self {
        let t = text_of(v)?;
        serde_json::from_str::<Value>(&t).ok()
    }
    /// Python reads both NULL and "null" as None: setting None over either is no change.
    fn same(&self, other: &Self) -> bool {
        let norm = |v: &Self| v.clone().filter(|x| !x.is_null());
        norm(self) == norm(other)
    }
}

/// `table!(Name, "table", [touch], { field: Type = default, ... })`: a row struct with an integer `id`, its
/// defaults, and select / insert / update / delete helpers. `touch` refreshes `updated_at` on every update
/// (SQLAlchemy's `onupdate=utcnow`).
macro_rules! table {
    ($name:ident, $table:literal, $touch:literal, { $($field:ident : $ty:ty = $default:expr),* $(,)? }) => {
        #[derive(Clone, Debug)]
        pub struct $name {
            pub id: i64,
            $(pub $field: $ty,)*
        }

        impl Default for $name {
            fn default() -> Self {
                $name { id: 0, $($field: $default,)* }
            }
        }

        #[allow(dead_code)]
        impl $name {
            pub const TABLE: &'static str = $table;
            pub const COLUMNS: &'static [&'static str] = &[$(stringify!($field)),*];

            /// The columns as template values (the attributes of the SQLAlchemy row).
            pub fn fields(&self) -> Vec<(&'static str, crate::web::jinja::Value)> {
                vec![("id", self.id.into()), $((stringify!($field), self.$field.clone().into())),*]
            }

            pub fn select_sql() -> String {
                let cols: Vec<String> = std::iter::once("id".to_string()).chain(Self::COLUMNS.iter().map(|c| format!("\"{}\"", c))).collect();
                format!("SELECT {} FROM {}", cols.join(", "), $table)
            }

            pub fn from_row(r: &Row) -> rusqlite::Result<Self> {
                let mut _i = 1usize;
                Ok($name {
                    id: Col::from_sql(r.get_ref(0)?),
                    $($field: { let v = Col::from_sql(r.get_ref(_i)?); _i += 1; v },)*
                })
            }

            fn values(&self) -> Vec<SqlValue> {
                vec![$(Col::to_sql(&self.$field)),*]
            }

            pub fn insert(&mut self, c: &Connection) -> rusqlite::Result<i64> {
                let cols: Vec<String> = Self::COLUMNS.iter().map(|c| format!("\"{}\"", c)).collect();
                let marks: Vec<&str> = Self::COLUMNS.iter().map(|_| "?").collect();
                let mut vals = self.values();
                let sql = if self.id != 0 {
                    vals.insert(0, SqlValue::Integer(self.id));
                    format!("INSERT INTO {} (id, {}) VALUES (?, {})", $table, cols.join(", "), marks.join(", "))
                } else {
                    format!("INSERT INTO {} ({}) VALUES ({})", $table, cols.join(", "), marks.join(", "))
                };
                c.execute(&sql, params_from_iter(vals))?;
                if self.id == 0 {
                    self.id = c.last_insert_rowid();
                }
                Ok(self.id)
            }

            pub fn update(&mut self, c: &Connection) -> rusqlite::Result<()> {
                if $touch {
                    self.touch();
                }
                let sets: Vec<String> = Self::COLUMNS.iter().map(|c| format!("\"{}\" = ?", c)).collect();
                let mut vals = self.values();
                vals.push(SqlValue::Integer(self.id));
                c.execute(&format!("UPDATE {} SET {} WHERE id = ?", $table, sets.join(", ")), params_from_iter(vals))?;
                Ok(())
            }

            /// Write the columns that changed since `before` (SQLAlchemy's flush): nothing at all when none did,
            /// and only then is `updated_at` refreshed. Returns whether a row was written.
            pub fn save(&mut self, c: &Connection, before: &Self) -> rusqlite::Result<bool> {
                let mut sets: Vec<String> = vec![];
                let mut vals: Vec<SqlValue> = vec![];
                $(
                    if !Col::same(&self.$field, &before.$field) {
                        sets.push(format!("\"{}\" = ?", stringify!($field)));
                        vals.push(Col::to_sql(&self.$field));
                    }
                )*
                if sets.is_empty() {
                    return Ok(false);
                }
                if $touch {
                    self.touch();
                    if let Some(i) = Self::COLUMNS.iter().position(|c| *c == "updated_at") {
                        let v = self.values().swap_remove(i);
                        let name = "\"updated_at\" = ?".to_string();
                        match sets.iter().position(|s| *s == name) {
                            Some(p) => vals[p] = v,
                            None => {
                                sets.push(name);
                                vals.push(v);
                            }
                        }
                    }
                }
                vals.push(SqlValue::Integer(self.id));
                c.execute(&format!("UPDATE {} SET {} WHERE id = ?", $table, sets.join(", ")), params_from_iter(vals))?;
                Ok(true)
            }

            pub fn get(c: &Connection, id: i64) -> rusqlite::Result<Option<Self>> {
                let mut st = c.prepare_cached(&format!("{} WHERE id = ?", Self::select_sql()))?;
                let mut rows = st.query([id])?;
                match rows.next()? {
                    Some(r) => Ok(Some(Self::from_row(r)?)),
                    None => Ok(None),
                }
            }

            /// Rows matching `tail` (a WHERE / ORDER BY clause, may be empty).
            pub fn query(c: &Connection, tail: &str, params: impl rusqlite::Params) -> rusqlite::Result<Vec<Self>> {
                let sql = if tail.is_empty() { Self::select_sql() } else { format!("{} {}", Self::select_sql(), tail) };
                let mut st = c.prepare_cached(&sql)?;
                let rows = st.query_map(params, |r| Self::from_row(r))?;
                rows.collect()
            }

            pub fn query_one(c: &Connection, tail: &str, params: impl rusqlite::Params) -> rusqlite::Result<Option<Self>> {
                Ok(Self::query(c, tail, params)?.into_iter().next())
            }

            pub fn delete(c: &Connection, id: i64) -> rusqlite::Result<()> {
                c.execute(&format!("DELETE FROM {} WHERE id = ?", $table), [id])?;
                Ok(())
            }
        }
    };
}

table!(User, "users", false, {
    email: String = String::new(),
    name: Option<String> = None,
    is_admin: bool = false,
    created_at: NaiveDateTime = utcnow(),
    last_login: Option<NaiveDateTime> = None,
});
impl User {
    fn touch(&mut self) {}
    pub fn label(&self) -> String {
        self.name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| self.email.clone())
    }
}

table!(AccountShare, "account_shares", false, {
    account_id: i64 = 0,
    user_id: i64 = 0,
    can_edit: bool = false,
    created_at: NaiveDateTime = utcnow(),
});
impl AccountShare {
    fn touch(&mut self) {}
}

table!(Account, "accounts", true, {
    name: String = String::new(),
    owner_id: Option<i64> = None,
    institution: Option<String> = None,
    account_type: String = "checking".into(),
    currency: String = "EUR".into(),
    balance: Decimal = Decimal::ZERO,
    balance_date: Option<NaiveDateTime> = None,
    iban: Option<String> = None,
    bic: Option<String> = None,
    bank_code: Option<String> = None,
    branch_code: Option<String> = None,
    account_number: Option<String> = None,
    rib_key: Option<String> = None,
    holder_name: Option<String> = None,
    holder_alias: Option<String> = None,
    holder_address: Option<String> = None,
    // the people behind the account (civility, names, birth names, first names) as JSON; see services::holders
    holders: Json = None,
    // "gross" or "net": what the interest rate typed on the account means (see services::rates)
    interest_basis: Option<String> = None,
    interest_rate: Option<Decimal> = None,
    archived: bool = false,
    notes: Option<String> = None,
    updated_at: NaiveDateTime = utcnow(),
    // "import:<UTC time>" when an import created the account (it goes away with its last document), empty for accounts typed by hand
    origin: Option<String> = None,
});
impl Account {
    fn touch(&mut self) {
        self.updated_at = utcnow();
    }
    pub fn rib(&self) -> Option<String> {
        match (&self.bank_code, &self.branch_code, &self.account_number, &self.rib_key) {
            (Some(a), Some(b), Some(c), Some(d)) if !a.is_empty() && !b.is_empty() && !c.is_empty() && !d.is_empty() => {
                Some(format!("{a} {b} {c} {d}"))
            }
            _ => None,
        }
    }
    pub fn type_label(&self) -> String {
        account_type(&self.account_type).map(|t| t.1.to_string()).unwrap_or_else(|| self.account_type.clone())
    }
    pub fn category(&self) -> &'static str {
        account_type(&self.account_type).map(|t| t.2).unwrap_or("autres")
    }
}

table!(Holding, "holdings", true, {
    account_id: i64 = 0,
    symbol: Option<String> = None,
    isin: Option<String> = None,
    name: Option<String> = None,
    asset_class: String = "other".into(),
    quantity: Decimal = Decimal::ZERO,
    unit_price: Option<Decimal> = None,
    value: Option<Decimal> = None,
    value_currency: Option<String> = None,
    cost_basis: Option<Decimal> = None,
    extra: Json = None,
    manual: bool = false,
    updated_at: NaiveDateTime = utcnow(),
});
impl Holding {
    fn touch(&mut self) {
        self.updated_at = utcnow();
    }
}

table!(Document, "documents", false, {
    filename: String = String::new(),
    // the name the file had when it was dropped; `filename` becomes the standard name once it is imported
    original_filename: Option<String> = None,
    owner_id: Option<i64> = None,
    mime: Option<String> = None,
    size: i64 = 0,
    sha256: String = String::new(),
    blob_name: String = String::new(),
    kind: String = "statement".into(),
    account_id: Option<i64> = None,
    status: String = "uploaded".into(),
    duplicate_of: Option<i64> = None,
    adapter_id: Option<String> = None,
    parse_result: Json = None,
    uploaded_at: NaiveDateTime = utcnow(),
});
impl Document {
    fn touch(&mut self) {}
}

table!(Transaction, "transactions", false, {
    account_id: i64 = 0,
    fingerprint: String = String::new(),
    date: NaiveDate = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap(),
    label: String = String::new(),
    amount: Decimal = Decimal::ZERO,
    currency: String = "EUR".into(),
    category: Option<String> = None,
    source: String = "import".into(),
    document_id: Option<i64> = None,
    created_at: NaiveDateTime = utcnow(),
    value_date: Option<NaiveDate> = None,
    kind: Option<String> = None,
    counterparty_name: Option<String> = None,
    counterparty_iban: Option<String> = None,
    counterparty_id: Option<i64> = None,
    reference: Option<String> = None,
    memo: Option<String> = None,
    raw_label: Option<String> = None,
    details: Json = None,
});
impl Transaction {
    fn touch(&mut self) {}
    pub fn internal(&self) -> bool {
        self.details.as_ref().and_then(|d| d.get("internal")).map(truthy).unwrap_or(false)
    }
}

/// Python truthiness of a JSON value.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

table!(Statement, "statements", false, {
    account_id: i64 = 0,
    document_id: Option<i64> = None,
    period_start: Option<NaiveDate> = None,
    period_end: Option<NaiveDate> = None,
    opening_balance: Option<Decimal> = None,
    closing_balance: Option<Decimal> = None,
    tx_count: i64 = 0,
    added_count: i64 = 0,
    imported_at: NaiveDateTime = utcnow(),
});
impl Statement {
    fn touch(&mut self) {}
}

table!(FeeSummary, "fee_summaries", false, {
    account_id: i64 = 0,
    document_id: Option<i64> = None,
    period_start: NaiveDate = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap(),
    period_end: NaiveDate = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap(),
    fees_total: Decimal = Decimal::ZERO,
    interest_paid: Option<Decimal> = None,
    interest_earned: Option<Decimal> = None,
    services: Json = None,
    imported_at: NaiveDateTime = utcnow(),
});
impl FeeSummary {
    fn touch(&mut self) {}
}

table!(Loan, "loans", true, {
    account_id: i64 = 0,
    reference: Option<String> = None,
    project_reference: Option<String> = None,
    product: Option<String> = None,
    borrower: Option<String> = None,
    purpose: Option<String> = None,
    principal: Option<Decimal> = None,
    duration_months: Option<i64> = None,
    rate: Option<Decimal> = None,
    taeg: Option<Decimal> = None,
    interest_total: Option<Decimal> = None,
    insurance_mandatory: Option<Decimal> = None,
    insurance_optional: Option<Decimal> = None,
    guarantee_fees: Option<Decimal> = None,
    total_cost: Option<Decimal> = None,
    offer_date: Option<NaiveDate> = None,
    first_due_date: Option<NaiveDate> = None,
    end_date: Option<NaiveDate> = None,
    offer_document_id: Option<i64> = None,
    schedule_document_id: Option<i64> = None,
    updated_at: NaiveDateTime = utcnow(),
});
impl Loan {
    fn touch(&mut self) {
        self.updated_at = utcnow();
    }
}

table!(LoanInstallment, "loan_installments", false, {
    loan_id: i64 = 0,
    document_id: Option<i64> = None,
    source: String = String::new(),
    number: Option<i64> = None,
    due_date: Option<NaiveDate> = None,
    capital: Decimal = Decimal::ZERO,
    interest: Decimal = Decimal::ZERO,
    deferred_interest: Decimal = Decimal::ZERO,
    insurance: Decimal = Decimal::ZERO,
    total: Decimal = Decimal::ZERO,
    remaining: Decimal = Decimal::ZERO,
});
impl LoanInstallment {
    fn touch(&mut self) {}
}

table!(LoanEvent, "loan_events", false, {
    loan_id: i64 = 0,
    document_id: Option<i64> = None,
    kind: String = "prepayment".into(),
    on_date: NaiveDate = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap(),
    issued_on: Option<NaiveDate> = None,
    capital_before: Decimal = Decimal::ZERO,
    capital_after: Decimal = Decimal::ZERO,
    last_due_date: Option<NaiveDate> = None,
    last_payment: Option<Decimal> = None,
    first_due_date: Option<NaiveDate> = None,
    first_payment: Option<Decimal> = None,
    end_before: Option<NaiveDate> = None,
    end_after: Option<NaiveDate> = None,
});
impl LoanEvent {
    fn touch(&mut self) {}
}

table!(CategoryRule, "category_rules", false, {
    owner_id: Option<i64> = None,
    key: String = String::new(),
    category: String = String::new(),
    created_at: NaiveDateTime = utcnow(),
});
impl CategoryRule {
    fn touch(&mut self) {}
}

table!(GapAck, "gap_acks", false, {
    account_id: i64 = 0,
    period_start: NaiveDate = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap(),
    period_end: NaiveDate = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap(),
    note: Option<String> = None,
    created_at: NaiveDateTime = utcnow(),
});
impl GapAck {
    fn touch(&mut self) {}
}

// A person's card: who is behind one or several accounts (a joint account links two cards). Names are lists because
// documents ask for one name or both, the birth name, several first names.
table!(PersonCard, "people", false, {
    owner_id: Option<i64> = None,
    civility: String = String::new(),
    last_names: Json = None,
    birth_names: Json = None,
    first_names: Json = None,
    notes: Option<String> = None,
    created_at: NaiveDateTime = utcnow(),
});
impl PersonCard {
    fn touch(&mut self) {}
}

table!(Counterparty, "counterparties", false, {
    name: String = String::new(),
    kind: String = "person".into(),
    phone: Option<String> = None,
    email: Option<String> = None,
    notes: Option<String> = None,
    created_at: NaiveDateTime = utcnow(),
});
impl Counterparty {
    fn touch(&mut self) {}
}

table!(CounterpartyIdentifier, "counterparty_identifiers", false, {
    counterparty_id: i64 = 0,
    kind: String = String::new(),
    value: String = String::new(),
    label: Option<String> = None,
});
impl CounterpartyIdentifier {
    fn touch(&mut self) {}
}

table!(Snapshot, "snapshots", false, {
    account_id: i64 = 0,
    day: NaiveDate = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap(),
    value: Decimal = Decimal::ZERO,
});
impl Snapshot {
    fn touch(&mut self) {}
}

// price_cache and settings are keyed by text.

pub fn price_cache_get(c: &Connection, key: &str) -> rusqlite::Result<Option<(String, NaiveDateTime)>> {
    let mut st = c.prepare_cached("SELECT value, fetched_at FROM price_cache WHERE \"key\" = ?")?;
    let mut rows = st.query([key])?;
    Ok(match rows.next()? {
        Some(r) => Some((String::from_sql(r.get_ref(0)?), NaiveDateTime::from_sql(r.get_ref(1)?))),
        None => None,
    })
}

pub fn price_cache_set(c: &Connection, key: &str, value: &str, fetched_at: NaiveDateTime) -> rusqlite::Result<()> {
    let n = c.execute("UPDATE price_cache SET value = ?, fetched_at = ? WHERE \"key\" = ?", rusqlite::params![value, fmt_datetime(&fetched_at), key])?;
    if n == 0 {
        c.execute("INSERT INTO price_cache (\"key\", value, fetched_at) VALUES (?, ?, ?)", rusqlite::params![key, value, fmt_datetime(&fetched_at)])?;
    }
    Ok(())
}

pub fn setting_get(c: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    let mut st = c.prepare_cached("SELECT value FROM settings WHERE \"key\" = ?")?;
    let mut rows = st.query([key])?;
    Ok(match rows.next()? {
        Some(r) => Some(String::from_sql(r.get_ref(0)?)),
        None => None,
    })
}

pub fn setting_delete(c: &Connection, key: &str) -> rusqlite::Result<()> {
    c.execute("DELETE FROM settings WHERE \"key\" = ?", [key])?;
    Ok(())
}

pub fn setting_set(c: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    let n = c.execute("UPDATE settings SET value = ? WHERE \"key\" = ?", rusqlite::params![value, key])?;
    if n == 0 {
        c.execute("INSERT INTO settings (\"key\", value) VALUES (?, ?)", rusqlite::params![key, value])?;
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------- constants

/// code -> (label, category), in the Python order.
pub static ACCOUNT_TYPES: &[(&str, &str, &str)] = &[
    ("checking", "Compte courant", "liquidites"),
    ("card", "Carte (débit différé)", "liquidites"),
    ("cash", "Espèces", "liquidites"),
    ("livret_a", "Livret A", "epargne"),
    ("ldds", "LDDS", "epargne"),
    ("lep", "LEP", "epargne"),
    ("livret_jeune", "Livret Jeune", "epargne"),
    ("cel", "CEL", "epargne"),
    ("pel", "PEL", "epargne"),
    ("savings", "Livret / épargne bancaire", "epargne"),
    ("term_deposit", "Compte à terme", "epargne"),
    ("pea", "PEA", "investissements"),
    ("cto", "Compte-titres", "investissements"),
    ("life_insurance", "Assurance-vie", "investissements"),
    ("per", "PER", "investissements"),
    ("employee_savings", "Épargne salariale (PEE/PERCO)", "investissements"),
    ("crowdfunding", "Crowdfunding / crowdlending", "investissements"),
    ("scpi", "SCPI", "immobilier"),
    ("crypto_exchange", "Plateforme crypto", "crypto"),
    ("crypto_wallet", "Wallet crypto", "crypto"),
    ("real_estate", "Bien immobilier", "immobilier"),
    ("other_asset", "Autre actif", "autres"),
    ("loan", "Crédit", "dettes"),
    ("mortgage", "Crédit immobilier", "dettes"),
];

pub fn account_type(code: &str) -> Option<&'static (&'static str, &'static str, &'static str)> {
    ACCOUNT_TYPES.iter().find(|t| t.0 == code)
}

pub static CATEGORIES: &[(&str, &str)] = &[
    ("liquidites", "Liquidités"),
    ("epargne", "Épargne"),
    ("investissements", "Investissements"),
    ("crypto", "Crypto"),
    ("immobilier", "Immobilier"),
    ("autres", "Autres"),
    ("dettes", "Dettes"),
];

pub fn category_label(code: &str) -> &'static str {
    CATEGORIES.iter().find(|c| c.0 == code).map(|c| c.1).unwrap_or("")
}

pub static COUNTERPARTY_KINDS: &[(&str, &str)] =
    &[("person", "Personne"), ("company", "Entreprise / organisme"), ("self", "Moi (autre compte)"), ("other", "Autre")];
