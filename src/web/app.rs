//! The web application: the routes, the guard in front of them and the page rendering of the Python version
//! (mymoney/web/app.py), with the same URLs, forms, redirects, messages and status codes.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use chrono::{Duration, NaiveDate};
use rust_decimal::Decimal;

use super::cloudflare::CloudflareAccess;
use super::forms::{self, Field, FormData};
use super::http::{http_date, Request, Response};
use super::jinja::{Environment, Value};
use super::security::{same, Sessions, WebSession, COOKIE, SECURITY_HEADERS};
use super::{charts, pwa, themes, views};
use crate::adapters::registry;
use crate::app::{MyMoney, QUARANTINE_HOURS};
use crate::config::Settings;
use crate::error::{AppError, AppResult};
use crate::models::{Account, AccountShare, Counterparty, Document, Holding, Loan, LoanEvent, LoanInstallment, Transaction, User, ACCOUNT_TYPES, CATEGORIES, COUNTERPARTY_KINDS};
use crate::py::{self, PyDec};
use crate::services::access::{self, Identity};
use crate::services::{analytics, people as people_service, categories as category_service, counterparties as contact_service, coverage as coverage_service, loans as loan_service, rates, users as user_service};

const PUBLIC_PATHS: &[&str] = &["/static/", "/themes.css", "/healthz", "/login", "/setup", "/manifest.webmanifest", "/sw.js", "/icons/"];

pub const KIND_LABELS: &[(&str, &str)] = &[
    ("card", "Carte"),
    ("transfer", "Virement"),
    ("instant_transfer", "Virement instantané"),
    ("direct_debit", "Prélèvement"),
    ("loan_payment", "Prêt"),
    ("fee", "Frais"),
    ("cash", "Retrait"),
    ("cheque", "Chèque"),
    ("interest", "Intérêts"),
];
const ANALYTICS_VIEWS: &[(&str, &str)] = &[("evolution", "Évolution"), ("depenses", "Dépenses et revenus"), ("dettes", "Dettes")];
pub const DOCUMENT_STATUS: &[(&str, &str)] = &[
    ("uploaded", "stocké"),
    ("needs_choice", "à préciser"),
    ("parsed", "à valider"),
    ("imported", "importé"),
    ("failed", "non reconnu"),
    ("duplicate", "doublon écarté"),
    ("copy", "copie"),
];

fn analytics_groups(vue: &str) -> &'static [(&'static str, &'static str)] {
    match vue {
        "evolution" => &[("total", "Total"), ("compte", "Par compte"), ("categorie", "Par catégorie")],
        _ => &[],
    }
}

// ----------------------------------------------------------------------------------------- errors

/// What stops a request: an application error (as the Python exceptions), an HTTPException, a validation error.
pub enum Fail {
    App(AppError),
    Http(u16, String),
    Invalid(Vec<serde_json::Value>),
    Internal(String),
}

impl From<AppError> for Fail {
    fn from(e: AppError) -> Fail {
        Fail::App(e)
    }
}

impl From<super::jinja::Error> for Fail {
    fn from(e: super::jinja::Error) -> Fail {
        Fail::Internal(e.0)
    }
}

type Out = Result<Response, Fail>;

fn http(status: u16, detail: &str) -> Fail {
    Fail::Http(status, detail.to_string())
}

fn not_found() -> Fail {
    http(404, "Not Found")
}

fn json_response(status: u16, value: &serde_json::Value) -> Response {
    // Starlette's JSONResponse: compact separators, non-ASCII kept
    Response::new(status, "application/json", serde_json::to_string(value).unwrap())
}

fn html(status: u16, body: String) -> Response {
    Response::new(status, "text/html; charset=utf-8", body)
}

/// Starlette's RedirectResponse: the location quoted, an empty body.
fn redirect_response(url: &str, status: u16) -> Response {
    let mut r = Response::new(status, "", Vec::new());
    r.add("location", py::quote(url, ":/%#?=@[]!$&'()*+,;"));
    r
}

/// `redirect(url, msg)`: 303, with the message in the query (before any #anchor).
fn redirect(url: &str, msg: Option<&str>) -> Response {
    let mut target = url.to_string();
    if let Some(m) = msg.filter(|m| !m.is_empty()) {
        let (base, fragment) = match url.split_once('#') {
            Some((b, f)) => (b.to_string(), f.to_string()),
            None => (url.to_string(), String::new()),
        };
        target = format!("{base}{}msg={}{}", if base.contains('?') { "&" } else { "?" }, py::quote(m, "/"), if fragment.is_empty() { String::new() } else { format!("#{fragment}") });
    }
    redirect_response(&target, 303)
}

fn msg(url: &str, m: impl AsRef<str>) -> Out {
    Ok(redirect(url, Some(m.as_ref())))
}

/// Only local paths are accepted as a return address (no open redirect).
fn safe_next(value: Option<&str>, default: &str) -> String {
    match value {
        Some(v) if v.starts_with('/') && !v.starts_with("//") => v.to_string(),
        _ => default.to_string(),
    }
}

fn str_of(e: &AppError) -> String {
    e.to_string()
}

// --------------------------------------------------------------------------------------- integers

/// Pydantic's lax integer from a string: spaces around, a sign, "_" between digits, ".0" allowed.
fn pydantic_int(s: &str) -> Option<i64> {
    let t = s.trim();
    let (sign, body) = match t.as_bytes().first() {
        Some(b'-') => (-1i128, &t[1..]),
        Some(b'+') => (1, &t[1..]),
        _ => (1, t),
    };
    let (int, frac) = match body.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (body, None),
    };
    if let Some(f) = frac {
        if f.is_empty() || !f.chars().all(|c| c == '0') {
            return None;
        }
    }
    if int.is_empty() || int.starts_with('_') || int.ends_with('_') || int.contains("__") || !int.chars().all(|c| c.is_ascii_digit() || c == '_') {
        return None;
    }
    let digits: String = int.chars().filter(|c| *c != '_').collect();
    let n: i128 = digits.parse().ok()?;
    i64::try_from(sign * n).ok()
}

fn int_error(loc: Vec<serde_json::Value>, input: &str) -> serde_json::Value {
    serde_json::json!({"type": "int_parsing", "loc": loc, "msg": "Input should be a valid integer, unable to parse string as an integer", "input": input})
}

// ---------------------------------------------------------------------------------------- request

pub struct Cx<'a> {
    req: &'a Request,
    token: Option<String>,
    session: Option<WebSession>,
    cf_email: Option<String>,
    query: Vec<(String, String)>,
    params: Vec<String>,
}

impl<'a> Cx<'a> {
    /// `request.query_params.get(name)`: the last value.
    fn q(&self, name: &str) -> Option<&str> {
        self.query.iter().rev().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
    fn qs(&self, name: &str) -> String {
        self.q(name).unwrap_or("").to_string()
    }
    fn me(&self) -> Option<Identity> {
        self.session.as_ref().and_then(|s| s.identity.clone())
    }
}

/// Path and query parameters validated together, like FastAPI (every error reported at once, as a 422).
struct Params<'c, 'a> {
    cx: &'c Cx<'a>,
    errors: Vec<serde_json::Value>,
}

impl<'c, 'a> Params<'c, 'a> {
    fn new(cx: &'c Cx<'a>) -> Self {
        Params { cx, errors: vec![] }
    }
    fn path_int(&mut self, i: usize, name: &str) -> i64 {
        let raw = self.cx.params[i].clone();
        match pydantic_int(&raw) {
            Some(n) => n,
            None => {
                self.errors.push(int_error(vec!["path".into(), name.into()], &raw));
                0
            }
        }
    }
    fn query_int(&mut self, name: &str, default: i64) -> i64 {
        match self.cx.q(name) {
            None => default,
            Some(raw) => match pydantic_int(raw) {
                Some(n) => n,
                None => {
                    self.errors.push(int_error(vec!["query".into(), name.into()], raw));
                    default
                }
            },
        }
    }
    fn query_opt_int(&mut self, name: &str) -> Option<i64> {
        let raw = self.cx.q(name)?.to_string();
        match pydantic_int(&raw) {
            Some(n) => Some(n),
            None => {
                self.errors.push(int_error(vec!["query".into(), name.into()], &raw));
                None
            }
        }
    }
    fn query_int_list(&mut self, name: &str) -> Vec<i64> {
        let mut out = vec![];
        let values: Vec<String> = self.cx.query.iter().filter(|(k, _)| k == name).map(|(_, v)| v.clone()).collect();
        for (i, raw) in values.iter().enumerate() {
            match pydantic_int(raw) {
                Some(n) => out.push(n),
                None => self.errors.push(int_error(vec!["query".into(), name.into(), i.into()], raw)),
            }
        }
        out
    }
    fn check(self) -> Result<(), Fail> {
        if self.errors.is_empty() {
            Ok(())
        } else {
            Err(Fail::Invalid(self.errors))
        }
    }
}

// --------------------------------------------------------------------------------------------- app

pub struct Web {
    pub core: Arc<MyMoney>,
    pub settings: Settings,
    cf: CloudflareAccess,
    pub sessions: Arc<Sessions>,
    env: Environment,
    app_version: String,
    themes_css: String,
    started: chrono::DateTime<chrono::Utc>,
}

macro_rules! template_sources {
    ($($name:literal),* $(,)?) => {
        &[$(($name, include_str!(concat!("../../web/templates/", $name)))),*]
    };
}

const TEMPLATES: &[(&str, &str)] = template_sources!(
    "_loan.html",
    "_macros.html",
    "account.html",
    "account_form.html",
    "accounts.html",
    "people.html",
    "tax.html",
    "person.html",
    "analytics.html",
    "base.html",
    "contact.html",
    "contact_identify.html",
    "contacts.html",
    "dashboard.html",
    "denied.html",
    "document.html",
    "documents.html",
    "first_user.html",
    "login.html",
    "rebuild.html",
    "recovery.html",
    "settings.html",
    "setup.html",
);

const STATIC: &[(&str, &[u8], &str)] = &[
    ("app.js", include_bytes!("../../web/static/app.js"), "text/javascript; charset=utf-8"),
    ("icon.svg", include_bytes!("../../web/static/icon.svg"), "image/svg+xml"),
    ("logo.svg", include_bytes!("../../web/static/logo.svg"), "image/svg+xml"),
    ("style.css", include_bytes!("../../web/static/style.css"), "text/css; charset=utf-8"),
    ("modern.css", include_bytes!("../../web/static/modern.css"), "text/css; charset=utf-8"),
    ("theme.js", include_bytes!("../../web/static/theme.js"), "text/javascript; charset=utf-8"),
];

#[derive(Clone, Copy, PartialEq, Debug)]
enum R {
    ThemesCss,
    Static,
    Manifest,
    ServiceWorker,
    Icon,
    Healthz,
    SetupPage,
    Setup,
    LoginPage,
    Login,
    FirstUserPage,
    FirstUser,
    Logout,
    Dashboard,
    GapAck,
    GapRestore,
    Analytics,
    AnalyticsCategorize,
    AccountNew,
    AccountList,
    PeopleList,
    PersonNew,
    PersonCreate,
    PersonDetail,
    PersonUpdate,
    PersonDelete,
    AccountQuick,
    AccountRate,
    RatesRefresh,
    TaxPage,
    TaxSave,
    TaxReviewed,
    TaxSnooze,
    TaxReset,
    AccountCreate,
    AccountDetail,
    ShareAdd,
    ShareRemove,
    LoanFirstDue,
    AccountEdit,
    AccountUpdate,
    AccountDelete,
    HoldingAdd,
    HoldingDelete,
    Documents,
    DocumentsInvalidate,
    DocumentsDownload,
    DocumentsUpload,
    DocumentsRebuild,
    DocumentsRefresh,
    DocumentsReview,
    Document,
    DocumentAnalyze,
    DocumentImport,
    DocumentDedup,
    DocumentKeep,
    DocumentUndo,
    DocumentRefresh,
    DocumentDownload,
    DocumentDelete,
    Contacts,
    ContactIdentifyPage,
    ContactIdentify,
    ContactNew,
    ContactCreate,
    ContactDetail,
    ContactUpdate,
    ContactAddIdentifier,
    ContactRemoveIdentifier,
    ContactMerge,
    ContactDelete,
    Settings,
    SettingsPassword,
    SettingsRecovery,
    SettingsUserAdd,
    SettingsUserDelete,
    SettingsUserRole,
    SettingsUserPassword,
    SettingsGeneral,
}

/// The routes in the order FastAPI registered them: the first that matches path and method wins.
const ROUTES: &[(&str, &str, R)] = &[
    ("GET", "/themes.css", R::ThemesCss),
    ("*", "/static/{path:path}", R::Static),
    ("GET", "/manifest.webmanifest", R::Manifest),
    ("GET", "/sw.js", R::ServiceWorker),
    ("GET", "/icons/{name}", R::Icon),
    ("GET", "/healthz", R::Healthz),
    ("GET", "/setup", R::SetupPage),
    ("POST", "/setup", R::Setup),
    ("GET", "/login", R::LoginPage),
    ("POST", "/login", R::Login),
    ("GET", "/first-user", R::FirstUserPage),
    ("POST", "/first-user", R::FirstUser),
    ("POST", "/logout", R::Logout),
    ("GET", "/", R::Dashboard),
    ("POST", "/gaps/acknowledge", R::GapAck),
    ("POST", "/gaps/{ack_id}/restore", R::GapRestore),
    ("GET", "/analytics", R::Analytics),
    ("POST", "/analytics/categorize", R::AnalyticsCategorize),
    ("GET", "/accounts", R::AccountList),
    ("GET", "/people", R::PeopleList),
    ("GET", "/people/new", R::PersonNew),
    ("POST", "/people/new", R::PersonCreate),
    ("GET", "/people/{person_id}", R::PersonDetail),
    ("POST", "/people/{person_id}", R::PersonUpdate),
    ("POST", "/people/{person_id}/delete", R::PersonDelete),
    ("GET", "/accounts/new", R::AccountNew),
    ("POST", "/accounts/{account_id}/quick", R::AccountQuick),
    ("POST", "/accounts/{account_id}/rate", R::AccountRate),
    ("POST", "/settings/rates/refresh", R::RatesRefresh),
    ("GET", "/settings/tax", R::TaxPage),
    ("POST", "/settings/tax", R::TaxSave),
    ("POST", "/settings/tax/reviewed", R::TaxReviewed),
    ("POST", "/settings/tax/snooze", R::TaxSnooze),
    ("POST", "/settings/tax/reset", R::TaxReset),
    ("POST", "/accounts/new", R::AccountCreate),
    ("GET", "/accounts/{account_id}", R::AccountDetail),
    ("POST", "/accounts/{account_id}/shares", R::ShareAdd),
    ("POST", "/accounts/{account_id}/shares/{user_id}/delete", R::ShareRemove),
    ("POST", "/accounts/{account_id}/loan/first-due", R::LoanFirstDue),
    ("GET", "/accounts/{account_id}/edit", R::AccountEdit),
    ("POST", "/accounts/{account_id}/edit", R::AccountUpdate),
    ("POST", "/accounts/{account_id}/delete", R::AccountDelete),
    ("POST", "/accounts/{account_id}/holdings", R::HoldingAdd),
    ("POST", "/holdings/{holding_id}/delete", R::HoldingDelete),
    ("GET", "/documents", R::Documents),
    ("POST", "/documents/invalidate", R::DocumentsInvalidate),
    ("POST", "/documents/download", R::DocumentsDownload),
    ("POST", "/documents", R::DocumentsUpload),
    ("POST", "/documents/rebuild", R::DocumentsRebuild),
    ("POST", "/documents/refresh", R::DocumentsRefresh),
    ("GET", "/documents/review", R::DocumentsReview),
    ("GET", "/documents/{doc_id}", R::Document),
    ("POST", "/documents/{doc_id}/analyze", R::DocumentAnalyze),
    ("POST", "/documents/{doc_id}/import/{index}", R::DocumentImport),
    ("POST", "/documents/{doc_id}/dedup/{index}", R::DocumentDedup),
    ("POST", "/documents/{doc_id}/keep", R::DocumentKeep),
    ("POST", "/documents/{doc_id}/undo", R::DocumentUndo),
    ("POST", "/documents/{doc_id}/refresh", R::DocumentRefresh),
    ("GET", "/documents/{doc_id}/download", R::DocumentDownload),
    ("POST", "/documents/{doc_id}/delete", R::DocumentDelete),
    ("GET", "/contacts", R::Contacts),
    ("GET", "/contacts/identify", R::ContactIdentifyPage),
    ("POST", "/contacts/identify", R::ContactIdentify),
    ("GET", "/contacts/new", R::ContactNew),
    ("POST", "/contacts/new", R::ContactCreate),
    ("GET", "/contacts/{contact_id}", R::ContactDetail),
    ("POST", "/contacts/{contact_id}", R::ContactUpdate),
    ("POST", "/contacts/{contact_id}/identifiers", R::ContactAddIdentifier),
    ("POST", "/contacts/{contact_id}/identifiers/{identifier_id}/delete", R::ContactRemoveIdentifier),
    ("POST", "/contacts/{contact_id}/merge", R::ContactMerge),
    ("POST", "/contacts/{contact_id}/delete", R::ContactDelete),
    ("GET", "/settings", R::Settings),
    ("POST", "/settings/password", R::SettingsPassword),
    ("POST", "/settings/recovery", R::SettingsRecovery),
    ("POST", "/settings/users", R::SettingsUserAdd),
    ("POST", "/settings/users/{user_id}/delete", R::SettingsUserDelete),
    ("POST", "/settings/users/{user_id}/role", R::SettingsUserRole),
    ("POST", "/settings/users/{user_id}/password", R::SettingsUserPassword),
    ("POST", "/settings/general", R::SettingsGeneral),
];

/// Starlette's path matching: `{name}` is one segment (not empty), `{name:path}` the rest.
fn match_path(pattern: &str, path: &str) -> Option<Vec<String>> {
    if let Some(prefix) = pattern.strip_suffix("{path:path}") {
        return path.strip_prefix(prefix).map(|rest| vec![rest.to_string()]);
    }
    let p: Vec<&str> = pattern.split('/').collect();
    let s: Vec<&str> = path.split('/').collect();
    if p.len() != s.len() {
        return None;
    }
    let mut params = vec![];
    for (a, b) in p.iter().zip(s.iter()) {
        if a.starts_with('{') {
            if b.is_empty() {
                return None;
            }
            params.push(b.to_string());
        } else if a != b {
            return None;
        }
    }
    Some(params)
}

enum Matched {
    Full(R, Vec<String>),
    Method(Vec<&'static str>),
    None,
}

fn find_route(method: &str, path: &str) -> Matched {
    let mut allowed: Vec<&'static str> = vec![];
    for (m, pattern, r) in ROUTES {
        if let Some(params) = match_path(pattern, path) {
            // FastAPI routes answer their own method only (no implicit HEAD); the static files mount takes any
            let ok = *m == "*" || *m == method;
            if ok {
                return Matched::Full(*r, params);
            }
            if allowed.is_empty() {
                allowed.push(m);
            }
        }
    }
    if allowed.is_empty() {
        Matched::None
    } else {
        Matched::Method(allowed)
    }
}

fn origin_netloc(origin: &str) -> String {
    // urlparse(origin).netloc
    let rest = match origin.find(':') {
        Some(i) if i > 0 && origin[..i].chars().next().unwrap().is_ascii_alphabetic() && origin[..i].chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)) => &origin[i + 1..],
        _ => origin,
    };
    match rest.strip_prefix("//") {
        Some(r) => r.split(['/', '?', '#']).next().unwrap_or("").to_string(),
        None => String::new(),
    }
}

impl Web {
    pub fn new(core: Arc<MyMoney>) -> Result<Arc<Web>, String> {
        let settings = core.settings.clone();
        let cf = CloudflareAccess::new(settings.cf_team_domain.as_deref(), settings.cf_audience.as_deref());
        Self::with_cloudflare(core, cf)
    }

    /// With a given Cloudflare Access verifier (the tests give it the keys to trust).
    pub fn with_cloudflare(core: Arc<MyMoney>, cf: CloudflareAccess) -> Result<Arc<Web>, String> {
        let settings = core.settings.clone();
        let sessions = Arc::new(Sessions::new(if settings.auto_lock_minutes != 0 { settings.auto_lock_minutes } else { 24 * 60 }));
        {
            let s = sessions.clone();
            core.vault.on_lock(Box::new(move || {
                s.clear();
                Ok(())
            }));
        }
        let mut env = Environment::new(TEMPLATES).map_err(|e| e.0)?;
        env.add_filter("money", views::f_money);
        env.add_filter("money_text", views::f_money_text);
        env.add_filter("qty", views::f_qty);
        env.add_filter("frdate", views::f_frdate);
        env.add_filter("iban", views::f_iban);
        env.add_filter("pct", views::f_pct);
        Ok(Arc::new(Web {
            core,
            settings,
            cf,
            sessions,
            env,
            app_version: std::env::var("MYMONEY_VERSION").unwrap_or_else(|_| "dev".into()),
            themes_css: themes::stylesheet(),
            started: chrono::Utc::now(),
        }))
    }

    fn globals(&self) -> HashMap<String, Value> {
        let mut g: HashMap<String, Value> = HashMap::new();
        g.insert(
            "ACCOUNT_TYPES".into(),
            Value::dict(ACCOUNT_TYPES.iter().map(|(code, label, cat)| (Value::str(code), Value::tuple(vec![Value::str(label), Value::str(cat)]))).collect()),
        );
        g.insert("CATEGORIES".into(), views::str_map(CATEGORIES));
        g.insert("COUNTERPARTY_KINDS".into(), views::str_map(COUNTERPARTY_KINDS));
        g.insert("KIND_LABELS".into(), views::str_map(KIND_LABELS));
        g.insert("DOCUMENT_STATUS".into(), views::str_map(DOCUMENT_STATUS));
        g.insert("DOC_KINDS".into(), views::str_map(views::DOC_KINDS));
        g.insert("BLANK_HOLDERS".into(), Value::from(&crate::services::holders::form_blocks(&[], 2)));
        g.insert("LOAN_KINDS".into(), Value::tuple(loan_service::LOAN_KINDS.iter().map(|k| Value::str(k)).collect()));
        g.insert(
            "THEME_GROUPS".into(),
            Value::list(
                themes::groups()
                    .into_iter()
                    .map(|(title, items)| {
                        Value::tuple(vec![
                            Value::str(title),
                            Value::list(items.into_iter().map(|t| Value::obj("Theme", vec![("id", t.id.into()), ("label", t.label.into()), ("kind", t.kind.into())])).collect()),
                        ])
                    })
                    .collect(),
            ),
        );
        g.insert("identity_text".into(), Value::func(|a, _| views::identity_text(a.first().unwrap_or(&Value::None))));
        g.insert(
            "reliability".into(),
            Value::func(|a, _| {
                let d = a.first().cloned().unwrap_or(Value::None);
                super::jinja::getattr(&d, "__reliability")
            }),
        );
        g.insert("APP_VERSION".into(), Value::str(&self.app_version));
        g.insert("READER_VERSION".into(), Value::str(registry::reader_version()));
        let core = self.core.clone();
        g.insert(
            "contact_choices".into(),
            Value::func(move |_, _| {
                let choices = core.session(contact_service::choices).map_err(|e| super::jinja::Error(e.to_string()))?;
                Ok(Value::list(choices.into_iter().map(|(id, name)| Value::tuple(vec![id.into(), name.into()])).collect()))
            }),
        );
        g.insert("QUARANTINE_HOURS".into(), Value::Int(QUARANTINE_HOURS));
        g
    }

    fn render(&self, cx: &Cx, name: &str, status: u16, vars: Vec<(&str, Value)>) -> Out {
        let mut context: HashMap<String, Value> = vars.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        context.insert("request".into(), Value::obj("Request", vec![("url", Value::obj("URL", vec![("path", cx.req.path.clone().into())]))]));
        context.insert("csrf".into(), Value::str(cx.session.as_ref().map(|s| s.csrf.clone()).unwrap_or_default()));
        context.insert("unlocked".into(), Value::Bool(self.core.vault.unlocked()));
        context.insert("flash".into(), cx.q("msg").map(Value::str).unwrap_or(Value::None));
        context.insert("me".into(), cx.me().map(|i| views::identity(&i)).unwrap_or(Value::None));
        context.insert("cf_active".into(), Value::Bool(self.cf.enabled() && cx.cf_email.as_deref().map(|e| !e.is_empty()).unwrap_or(false)));
        // the monthly reminder of the administrator: are the tax rates still current?
        if self.core.vault.unlocked() && status == 200 && cx.req.path != "/settings/tax" && cx.me().map(|m| m.is_admin).unwrap_or(false) && cx.req.method == "GET" {
            let table = self.core.tax_table();
            let at = crate::services::prices::now();
            if table.due(at) {
                let differences = crate::services::fiscal::differences(&table, crate::services::public_rates::current().as_ref(), at);
                context.insert(
                    "tax_reminder".into(),
                    Value::sdict(vec![
                        ("days", table.days_since_review(at).map(Value::Int).unwrap_or(Value::None)),
                        ("differences", Value::list(differences.into_iter().map(Value::from).collect())),
                        ("year", (table.year as i64).into()),
                    ]),
                );
            }
        }
        if matches!(name, "account_form.html" | "document.html") && self.core.vault.unlocked() {
            // the people a holder can be picked from
            let cards = self.core.session(|c| people_service::visible(c))?;
            context.insert("people_options".into(), Value::list(cards.iter().map(views::person_card).collect()));
        }
        let body = self.env.render(name, self.globals(), context)?;
        Ok(html(status, body))
    }

    fn page(&self, cx: &Cx, name: &str, vars: Vec<(&str, Value)>) -> Out {
        self.render(cx, name, 200, vars)
    }

    fn trusted_origin(&self, req: &Request) -> bool {
        let origin = match req.header("origin") {
            None => return true,
            Some(o) => o,
        };
        if origin.is_empty() || origin == "null" || Some(origin_netloc(origin).as_str()) == req.header("host") {
            return true;
        }
        let allowed: BTreeSet<String> = self.settings.trusted_origins.split(',').filter(|o| !py::strip(o).is_empty()).map(|o| py::strip(o).trim_end_matches('/').to_string()).collect();
        allowed.contains(origin.trim_end_matches('/'))
    }

    /// The scheme of the request as uvicorn set it: "http", or what a trusted proxy says in X-Forwarded-Proto
    /// (`MYMONEY_TRUSTED_PROXIES`, 127.0.0.1 by default).
    fn scheme(&self, req: &Request) -> String {
        let trusted = std::env::var("MYMONEY_TRUSTED_PROXIES").unwrap_or_else(|_| "127.0.0.1".into());
        let from_trusted = req.peer.map(|ip| proxy_trusted(&trusted, ip)).unwrap_or(false);
        if from_trusted {
            if let Some(p) = req.header("x-forwarded-proto") {
                let p = p.trim();
                if ["http", "https", "ws", "wss"].contains(&p) {
                    return p.to_string();
                }
            }
        }
        "http".into()
    }

    /// The browser talks HTTPS even when the tunnel talks plain http to the app: the cookie can be marked Secure.
    fn https_in_front(&self, req: &Request) -> bool {
        let forwarded = req.header("x-forwarded-proto").unwrap_or("").split(',').next().unwrap_or("").trim().to_lowercase();
        forwarded == "https" || req.header("cf-visitor").unwrap_or("").contains("\"https\"")
    }

    fn cookie_secure(&self, req: &Request) -> bool {
        self.settings.cookie_secure || self.https_in_front(req)
    }

    fn set_session_cookie(&self, req: &Request, resp: &mut Response, token: &str) {
        let secure = if self.cookie_secure(req) { "; Secure" } else { "" };
        resp.add("set-cookie", format!("{COOKIE}={token}; HttpOnly; Path=/; SameSite=strict{secure}"));
    }

    fn start_session(&self, req: &Request, resp: &mut Response, identity: Option<Identity>) {
        let token = self.sessions.create(identity);
        self.set_session_cookie(req, resp, &token);
    }

    fn security_headers(&self, req: &Request, resp: &mut Response) {
        for (k, v) in SECURITY_HEADERS {
            resp.set_default(k, v);
        }
        if self.cookie_secure(req) {
            resp.set_default("Strict-Transport-Security", "max-age=31536000");
        }
    }

    /// The middleware: origin check, session, Cloudflare Access, the redirects to setup / login / first user.
    pub fn handle(&self, req: Request) -> Response {
        let path = req.path.clone();
        // charts are drawn on a narrower canvas for a phone, so their text stays readable
        charts::set_compact(req.header("user-agent").map(|u| u.contains("Mobi")).unwrap_or(false));
        if req.method == "POST" && !self.trusted_origin(&req) {
            return Response::new(403, "", "Origine refusée");
        }
        let cookies = forms::cookies(&req);
        let token = cookies.get(COOKIE).cloned();
        let mut cx = Cx { req: &req, token: token.clone(), session: self.sessions.get(token.as_deref()), cf_email: None, query: forms::parse_qsl(&super::http::latin1(req.query.as_bytes())), params: vec![] };
        let public = PUBLIC_PATHS.iter().any(|p| path.starts_with(p));
        let mut new_token: Option<String> = None;
        let mut identity = cx.me();
        let initialized = self.core.vault.initialized();
        if self.cf.enabled() && initialized && !["/static/", "/icons/", "/themes.css", "/healthz"].iter().any(|p| path.starts_with(p)) {
            cx.cf_email = self.cf.verify(req.header(super::cloudflare::HEADER));
            if let Some(email) = cx.cf_email.clone() {
                if self.core.vault.unlocked() && identity.as_ref().map(|i| i.email != email).unwrap_or(true) {
                    let found = match self.core.identity_for_email(Some(&email)) {
                        Ok(f) => f,
                        Err(e) => return self.failure(&cx, Fail::App(e)),
                    };
                    match found {
                        Some(f) => {
                            let t = self.sessions.create(Some(f.clone()));
                            cx.session = self.sessions.get(Some(&t));
                            cx.token = Some(t.clone());
                            new_token = Some(t);
                            identity = Some(f);
                        }
                        None => {
                            let has_users = match self.core.has_users() {
                                Ok(h) => h,
                                Err(e) => return self.failure(&cx, Fail::App(e)),
                            };
                            if has_users {
                                let message = format!("Ton adresse ({email}) n'a pas de compte sur myMoney : demande à un administrateur de la créer.");
                                let mut resp = match self.render(&cx, "denied.html", 403, vec![("message", message.into()), ("cf_email", email.clone().into())]) {
                                    Ok(r) => r,
                                    Err(f) => return self.failure(&cx, f),
                                };
                                for (k, v) in SECURITY_HEADERS {
                                    resp.set_default(k, v);
                                }
                                return resp;
                            }
                        }
                    }
                }
            }
        }
        if !public {
            if !initialized {
                return redirect_response("/setup", 303);
            }
            if cx.session.is_none() || !self.core.vault.unlocked() {
                if cx.session.is_some() {
                    self.sessions.discard(token.as_deref());
                }
                return redirect_response("/login", 303);
            }
            if identity.is_none() && path != "/first-user" && path != "/logout" {
                return redirect_response("/first-user", 303);
            }
            self.core.vault.touch();
        }
        let outcome = access::as_user(identity.clone(), || self.dispatch(&mut cx));
        let mut resp = match outcome {
            Ok(r) => r,
            Err(Fail::App(AppError::Locked)) => redirect_response("/login", 303),
            Err(f) => {
                let r = self.failure(&cx, f);
                if r.status == 500 {
                    return r; // an unhandled exception: Starlette's plain error page, without the middleware's headers
                }
                r
            }
        };
        if let Some(t) = new_token {
            self.set_session_cookie(&req, &mut resp, &t);
        }
        self.security_headers(&req, &mut resp);
        resp
    }

    /// The exception handlers: AccessDenied as the "denied" page (404), HTTPException as JSON, the rest as a 500.
    fn failure(&self, cx: &Cx, f: Fail) -> Response {
        match f {
            Fail::App(AppError::Denied(m)) => match self.render(cx, "denied.html", 404, vec![("message", m.into())]) {
                Ok(r) => r,
                Err(_) => internal_error(),
            },
            Fail::Http(status, detail) => json_response(status, &serde_json::json!({ "detail": detail })),
            Fail::Invalid(errors) => json_response(422, &serde_json::json!({ "detail": errors })),
            Fail::App(e) => {
                log::error!("Exception in ASGI application: {e}");
                internal_error()
            }
            Fail::Internal(e) => {
                log::error!("Exception in ASGI application: {e}");
                internal_error()
            }
        }
    }

    fn dispatch(&self, cx: &mut Cx) -> Out {
        let (route, params) = match find_route(&cx.req.method, &cx.req.path) {
            Matched::Full(r, p) => (r, p),
            Matched::Method(allowed) => {
                let mut r = json_response(405, &serde_json::json!({"detail": "Method Not Allowed"}));
                r.add("allow", allowed.join(", "));
                return Ok(r);
            }
            Matched::None => {
                // redirect_slashes: the same path with (or without) a final "/" exists
                let path = &cx.req.path;
                let other = if path.ends_with('/') { path.trim_end_matches('/').to_string() } else { format!("{path}/") };
                if path != "/" && !matches!(find_route(&cx.req.method, &other), Matched::None) {
                    let host = cx.req.header("host").unwrap_or("localhost");
                    let query = if cx.req.query.is_empty() { String::new() } else { format!("?{}", cx.req.query) };
                    return Ok(redirect_response(&format!("{}://{host}{other}{query}", self.scheme(cx.req)), 307));
                }
                return Err(not_found());
            }
        };
        cx.params = params;
        let cx: &Cx = cx;
        match route {
            R::ThemesCss => Ok(Response::new(200, "text/css; charset=utf-8", self.themes_css.clone())),
            R::Static => self.static_file(cx),
            R::Manifest => Ok(Response::new(200, "application/manifest+json", pwa::manifest())),
            R::ServiceWorker => {
                let mut r = Response::new(200, "text/javascript; charset=utf-8", pwa::SERVICE_WORKER);
                r.add("service-worker-allowed", "/");
                Ok(r)
            }
            R::Icon => match pwa::icon(&cx.params[0]) {
                Some(png) => {
                    let mut r = Response::new(200, "image/png", png.to_vec());
                    r.add("cache-control", "public, max-age=86400");
                    Ok(r)
                }
                None => Err(not_found()),
            },
            R::Healthz => Ok(json_response(200, &serde_json::json!({"ok": true}))),
            R::SetupPage => self.setup_page(cx),
            R::Setup => self.setup(cx),
            R::LoginPage => self.login_page(cx),
            R::Login => self.login(cx),
            R::FirstUserPage => self.first_user_page(cx),
            R::FirstUser => self.first_user(cx),
            R::Logout => self.logout(cx),
            R::Dashboard => self.dashboard(cx),
            R::GapAck => self.gap_acknowledge(cx),
            R::GapRestore => self.gap_restore(cx),
            R::Analytics => self.analytics_page(cx),
            R::AnalyticsCategorize => self.analytics_categorize(cx),
            R::AccountList => self.accounts_page(cx),
            R::PeopleList => self.people_page(cx),
            R::PersonNew => self.person_page(cx, None),
            R::PersonCreate => self.person_create(cx),
            R::PersonDetail => self.person_detail(cx),
            R::PersonUpdate => self.person_update(cx),
            R::PersonDelete => self.person_delete(cx),
            R::AccountQuick => self.account_quick(cx),
            R::AccountRate => self.account_rate(cx),
            R::RatesRefresh => self.rates_refresh(cx),
            R::TaxPage => self.tax_page(cx),
            R::TaxSave => self.tax_save(cx),
            R::TaxReviewed => self.tax_reviewed(cx),
            R::TaxSnooze => self.tax_snooze(cx),
            R::TaxReset => self.tax_reset(cx),
            R::AccountNew => self.page(cx, "account_form.html", vec![("account", Value::None)]),
            R::AccountCreate => self.account_create(cx),
            R::AccountDetail => self.account_detail(cx),
            R::ShareAdd => self.share_add(cx),
            R::ShareRemove => self.share_remove(cx),
            R::LoanFirstDue => self.loan_first_due(cx),
            R::AccountEdit => self.account_edit(cx),
            R::AccountUpdate => self.account_update(cx),
            R::AccountDelete => self.account_delete(cx),
            R::HoldingAdd => self.holding_add(cx),
            R::HoldingDelete => self.holding_delete(cx),
            R::Documents => self.documents_page(cx),
            R::DocumentsInvalidate => self.documents_invalidate(cx),
            R::DocumentsDownload => self.documents_download(cx),
            R::DocumentsUpload => self.documents_upload(cx),
            R::DocumentsRebuild => self.documents_rebuild(cx),
            R::DocumentsRefresh => self.documents_refresh(cx),
            R::DocumentsReview => self.documents_review(cx),
            R::Document => self.document_review(cx),
            R::DocumentAnalyze => self.document_analyze(cx),
            R::DocumentImport => self.document_import(cx),
            R::DocumentDedup => self.document_dedup(cx),
            R::DocumentKeep => self.document_keep(cx),
            R::DocumentUndo => self.document_undo(cx),
            R::DocumentRefresh => self.document_refresh(cx),
            R::DocumentDownload => self.document_download(cx),
            R::DocumentDelete => self.document_delete(cx),
            R::Contacts => self.contacts_page(cx),
            R::ContactIdentifyPage => self.contact_identify_page(cx),
            R::ContactIdentify => self.contact_identify(cx),
            R::ContactNew => self.page(cx, "contact.html", vec![("contact", Value::None), ("transactions", Value::list(vec![])), ("others", Value::list(vec![]))]),
            R::ContactCreate => self.contact_create(cx),
            R::ContactDetail => self.contact_detail(cx),
            R::ContactUpdate => self.contact_update(cx),
            R::ContactAddIdentifier => self.contact_add_identifier(cx),
            R::ContactRemoveIdentifier => self.contact_remove_identifier(cx),
            R::ContactMerge => self.contact_merge(cx),
            R::ContactDelete => self.contact_delete(cx),
            R::Settings => self.settings_page(cx),
            R::SettingsPassword => self.settings_password(cx),
            R::SettingsRecovery => self.settings_recovery(cx),
            R::SettingsUserAdd => self.settings_user_add(cx),
            R::SettingsUserDelete => self.settings_user_delete(cx),
            R::SettingsUserRole => self.settings_user_role(cx),
            R::SettingsUserPassword => self.settings_user_password(cx),
            R::SettingsGeneral => self.settings_general(cx),
        }
    }

    fn static_file(&self, cx: &Cx) -> Out {
        if cx.req.method != "GET" && cx.req.method != "HEAD" {
            // Starlette's StaticFiles (1.10+): HTTPException(405, headers={"Allow": "GET, HEAD"})
            let mut r = json_response(405, &serde_json::json!({"detail": "Method Not Allowed"}));
            r.add("allow", "GET, HEAD");
            return Ok(r);
        }
        let name = &cx.params[0];
        let Some((_, data, ctype)) = STATIC.iter().find(|(n, _, _)| n == name) else { return Err(not_found()) };
        let etag = format!("\"{}\"", hex::encode(<md5::Md5 as md5::Digest>::digest(data)));
        let modified = http_date(self.started);
        if cx.req.header("if-none-match").map(|v| v.split(',').any(|t| t.trim().trim_start_matches("W/") == etag)).unwrap_or(false) {
            let mut r = Response::new(304, "", Vec::new());
            r.add("etag", etag);
            r.add("last-modified", modified);
            return Ok(r);
        }
        let mut base: Vec<(String, String)> = vec![("last-modified".into(), modified.clone()), ("etag".into(), etag.clone()), ("accept-ranges".into(), "bytes".into())];
        let simple = |base: &Vec<(String, String)>| {
            let mut r = Response::new(200, ctype, data.to_vec());
            r.headers.extend(base.iter().cloned());
            r
        };
        let range = cx.req.header("range");
        let if_range = cx.req.header("if-range");
        let Some(range) = range else { return Ok(simple(&base)) };
        if let Some(ir) = if_range {
            if ir != modified && ir != etag {
                return Ok(simple(&base));
            }
        }
        let size = data.len();
        let ranges = match parse_range(range, size) {
            Ok(r) => r,
            Err(RangeError::Malformed(m)) => return Ok(Response::new(400, "text/plain; charset=utf-8", m)),
            Err(RangeError::Unsatisfiable) => {
                let mut r = Response::new(416, "text/plain; charset=utf-8", Vec::new());
                r.add("content-range", format!("bytes */{size}"));
                return Ok(r);
            }
        };
        match ranges.len() {
            0 => Ok(simple(&base)),
            1 => {
                let (start, end) = ranges[0];
                let mut r = Response::new(206, ctype, data[start..end].to_vec());
                base.push(("content-range".into(), format!("bytes {start}-{}/{size}", end - 1)));
                r.headers.extend(base);
                Ok(r)
            }
            _ => {
                let boundary = crate::app::token_hex(13);
                let mut body: Vec<u8> = vec![];
                for (start, end) in &ranges {
                    body.extend_from_slice(format!("--{boundary}\r\nContent-Type: {ctype}\r\nContent-Range: bytes {start}-{}/{size}\r\n\r\n", end - 1).as_bytes());
                    body.extend_from_slice(&data[*start..*end]);
                    body.extend_from_slice(b"\r\n");
                }
                body.extend_from_slice(format!("--{boundary}--").as_bytes());
                let mut r = Response::new(206, &format!("multipart/byteranges; boundary={boundary}"), body);
                r.headers.extend(base);
                Ok(r)
            }
        }
    }

    // ------------------------------------------------------------------------------------- forms

    fn form(&self, cx: &Cx) -> Result<FormData, Fail> {
        forms::read_form(cx.req).map_err(|e| http(400, &e))
    }

    /// Parse a POSTed form and check its CSRF token.
    fn verified_form(&self, cx: &Cx) -> Result<HashMap<String, String>, Fail> {
        let form = self.form(cx)?;
        self.check_csrf(cx, &form, "Jeton CSRF invalide : recharge la page")?;
        Ok(form.strings())
    }

    fn check_csrf(&self, cx: &Cx, form: &FormData, detail: &str) -> Result<(), Fail> {
        let ok = match (&cx.session, form.get("csrf")) {
            (Some(s), Some(Field::Text(t))) => same(t, &s.csrf),
            _ => false,
        };
        if ok {
            Ok(())
        } else {
            Err(http(403, detail))
        }
    }
}

/// uvicorn's trusted hosts: "*", addresses and networks ("172.17.0.0/16"), separated by commas.
fn proxy_trusted(list: &str, ip: std::net::IpAddr) -> bool {
    list.split(',').map(|x| x.trim()).filter(|x| !x.is_empty()).any(|entry| {
        if entry == "*" {
            return true;
        }
        match entry.split_once('/') {
            None => entry.parse::<std::net::IpAddr>().map(|a| a == ip).unwrap_or(false),
            Some((net, bits)) => {
                let Ok(bits) = bits.parse::<u32>() else { return false };
                match (net.parse::<std::net::IpAddr>(), ip) {
                    (Ok(std::net::IpAddr::V4(n)), std::net::IpAddr::V4(a)) if bits <= 32 => {
                        let mask = if bits == 0 { 0 } else { u32::MAX << (32 - bits) };
                        u32::from(n) & mask == u32::from(a) & mask
                    }
                    (Ok(std::net::IpAddr::V6(n)), std::net::IpAddr::V6(a)) if bits <= 128 => {
                        let mask = if bits == 0 { 0 } else { u128::MAX << (128 - bits) };
                        u128::from(n) & mask == u128::from(a) & mask
                    }
                    _ => false,
                }
            }
        }
    })
}

enum RangeError {
    Malformed(String),
    Unsatisfiable,
}

/// Starlette's `FileResponse._parse_range_header`.
fn parse_range(header: &str, size: usize) -> Result<Vec<(usize, usize)>, RangeError> {
    let malformed = |m: &str| RangeError::Malformed(m.to_string());
    let Some((units, spec)) = header.split_once('=') else { return Err(malformed("Malformed range header.")) };
    if units.trim().to_lowercase() != "bytes" {
        return Err(malformed("Only support bytes range"));
    }
    if spec.matches(',').count() + 1 > 100 {
        return Ok(vec![]);
    }
    let size_i = size as i64;
    let mut ranges: Vec<(i64, i64)> = vec![];
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() || part == "-" || !part.contains('-') {
            continue;
        }
        let (a, b) = part.split_once('-').unwrap();
        let (a, b) = (a.trim(), b.trim());
        let parsed = (|| -> Option<(i64, i64)> {
            let start = if !a.is_empty() { py::py_int(a).ok()? } else { (size_i - py::py_int(b).ok()?).max(0) };
            let end = if !a.is_empty() && !b.is_empty() && py::py_int(b).ok()? < size_i { py::py_int(b).ok()? + 1 } else { size_i };
            Some((start, end))
        })();
        if let Some(r) = parsed {
            ranges.push(r);
        }
    }
    if ranges.is_empty() {
        return Err(malformed("Range header: range must be requested"));
    }
    if ranges.iter().any(|(s, _)| !(0 <= *s && *s < size_i)) {
        return Err(RangeError::Unsatisfiable);
    }
    if ranges.iter().any(|(s, e)| s >= e) {
        return Err(malformed("Range header: start must be less than end"));
    }
    if ranges.len() > 1 {
        ranges.sort();
        let mut merged: Vec<(i64, i64)> = vec![ranges[0]];
        for (s, e) in ranges.into_iter().skip(1) {
            let last = merged.last_mut().unwrap();
            if s <= last.1 {
                last.1 = last.1.max(e);
            } else {
                merged.push((s, e));
            }
        }
        ranges = merged;
    }
    Ok(ranges.into_iter().map(|(s, e)| (s as usize, e as usize)).collect())
}

fn internal_error() -> Response {
    Response::new(500, "text/plain; charset=utf-8", "Internal Server Error")
}

fn get<'m>(form: &'m HashMap<String, String>, key: &str) -> Option<&'m str> {
    form.get(key).map(|s| s.as_str())
}

fn get_or<'m>(form: &'m HashMap<String, String>, key: &str, default: &'m str) -> &'m str {
    get(form, key).unwrap_or(default)
}

/// `date.fromisoformat(s)`, with Python's error messages.
fn fromisoformat(s: &str) -> Result<NaiveDate, String> {
    let invalid = || format!("Invalid isoformat string: {}", py::repr_str(s));
    let b = s.as_bytes();
    if !matches!(b.len(), 7 | 8 | 10) || !s.is_ascii() {
        return Err(invalid());
    }
    let digits = |from: usize, n: usize| -> Option<i64> {
        let part = b.get(from..from + n)?;
        if part.iter().all(|c| c.is_ascii_digit()) {
            std::str::from_utf8(part).ok()?.parse().ok()
        } else {
            None
        }
    };
    let year = digits(0, 4).ok_or_else(invalid)?;
    let sep = b[4] == b'-';
    let mut p = if sep { 5 } else { 4 };
    if b.get(p) == Some(&b'W') {
        p += 1;
        let week = digits(p, 2).ok_or_else(invalid)?;
        p += 2;
        let mut day = 1;
        if p < b.len() {
            if sep {
                if b[p] != b'-' {
                    return Err(invalid());
                }
                p += 1;
            }
            day = digits(p, 1).ok_or_else(invalid)?;
            p += 1;
        }
        if p != b.len() {
            return Err(invalid());
        }
        if !(1..=9999).contains(&year) {
            return Err(format!("year {year} is out of range"));
        }
        let weeks = NaiveDate::from_ymd_opt(year as i32, 12, 28).map(|d| chrono::Datelike::iso_week(&d).week()).unwrap_or(52) as i64;
        if !(1..=weeks).contains(&week) || !(1..=7).contains(&day) {
            return Err(invalid());
        }
        let wd = [chrono::Weekday::Mon, chrono::Weekday::Tue, chrono::Weekday::Wed, chrono::Weekday::Thu, chrono::Weekday::Fri, chrono::Weekday::Sat, chrono::Weekday::Sun][(day - 1) as usize];
        return NaiveDate::from_isoywd_opt(year as i32, week as u32, wd).ok_or_else(invalid);
    }
    let month = digits(p, 2).ok_or_else(invalid)?;
    p += 2;
    if sep {
        if b.get(p) != Some(&b'-') {
            return Err(invalid());
        }
        p += 1;
    }
    let day = digits(p, 2).ok_or_else(invalid)?;
    if p + 2 != b.len() {
        return Err(invalid());
    }
    crate::adapters::common::py_date(year, month, day)
}

fn py_date(s: &str) -> Option<NaiveDate> {
    fromisoformat(s).ok()
}

// ---------------------------------------------------------------------------------------- pages

impl Web {
    fn setup_page(&self, cx: &Cx) -> Out {
        if self.core.vault.initialized() {
            return Ok(redirect("/login", None));
        }
        self.page(cx, "setup.html", vec![])
    }

    fn setup(&self, cx: &Cx) -> Out {
        if self.core.vault.initialized() {
            return Ok(redirect("/login", None));
        }
        let form = self.form(cx)?;
        let text = |k: &str| form.get(k).map(field_str).unwrap_or_default();
        let token = py::strip(&text("setup_token")).to_string();
        if !same(&token, &self.core.setup_token) {
            return self.render(cx, "setup.html", 403, vec![("error", "Code d'installation incorrect : il est affiché dans les logs du conteneur".into())]);
        }
        let (password, confirm) = (text("password"), text("confirm"));
        if password != confirm {
            return self.render(cx, "setup.html", 400, vec![("error", "Les deux mots de passe ne correspondent pas".into())]);
        }
        let name = text("name");
        let recovery = match self.core.setup(&text("email"), Some(&name), &password) {
            Ok(r) => r,
            Err(AppError::Value(e)) => return self.render(cx, "setup.html", 400, vec![("error", e.into())]),
            Err(e) => return Err(e.into()),
        };
        let identity = self.core.identity_for_email(Some(&text("email")))?;
        let anonymous = Cx { req: cx.req, token: None, session: None, cf_email: cx.cf_email.clone(), query: cx.query.clone(), params: vec![] };
        let mut resp = self.page(&anonymous, "recovery.html", vec![("recovery", recovery.into())])?;
        self.start_session(cx.req, &mut resp, identity);
        Ok(resp)
    }

    fn login_page(&self, cx: &Cx) -> Out {
        if !self.core.vault.initialized() {
            return Ok(redirect("/setup", None));
        }
        if cx.me().is_some() && self.core.vault.unlocked() {
            return Ok(redirect("/", None));
        }
        self.page(cx, "login.html", self.login_vars(cx))
    }

    fn login_vars(&self, cx: &Cx) -> Vec<(&'static str, Value)> {
        vec![
            ("legacy", self.core.legacy_login().into()),
            ("cf_email", cx.cf_email.clone().into()),
            ("locked", (!self.core.vault.unlocked()).into()),
        ]
    }

    fn login(&self, cx: &Cx) -> Out {
        let form = self.form(cx)?;
        let text = |k: &str| form.get(k).map(field_str).unwrap_or_default();
        let email = py::strip(&text("email")).to_string();
        let recovery = form.get("recovery").map(field_truthy).unwrap_or(false);
        let outcome: AppResult<Option<Identity>> = if recovery {
            self.core.authenticate_recovery(&text("recovery"))
        } else if self.core.legacy_login() && email.is_empty() {
            self.core.check_password(&text("password"), None).map(|_| None)
        } else {
            self.core.authenticate(&email, &text("password")).map(Some)
        };
        let identity = match outcome {
            Ok(i) => i,
            Err(AppError::InvalidPassword) => {
                let mut vars = self.login_vars(cx);
                vars.insert(0, ("error", "E-mail ou mot de passe incorrect".into()));
                return self.render(cx, "login.html", 401, vars);
            }
            Err(AppError::Throttled(m)) => {
                let mut vars = self.login_vars(cx);
                vars.insert(0, ("error", m.into()));
                return self.render(cx, "login.html", 429, vars);
            }
            Err(e) => return Err(e.into()),
        };
        let target = match &identity {
            None => "/first-user",
            Some(_) if recovery => "/settings?recovered=1",
            Some(_) => "/",
        };
        let mut resp = redirect(target, None);
        self.start_session(cx.req, &mut resp, identity);
        Ok(resp)
    }

    fn first_user_page(&self, cx: &Cx) -> Out {
        if self.core.has_users()? {
            return Ok(redirect("/", None));
        }
        self.page(cx, "first_user.html", vec![("cf_email", cx.cf_email.clone().unwrap_or_default().into())])
    }

    fn first_user(&self, cx: &Cx) -> Out {
        let form = self.verified_form(cx)?;
        let email = get_or(&form, "email", "").to_string();
        match self.core.create_first_user(&email, get(&form, "name")) {
            Ok(identity) => {
                self.sessions.set_identity(cx.token.as_deref().unwrap_or(""), Some(identity));
                msg("/", "Compte administrateur créé : les comptes et documents existants sont à toi")
            }
            Err(AppError::Value(e)) => self.render(cx, "first_user.html", 400, vec![("error", e.into()), ("cf_email", email.into())]),
            Err(e) => Err(e.into()),
        }
    }

    fn logout(&self, cx: &Cx) -> Out {
        let form = self.verified_form(cx)?;
        let me = cx.me();
        self.sessions.discard(cx.token.as_deref());
        let lock = get(&form, "lock") == Some("1") && me.map(|m| m.is_admin).unwrap_or(false) && self.settings.unlock_password_file.as_deref().map(|p| p.is_empty()).unwrap_or(true);
        if lock {
            self.core.vault.lock();
        }
        let mut resp = if cx.cf_email.is_some() && !lock {
            redirect_response(self.cf.logout_url(), 303)
        } else {
            redirect("/login", Some(if lock { "Coffre verrouillé" } else { "Déconnecté" }))
        };
        resp.add("set-cookie", format!("{COOKIE}=\"\"; expires={}; Max-Age=0; Path=/; SameSite=lax", http_date(chrono::Utc::now())));
        Ok(resp)
    }

    // ----------------------------------------------------------------------------------- dashboard

    fn account_values(&self, values: &[crate::services::valuation::AccountValue]) -> AppResult<Vec<Value>> {
        self.core.session(|c| {
            values
                .iter()
                .map(|v| {
                    let owner = match v.account.owner_id {
                        Some(id) => User::get(c, id)?.map(|u| views::user(&u)).unwrap_or(Value::None),
                        None => Value::None,
                    };
                    let holdings: Vec<Value> = v.holdings.iter().map(|h| views::holding(&h.holding)).collect();
                    let account = views::account_with(&v.account, owner, holdings);
                    let hv: Vec<Value> = v
                        .holdings
                        .iter()
                        .map(|h| Value::obj("HoldingValue", vec![("holding", views::holding(&h.holding)), ("unit_price_eur", h.unit_price_eur.into()), ("value_eur", h.value_eur.into())]))
                        .collect();
                    Ok(Value::obj(
                        "AccountValue",
                        vec![
                            ("account", account),
                            ("cash_eur", v.cash_eur.into()),
                            ("holdings", Value::list(hv)),
                            ("warnings", Value::list(v.warnings.iter().map(|w| w.clone().into()).collect())),
                            ("total", v.total().into()),
                        ],
                    ))
                })
                .collect()
        })
    }

    /// The wealth line of the dashboard: the same reconstruction as the analyses, for the period chosen.
    fn wealth_chart(&self, accounts: &[crate::services::valuation::AccountValue], periode: &str, with_loans: bool) -> AppResult<String> {
        let today = rates::today();
        self.core.session(|c| {
            let mut histories: HashMap<i64, analytics::History> = HashMap::new();
            let mut by_id: HashMap<i64, Account> = HashMap::new();
            let mut ids: Vec<i64> = vec![];
            for v in accounts {
                if !with_loans && v.account.category() == "dettes" {
                    continue; // a loan is not part of what one owns
                }
                let holdings: Vec<Holding> = v.holdings.iter().map(|h| h.holding.clone()).collect();
                histories.insert(v.account.id, analytics::account_history(c, &v.account, &holdings, v.total(), today)?);
                by_id.insert(v.account.id, v.account.clone());
                ids.push(v.account.id);
            }
            let start = analytics::period_start(periode, today, analytics::earliest_day(&histories, &ids, today));
            let series = analytics::build_series(&histories, &by_id, &ids, "total", start, today);
            Ok(charts::line_chart(&series, "Évolution du patrimoine"))
        })
    }

    fn dashboard(&self, cx: &Cx) -> Out {
        let periode = cx.q("periode").filter(|p| analytics::PERIODS.iter().any(|(k, _, _)| k == p)).unwrap_or("1a").to_string();
        let with_loans = cx.q("dettes") == Some("1"); // the wealth line, loans included
        let portfolio = self.core.portfolio(true)?;
        let (to_identify, gaps) = self.core.session(|c| Ok((contact_service::unresolved(c, false, None, None)?.len(), coverage_service::all_gaps(c, false)?)))?;
        let values = self.account_values(&portfolio.accounts)?;
        // accounts whose rate only the user knows
        let editable = self.core.session(|c| access::editable_ids(c))?;
        let need_rate: Vec<Value> = portfolio
            .accounts
            .iter()
            .zip(values.iter())
            .filter(|(v, _)| rates::needs_rate(&v.account.account_type, v.account.interest_rate) && editable.as_ref().map(|e| e.contains(&v.account.id)).unwrap_or(true))
            .filter_map(|(_, value)| value.attr("account"))
            .collect();
        let mut groups: Vec<(&str, Vec<Value>)> = vec![];
        for (v, value) in portfolio.accounts.iter().zip(values.iter()) {
            let cat = v.account.category();
            match groups.iter_mut().find(|(c, _)| *c == cat) {
                Some((_, list)) => list.push(value.clone()),
                None => groups.push((cat, vec![value.clone()])),
            }
        }
        let groups: Vec<Value> = CATEGORIES
            .iter()
            .filter_map(|(code, label)| groups.iter().find(|(c, _)| c == code).map(|(_, list)| Value::tuple(vec![Value::str(code), Value::str(label), Value::list(list.clone())])))
            .collect();
        let p = Value::obj(
            "Portfolio",
            vec![
                ("accounts", Value::list(values)),
                ("total", portfolio.total.into()),
                ("by_category", Value::list(portfolio.by_category.iter().map(|(c, l, v)| Value::tuple(vec![Value::str(c), Value::str(l), (*v).into()])).collect())),
                ("assets", portfolio.assets.into()),
                ("debts", portfolio.debts.into()),
                ("loans", portfolio.loans.into()),
                ("wealth", portfolio.wealth.into()),
                ("history", Value::list(portfolio.history.iter().map(|(d, v)| Value::tuple(vec![(*d).into(), (*v).into()])).collect())),
            ],
        );
        self.page(
            cx,
            "dashboard.html",
            vec![
                ("portfolio", p),
                ("groups", Value::list(groups)),
                ("need_rate", Value::list(need_rate)),
                ("chart", self.wealth_chart(&portfolio.accounts, &periode, with_loans)?.into()),
                ("periode", periode.clone().into()),
                ("with_loans", with_loans.into()),
                ("periods", Value::list(analytics::PERIODS.iter().map(|(code, label, _)| Value::tuple(vec![Value::str(code), Value::str(label), Value::str(format!("/?periode={code}{}", if with_loans { "&dettes=1" } else { "" }))])).collect())),
                ("to_identify", to_identify.into()),
                ("gaps", Value::list(gaps.iter().map(views::gap).collect())),
            ],
        )
    }

    // ----------------------------------------------------------------------------------------- gaps

    fn gap_acknowledge(&self, cx: &Cx) -> Out {
        let form = self.verified_form(cx)?;
        let target = safe_next(get(&form, "next"), "/");
        let parsed: Result<(i64, NaiveDate, NaiveDate), Option<String>> = (|| {
            let id = form.get("account_id").ok_or(None)?;
            let id = py::py_int(id).map_err(Some)?;
            let start = form.get("start").ok_or(None)?;
            let start = fromisoformat(start).map_err(Some)?;
            let end = form.get("end").ok_or(None)?;
            let end = fromisoformat(end).map_err(Some)?;
            Ok((id, start, end))
        })();
        let (id, start, end) = match parsed {
            Ok(v) => v,
            Err(m) => return msg(&target, m.filter(|m| !m.is_empty()).unwrap_or_else(|| "Période invalide".into())),
        };
        match self.core.session(|c| coverage_service::acknowledge(c, id, start, end, get(&form, "note"))) {
            Ok(()) => msg(&target, "Noté : ce trou n'est plus signalé (tu peux le remettre en alerte depuis la page du compte)"),
            Err(AppError::Value(m)) => msg(&target, if m.is_empty() { "Période invalide".to_string() } else { m }),
            Err(e) => Err(e.into()),
        }
    }

    fn gap_restore(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let ack_id = p.path_int(0, "ack_id");
        p.check()?;
        let form = self.verified_form(cx)?;
        self.core.session(|c| coverage_service::restore(c, ack_id))?;
        msg(&safe_next(get(&form, "next"), "/"), "Ce trou est de nouveau signalé")
    }
}

fn field_str(f: &Field) -> String {
    match f {
        Field::Text(t) => t.clone(),
        Field::File(u) => format!("UploadFile(filename={}, size={}, headers=Headers({{}}))", py::repr_str(&u.filename), u.data.len()),
    }
}

fn field_truthy(f: &Field) -> bool {
    match f {
        Field::Text(t) => !t.is_empty(),
        Field::File(_) => true,
    }
}

// ------------------------------------------------------------------------------------- analytics

impl Web {
    fn analytics_page(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let c_ids = p.query_int_list("c");
        let interne = p.query_int("interne", 0);
        let sans_dettes = p.query_int("sans_dettes", 0);
        let avec_dettes = p.query_int("avec_dettes", 0); // the total line with the loans (net wealth)
        p.check()?;
        let mut vue = cx.q("vue").unwrap_or("evolution").to_string();
        if vue == "flux" {
            vue = "depenses".into(); // the monthly flows now sit on the spending page
        }
        if !ANALYTICS_VIEWS.iter().any(|(k, _)| *k == vue) {
            vue = "evolution".into();
        }
        // two dates from the calendar replace the preset periods
        let du = py_date(&cx.qs("du"));
        let au = py_date(&cx.qs("au"));
        let custom = du.is_some() || au.is_some();
        let mut periode = cx.q("periode").unwrap_or("1a").to_string();
        if !analytics::PERIODS.iter().any(|(k, _, _)| *k == periode) {
            periode = "1a".into();
        }
        if custom {
            periode = "perso".into();
        }
        let mut sens = cx.q("sens").unwrap_or("sorties").to_string();
        if sens != "sorties" && sens != "entrees" {
            sens = "sorties".into();
        }
        let direction = if sens == "sorties" { "out" } else { "in" };
        let mut cat = cx.qs("cat");
        if !(category_service::get(&cat).is_some() && vue == "depenses") {
            cat = String::new();
        }
        let mut mois = cx.qs("mois");
        let month_range = (|| {
            let parts: Vec<&str> = mois.split('-').collect();
            if parts.len() != 2 {
                return None;
            }
            let (year, month) = (py::py_int(parts[0]).ok()?, py::py_int(parts[1]).ok()?);
            if !(1..=9999).contains(&year) || !(1..=12).contains(&month) {
                return None;
            }
            let first = NaiveDate::from_ymd_opt(year as i32, month as u32, 1)?;
            let (ny, nm) = (year + month / 12, month % 12 + 1);
            if ny > 9999 {
                return None;
            }
            Some((first, NaiveDate::from_ymd_opt(ny as i32, nm as u32, 1)? - Duration::days(1)))
        })();
        if month_range.is_none() {
            mois = String::new();
        }
        let groups = analytics_groups(&vue);
        let par = cx.qs("par");
        let par = if groups.iter().any(|(k, _)| *k == par) { par } else { groups.first().map(|g| g.0.to_string()).unwrap_or_default() };
        let portfolio = self.core.portfolio(false)?;
        let mut order: Vec<i64> = vec![];
        let mut values: HashMap<i64, &crate::services::valuation::AccountValue> = HashMap::new();
        for v in &portfolio.accounts {
            if !values.contains_key(&v.account.id) {
                order.push(v.account.id);
            }
            values.insert(v.account.id, v);
        }
        let accounts: HashMap<i64, Account> = values.iter().map(|(i, v)| (*i, v.account.clone())).collect();
        let mut selected: Vec<i64> = c_ids.into_iter().filter(|i| accounts.contains_key(i)).collect();
        if selected.is_empty() {
            selected = order.clone();
        }
        let everything = selected.iter().collect::<BTreeSet<_>>() == order.iter().collect::<BTreeSet<_>>();
        let today = rates::today();
        let end = au.map(|d| d.min(today)).unwrap_or(today);
        let (du_text, au_text) = (du.map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default(), au.map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default());
        let interne_text = if interne != 0 { interne.to_string() } else { String::new() };
        let url = |over: &[(&str, &str)]| -> String {
            let mut params: Vec<(&str, String)> = vec![
                ("vue", vue.clone()),
                ("periode", if custom { String::new() } else { periode.clone() }),
                ("du", du_text.clone()),
                ("au", au_text.clone()),
                ("par", par.clone()),
                ("interne", interne_text.clone()),
                ("sans_dettes", if sans_dettes != 0 { "1".to_string() } else { String::new() }),
                ("avec_dettes", if avec_dettes != 0 { "1".to_string() } else { String::new() }),
                ("sens", if sens == "sorties" { String::new() } else { sens.clone() }),
                ("cat", cat.clone()),
                ("mois", mois.clone()),
            ];
            for (k, v) in over {
                match params.iter_mut().find(|(pk, _)| pk == k) {
                    Some(slot) => slot.1 = v.to_string(),
                    None => params.push((k, v.to_string())),
                }
            }
            let mut pairs: Vec<(String, String)> = params.into_iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| (k.to_string(), v)).collect();
            if !everything {
                pairs.extend(selected.iter().map(|i| ("c".to_string(), i.to_string())));
            }
            format!("/analytics?{}", py::urlencode(&pairs))
        };
        let (histories, start, flows, reports) = self.core.session(|c| {
            let mut histories: HashMap<i64, analytics::History> = HashMap::new();
            for i in &selected {
                let v = values[i];
                let holdings: Vec<Holding> = v.holdings.iter().map(|h| h.holding.clone()).collect();
                histories.insert(*i, analytics::account_history(c, &accounts[i], &holdings, v.total(), today)?);
            }
            let earliest = analytics::earliest_day(&histories, &selected, today);
            let start = if custom { du.unwrap_or(earliest).max(earliest).min(end) } else { analytics::period_start(&periode, today, earliest) };
            let flows = if vue == "depenses" { analytics::monthly_flows(c, &selected, start, end, interne != 0)? } else { vec![] };
            // both directions side by side; inside a category only the direction it belongs to
            let mut reports: Vec<(&str, analytics::SpendingReport)> = vec![];
            if vue == "depenses" {
                let (rs, re) = month_range.unwrap_or((start, end));
                for dir in ["out", "in"] {
                    if cat.is_empty() || dir == direction {
                        reports.push((dir, analytics::spending_report(c, &selected, rs, re, interne != 0, dir, if cat.is_empty() { None } else { Some(cat.as_str()) })?));
                    }
                }
            }
            Ok((histories, start, flows, reports))
        })?;
        let sel_values: Vec<&crate::services::valuation::AccountValue> = selected.iter().map(|i| values[i]).collect();
        let zero = PyDec::int(0);
        // what each account was worth on the last day shown (today, unless the calendar says otherwise)
        let value_at = |id: i64| -> Decimal {
            if end >= today {
                values[&id].total()
            } else {
                histories.get(&id).and_then(|h| h.range(..=end).next_back().map(|(_, v)| *v)).unwrap_or(Decimal::ZERO)
            }
        };
        // A loan is not part of what one owns: the total, the assets and the variation leave the loans out, which are
        // shown on their own ("Dettes").
        let is_loan = |v: &crate::services::valuation::AccountValue| v.account.category() == "dettes";
        let assets = sel_values.iter().filter(|v| !is_loan(v)).map(|v| value_at(v.account.id)).filter(|t| *t > Decimal::ZERO).fold(PyDec::int(0), |a, t| a.add(&PyDec::from(&t)));
        let overdrafts = sel_values.iter().filter(|v| !is_loan(v)).map(|v| value_at(v.account.id)).filter(|t| *t < Decimal::ZERO).fold(PyDec::int(0), |a, t| a.add(&PyDec::from(&t)));
        let debts = sel_values.iter().filter(|v| is_loan(v)).map(|v| value_at(v.account.id)).fold(PyDec::int(0), |a, t| a.add(&PyDec::from(&t)));
        let hundred = PyDec::int(100);
        let mut rows: Vec<(Value, PyDec, Option<PyDec>)> = vec![];
        let mut row_values = vec![];
        let account_objs = self.core.session(|c| {
            let mut m: HashMap<i64, Value> = HashMap::new();
            for (i, a) in &accounts {
                m.insert(*i, views::account(c, a)?);
            }
            Ok(m)
        })?;
        for v in &sel_values {
            let total = PyDec::from(&value_at(v.account.id));
            let before = histories[&v.account.id].get(&start).map(PyDec::from);
            let share = if !assets.is_zero() && total.cmp(&zero).is_gt() { total.div(&assets).map(|q| q.mul(&hundred)) } else { None };
            let delta = before.as_ref().map(|b| total.sub(b));
            let pct = match &before {
                Some(b) if !b.is_zero() => total.sub(b).div(&b.abs()).map(|q| q.mul(&hundred)),
                _ => None,
            };
            rows.push((account_objs[&v.account.id].clone(), total.clone(), delta.clone()));
            row_values.push(Value::sdict(vec![
                ("account", account_objs[&v.account.id].clone()),
                ("total", Value::pydec(total)),
                ("share", share.map(Value::pydec).unwrap_or(Value::None)),
                ("delta", delta.map(Value::pydec).unwrap_or(Value::None)),
                ("pct", pct.map(Value::pydec).unwrap_or(Value::None)),
            ]));
        }
        let compared: Vec<&(Value, PyDec, Option<PyDec>)> = rows.iter().zip(&sel_values).filter(|(r, v)| r.2.is_some() && !is_loan(v)).map(|(r, _)| r).collect();
        let base = compared.iter().fold(PyDec::int(0), |a, r| a.add(&r.1.sub(r.2.as_ref().unwrap())));
        let variation = if compared.is_empty() {
            Value::None
        } else {
            let delta = compared.iter().fold(PyDec::int(0), |a, r| a.add(r.2.as_ref().unwrap()));
            let pct = if !base.is_zero() { delta.div(&base.abs()).map(|q| Value::pydec(q.mul(&hundred))).unwrap_or(Value::None) } else { Value::None };
            Value::sdict(vec![("delta", Value::pydec(delta)), ("pct", pct)])
        };
        let mut vars: Vec<(&str, Value)> = vec![];
        match vue.as_str() {
            "evolution" => {
                // a loan of several tens of thousands flattens everything else: the debts can be left out of the lines
                let drawn: Vec<i64> = selected
                    .iter()
                    .copied()
                    .filter(|i| {
                        if accounts[i].category() != "dettes" {
                            true
                        } else if par == "total" {
                            avec_dettes != 0 // the total is what is owned; the loans only come in on request
                        } else {
                            sans_dettes == 0
                        }
                    })
                    .collect();
                let series = analytics::build_series(&histories, &accounts, &drawn, &par, start, end);
                let stats = Value::dict(
                    series
                        .iter()
                        .map(|s| {
                            let st = analytics::stats(&s.points);
                            let v = match st {
                                None => Value::None,
                                Some(st) => Value::obj(
                                    "Stats",
                                    vec![
                                        ("first", st.first.into()),
                                        ("last", st.last.into()),
                                        ("low", st.low.into()),
                                        ("high", st.high.into()),
                                        ("delta", st.delta().into()),
                                        ("pct", st.pct().map(Value::pydec).unwrap_or(Value::None)),
                                    ],
                                ),
                            };
                            (Value::str(&s.key), v)
                        })
                        .collect(),
                );
                vars.push(("chart", charts::line_chart(&series, "Évolution").into()));
                vars.push(("series", Value::list(series.iter().map(series_value).collect())));
                vars.push(("stats", stats));
            }
            "dettes" => {
                let loan_ids: Vec<i64> = selected.iter().copied().filter(|i| accounts[i].category() == "dettes").collect();
                // the debt as a positive amount: what is still owed
                let owed = |mut series: Vec<analytics::Series>| -> Vec<analytics::Series> {
                    for s in series.iter_mut() {
                        for point in s.points.iter_mut() {
                            point.1 = py::neg(point.1);
                        }
                    }
                    series
                };
                let mut series = owed(analytics::build_series(&histories, &accounts, &loan_ids, "compte", start, end));
                if loan_ids.len() > 1 {
                    let mut all = owed(analytics::build_series(&histories, &accounts, &loan_ids, "total", start, end));
                    all.append(&mut series);
                    series = all;
                }
                let rows: Vec<Value> = self.core.session(|c| {
                    let mut out = vec![];
                    for id in &loan_ids {
                        let account = &accounts[id];
                        let loan = loan_service::loan_of(c, account.id)?;
                        let next = match &loan {
                            Some(l) => loan_service::build(c, l, Some(today))?.rows.into_iter().find(|r| r.kind == "installment" && r.status == "next"),
                            None => None,
                        };
                        out.push(Value::sdict(vec![
                            ("account", account_objs[id].clone()),
                            ("remaining", Value::pydec(PyDec::from(&value_at(*id)).neg())),
                            ("rate", loan.as_ref().and_then(|l| l.rate).map(Value::from).unwrap_or(Value::None)),
                            ("principal", loan.as_ref().and_then(|l| l.principal).map(Value::from).unwrap_or(Value::None)),
                            ("end_date", loan.as_ref().and_then(|l| l.end_date).map(Value::from).unwrap_or(Value::None)),
                            ("next_date", next.as_ref().and_then(|r| r.date).map(Value::from).unwrap_or(Value::None)),
                            ("next_total", next.as_ref().map(|r| Value::from(r.total)).unwrap_or(Value::None)),
                        ]));
                    }
                    Ok(out)
                })?;
                vars.push(("chart", charts::line_chart(&series, "Capital restant dû").into()));
                vars.push(("loan_rows", Value::list(rows)));
                vars.push(("loan_count", Value::Int(loan_ids.len() as i64)));
            }
            _ => {
                let palette = charts::PALETTE_SIZE;
                let mut sides: Vec<Value> = vec![];
                for (dir, report) in &reports {
                    let out = *dir == "out";
                    let top: Vec<&analytics::CategoryTotal> = report.categories.iter().take(palette - 1).collect();
                    let rest = report.categories.iter().skip(palette - 1).fold(PyDec::int(0), |a, x| a.add(&PyDec::from(&x.total)));
                    let mut ring: Vec<(String, String, Decimal)> = top.iter().map(|x| (x.id.to_string(), x.label.to_string(), x.total)).collect();
                    if !rest.is_zero() {
                        ring.push(("reste".into(), "Autres catégories".into(), rest.to_decimal()));
                    }
                    let sens_param = if out { "" } else { "entrees" };
                    let links = Value::dict(report.categories.iter().map(|x| (Value::str(x.id), Value::str(url(&[("cat", x.id), ("sens", sens_param)])))).collect());
                    sides.push(Value::sdict(vec![
                        ("key", (*dir).into()),
                        ("title", if out { "Dépenses" } else { "Revenus" }.into()),
                        ("where", if out { "Où est parti l'argent" } else { "D'où il vient" }.into()),
                        ("top_title", if out { "Plus gros bénéficiaires" } else { "Plus grosses sources" }.into()),
                        ("css", if out { "out" } else { "in" }.into()),
                        ("donut", if cat.is_empty() { charts::donut(&ring, if out { "Dépenses par catégorie" } else { "Revenus par catégorie" }) } else { String::new() }.into()),
                        ("links", links),
                        ("report", self.report_value(report)?),
                    ]));
                }
                let first = reports.first().map(|(_, r)| r.clone()).unwrap_or_default();
                if !cat.is_empty() {
                    vars.push(("month_chart", charts::month_bars(&first.months, "Par mois", if direction == "out" { "neg" } else { "pos" }).into()));
                    vars.push(("options", Value::list(category_service::options(direction).into_iter().map(category_value).collect())));
                    vars.push(("report", self.report_value(&first)?));
                }
                let mut all_options: Vec<Value> = category_service::options("out").into_iter().map(category_value).collect();
                all_options.extend(category_service::options("in").into_iter().map(category_value));
                vars.push(("options_all", Value::list(all_options)));
                vars.push(("sides", Value::list(sides)));
                vars.push(("flow_chart", charts::flow_chart(&flows, "Entrées et sorties par mois").into()));
                vars.push(("flows", Value::list(flows.iter().map(flow_value).collect())));
                let links: Vec<(Value, Value)> = flows.iter().map(|f| (Value::Date(f.month), Value::str(url(&[("cat", ""), ("sens", ""), ("mois", &f.month.format("%Y-%m").to_string())])))).collect();
                vars.push(("flow_links", Value::dict(links)));
                vars.push(("total_out", first.total_out.into()));
                vars.push(("total_in", first.total_in.into()));
                vars.push(("saved", first.saved().into()));
                vars.push(("all_link", url(&[("cat", "")]).into()));
                vars.push(("month_off_link", url(&[("mois", "")]).into()));
                vars.push(("month_label", month_range.map(|r| r.0.format("%m/%Y").to_string()).unwrap_or_default().into()));
                vars.push(("this_url", url(&[]).into()));
                vars.push(("sens", sens.clone().into()));
                vars.push(("cat", cat.clone().into()));
                vars.push(("mois", mois.clone().into()));
            }
        }
        let selected_set: Vec<Value> = selected.iter().collect::<BTreeSet<_>>().into_iter().map(|i| Value::Int(*i)).collect();
        let mut base_vars: Vec<(&str, Value)> = vec![
            ("vue", vue.clone().into()),
            ("periode", periode.clone().into()),
            ("par", par.clone().into()),
            ("interne", interne.into()),
            ("accounts", Value::list(order.iter().map(|i| account_objs[i].clone()).collect())),
            ("selected", Value::list(selected_set)),
            ("everything", everything.into()),
            ("rows", Value::list(row_values)),
            ("assets", Value::pydec(assets.clone())),
            ("debts", Value::pydec(debts.clone())),
            ("total", Value::pydec(assets.add(&overdrafts))),
            ("variation", variation),
            ("start", start.into()),
            ("end", end.into()),
            ("today", today.into()),
            ("du", du_text.clone().into()),
            ("au", au_text.clone().into()),
            ("custom", custom.into()),
            ("sans_dettes", (sans_dettes != 0).into()),
            ("avec_dettes", (avec_dettes != 0).into()),
            ("net", Value::pydec(assets.add(&overdrafts).add(&debts))),
            ("has_debts", selected.iter().any(|i| accounts[i].category() == "dettes").into()),
            ("views", Value::list(ANALYTICS_VIEWS.iter().map(|(code, label)| Value::tuple(vec![Value::str(code), Value::str(label), url(&[("vue", code), ("par", ""), ("cat", ""), ("mois", ""), ("sens", "")]).into()])).collect())),
            ("periods", Value::list(analytics::PERIODS.iter().map(|(code, label, _)| Value::tuple(vec![Value::str(code), Value::str(label), url(&[("periode", code), ("du", ""), ("au", ""), ("mois", "")]).into()])).collect())),
            ("group_options", Value::list(groups.iter().map(|(code, label)| Value::tuple(vec![Value::str(code), Value::str(label), url(&[("par", code)]).into()])).collect())),
            ("palette", Value::Int(charts::PALETTE_SIZE as i64)),
        ];
        base_vars.extend(vars);
        self.page(cx, "analytics.html", base_vars)
    }

    fn report_value(&self, r: &analytics::SpendingReport) -> AppResult<Value> {
        let merchant = |m: &analytics::MerchantTotal| {
            Value::obj(
                "MerchantTotal",
                vec![
                    ("key", m.key.clone().into()),
                    ("label", m.label.clone().into()),
                    ("contact_id", m.contact_id.into()),
                    ("total", m.total.into()),
                    ("count", m.count.into()),
                    ("category", m.category.into()),
                    ("ruled", m.ruled.into()),
                ],
            )
        };
        let fee_year = |f: &analytics::FeeYear| {
            Value::obj(
                "FeeYear",
                vec![
                    ("account_id", f.account_id.into()),
                    ("account_name", f.account_name.clone().into()),
                    ("document_id", f.document_id.into()),
                    ("period_start", f.period_start.into()),
                    ("period_end", f.period_end.into()),
                    ("charged", f.charged.into()),
                    ("refunded", f.refunded.into()),
                    ("net", f.net.into()),
                    ("interest_paid", f.interest_paid.into()),
                    ("interest_earned", f.interest_earned.into()),
                    ("services", Value::list(f.services.iter().map(Value::from).collect())),
                    ("operations", f.operations.into()),
                    ("operations_count", f.operations_count.into()),
                    ("operations_refunded", f.operations_refunded.into()),
                    ("gap", f.gap().into()),
                ],
            )
        };
        let fees = &r.fees;
        Ok(Value::obj(
            "SpendingReport",
            vec![
                ("direction", r.direction.clone().into()),
                ("total", r.total.into()),
                ("total_out", r.total_out.into()),
                ("total_in", r.total_in.into()),
                ("categories", Value::list(r.categories.iter().map(category_total).collect())),
                ("months", Value::list(r.months.iter().map(|(d, v)| Value::tuple(vec![(*d).into(), (*v).into()])).collect())),
                ("category", r.category.as_ref().map(category_total).unwrap_or(Value::None)),
                ("merchants", Value::list(r.merchants.iter().map(merchant).collect())),
                ("operations", Value::list(r.operations.iter().map(|(t, name)| Value::tuple(vec![views::transaction(t, Value::None), name.clone().into()])).collect())),
                ("top_merchants", Value::list(r.top_merchants.iter().map(merchant).collect())),
                (
                    "fees",
                    Value::obj(
                        "FeesReport",
                        vec![
                            ("operations_total", fees.operations_total.into()),
                            ("operations_count", fees.operations_count.into()),
                            ("operations_refunded", fees.operations_refunded.into()),
                            ("summaries", Value::list(fees.summaries.iter().map(fee_year).collect())),
                            ("summaries_total", fees.summaries_total().into()),
                            ("interest_paid", fees.interest_paid().into()),
                            ("refunded", fees.refunded().into()),
                        ],
                    ),
                ),
                ("saved", r.saved().into()),
            ],
        ))
    }

    fn analytics_categorize(&self, cx: &Cx) -> Out {
        let form = self.verified_form(cx)?;
        let target = safe_next(get(&form, "next"), "/analytics?vue=depenses");
        match self.core.session(|c| category_service::set_rule(c, get_or(&form, "key", ""), get_or(&form, "category", ""))) {
            Ok(()) => {}
            Err(AppError::Value(m)) => return msg(&target, m),
            Err(e) => return Err(e.into()),
        }
        let what = get(&form, "category");
        let label = get(&form, "label").filter(|l| !l.is_empty()).unwrap_or("Ce bénéficiaire");
        if what == Some("auto") {
            return msg(&target, format!("« {label} » : classement automatique rétabli"));
        }
        let cat_label = what.and_then(category_service::get).map(|c| c.label).ok_or_else(|| Fail::Internal(format!("KeyError: {:?}", what)))?;
        msg(&target, format!("« {label} » est maintenant classé dans « {cat_label} » (toutes ses opérations, y compris les prochaines)"))
    }
}

fn series_value(s: &analytics::Series) -> Value {
    Value::obj(
        "Series",
        vec![("key", s.key.clone().into()), ("label", s.label.clone().into()), ("points", Value::list(s.points.iter().map(|(d, v)| Value::tuple(vec![(*d).into(), (*v).into()])).collect()))],
    )
}

fn flow_value(f: &analytics::MonthFlow) -> Value {
    Value::obj("MonthFlow", vec![("month", f.month.into()), ("income", f.income.into()), ("expense", f.expense.into()), ("net", f.net().into())])
}

fn category_value(c: &category_service::Category) -> Value {
    Value::obj("Category", vec![("id", c.id.into()), ("label", c.label.into()), ("direction", c.direction.into())])
}

fn category_total(c: &analytics::CategoryTotal) -> Value {
    Value::obj(
        "CategoryTotal",
        vec![("id", c.id.into()), ("label", c.label.into()), ("total", c.total.into()), ("count", c.count.into()), ("share", Value::pydec(c.share.clone()))],
    )
}

// -------------------------------------------------------------------------------------- accounts

impl Web {
    fn account_create(&self, cx: &Cx) -> Out {
        let form = self.verified_form(cx)?;
        match self.core.save_account(None, &form) {
            Ok(id) => msg(&format!("/accounts/{id}"), "Compte créé"),
            Err(e) => self.render(cx, "account_form.html", 400, vec![("account", Value::None), ("error", format!("Valeur invalide : {}", str_of(&e)).into())]),
        }
    }

    fn account_detail(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let account_id = p.path_int(0, "account_id");
        let page = p.query_int("page", 1);
        p.check()?;
        let per_page: i64 = 100;
        struct Data {
            vars: Vec<(&'static str, Value)>,
            account: Account,
            holdings: Vec<Holding>,
            loan_rate: Option<Decimal>,
            has_loan: bool,
        }
        let data = self.core.session(|c| {
            let Some(account) = Account::get(c, account_id)? else { return Err(AppError::Other("__404__".into())) };
            let my_role = access::role(c, &account)?;
            let shares_rows: Vec<AccountShare> = user_service::shares_of(c, account_id)?;
            let mut shares = vec![];
            for sh in &shares_rows {
                let u = User::get(c, sh.user_id)?.map(|u| views::user(&u)).unwrap_or(Value::None);
                shares.push(Value::tuple(vec![u, sh.can_edit.into()]));
            }
            let owner = match account.owner_id {
                Some(id) => User::get(c, id)?.map(|u| views::user(&u)).unwrap_or(Value::None),
                None => Value::None,
            };
            let sharable: Vec<Value> = if my_role == "owner" {
                user_service::all_users(c)?
                    .into_iter()
                    .filter(|u| Some(u.id) != account.owner_id && shares_rows.iter().all(|sh| sh.user_id != u.id))
                    .map(|u| views::user(&u))
                    .collect()
            } else {
                vec![]
            };
            let holdings = Holding::query(c, "WHERE account_id = ? ORDER BY id", [account_id])?;
            let total_tx: i64 = c.query_row("SELECT count(*) FROM transactions WHERE account_id = ?", [account_id], |r| r.get(0))?;
            let transactions = Transaction::query(
                c,
                "WHERE account_id = ? ORDER BY date DESC, id DESC LIMIT ? OFFSET ?",
                rusqlite::params![account_id, per_page, (page - 1) * per_page],
            )?;
            let fee_rows = views::fee_summaries(c, &account)?;
            let rules = category_service::load_rules(c)?;
            let mut tx_values = vec![];
            let mut tx_categories = vec![];
            for t in &transactions {
                let cp = match t.counterparty_id {
                    Some(id) => Counterparty::get(c, id)?,
                    None => None,
                };
                let cat = category_service::categorize(t, cp.as_ref().map(|x| x.name.as_str()), &rules);
                tx_categories.push((Value::Int(t.id), Value::str(category_service::get(cat).map(|x| x.label).unwrap_or(""))));
                let cpv = cp.as_ref().map(|x| views::counterparty(x, &[])).unwrap_or(Value::None);
                tx_values.push(views::transaction(t, cpv));
            }
            let loan = Loan::query_one(c, "WHERE account_id = ?", [account_id])?;
            let (loan_value, loan_plan, loan_docs, loan_rate) = match &loan {
                Some(l) => {
                    let mut l = l.clone();
                    let plan = loan_service::rebuild(c, &mut l, None)?;
                    let inst = LoanInstallment::query(c, "WHERE loan_id = ? ORDER BY id", [l.id])?;
                    let events = LoanEvent::query(c, "WHERE loan_id = ? ORDER BY on_date", [l.id])?;
                    let mut ids: Vec<i64> = [l.offer_document_id, l.schedule_document_id].into_iter().flatten().collect();
                    ids.extend(events.iter().filter_map(|e| e.document_id));
                    let mut docs = vec![];
                    for id in ids {
                        if let Some(d) = Document::get(c, id)? {
                            if !docs.iter().any(|(k, _): &(Value, Value)| models_eq(k, d.id)) {
                                docs.push((Value::Int(d.id), views::document(&d)));
                            }
                        }
                    }
                    (views::loan(&l, &inst, &events), views::loan_plan(&plan), Value::dict(docs), l.rate)
                }
                None => (Value::None, Value::None, Value::dict(vec![]), None),
            };
            let (statements, chain_issues) = crate::services::statements::coverage(c, account_id)?;
            let all_gaps = coverage_service::account_gaps(c, &account, true)?;
            let to_identify = contact_service::unresolved(c, false, None, Some(account_id))?;
            let account_after = Account::get(c, account_id)?.unwrap_or(account.clone());
            let vars: Vec<(&'static str, Value)> = vec![
                ("transactions", Value::list(tx_values)),
                ("page", page.into()),
                ("pages", (1i64.max((total_tx + per_page - 1).div_euclid(per_page))).into()),
                ("total_tx", total_tx.into()),
                ("statements", Value::list(statements.iter().map(views::statement).collect())),
                ("chain_issues", Value::list(chain_issues.into_iter().map(Value::from).collect())),
                ("fee_rows", Value::list(fee_rows)),
                ("open_gaps", Value::list(all_gaps.iter().filter(|g| !g.acknowledged()).map(views::gap).collect())),
                ("acked_gaps", Value::list(all_gaps.iter().filter(|g| g.acknowledged()).map(views::gap).collect())),
                ("to_identify", Value::list(to_identify.iter().map(unresolved_value).collect())),
                ("loan", loan_value),
                ("loan_plan", loan_plan),
                ("loan_docs", loan_docs),
                ("role", my_role.into()),
                ("shares", Value::list(shares)),
                ("owner", owner),
                ("sharable", Value::list(sharable)),
                ("tx_categories", Value::dict(tx_categories)),
            ];
            Ok(Data { vars, account: account_after, holdings, loan_rate, has_loan: loan.is_some() })
        });
        let data = match data {
            Err(AppError::Other(m)) if m == "__404__" => return Err(not_found()),
            other => other?,
        };
        let prices = self.core.prices();
        let mut valuator = crate::services::valuation::Valuator::new(&*prices);
        let with = vec![(data.account.clone(), data.holdings.clone())];
        valuator.prefetch(&with);
        let value = valuator.account_value(&data.account, &data.holdings);
        let (mut rate, mut rate_source) = rates::effective_rate(&data.account.account_type, data.account.interest_rate, None);
        if data.has_loan && data.loan_rate.is_some() {
            rate = data.loan_rate;
            rate_source = "lu dans les documents du prêt".into();
        }
        let account_value = self.core.session(|c| views::account(c, &data.account))?;
        let hv: Vec<Value> = value
            .holdings
            .iter()
            .map(|h| Value::obj("HoldingValue", vec![("holding", views::holding(&h.holding)), ("unit_price_eur", h.unit_price_eur.into()), ("value_eur", h.value_eur.into())]))
            .collect();
        let value_obj = Value::obj(
            "AccountValue",
            vec![
                ("account", account_value.clone()),
                ("cash_eur", value.cash_eur.into()),
                ("holdings", Value::list(hv)),
                ("warnings", Value::list(value.warnings.iter().map(|w| w.clone().into()).collect())),
                ("total", value.total().into()),
            ],
        );
        let account_type = data.account.account_type.as_str();
        // the rate as given, and its other side once the taxes of its product are taken into account
        let table = self.core.tax_table();
        let (tax_name, share, tax_ir, tax_social) = rates::tax_for(account_type, &table);
        let (rate_gross, rate_net, taxed) = match rate {
            Some(r) if !data.has_loan => {
                // a rate read from the official pages, or from the table, is a regulated one: tax free, same both ways
                let basis = if data.account.interest_rate.is_some() { data.account.interest_basis.as_deref() } else { None };
                let (g, n) = rates::gross_and_net(r, basis, share);
                (Value::from(g), Value::from(n), !share.is_zero())
            }
            _ => (Value::None, Value::None, false),
        };
        // a rate typed on the account, or one that nobody publishes, is the user's to give and to change; the official
        // rates and a loan's rate come from elsewhere (typing one over them stays possible from the account's form)
        let rate_needed = rates::needs_rate(account_type, data.account.interest_rate) && !data.has_loan;
        let rate_editable = !data.has_loan && (data.account.interest_rate.is_some() || rates::ASK_RATE_TYPES.contains(&account_type));
        let life = self.core.session(|c| Ok(crate::services::life_insurance::summary(c, data.account.id)))?;
        let life_value = match life {
            Some(l) => Value::sdict(vec![
                ("base_date", l.get("base_date").and_then(|d| d.as_str()).and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()).map(Value::from).unwrap_or(Value::None)),
                ("base_from", l.get("base_from").and_then(|d| d.as_str()).unwrap_or("").into()),
                ("letters", l.get("letters").and_then(|d| d.as_i64()).unwrap_or(0).into()),
                ("payments", l.get("payments").and_then(|d| d.as_str()).and_then(py::dec_parse).map(Value::from).unwrap_or(Value::None)),
                ("lines", l.get("lines").and_then(|d| d.as_i64()).unwrap_or(0).into()),
                ("warnings", Value::list(l.get("warnings").and_then(|w| w.as_array()).map(|w| w.iter().filter_map(|x| x.as_str()).map(|x| Value::from(x)).collect()).unwrap_or_default())),
                ("notes", Value::list(l.get("notes").and_then(|w| w.as_array()).map(|w| w.iter().filter_map(|x| x.as_str()).map(|x| Value::from(x)).collect()).unwrap_or_default())),
            ]),
            None => Value::None,
        };
        let mut vars: Vec<(&str, Value)> = vec![("life", life_value), ("account", account_value), ("value", value_obj), ("rate", rate.into()), ("rate_source", rate_source.into()), ("rate_needed", rate_needed.into()), ("rate_editable", rate_editable.into()), ("rate_gross", rate_gross), ("rate_net", rate_net), ("rate_taxed", taxed.into()), ("tax_name", tax_name.into()), ("tax_pct", Value::from(tax_ir + tax_social))];
        vars.extend(data.vars);
        self.page(cx, "account.html", vars)
    }

    fn share_add(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let account_id = p.path_int(0, "account_id");
        p.check()?;
        let form = self.verified_form(cx)?;
        let target = format!("/accounts/{account_id}#partage");
        let outcome = self.core.session(|c| {
            access::check_owner(c, account_id)?;
            let account = Account::get(c, account_id)?.ok_or_else(|| AppError::Other("account".into()))?;
            let uid = py::py_int(get(&form, "user_id").filter(|s| !s.is_empty()).unwrap_or("0")).map_err(AppError::Value)?;
            let Some(target) = User::get(c, uid)? else { return Err(AppError::Value("Choisis la personne avec qui partager".into())) };
            user_service::share(c, &account, &target, get(&form, "mode") == Some("edit"))?;
            Ok(target.label())
        });
        match outcome {
            Ok(label) => msg(&target, format!("Compte partagé avec {label}")),
            Err(AppError::Value(m)) => msg(&target, m),
            Err(e) => Err(e.into()),
        }
    }

    fn share_remove(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let account_id = p.path_int(0, "account_id");
        let user_id = p.path_int(1, "user_id");
        p.check()?;
        self.verified_form(cx)?;
        let me = cx.me().ok_or_else(|| Fail::Internal("no identity".into()))?;
        self.core.session(|c| {
            access::check_read(c, account_id)?;
            if user_id != me.id {
                access::check_owner(c, account_id)?;
            }
            let account = Account::get(c, account_id)?.ok_or_else(|| AppError::Other("account".into()))?;
            user_service::unshare(c, &account, user_id)
        })?;
        if user_id == me.id {
            return msg("/", "Tu n'as plus accès à ce compte");
        }
        msg(&format!("/accounts/{account_id}#partage"), "Partage retiré")
    }

    fn loan_first_due(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let account_id = p.path_int(0, "account_id");
        p.check()?;
        let form = self.verified_form(cx)?;
        let Some(day) = py_date(get_or(&form, "first_due", "")) else { return msg(&format!("/accounts/{account_id}"), "Date invalide") };
        let found = self.core.session(|c| {
            let Some(_account) = Account::get(c, account_id)? else { return Ok(false) };
            let Some(mut loan) = Loan::query_one(c, "WHERE account_id = ?", [account_id])? else { return Ok(false) };
            access::check_edit(c, account_id)?;
            loan_service::set_first_due(c, &mut loan, day)?;
            Ok(true)
        })?;
        if !found {
            return Err(not_found());
        }
        msg(&format!("/accounts/{account_id}"), "Date de la première échéance enregistrée")
    }

    fn account_edit(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let account_id = p.path_int(0, "account_id");
        p.check()?;
        let account = self.core.session(|c| {
            let Some(a) = Account::get(c, account_id)? else { return Ok(None) };
            access::check_edit(c, account_id)?;
            Ok(Some(views::account(c, &a)?))
        })?;
        match account {
            None => Err(not_found()),
            Some(a) => self.page(cx, "account_form.html", vec![("account", a)]),
        }
    }

    fn account_update(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let account_id = p.path_int(0, "account_id");
        p.check()?;
        let form = self.verified_form(cx)?;
        match self.core.save_account(Some(account_id), &form) {
            Ok(_) => msg(&format!("/accounts/{account_id}"), "Compte mis à jour"),
            Err(e) => msg(&format!("/accounts/{account_id}/edit"), format!("Valeur invalide : {}", str_of(&e))),
        }
    }

    /// A card with its accounts and what they are worth.
    fn person_value(&self, c: &rusqlite::Connection, card: &crate::models::PersonCard, totals: &HashMap<i64, Decimal>) -> AppResult<Value> {
        let accounts = people_service::accounts_of(c, card.id)?;
        let mut sum = Decimal::ZERO;
        let mut rows = vec![];
        for a in &accounts {
            let total = totals.get(&a.id).copied().unwrap_or(a.balance);
            if !a.archived {
                sum += total;
            }
            let joint = crate::services::holders::of_account(a).len() > 1;
            rows.push(Value::sdict(vec![("account", views::account_with(a, Value::None, vec![])), ("total", total.into()), ("joint", joint.into())]));
        }
        Ok(Value::sdict(vec![
            ("card", views::person_card(card)),
            ("accounts", Value::list(rows)),
            ("total", sum.into()),
            ("ids", Value::list(accounts.iter().filter(|a| !a.archived).map(|a| Value::Int(a.id)).collect())),
            ("editable", people_service::can_edit(c, card)?.into()),
        ]))
    }

    fn people_totals(&self) -> AppResult<HashMap<i64, Decimal>> {
        let portfolio = self.core.portfolio(false)?;
        Ok(portfolio.accounts.iter().map(|v| (v.account.id, v.total())).collect())
    }

    fn people_page(&self, cx: &Cx) -> Out {
        let totals = self.people_totals()?;
        let rows = self.core.session(|c| {
            people_service::backfill(c)?; // accounts typed before the cards existed get theirs
            let cards = people_service::visible(c)?;
            cards.iter().map(|k| self.person_value(c, k, &totals)).collect::<AppResult<Vec<_>>>()
        })?;
        self.page(cx, "people.html", vec![("rows", Value::list(rows))])
    }

    fn person_page(&self, cx: &Cx, id: Option<i64>) -> Out {
        let totals = self.people_totals()?;
        let found = self.core.session(|c| match id {
            Some(id) => {
                let card = people_service::get_visible(c, id).map_err(|_| AppError::Other("__404__".into()))?;
                Ok((self.person_value(c, &card, &totals)?, crate::services::holders::form_blocks(&[people_service::person_of(&card)], 1)))
            }
            None => Ok((Value::None, crate::services::holders::form_blocks(&[], 1))),
        });
        let (value, blocks) = match found {
            Err(AppError::Other(m)) if m == "__404__" => return Err(not_found()),
            other => other?,
        };
        self.page(cx, "person.html", vec![("person", value), ("blocks", Value::from(&blocks))])
    }

    fn person_detail(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let id = p.path_int(0, "person_id");
        p.check()?;
        self.person_page(cx, Some(id))
    }

    /// The first person of the form (`holder_0_*`) is the card's content.
    fn card_form(form: &HashMap<String, String>) -> Option<crate::services::holders::Person> {
        crate::services::holders::from_form(form).and_then(|v| v.into_iter().next()).filter(|p| !p.is_empty())
    }

    fn person_create(&self, cx: &Cx) -> Out {
        let form = self.verified_form(cx)?;
        let Some(p) = Self::card_form(&form) else { return msg("/people/new", "Indique au moins un nom") };
        let id = self.core.session(|c| {
            let mut card = crate::models::PersonCard { owner_id: access::owner_for_new(c)?, notes: Some(py::strip(get_or(&form, "notes", "")).to_string()).filter(|s| !s.is_empty()), ..Default::default() };
            people_service::apply(&mut card, &p);
            Ok(card.insert(c)?)
        })?;
        msg(&format!("/people/{id}"), "Personne enregistrée")
    }

    fn person_update(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let id = p.path_int(0, "person_id");
        p.check()?;
        let form = self.verified_form(cx)?;
        let Some(person) = Self::card_form(&form) else { return msg(&format!("/people/{id}"), "Indique au moins un nom") };
        let done = self.core.session(|c| {
            let mut card = people_service::get_visible(c, id)?;
            if !people_service::can_edit(c, &card)? {
                return Err(AppError::Denied("Cette fiche t'est partagée en lecture seule".into()));
            }
            let before = card.clone();
            people_service::apply(&mut card, &person);
            card.notes = Some(py::strip(get_or(&form, "notes", "")).to_string()).filter(|s| !s.is_empty());
            card.save(c, &before)?;
            people_service::sync(c, id)?; // the accounts, and the names of their files, follow
            Ok(())
        });
        match done {
            Ok(()) => msg(&format!("/people/{id}"), "Fiche mise à jour (comptes et fichiers renommés)"),
            Err(e) => Err(e.into()),
        }
    }

    fn person_delete(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let id = p.path_int(0, "person_id");
        p.check()?;
        self.verified_form(cx)?;
        self.core.session(|c| {
            let card = people_service::get_visible(c, id)?;
            if !people_service::can_edit(c, &card)? {
                return Err(AppError::Denied("Cette fiche t'est partagée en lecture seule".into()));
            }
            people_service::delete(c, id)
        })?;
        msg("/people", "Fiche supprimée (les comptes gardent les noms, sans lien)")
    }

    /// "Taux à renseigner": the rate of an account, typed (a comma or a point), or "sans intérêt" (0 %).
    fn account_rate(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let account_id = p.path_int(0, "account_id");
        p.check()?;
        let form = self.verified_form(cx)?;
        let back = safe_next(get(&form, "next"), &format!("/accounts/{account_id}"));
        let rate = if get(&form, "none").is_some() {
            Some(Decimal::ZERO)
        } else {
            let text = py::strip(&get_or(&form, "rate", "").replace(',', ".").replace('%', "")).to_string();
            crate::adapters::common::py_decimal(&text).filter(|r| *r >= Decimal::ZERO && *r <= Decimal::from(100))
        };
        let Some(rate) = rate else { return msg(&back, "Taux invalide : écris par exemple 2,5") };
        match self.core.set_rate(account_id, rate, get(&form, "basis")) {
            Ok(()) => msg(&back, if rate.is_zero() { "Compte noté sans intérêt".to_string() } else { format!("Taux enregistré : {} %", py::dec_str(&rate).replace('.', ",")) }),
            Err(e) => Err(e.into()),
        }
    }

    // ------------------------------------------------------------------------------------------ tax table

    fn tax_page(&self, cx: &Cx) -> Out {
        self.admin_only(cx)?;
        let table = self.core.tax_table();
        let at = crate::services::prices::now();
        let online = crate::services::public_rates::current();
        let differences = crate::services::fiscal::differences(&table, online.as_ref(), at);
        let defaults = crate::services::fiscal::defaults();
        let rows: Vec<Value> = table
            .rows
            .iter()
            .map(|r| {
                let default = defaults.iter().find(|d| d.key == r.key);
                Value::sdict(vec![
                    ("key", r.key.into()),
                    ("label", r.label.into()),
                    ("note", r.note.into()),
                    ("ir", r.ir.map(Value::from).unwrap_or(Value::None)),
                    ("social", Value::from(r.social)),
                    ("total", r.total().map(Value::from).unwrap_or(Value::None)),
                    ("flat", r.ir.is_some().into()),
                    ("edited", table.edited.contains(&r.key).into()),
                    ("default_ir", default.and_then(|d| d.ir).map(Value::from).unwrap_or(Value::None)),
                    ("default_social", default.map(|d| Value::from(d.social)).unwrap_or(Value::None)),
                ])
            })
            .collect();
        let when = |t: Option<f64>| t.and_then(|t| chrono::DateTime::from_timestamp(t as i64, 0)).map(|d| d.with_timezone(&chrono::Local).format("%d/%m/%Y").to_string()).unwrap_or_default();
        self.page(
            cx,
            "tax.html",
            vec![
                ("rows", Value::list(rows)),
                ("year", (table.year as i64).into()),
                ("reviewed_at", when(table.reviewed_at).into()),
                ("days", table.days_since_review(at).map(Value::Int).unwrap_or(Value::None)),
                ("due", table.due(at).into()),
                ("differences", Value::list(differences.into_iter().map(Value::from).collect())),
                ("source_url", crate::services::fiscal::SOURCE_URL.into()),
                ("source_name", crate::services::fiscal::SOURCE_NAME.into()),
            ],
        )
    }

    fn tax_save(&self, cx: &Cx) -> Out {
        let form = self.verified_form(cx)?;
        self.admin_only(cx)?;
        let current = self.core.tax_table();
        let mut table = crate::services::fiscal::from_form(&current, &form.into_iter().collect());
        // saving the figures after looking at them is a check
        table.reviewed_at = Some(crate::services::prices::now());
        table.snoozed_until = None;
        self.core.save_tax_table(&table)?;
        msg("/settings/tax", "Taux fiscaux enregistrés et marqués comme vérifiés")
    }

    fn tax_reviewed(&self, cx: &Cx) -> Out {
        let form = self.verified_form(cx)?;
        self.admin_only(cx)?;
        let mut table = self.core.tax_table();
        table.reviewed_at = Some(crate::services::prices::now());
        table.snoozed_until = None;
        self.core.save_tax_table(&table)?;
        msg(&safe_next(get(&form, "next"), "/settings/tax"), "Taux fiscaux confirmés : prochain rappel dans un mois")
    }

    fn tax_snooze(&self, cx: &Cx) -> Out {
        let form = self.verified_form(cx)?;
        self.admin_only(cx)?;
        let mut table = self.core.tax_table();
        table.snoozed_until = Some(crate::services::prices::now() + crate::services::fiscal::SNOOZE_SECONDS);
        self.core.save_tax_table(&table)?;
        Ok(redirect(&safe_next(get(&form, "next"), "/"), None))
    }

    fn tax_reset(&self, cx: &Cx) -> Out {
        self.verified_form(cx)?;
        self.admin_only(cx)?;
        let mut table = crate::services::fiscal::TaxTable::default();
        table.reviewed_at = self.core.tax_table().reviewed_at;
        self.core.save_tax_table(&table)?;
        msg("/settings/tax", "Valeurs de 2026 rétablies")
    }

    fn rates_refresh(&self, cx: &Cx) -> Out {
        self.verified_form(cx)?;
        let state = crate::services::public_rates::refresh(&self.settings.data_dir, true, crate::services::prices::now(), &crate::services::public_rates::http_get);
        match state.error {
            None => msg("/settings#taux", "Taux réglementés lus sur service-public.gouv.fr"),
            Some(e) => msg("/settings#taux", format!("Lecture impossible ({e}) : la table intégrée s'applique")),
        }
    }

    fn account_quick(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let account_id = p.path_int(0, "account_id");
        p.check()?;
        let form = self.verified_form(cx)?;
        let get = |k: &str| form.get(k).cloned().unwrap_or_default();
        match self.core.quick_edit_account(account_id, &get("name"), &get("institution"), &get("account_type")) {
            Ok(()) => msg("/accounts", "Compte mis à jour"),
            Err(e) => msg("/accounts", format!("Modification impossible : {}", str_of(&e))),
        }
    }

    /// Every account the user can see, with its details, its documents and the quick edits.
    fn accounts_page(&self, cx: &Cx) -> Out {
        let portfolio = self.core.portfolio(false)?;
        let values = self.account_values(&portfolio.accounts)?;
        let (archived, counts, editable) = self.core.session(|c| {
            let archived = access::only_readable(c, Account::query(c, "WHERE archived = 1 ORDER BY institution, name", [])?)?;
            let mut counts: HashMap<i64, i64> = HashMap::new();
            let mut st = c.prepare("SELECT account_id, COUNT(*) FROM documents WHERE account_id IS NOT NULL GROUP BY account_id")?;
            for row in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
                let (id, n) = row?;
                counts.insert(id, n);
            }
            Ok((archived, counts, access::editable_ids(c)?))
        })?;
        let filtre = cx.qs("filtre");
        let keep = |account: &Value| match filtre.as_str() {
            "joints" => account.attr("joint").map(|j| j.truthy()).unwrap_or(false),
            "individuels" => !account.attr("joint").map(|j| j.truthy()).unwrap_or(false),
            _ => true,
        };
        let row = |account: Value, id: i64, total: Value| {
            Value::sdict(vec![
                ("account", account),
                ("total", total),
                ("docs", counts.get(&id).copied().unwrap_or(0).into()),
                ("editable", editable.as_ref().map(|e| e.contains(&id)).unwrap_or(true).into()),
            ])
        };
        let mut groups: Vec<(&str, Vec<Value>)> = vec![];
        for (v, value) in portfolio.accounts.iter().zip(values.iter()) {
            let account = value.attr("account").unwrap_or(Value::None);
            if !keep(&account) {
                continue;
            }
            let r = row(account, v.account.id, v.total().into());
            match groups.iter_mut().find(|(c, _)| *c == v.account.category()) {
                Some((_, list)) => list.push(r),
                None => groups.push((v.account.category(), vec![r])),
            }
        }
        let groups: Vec<Value> = CATEGORIES
            .iter()
            .filter_map(|(code, label)| groups.iter().find(|(c, _)| c == code).map(|(_, list)| Value::tuple(vec![Value::str(code), Value::str(label), Value::list(list.clone())])))
            .collect();
        let archived: Vec<Value> = archived.iter().map(|a| (views::account_with(a, Value::None, vec![]), a)).filter(|(v, _)| keep(v)).map(|(v, a)| row(v, a.id, a.balance.into())).collect();
        self.page(cx, "accounts.html", vec![("groups", Value::list(groups)), ("archived", Value::list(archived)), ("total", portfolio.total.into()), ("filtre", filtre.into())])
    }

    fn account_delete(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let account_id = p.path_int(0, "account_id");
        p.check()?;
        self.verified_form(cx)?;
        self.core.delete_account(account_id)?;
        msg("/", "Compte supprimé")
    }

    fn holding_add(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let account_id = p.path_int(0, "account_id");
        p.check()?;
        let form = self.verified_form(cx)?;
        match self.core.add_holding(account_id, &form) {
            Ok(()) => msg(&format!("/accounts/{account_id}"), "Position ajoutée"),
            Err(e) => msg(&format!("/accounts/{account_id}"), format!("Position invalide : {}", str_of(&e))),
        }
    }

    fn holding_delete(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let holding_id = p.path_int(0, "holding_id");
        p.check()?;
        let form = self.verified_form(cx)?;
        self.core.delete_holding(holding_id)?;
        let account = py::py_int(get_or(&form, "account_id", "0")).map_err(|e| Fail::Internal(e))?;
        msg(&format!("/accounts/{account}"), "Position supprimée")
    }
}

fn models_eq(k: &Value, id: i64) -> bool {
    k.as_int() == Some(id)
}

fn unresolved_value(u: &contact_service::Unresolved) -> Value {
    Value::obj(
        "Unresolved",
        vec![
            ("kind", u.kind.clone().into()),
            ("key", u.key.clone().into()),
            ("name", views::opt_str(&u.name)),
            ("iban", views::opt_str(&u.iban)),
            ("count", u.count.into()),
            ("total_in", u.total_in.into()),
            ("total_out", u.total_out.into()),
            ("last", u.last.into()),
            ("accounts", Value::list(u.accounts.iter().map(|a| Value::Int(*a)).collect())),
        ],
    )
}

// ------------------------------------------------------------------------------------- documents

impl Web {
    fn documents_page(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let account_id = p.query_opt_int("account_id");
        p.check()?;
        let (start, end) = (cx.qs("start"), cx.qs("end"));
        self.core.purge_quarantine()?;
        let mut missing = Value::None;
        if let Some(aid) = account_id.filter(|a| *a != 0) {
            if !start.is_empty() && !end.is_empty() {
                if let (Some(s), Some(e)) = (py_date(&start), py_date(&end)) {
                    missing = self.core.session(|c| {
                        let Some(account) = Account::get(c, aid)? else { return Ok(Value::None) };
                        let visible = if access::restricted() { access::readable_ids(c)?.map(|ids| ids.contains(&account.id)).unwrap_or(true) } else { true };
                        Ok(if visible { Value::sdict(vec![("account", account.name.clone().into()), ("label", coverage_service::describe(s, e).into())]) } else { Value::None })
                    })?;
                }
            }
        }
        self.core.refresh_stale_waiting()?;
        let everything = self.core.session(access::visible_documents)?;
        let names: HashMap<i64, String> = everything.iter().map(|d| (d.id, d.filename.clone())).collect();
        let documents: Vec<Document> = everything.iter().filter(|d| d.status != "duplicate").cloned().collect();
        let aside: Vec<Value> = everything
            .iter()
            .filter(|d| d.status == "duplicate")
            .map(|d| Value::tuple(vec![doc_value(d), d.duplicate_of.and_then(|o| names.get(&o).cloned()).into()]))
            .collect();
        let pending: Vec<Value> = documents.iter().filter(|d| d.status == "parsed").map(doc_value).collect();
        let reasons = if documents.iter().any(|d| d.status == "parsed") {
            Value::dict(self.core.pending_reasons()?.into_iter().map(|(id, r)| (Value::Int(id), Value::str(r))).collect())
        } else {
            Value::dict(vec![])
        };
        let catalog: Vec<Value> = registry::catalog()
            .into_iter()
            .map(|(inst, adapters)| Value::tuple(vec![inst.into(), Value::list(adapters.into_iter().map(adapter_value).collect())]))
            .collect();
        let imported_count = documents.iter().filter(|d| d.status == "imported").count();
        let accounts: HashMap<i64, Account> = self.core.session(|c| Ok(Account::query(c, "", [])?.into_iter().map(|a| (a.id, a)).collect()))?;
        let stale_count = self.core.stale_documents()?.len();
        self.page(
            cx,
            "documents.html",
            vec![
                ("documents", Value::list(documents.iter().map(doc_value).collect())),
                ("aside", Value::list(aside)),
                ("missing", missing),
                ("max_mb", self.settings.max_upload_mb.into()),
                ("catalog", Value::list(catalog)),
                ("tree", tree_with_reliability(&documents, &accounts)),
                ("stale_count", stale_count.into()),
                ("pending", Value::list(pending)),
                ("reasons", reasons),
                ("imported_count", imported_count.into()),
            ],
        )
    }

    fn documents_invalidate(&self, cx: &Cx) -> Out {
        let form = self.form(cx)?;
        self.check_csrf(cx, &form, "Jeton CSRF invalide")?;
        let scope = form.get("scope").map(field_str).unwrap_or_default();
        let (imported, groups) = self.core.session(|c| {
            let imported: Vec<Document> = access::visible_documents(c)?.into_iter().filter(|d| d.status == "imported").collect();
            let accounts: HashMap<i64, Account> = Account::query(c, "", [])?.into_iter().map(|a| (a.id, a)).collect();
            let groups: Vec<(i64, (String, String))> = imported.iter().map(|d| (d.id, (views::institution_of(d, &accounts), views::document_group(d).1))).collect();
            Ok((imported, groups))
        })?;
        let ids: Vec<i64> = if scope == "all" {
            imported.iter().map(|d| d.id).collect()
        } else if let Some(bank) = scope.strip_prefix("bank:") {
            groups.iter().filter(|(_, (b, _))| b == bank).map(|(i, _)| *i).collect()
        } else if let Some(rest) = scope.strip_prefix("kind:") {
            let (bank, kind) = rest.split_once("::").unwrap_or((rest, ""));
            groups.iter().filter(|(_, (b, k))| b == bank && k == kind).map(|(i, _)| *i).collect()
        } else if let Some(id) = scope.strip_prefix("account:") {
            let id = py::py_int(id).unwrap_or(0);
            imported.iter().filter(|d| d.account_id == Some(id)).map(|d| d.id).collect()
        } else if let Some(rest) = scope.strip_prefix("akind:") {
            let (id, kind) = rest.split_once("::").unwrap_or((rest, ""));
            let id = py::py_int(id).unwrap_or(0);
            imported.iter().zip(groups.iter()).filter(|(d, (_, (_, k)))| d.account_id.unwrap_or(0) == id && k == kind).map(|(d, _)| d.id).collect()
        } else {
            let chosen: BTreeSet<i64> = form
                .getlist("ids")
                .into_iter()
                .filter_map(|f| f.text().filter(|t| py::isdigit(t)).and_then(|t| py::py_int(t).ok()))
                .collect();
            if chosen.is_empty() {
                return msg("/documents", "Coche au moins un document à invalider");
            }
            groups.iter().filter(|(i, _)| chosen.contains(i)).map(|(i, _)| *i).collect()
        };
        let count = self.core.invalidate_documents(&ids)?;
        msg(
            "/documents",
            if count > 0 { format!("{count} import(s) invalidé(s) : les fichiers sont conservés et reviennent dans « À valider »") } else { "Aucun import à invalider".to_string() },
        )
    }

    fn documents_download(&self, cx: &Cx) -> Out {
        let form = self.form(cx)?;
        self.check_csrf(cx, &form, "Jeton CSRF invalide")?;
        let mut ids: Vec<i64> = vec![];
        for raw in form.getlist("ids") {
            if let Some(t) = raw.text().filter(|t| py::isdigit(t)) {
                if let Ok(i) = py::py_int(t) {
                    if !ids.contains(&i) {
                        ids.push(i);
                    }
                }
            }
        }
        if ids.is_empty() {
            return msg("/documents", "Coche au moins un document à télécharger");
        }
        let mut files = vec![];
        for i in &ids {
            files.push(self.core.read_document(*i)?);
        }
        if files.len() == 1 {
            let (filename, data) = files.pop().unwrap();
            return Ok(download(&filename, data, "application/octet-stream"));
        }
        if files.iter().map(|(_, d)| d.len()).sum::<usize>() > 400 * 1024 * 1024 {
            return msg("/documents", "Sélection trop volumineuse : télécharge-la en plusieurs fois");
        }
        let archive = views::zip_documents(&files);
        let name = format!("myMoney-documents-{}.zip", rates::today().format("%Y%m%d"));
        Ok(download(&name, archive, "application/zip"))
    }

    fn documents_upload(&self, cx: &Cx) -> Out {
        let form = self.form(cx)?;
        self.check_csrf(cx, &form, "Jeton CSRF invalide")?;
        self.core.purge_quarantine()?;
        let max = self.settings.max_upload_mb;
        let mut outcomes: Vec<(i64, String, Option<serde_json::Map<String, serde_json::Value>>)> = vec![];
        for upload in form.getlist("files") {
            let Field::File(upload) = upload else { continue };
            if upload.data.len() as i64 > max * 1024 * 1024 {
                return msg("/documents", format!("{} dépasse {max} Mo", upload.filename));
            }
            let filename = if upload.filename.is_empty() { "document" } else { upload.filename.as_str() };
            let (doc_id, existing) = self.core.store_document(filename, &upload.data)?;
            if existing {
                if !self.core.document_visible(doc_id)? {
                    return msg("/documents", "Ce fichier est déjà dans le coffre (déposé par quelqu'un d'autre) : rien n'est ajouté");
                }
                outcomes.push((doc_id, "existing".into(), None));
                continue;
            }
            let status = self.core.analyze_document(doc_id, None, Some(upload.data.clone()))?;
            let verdict = if status == "parsed" { self.core.check_duplicate(doc_id)? } else { None };
            let imported = if status == "parsed" && verdict.is_none() { self.core.auto_import(doc_id)? } else { None };
            let state = match verdict {
                Some("identical") => "duplicate".to_string(),
                Some(_) => "differs".to_string(),
                None if imported.is_some() => "imported".to_string(),
                None => status,
            };
            outcomes.push((doc_id, state, imported));
        }
        if outcomes.is_empty() {
            return msg("/documents", "Aucun fichier reçu");
        }
        if outcomes.len() == 1 {
            let (doc_id, state, imported) = outcomes.pop().unwrap();
            return match state.as_str() {
                "existing" => msg(&format!("/documents/{doc_id}"), "Ce fichier est strictement identique à un document déjà déposé : rien n'est ajouté"),
                "duplicate" => msg(&format!("/documents/{doc_id}"), "Doublon : aucune différence avec le fichier déjà importé"),
                "differs" => msg(&format!("/documents/{doc_id}"), "Ce relevé existe déjà mais le fichier diffère : choisis quoi en faire"),
                _ => match imported {
                    Some(result) if !result.is_empty() => msg(&after_import(&result, doc_id), import_message(&result, true)?),
                    _ => Ok(redirect(&format!("/documents/{doc_id}"), None)),
                },
            };
        }
        let count = |k: &str| outcomes.iter().filter(|(_, s, _)| s == k).count();
        let (imported, existing, duplicate, differs) = (count("imported"), count("existing"), count("duplicate"), count("differs"));
        let review = outcomes.len() - imported - existing - duplicate - differs;
        let mut parts = vec![format!("{} fichier(s)", outcomes.len()), format!("{imported} importé(s) automatiquement")];
        if review > 0 {
            parts.push(format!("{review} à valider : bouton « Tout valider » ci-dessous"));
        }
        if duplicate > 0 {
            parts.push(format!("{duplicate} doublon(s) sans différence écarté(s)"));
        }
        if differs > 0 {
            parts.push(format!("{differs} déjà importé(s) mais différent(s) : à trancher"));
        }
        if existing > 0 {
            parts.push(format!("{existing} strictement identique(s) à un fichier déjà déposé"));
        }
        msg("/documents", parts.join(" · "))
    }

    fn documents_rebuild(&self, cx: &Cx) -> Out {
        self.verified_form(cx)?;
        let outcome = self.core.rebuild_imports()?;
        self.page(cx, "rebuild.html", vec![("outcome", Value::from(&serde_json::Value::Object(outcome)))])
    }

    /// "Actualiser à la dernière version": every document read by an older version (validated or not) is read again.
    fn documents_refresh(&self, cx: &Cx) -> Out {
        self.verified_form(cx)?;
        let (done, failed) = self.core.refresh_documents(None)?;
        let mut text = if done > 0 { format!("{done} document(s) actualisé(s) à la dernière version") } else { "Aucun document à actualiser".to_string() };
        if failed > 0 {
            text += &format!(" · {failed} n'ont pas pu être relus (leur ancienne lecture est conservée)");
        }
        msg("/documents", text)
    }

    fn document_refresh(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let doc_id = p.path_int(0, "doc_id");
        p.check()?;
        self.verified_form(cx)?;
        match self.core.refresh_document(doc_id) {
            Ok(_) => msg(&format!("/documents/{doc_id}"), "Document actualisé à la dernière version"),
            Err(e) => msg(&format!("/documents/{doc_id}"), format!("Actualisation impossible : {}", str_of(&e))),
        }
    }

    fn documents_review(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let after = p.query_opt_int("after");
        p.check()?;
        let (nxt, waiting) = self.core.next_pending(after)?;
        match nxt {
            None => {
                let done = format!("Revue terminée{}", if waiting > 0 { format!(" : {waiting} document(s) restent à valider") } else { " : plus rien à valider".into() });
                msg("/documents", done)
            }
            Some(n) => Ok(redirect(&format!("/documents/{n}?review=1"), None)),
        }
    }

    /// In review mode, the next document follows whatever was just done; otherwise the usual page.
    fn after_step(&self, doc_id: i64, form: &HashMap<String, String>, fallback: &str, message: Option<String>) -> Out {
        if get(form, "review").map(|r| !r.is_empty()).unwrap_or(false) {
            let (nxt, waiting) = self.core.next_pending(Some(doc_id))?;
            if let Some(n) = nxt {
                return Ok(redirect(&format!("/documents/{n}?review=1"), message.as_deref()));
            }
            let tail = format!("Revue terminée{}", if waiting > 0 { format!(" : {waiting} document(s) restent à valider") } else { " : plus rien à valider".into() });
            let text = match message.filter(|m| !m.is_empty()) {
                Some(m) => format!("{m} · {tail}"),
                None => tail,
            };
            return msg("/documents", text);
        }
        Ok(redirect(fallback, message.as_deref()))
    }

    fn document_review(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let doc_id = p.path_int(0, "doc_id");
        let review_flag = p.query_int("review", 0);
        p.check()?;
        let Some(review) = self.core.review_document(doc_id)? else { return Err(not_found()) };
        let doc = &review.doc;
        let result = doc.parse_result.clone().filter(|r| crate::models::truthy(r)).unwrap_or(serde_json::json!({}));
        let mut candidates = vec![];
        for c in result.get("candidates").and_then(|c| c.as_array()).cloned().unwrap_or_default() {
            let id = c.get("adapter").and_then(|a| a.as_str()).unwrap_or("");
            if let Ok(a) = registry::get_adapter(id) {
                candidates.push(Value::tuple(vec![adapter_value(a), Value::from(&c)]));
            }
        }
        let (to_identify, original, plans, accounts) = self.core.session(|c| {
            let to_identify = if doc.status == "imported" { contact_service::unresolved(c, false, Some(doc_id), None)? } else { vec![] };
            let original = match doc.duplicate_of {
                Some(o) => Document::get(c, o)?.map(|d| doc_value(&d)).unwrap_or(Value::None),
                None => Value::None,
            };
            let plans = review.plans.iter().map(|p| views::plan(c, p)).collect::<AppResult<Vec<_>>>()?;
            let accounts = review.accounts.iter().map(|a| views::account(c, a)).collect::<AppResult<Vec<_>>>()?;
            Ok((to_identify, original, plans, accounts))
        })?;
        // As in the Python version, `review` then names the review itself (always truthy): every waiting document shows
        // the review bar, whatever the query says.
        let _ = review_flag;
        let queue = if doc.status == "parsed" {
            let (nxt, waiting) = self.core.next_pending(Some(doc_id))?;
            Value::sdict(vec![("waiting", waiting.into()), ("next", nxt.into())])
        } else {
            Value::None
        };
        self.page(
            cx,
            "document.html",
            vec![
                ("doc", doc_value(doc)),
                ("result", Value::from(&result)),
                ("current_reading", self.core.reading_is_current(doc).into()),
                ("plans", Value::list(plans)),
                ("accounts", Value::list(accounts)),
                ("candidates", Value::list(candidates)),
                ("adapters", Value::list(registry::adapters().into_iter().map(adapter_value).collect())),
                ("to_identify", Value::list(to_identify.iter().map(unresolved_value).collect())),
                ("original", original),
                ("queue", queue),
            ],
        )
    }

    fn document_analyze(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let doc_id = p.path_int(0, "doc_id");
        p.check()?;
        let form = self.verified_form(cx)?;
        let adapter_id = get(&form, "adapter").filter(|a| !a.is_empty());
        let outcome: AppResult<()> = (|| {
            if let Some(a) = adapter_id {
                registry::get_adapter(a).map_err(AppError::Value)?;
            }
            if self.core.analyze_document(doc_id, adapter_id, None)? == "parsed" {
                self.core.check_duplicate(doc_id)?;
            }
            Ok(())
        })();
        match outcome {
            Ok(()) => Ok(redirect(&format!("/documents/{doc_id}"), None)),
            Err(e) => msg(&format!("/documents/{doc_id}"), format!("Analyse impossible : {}", str_of(&e))),
        }
    }

    fn document_import(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let doc_id = p.path_int(0, "doc_id");
        let index = p.path_int(1, "index");
        p.check()?;
        let form = self.verified_form(cx)?;
        let outcome = if index < 0 { Err(AppError::Other("list index out of range".into())) } else { self.core.import_statement(doc_id, index as usize, &form) };
        match outcome {
            Ok(result) => {
                let message = import_message(&result, false)?;
                self.after_step(doc_id, &form, &after_import(&result, doc_id), Some(message))
            }
            Err(e) => msg(&format!("/documents/{doc_id}{}", if get(&form, "review").map(|r| !r.is_empty()).unwrap_or(false) { "?review=1" } else { "" }), format!("Import impossible : {}", str_of(&e))),
        }
    }

    fn document_dedup(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let doc_id = p.path_int(0, "doc_id");
        let index = p.path_int(1, "index");
        p.check()?;
        let form = self.verified_form(cx)?;
        let action = get(&form, "action").unwrap_or("");
        let index_ok = |i: i64| if i < 0 { Err(AppError::Other("list index out of range".into())) } else { Ok(i as usize) };
        let outcome: Result<Option<Out>, AppError> = (|| {
            Ok(match action {
                "replace" => {
                    let r = self.core.replace_document(doc_id, index_ok(index)?)?;
                    let text = format!(
                        "« {} » remplacé : {} opération(s) retirée(s), {} ajoutée(s) depuis le nouveau fichier",
                        json_text(&r, "replaced"),
                        json_text(&r, "removed"),
                        json_text(&r, "added")
                    );
                    Some(self.after_step(doc_id, &form, &format!("/accounts/{}", json_text(&r, "account_id")), Some(text)))
                }
                "keep" => {
                    let r = self.core.import_default(doc_id, index_ok(index)?, true)?;
                    let text = format!("Les deux fichiers sont gardés. {}", import_message(&r, false).map_err(|_| AppError::Other("KeyError".into()))?);
                    Some(self.after_step(doc_id, &form, &format!("/accounts/{}", json_text(&r, "account_id")), Some(text)))
                }
                "copy" => {
                    self.core.keep_duplicate(doc_id)?;
                    Some(self.after_step(doc_id, &form, &format!("/documents/{doc_id}"), Some("Copie conservée à côté de l'original (rien n'est importé)".into())))
                }
                "discard" => {
                    self.core.delete_document(doc_id)?;
                    Some(self.after_step(doc_id, &form, "/documents", Some("Nouveau fichier écarté : l'ancien reste en place".into())))
                }
                _ => None,
            })
        })();
        match outcome {
            Ok(Some(out)) => match out {
                Err(Fail::App(e)) => msg(&format!("/documents/{doc_id}"), format!("Impossible : {}", str_of(&e))),
                other => other,
            },
            Ok(None) => msg(&format!("/documents/{doc_id}"), "Action inconnue"),
            Err(e) => msg(&format!("/documents/{doc_id}"), format!("Impossible : {}", str_of(&e))),
        }
    }

    fn document_keep(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let doc_id = p.path_int(0, "doc_id");
        p.check()?;
        self.verified_form(cx)?;
        self.core.keep_duplicate(doc_id)?;
        msg(&format!("/documents/{doc_id}"), "Copie conservée à côté de l'original (rien n'est importé)")
    }

    fn document_undo(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let doc_id = p.path_int(0, "doc_id");
        p.check()?;
        self.verified_form(cx)?;
        let removed = self.core.undo_import(doc_id)?;
        msg(&format!("/documents/{doc_id}"), format!("Import annulé : {removed} opération(s) retirée(s)"))
    }

    fn document_download(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let doc_id = p.path_int(0, "doc_id");
        p.check()?;
        let (filename, data) = self.core.read_document(doc_id)?;
        Ok(download(&filename, data, "application/octet-stream"))
    }

    fn document_delete(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let doc_id = p.path_int(0, "doc_id");
        p.check()?;
        self.verified_form(cx)?;
        self.core.delete_document(doc_id)?;
        msg("/documents", "Document supprimé")
    }
}

fn download(filename: &str, data: Vec<u8>, ctype: &str) -> Response {
    let mut r = Response::new(200, ctype, data);
    r.add("content-disposition", format!("attachment; filename*=UTF-8''{}", py::quote(filename, "/")));
    r
}

fn json_text(m: &serde_json::Map<String, serde_json::Value>, key: &str) -> String {
    match m.get(key) {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Null) | None => "None".into(),
        Some(v) => Value::from(v).to_str(),
    }
}

fn after_import(result: &serde_json::Map<String, serde_json::Value>, doc_id: i64) -> String {
    match result.get("account_id").filter(|v| crate::models::truthy(v)) {
        Some(id) => format!("/accounts/{}", Value::from(id).to_str()),
        None => format!("/documents/{doc_id}"),
    }
}

fn import_message(result: &serde_json::Map<String, serde_json::Value>, auto: bool) -> Result<String, Fail> {
    let kind = result.get("kind").and_then(|k| k.as_str());
    if kind == Some("info") {
        return Ok("Document classé avec tes papiers : il ne contient aucune opération".into());
    }
    let prefix = |p: &str| if auto { p.to_string() } else { String::new() };
    if let Some(k) = kind.filter(|k| loan_service::LOAN_KINDS.contains(k)) {
        let text = match k {
            "loan_offer" => "offre de prêt enregistrée (montant, taux, coût et échéances du contrat)",
            "loan_event" => "remboursement anticipé enregistré, plan du prêt mis à jour",
            _ => "tableau d'amortissement enregistré, plan du prêt mis à jour",
        };
        return Ok(format!("{}{text}", prefix("Classé automatiquement : ")));
    }
    if kind == Some("fees_summary") {
        return Ok(format!("{}récapitulatif des frais {} rattaché au compte", prefix("Classé automatiquement : "), json_text(result, "year")));
    }
    let need = |k: &str| result.get(k).cloned().ok_or_else(|| Fail::Internal(format!("KeyError: '{k}'")));
    let mut text = format!("{} opération(s) ajoutée(s)", Value::from(&need("added")?).to_str());
    let exact = need("skipped_exact")?;
    if crate::models::truthy(&exact) {
        text += &format!(" · {} doublon(s) exact(s) ignoré(s)", Value::from(&exact).to_str());
    }
    let probable = need("skipped_probable")?;
    if crate::models::truthy(&probable) {
        text += &format!(" · {} doublon(s) probable(s) ignoré(s)", Value::from(&probable).to_str());
    }
    Ok(format!("{}{text}", if auto { "Importé automatiquement : " } else { "Import terminé : " }))
}

fn adapter_value(a: &'static dyn crate::adapters::base::Adapter) -> Value {
    Value::obj(
        "Adapter",
        vec![("id", a.id().into()), ("label", a.label().into()), ("description", a.description().into()), ("institution", a.institution().into()), ("kind", a.kind().into())],
    )
}

/// A document as the templates see it, with what `reliability(d)` says about it.
fn doc_value(d: &Document) -> Value {
    views::obj("Document", d.fields(), vec![("__reliability", views::reliability(d))])
}

fn tree_with_reliability(documents: &[Document], accounts: &HashMap<i64, Account>) -> Value {
    // the tree's documents are the same objects as the list: keep their reliability with them
    let tree = views::document_tree(documents, accounts);
    let by_id: HashMap<i64, Value> = documents.iter().map(|d| (d.id, doc_value(d))).collect();
    replace_docs(&tree, &by_id)
}

fn replace_docs(v: &Value, by_id: &HashMap<i64, Value>) -> Value {
    match v {
        Value::Obj(o) if o.class == "Document" => o.attrs.get("id").and_then(|i| i.as_int()).and_then(|i| by_id.get(&i).cloned()).unwrap_or_else(|| v.clone()),
        Value::List(l) => Value::list(l.iter().map(|x| replace_docs(x, by_id)).collect()),
        Value::Tuple(l) => Value::tuple(l.iter().map(|x| replace_docs(x, by_id)).collect()),
        Value::Dict(d) => Value::dict(d.items.values().map(|(k, x)| (k.clone(), replace_docs(x, by_id))).collect()),
        other => other.clone(),
    }
}

// -------------------------------------------------------------------------------------- contacts

impl Web {
    fn contacts_page(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let all = p.query_int("all", 0);
        p.check()?;
        let (summaries, pending) = self.core.session(|c| Ok((contact_service::summaries(c)?, contact_service::unresolved(c, all != 0, None, None)?)))?;
        let summaries: Vec<Value> = summaries
            .iter()
            .map(|s| {
                Value::obj(
                    "ContactSummary",
                    vec![
                        ("contact", views::counterparty(&s.contact, &s.identifiers)),
                        ("count", s.count.into()),
                        ("total_in", s.total_in.into()),
                        ("total_out", s.total_out.into()),
                        ("last", s.last.into()),
                    ],
                )
            })
            .collect();
        self.page(
            cx,
            "contacts.html",
            vec![("summaries", Value::list(summaries)), ("pending", Value::list(pending.iter().map(unresolved_value).collect())), ("include_shops", (all != 0).into())],
        )
    }

    fn contact_identify_page(&self, cx: &Cx) -> Out {
        let contacts = self.core.session(|c| Ok(Counterparty::query(c, "ORDER BY name", [])?))?;
        self.page(
            cx,
            "contact_identify.html",
            vec![
                ("printed", cx.qs("name").into()),
                ("iban", cx.qs("iban").into()),
                ("next", safe_next(Some(cx.q("next").unwrap_or("/contacts")), "/contacts").into()),
                ("contacts", Value::list(contacts.iter().map(|c| views::counterparty(c, &[])).collect())),
            ],
        )
    }

    fn contact_identify(&self, cx: &Cx) -> Out {
        let form = self.verified_form(cx)?;
        let target = safe_next(get(&form, "next"), "/contacts");
        match self.core.session(|c| contact_service::identify(c, &form)) {
            Ok(cp) => msg(&target, format!("« {} » enregistré : les prochains imports le reconnaîtront", cp.name)),
            Err(AppError::Value(m)) => msg(
                &format!(
                    "/contacts/identify?name={}&iban={}&next={}",
                    py::quote(get_or(&form, "printed_name", ""), "/"),
                    py::quote(get_or(&form, "iban", ""), "/"),
                    py::quote(&target, "/")
                ),
                m,
            ),
            Err(e) => Err(e.into()),
        }
    }

    fn contact_create(&self, cx: &Cx) -> Out {
        let form = self.verified_form(cx)?;
        let outcome = self.core.session(|c| {
            let cp = contact_service::save_contact(c, None, &form)?;
            contact_service::add_identifier(c, &cp, "iban", get(&form, "iban"), None)?;
            contact_service::add_identifier(c, &cp, "name", get(&form, "printed_name"), get(&form, "printed_name"))?;
            contact_service::relink(c)?;
            Ok(cp.id)
        });
        match outcome {
            Ok(id) => msg(&format!("/contacts/{id}"), "Contact créé"),
            Err(AppError::Value(m)) => self.render(
                cx,
                "contact.html",
                400,
                vec![("contact", Value::None), ("transactions", Value::list(vec![])), ("others", Value::list(vec![])), ("error", m.into())],
            ),
            Err(e) => Err(e.into()),
        }
    }

    fn contact_detail(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let contact_id = p.path_int(0, "contact_id");
        p.check()?;
        let data = self.core.session(|c| {
            let Some(contact) = Counterparty::get(c, contact_id)? else { return Ok(None) };
            let identifiers = contact_service::identifiers_of(c, contact_id)?;
            let visible = access::readable_ids(c)?;
            let transactions: Vec<Transaction> = Transaction::query(c, "WHERE counterparty_id = ? ORDER BY date DESC, id DESC LIMIT 2000", [contact_id])?
                .into_iter()
                .filter(|t| visible.as_ref().map(|v| v.contains(&t.account_id)).unwrap_or(true))
                .take(200)
                .collect();
            let account_names: Vec<(Value, Value)> = access::only_readable(c, Account::query(c, "", [])?)?.into_iter().map(|a| (Value::Int(a.id), Value::str(&a.name))).collect();
            let others = Counterparty::query(c, "WHERE id != ? ORDER BY name", [contact_id])?;
            Ok(Some(vec![
                ("contact", views::counterparty(&contact, &identifiers)),
                ("transactions", Value::list(transactions.iter().map(|t| views::transaction(t, Value::None)).collect())),
                ("others", Value::list(others.iter().map(|o| views::counterparty(o, &[])).collect())),
                ("account_names", Value::dict(account_names)),
            ]))
        })?;
        match data {
            None => Err(not_found()),
            Some(vars) => self.page(cx, "contact.html", vars),
        }
    }

    fn contact_update(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let contact_id = p.path_int(0, "contact_id");
        p.check()?;
        let form = self.verified_form(cx)?;
        self.core.session(|c| contact_service::save_contact(c, Some(contact_id), &form).map(|_| ()))?;
        msg(&format!("/contacts/{contact_id}"), "Contact enregistré")
    }

    fn contact_add_identifier(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let contact_id = p.path_int(0, "contact_id");
        p.check()?;
        let form = self.verified_form(cx)?;
        let outcome = self.core.session(|c| {
            let cp = Counterparty::get(c, contact_id)?.ok_or_else(|| AppError::Other("'NoneType' object has no attribute 'id'".into()))?;
            let added = contact_service::add_identifier(c, &cp, get_or(&form, "kind", "iban"), get(&form, "value"), get(&form, "value"))?;
            let linked = if added { contact_service::relink(c)? } else { 0 };
            Ok((added, linked))
        });
        match outcome {
            Ok((added, linked)) => msg(&format!("/contacts/{contact_id}"), if added { format!("Identifiant ajouté : {linked} opération(s) rattachée(s)") } else { "Rien à ajouter".into() }),
            Err(AppError::Value(m)) => msg(&format!("/contacts/{contact_id}"), m),
            Err(e) => Err(e.into()),
        }
    }

    fn contact_remove_identifier(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let contact_id = p.path_int(0, "contact_id");
        let identifier_id = p.path_int(1, "identifier_id");
        p.check()?;
        self.verified_form(cx)?;
        self.core.session(|c| contact_service::remove_identifier(c, identifier_id))?;
        msg(&format!("/contacts/{contact_id}"), "Identifiant retiré")
    }

    fn contact_merge(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let contact_id = p.path_int(0, "contact_id");
        p.check()?;
        let form = self.verified_form(cx)?;
        let target = py::py_int(get(&form, "target").filter(|t| !t.is_empty()).unwrap_or("0")).map_err(Fail::Internal)?;
        if target == 0 {
            return msg(&format!("/contacts/{contact_id}"), "Choisis le contact à conserver");
        }
        self.core.session(|c| contact_service::merge(c, contact_id, target))?;
        msg(&format!("/contacts/{target}"), "Contacts fusionnés")
    }

    fn contact_delete(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let contact_id = p.path_int(0, "contact_id");
        p.check()?;
        self.verified_form(cx)?;
        self.core.session(|c| contact_service::delete_contact(c, contact_id))?;
        msg("/contacts", "Contact supprimé (les opérations gardent le nom imprimé par la banque)")
    }
}

// -------------------------------------------------------------------------------------- settings

impl Web {
    fn admin_only(&self, cx: &Cx) -> Result<Identity, Fail> {
        match cx.me() {
            Some(m) if m.is_admin => Ok(m),
            _ => Err(http(403, "Réservé aux administrateurs")),
        }
    }

    fn settings_page(&self, cx: &Cx) -> Out {
        let me = cx.me().ok_or_else(|| Fail::Internal("no identity".into()))?;
        let mut history = rates::rate_history();
        history.sort_by(|a, b| b.0.cmp(&a.0));
        let rates_value = Value::list(
            history
                .iter()
                .map(|(since, r)| Value::tuple(vec![(*since).into(), Value::dict(r.iter().map(|(k, v)| (Value::str(k), (*v).into())).collect())]))
                .collect(),
        );
        let (people, owned) = self.core.session(|c| {
            let people = if me.is_admin { user_service::all_users(c)? } else { vec![] };
            let mut owned = vec![];
            for u in &people {
                let n: i64 = c.query_row("SELECT count(*) FROM accounts WHERE owner_id = ?", [u.id], |r| r.get(0))?;
                owned.push((Value::Int(u.id), Value::Int(n)));
            }
            Ok((people, owned))
        })?;
        let s = &self.settings;
        let settings_value = Value::obj(
            "Settings",
            vec![
                ("cookie_secure", s.cookie_secure.into()),
                ("auto_lock_minutes", s.auto_lock_minutes.into()),
                ("unlock_password_file", s.unlock_password_file.clone().into()),
                ("unlock_email", s.unlock_email.clone().into()),
                ("cf_team_domain", s.cf_team_domain.clone().into()),
                ("cf_audience", s.cf_audience.clone().into()),
                ("trusted_origins", s.trusted_origins.clone().into()),
                ("max_upload_mb", s.max_upload_mb.into()),
                ("argon2_memory_kib", (s.argon2_memory_kib as i64).into()),
                ("argon2_time_cost", (s.argon2_time_cost as i64).into()),
            ],
        );
        let online = crate::services::public_rates::current().unwrap_or_default();
        let when = |t: f64| if t > 0.0 { chrono::DateTime::from_timestamp(t as i64, 0).map(|d| d.with_timezone(&chrono::Local).format("%d/%m/%Y à %H:%M").to_string()).unwrap_or_default() } else { String::new() };
        let online_value = Value::sdict(vec![
            ("rates", Value::dict(online.rates.iter().map(|(k, v)| (Value::str(k), (*v).into())).collect())),
            ("read_at", when(online.read_at).into()),
            ("checked_at", when(online.checked_at).into()),
            ("error", online.error.clone().map(Value::from).unwrap_or(Value::None)),
            ("source", crate::services::public_rates::SOURCE.into()),
        ]);
        let table = self.core.tax_table();
        let at = crate::services::prices::now();
        let interets = table.get("interets").cloned();
        let flat_value = Value::sdict(vec![
            ("total", interets.as_ref().and_then(|r| r.total()).map(Value::from).unwrap_or(Value::None)),
            ("due", table.due(at).into()),
            ("year", (table.year as i64).into()),
        ]);
        let coingecko = !self.core.get_setting("coingecko_api_key", "")?.is_empty();
        self.page(
            cx,
            "settings.html",
            vec![
                ("rates", rates_value),
                ("online_rates", online_value),
                ("flat_tax", flat_value),
                ("settings", settings_value),
                ("people", Value::list(people.iter().map(views::user).collect())),
                ("owned", Value::dict(owned)),
                ("coingecko_key", coingecko.into()),
                ("recovered", cx.q("recovered").map(Value::str).unwrap_or(Value::None)),
                ("cf_enabled", self.cf.enabled().into()),
                ("cf_host", self.cf.host.clone().into()),
            ],
        )
    }

    fn settings_password(&self, cx: &Cx) -> Out {
        let form = self.verified_form(cx)?;
        let me = cx.me().ok_or_else(|| Fail::Internal("no identity".into()))?;
        if get(&form, "new") != get(&form, "confirm") {
            return msg("/settings", "Les deux nouveaux mots de passe ne correspondent pas");
        }
        let slot = user_service::slot_id(&me.email)?;
        match self.core.vault.change_password(get_or(&form, "current", ""), get_or(&form, "new", ""), Some(&slot)).map_err(crate::app::vault_error) {
            Ok(()) => msg("/settings", "Mot de passe changé"),
            Err(AppError::InvalidPassword) => msg("/settings", "Mot de passe actuel incorrect"),
            Err(AppError::Value(m)) => msg("/settings", m),
            Err(e) => Err(e.into()),
        }
    }

    fn settings_recovery(&self, cx: &Cx) -> Out {
        let form = self.verified_form(cx)?;
        let me = self.admin_only(cx)?;
        match self.core.check_password(get_or(&form, "current", ""), Some(&me.email)) {
            Ok(()) => {}
            Err(AppError::InvalidPassword) | Err(AppError::Throttled(_)) => return msg("/settings", "Mot de passe incorrect"),
            Err(e) => return Err(e.into()),
        }
        let recovery = self.core.vault.regenerate_recovery().map_err(|e| Fail::App(crate::app::vault_error(e)))?;
        self.page(cx, "recovery.html", vec![("recovery", recovery.into())])
    }

    fn settings_user_add(&self, cx: &Cx) -> Out {
        let form = self.verified_form(cx)?;
        self.admin_only(cx)?;
        let outcome = self.core.session(|c| {
            let user = user_service::add_with_password(c, &self.core.vault, get_or(&form, "email", ""), get(&form, "name"), get_or(&form, "password", ""), get(&form, "is_admin") == Some("on"))?;
            Ok(user.label())
        });
        match outcome {
            Ok(label) => msg("/settings#utilisateurs", format!("{label} peut maintenant se connecter (par Cloudflare ou avec son mot de passe)")),
            Err(AppError::Value(m)) => msg("/settings#utilisateurs", m),
            Err(e) => Err(e.into()),
        }
    }

    fn settings_user_delete(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let user_id = p.path_int(0, "user_id");
        p.check()?;
        self.verified_form(cx)?;
        let me = self.admin_only(cx)?;
        let outcome = self.core.session(|c| {
            let user = User::get(c, user_id)?;
            let heir = User::get(c, me.id)?;
            let Some(user) = user else { return Err(AppError::Value("Utilisateur introuvable".into())) };
            let heir = heir.ok_or_else(|| AppError::Other("heir".into()))?;
            let label = user.label();
            let moved = user_service::remove(c, &self.core.vault, &user, &heir)?;
            Ok((label, moved))
        });
        match outcome {
            Ok((label, moved)) => {
                self.sessions.drop_user(user_id);
                msg("/settings#utilisateurs", format!("{label} supprimé{}", if moved > 0 { format!(" : ses {moved} compte(s) sont maintenant à toi") } else { String::new() }))
            }
            Err(AppError::Value(m)) => msg("/settings#utilisateurs", m),
            Err(e) => Err(e.into()),
        }
    }

    fn settings_user_role(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let user_id = p.path_int(0, "user_id");
        p.check()?;
        let form = self.verified_form(cx)?;
        self.admin_only(cx)?;
        let make_admin = get(&form, "is_admin") == Some("on");
        let outcome = self.core.session(|c| {
            let Some(mut user) = User::get(c, user_id)? else { return Ok(Err(not_found())) };
            if user.is_admin && !make_admin && user_service::all_users(c)?.iter().filter(|u| u.is_admin).count() <= 1 {
                return Ok(Ok(None));
            }
            let before = user.clone();
            user.is_admin = make_admin;
            user.save(c, &before)?;
            Ok(Ok(Some(user_service::identity(&user))))
        })?;
        match outcome {
            Err(f) => Err(f),
            Ok(None) => msg("/settings#utilisateurs", "Il doit rester au moins un administrateur"),
            Ok(Some(identity)) => {
                self.sessions.update_identity(&identity);
                msg("/settings#utilisateurs", "Rôle modifié")
            }
        }
    }

    fn settings_user_password(&self, cx: &Cx) -> Out {
        let mut p = Params::new(cx);
        let user_id = p.path_int(0, "user_id");
        p.check()?;
        let form = self.verified_form(cx)?;
        self.admin_only(cx)?;
        let password = get_or(&form, "password", "");
        if let Err(m) = crate::security::vault::check_password_strength(password) {
            return msg("/settings#utilisateurs", m);
        }
        let outcome = self.core.session(|c| {
            let Some(user) = User::get(c, user_id)? else { return Ok(None) };
            let master = self.core.vault.master().map_err(|_| AppError::Locked)?;
            let password = form.get("password").ok_or_else(|| AppError::Other("KeyError: 'password'".into()))?;
            self.core.vault.keystore.set_password(&master, password, Some(&user_service::slot_id(&user.email)?)).map_err(|e| crate::app::vault_error(e.into()))?;
            Ok(Some(user.label()))
        });
        match outcome {
            Ok(None) => Err(not_found()),
            Ok(Some(label)) => msg("/settings#utilisateurs", format!("Nouveau mot de passe défini pour {label}")),
            Err(AppError::Value(m)) => msg("/settings#utilisateurs", m),
            Err(e) => Err(e.into()),
        }
    }

    fn settings_general(&self, cx: &Cx) -> Out {
        let form = self.verified_form(cx)?;
        self.admin_only(cx)?;
        if let Some(k) = get(&form, "coingecko_api_key").filter(|k| !k.is_empty()) {
            self.core.set_setting("coingecko_api_key", py::strip(k))?;
        }
        if get(&form, "clear_coingecko") == Some("on") {
            self.core.set_setting("coingecko_api_key", "")?;
        }
        msg("/settings", "Paramètres enregistrés")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ints_like_pydantic() {
        for (s, v) in [("1", Some(1)), (" 2 ", Some(2)), ("1_0", Some(10)), ("+3", Some(3)), ("1.0", Some(1)), ("1.00", Some(1)), ("-0", Some(0)), ("00012", Some(12)), ("\t7\n", Some(7))] {
            assert_eq!(pydantic_int(s), v, "{s:?}");
        }
        for s in ["1.5", "1e2", "0x10", "", " ", "١٢", "1__0", "_1", "1_", "+-1", "1.", ".5", "1 2", "１２", "abc"] {
            assert_eq!(pydantic_int(s), None, "{s:?}");
        }
    }

    #[test]
    fn routes_like_starlette() {
        assert!(matches!(find_route("GET", "/documents/download"), Matched::Full(R::Document, _)));
        assert!(matches!(find_route("POST", "/documents/download"), Matched::Full(R::DocumentsDownload, _)));
        assert!(matches!(find_route("GET", "/accounts/new"), Matched::Full(R::AccountNew, _)));
        assert!(matches!(find_route("DELETE", "/accounts/new"), Matched::Method(_)));
        assert!(matches!(find_route("HEAD", "/"), Matched::Method(_)));
        assert_eq!(fromisoformat("2024-02-30"), Err("day is out of range for month".into()));
        assert_eq!(fromisoformat("2024-13-01"), Err("month must be in 1..12".into()));
        assert_eq!(fromisoformat("0000-01-01"), Err("year 0 is out of range".into()));
        assert_eq!(fromisoformat("2024-1-01"), Err("Invalid isoformat string: '2024-1-01'".into()));
        assert_eq!(fromisoformat("20240105"), Ok(NaiveDate::from_ymd_opt(2024, 1, 5).unwrap()));
        assert_eq!(fromisoformat("2024-W01-1"), Ok(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()));
        assert!(matches!(find_route("GET", "/static/style.css"), Matched::Full(R::Static, _)));
        assert!(matches!(find_route("GET", "/nope"), Matched::None));
        assert_eq!(origin_netloc("http://nas.local:8000"), "nas.local:8000");
        assert_eq!(origin_netloc("nas.local"), "");
        let ip = |s: &str| s.parse::<std::net::IpAddr>().unwrap();
        assert!(proxy_trusted("127.0.0.1", ip("127.0.0.1")));
        assert!(proxy_trusted("10.0.0.1, 172.17.0.0/16", ip("172.17.3.4")));
        assert!(!proxy_trusted("172.17.0.1", ip("172.17.0.2")));
        assert!(proxy_trusted("*", ip("8.8.8.8")));
    }
}
