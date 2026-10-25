//! Application services shared by the web UI and the CLI.

use std::collections::{BTreeSet, HashMap};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration as StdDuration, Instant};

use chrono::{Duration, NaiveDate, NaiveDateTime, TimeZone};
use rand::RngCore;
use rusqlite::Connection;
use rust_decimal::Decimal;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::adapters::base::{ParsedStatement, SourceDocument};
use crate::adapters::registry::{get_adapter, identify, parse_with, reader_version};
use crate::config::Settings;
use crate::db::Database;
use crate::error::{value_err, AppError, AppResult};
use crate::models::{account_type, fmt_datetime, utcnow, Account, Document, Holding, Statement, Transaction, User, CATEGORIES};
use crate::py;
use crate::security::keystore::Keystore;
use crate::security::vault::{Vault, VaultError};
use crate::services::access::{self, Identity};
use crate::services::statements::{self as statement_service, Form, ImportPlan};
use crate::services::{holders, ledger, loans as loan_service, naming, prices::PriceService, users as user_service, valuation::{AccountValue, Valuator}};

/// how long a duplicate set aside waits for the user's decision
pub const QUARANTINE_HOURS: i64 = 24;

pub struct Portfolio {
    pub accounts: Vec<AccountValue>,
    pub total: Decimal,
    pub by_category: Vec<(&'static str, &'static str, Decimal)>,
    pub assets: Decimal,
    pub debts: Decimal,
    pub history: Vec<(NaiveDate, Decimal)>,
}

pub struct Review {
    pub doc: Document,
    pub plans: Vec<ImportPlan>,
    pub accounts: Vec<Account>,
}

struct Throttle {
    failed: u32,
    next_at: Instant,
}

pub struct MyMoney {
    pub settings: Settings,
    pub vault: Arc<Vault>,
    pub db: Arc<Database>,
    pending_prices: Arc<Mutex<Vec<(String, String)>>>,
    prices: Arc<Mutex<Option<Arc<PriceService>>>>,
    stop: Arc<AtomicBool>,
    throttle: Mutex<Throttle>,
    pub setup_token: String,
}

pub fn vault_error(e: VaultError) -> AppError {
    match e {
        VaultError::Invalid => AppError::InvalidPassword,
        VaultError::Weak(m) => AppError::Value(m),
        VaultError::Store(m) | VaultError::Open(m) => AppError::Value(m),
    }
}

pub fn token_urlsafe(n: usize) -> String {
    use base64::Engine;
    let mut bytes = vec![0u8; n];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub fn token_hex(n: usize) -> String {
    let mut bytes = vec![0u8; n];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn statements_of(doc: &Document) -> Vec<Value> {
    doc.parse_result.as_ref().and_then(|r| r.get("statements")).and_then(|s| s.as_array()).cloned().unwrap_or_default()
}

fn result_str(doc: &Document, key: &str) -> Option<String> {
    doc.parse_result.as_ref().and_then(|r| r.get(key)).and_then(|v| v.as_str()).map(|s| s.to_string())
}

/// `str(statements[0].get("period_end") or statements[0].get("period_start") or "")`
pub fn period_key(doc: &Document) -> String {
    let sts = statements_of(doc);
    let first = sts.first().cloned().unwrap_or(Value::Object(Map::new()));
    let pick = |k: &str| first.get(k).filter(|v| crate::models::truthy(v)).cloned();
    match pick("period_end").or_else(|| pick("period_start")) {
        Some(Value::String(s)) => s,
        Some(other) => py::json_dumps(&other),
        None => String::new(),
    }
}

/// Statements first: fees recaps and loan notices wait for the account a statement creates.
fn kind_rank(doc: &Document) -> i64 {
    match doc.kind.as_str() {
        "statement" | "savings_statement" => 0,
        "loan_offer" => 1,
        "fees_summary" | "loan_event" | "loan_schedule" => 2,
        _ => 0,
    }
}

impl MyMoney {
    pub fn new(settings: Settings) -> std::io::Result<Arc<MyMoney>> {
        settings.ensure_dirs()?;
        let keystore = Keystore { path: settings.keystore_path(), time_cost: settings.argon2_time_cost, memory_kib: settings.argon2_memory_kib };
        let vault = Arc::new(Vault::new(keystore));
        let db = Arc::new(Database::new(settings.db_path()));
        let prices: Arc<Mutex<Option<Arc<PriceService>>>> = Arc::new(Mutex::new(None));
        {
            let weak: Weak<Vault> = Arc::downgrade(&vault);
            let db = db.clone();
            vault.on_unlock(Box::new(move || {
                let v = weak.upgrade().ok_or("coffre")?;
                let key = v.subkey("database").map_err(|_| "Coffre verrouillé".to_string())?;
                db.open(&key)
            }));
        }
        {
            let db = db.clone();
            let prices = prices.clone();
            vault.on_lock(Box::new(move || {
                *prices.lock().unwrap() = None;
                db.close();
                Ok(())
            }));
        }
        Ok(Arc::new(MyMoney {
            settings,
            vault,
            db,
            pending_prices: Arc::new(Mutex::new(vec![])),
            prices,
            stop: Arc::new(AtomicBool::new(false)),
            throttle: Mutex::new(Throttle { failed: 0, next_at: Instant::now() }),
            setup_token: token_urlsafe(12),
        }))
    }

    pub fn session<T>(&self, f: impl FnOnce(&Connection) -> AppResult<T>) -> AppResult<T> {
        self.db.session(f)
    }

    /// The tests forget the failed logins (`core._failed_logins, core._next_login_at = 0, 0.0`).
    #[cfg(test)]
    pub fn reset_throttle(&self) {
        let mut t = self.throttle.lock().unwrap();
        t.failed = 0;
        t.next_at = Instant::now();
    }

    // ------------------------------------------------------------ security

    fn check_throttle(&self) -> AppResult<()> {
        let t = self.throttle.lock().unwrap();
        let now = Instant::now();
        if now < t.next_at {
            let left = (t.next_at - now).as_secs_f64() as i64 + 1;
            return Err(AppError::Throttled(format!("Trop d'essais : réessaie dans {left} s")));
        }
        Ok(())
    }

    fn failed(&self) {
        let mut t = self.throttle.lock().unwrap();
        t.failed += 1;
        if t.failed >= 3 {
            let wait = 300f64.min(2f64.powi(t.failed as i32 - 2));
            t.next_at = Instant::now() + StdDuration::from_secs_f64(wait);
        }
    }

    fn succeeded(&self) {
        self.throttle.lock().unwrap().failed = 0;
    }

    /// Verify a password (a user's, or the legacy master password before users exist), with an exponential delay
    /// after failures (brute force). Opens the vault when it is locked: everyone's data is behind the same key.
    pub fn check_password(&self, password: &str, email: Option<&str>) -> AppResult<()> {
        self.check_throttle()?;
        let slot = match email {
            Some(e) => match user_service::slot_id(e) {
                Ok(s) => Some(s),
                Err(_) => {
                    self.failed();
                    return Err(AppError::InvalidPassword);
                }
            },
            None => None,
        };
        let outcome: AppResult<()> = if self.vault.unlocked() {
            match self.vault.keystore.unlock(password, slot.as_deref()) {
                Ok(dek) if Ok(dek.clone()) == self.vault.master().map_err(|_| ()) => Ok(()),
                Ok(_) | Err(_) => Err(AppError::InvalidPassword),
            }
        } else {
            self.vault.unlock(password, slot.as_deref()).map_err(vault_error)
        };
        match outcome {
            Err(AppError::InvalidPassword) => {
                self.failed();
                Err(AppError::InvalidPassword)
            }
            Err(e) => Err(e),
            Ok(()) => {
                self.succeeded();
                Ok(())
            }
        }
    }

    /// The vault predates users: it still has a single password and nobody has an account yet.
    pub fn legacy_login(&self) -> bool {
        self.vault.initialized() && self.vault.keystore.has_legacy_slot()
    }

    /// First installation: the vault, and its first user (the administrator). Returns the recovery code.
    pub fn setup(&self, email: &str, name: Option<&str>, password: &str) -> AppResult<String> {
        let email = user_service::normalize_email(Some(email))?;
        let recovery = self.vault.setup(password, Some(&user_service::slot_id(&email)?)).map_err(vault_error)?;
        self.session(|c| user_service::create(c, &email, name, true).map(|_| ()))?;
        Ok(recovery)
    }

    /// Sign a user in with their own password. Fails with InvalidPassword for a wrong password or an unknown user.
    pub fn authenticate(&self, email: &str, password: &str) -> AppResult<Identity> {
        self.check_password(password, Some(email))?;
        self.session(|c| {
            let Some(mut user) = user_service::get_by_email(c, Some(email))? else { return Err(AppError::InvalidPassword) };
            let before = user.clone();
            user.last_login = Some(utcnow());
            user.save(c, &before)?;
            Ok(user_service::identity(&user))
        })
    }

    /// The recovery code opens the vault and signs in the first administrator (None while nobody has an account).
    pub fn authenticate_recovery(&self, code: &str) -> AppResult<Option<Identity>> {
        self.check_throttle()?;
        if let Err(e) = self.vault.unlock_with_recovery(code) {
            let e = vault_error(e);
            if matches!(e, AppError::InvalidPassword) {
                self.failed();
            }
            return Err(e);
        }
        self.succeeded();
        self.session(|c| Ok(user_service::first_admin(c)?.map(|a| user_service::identity(&a))))
    }

    /// The user an e-mail address belongs to (the vault must be open).
    pub fn identity_for_email(&self, email: Option<&str>) -> AppResult<Option<Identity>> {
        self.session(|c| {
            let Some(mut user) = user_service::get_by_email(c, email)? else { return Ok(None) };
            let before = user.clone();
            user.last_login = Some(utcnow());
            user.save(c, &before)?;
            Ok(Some(user_service::identity(&user)))
        })
    }

    pub fn identity_by_id(&self, user_id: Option<i64>) -> AppResult<Option<Identity>> {
        let Some(id) = user_id else { return Ok(None) };
        self.session(|c| Ok(User::get(c, id)?.map(|u| user_service::identity(&u))))
    }

    pub fn has_users(&self) -> AppResult<bool> {
        self.session(|c| Ok(user_service::count(c)? > 0))
    }

    /// A vault from before users existed: its password becomes the administrator's, and the data becomes theirs.
    pub fn create_first_user(&self, email: &str, name: Option<&str>) -> AppResult<Identity> {
        let email = user_service::normalize_email(Some(email))?;
        self.session(|c| {
            if user_service::count(c)? > 0 {
                return value_err("Il existe déjà des utilisateurs");
            }
            let mut user = user_service::create(c, &email, name, true)?;
            user_service::claim_legacy_data(c, &user)?;
            self.vault.keystore.adopt_legacy_slot(&user_service::slot_id(&email)?).map_err(|e| vault_error(e.into()))?;
            let before = user.clone();
            user.last_login = Some(utcnow());
            user.save(c, &before)?;
            Ok(user_service::identity(&user))
        })
    }

    pub fn auto_unlock(&self) -> AppResult<()> {
        let Some(path) = self.settings.unlock_password_file.clone().filter(|p| !p.is_empty()) else { return Ok(()) };
        if !self.vault.initialized() || self.vault.unlocked() {
            return Ok(());
        }
        let password = py::strip(&std::fs::read_to_string(&path)?).to_string();
        if let Some(email) = self.settings.unlock_email.clone().filter(|e| !e.is_empty()) {
            self.vault.unlock(&password, Some(&user_service::slot_id(&email)?)).map_err(vault_error)?;
        } else if self.vault.keystore.has_legacy_slot() {
            self.vault.unlock(&password, None).map_err(vault_error)?;
        } else {
            // no e-mail given: the password belongs to one of the slots
            let mut opened = false;
            for slot in self.vault.keystore.slots() {
                if self.vault.unlock(&password, Some(&slot)).is_ok() {
                    opened = true;
                    break;
                }
            }
            if !opened {
                return Err(AppError::InvalidPassword);
            }
        }
        log::warn!("Coffre déverrouillé automatiquement depuis {path} (voir SECURITY.md)");
        Ok(())
    }

    pub fn start_background(self: &Arc<Self>, web_sessions_active: Arc<dyn Fn() -> bool + Send + Sync>) {
        let me = Arc::downgrade(self);
        let stop = self.stop.clone();
        std::thread::Builder::new()
            .name("background".into())
            .spawn(move || loop {
                for _ in 0..30 {
                    std::thread::sleep(StdDuration::from_secs(1));
                    if stop.load(Ordering::SeqCst) {
                        return;
                    }
                }
                let Some(core) = me.upgrade() else { return };
                let s = &core.settings;
                if core.vault.unlocked()
                    && s.auto_lock_minutes > 0
                    && s.unlock_password_file.as_deref().map(|p| p.is_empty()).unwrap_or(true)
                    && !web_sessions_active()
                    && core.vault.idle_seconds() > (s.auto_lock_minutes * 60) as f64
                {
                    log::info!("Verrouillage automatique après inactivité");
                    core.vault.lock();
                }
            })
            .ok();
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    // -------------------------------------------------------------- prices

    pub fn prices(&self) -> Arc<PriceService> {
        let mut guard = self.prices.lock().unwrap();
        if let Some(p) = guard.as_ref() {
            return p.clone();
        }
        let db = self.db.clone();
        let cache_get = Box::new(move |key: &str| -> Option<(String, f64)> {
            db.read(|c| Ok(crate::models::price_cache_get(c, key)?)).ok().flatten().map(|(v, at)| (v, local_timestamp(at)))
        });
        let pending = self.pending_prices.clone();
        let cache_set = Box::new(move |key: &str, value: &str| {
            let mut p = pending.lock().unwrap();
            match p.iter_mut().find(|(k, _)| k == key) {
                Some(e) => e.1 = value.to_string(),
                None => p.push((key.to_string(), value.to_string())),
            }
        });
        let key = self.get_setting("coingecko_api_key", "").ok().filter(|k| !k.is_empty());
        let service = Arc::new(PriceService::new(key, Some(cache_get), Some(cache_set)));
        *guard = Some(service.clone());
        service
    }

    fn flush_prices(&self) -> AppResult<()> {
        let pending: Vec<(String, String)> = std::mem::take(&mut *self.pending_prices.lock().unwrap());
        if pending.is_empty() {
            return Ok(());
        }
        self.session(|c| {
            for (key, value) in &pending {
                crate::models::price_cache_set(c, key, value, chrono::Local::now().naive_local())?;
            }
            Ok(())
        })
    }

    // ------------------------------------------------------------ settings

    pub fn get_setting(&self, key: &str, default: &str) -> AppResult<String> {
        self.session(|c| Ok(crate::models::setting_get(c, key)?.unwrap_or_else(|| default.to_string())))
    }

    pub fn set_setting(&self, key: &str, value: &str) -> AppResult<()> {
        self.session(|c| Ok(crate::models::setting_set(c, key, value)?))?;
        if key == "coingecko_api_key" {
            *self.prices.lock().unwrap() = None;
        }
        Ok(())
    }

    // ----------------------------------------------------------- portfolio

    pub fn portfolio(&self, with_history: bool) -> AppResult<Portfolio> {
        let prices = self.prices();
        let out = self.session(|c| {
            loan_service::refresh_all(c, None)?; // the capital owed moves with every instalment
            let accounts = access::only_readable(c, Account::query(c, "WHERE archived = 0 ORDER BY institution, name", [])?)?;
            let with: Vec<(Account, Vec<Holding>)> = accounts.into_iter().map(|a| {
                let h = Holding::query(c, "WHERE account_id = ?", [a.id]).unwrap_or_default();
                (a, h)
            }).collect();
            let mut valuator = Valuator::new(&*prices);
            valuator.prefetch(&with);
            let values: Vec<AccountValue> = with.iter().map(|(a, h)| valuator.account_value(a, h)).collect();
            let mut totals: HashMap<&'static str, Decimal> = HashMap::new();
            for v in &values {
                let e = totals.entry(v.account.category()).or_insert(Decimal::ZERO);
                *e += v.total();
                ledger::record_snapshot(c, v.account.id, v.total(), None)?;
            }
            let mut history = vec![];
            if with_history {
                let mut st = c.prepare_cached("SELECT day, account_id, value FROM snapshots ORDER BY day")?;
                let rows: Vec<(NaiveDate, i64, Decimal)> = st
                    .query_map([], |r| {
                        Ok((
                            <NaiveDate as crate::models::Col>::from_sql(r.get_ref(0)?),
                            r.get::<_, i64>(1)?,
                            <Decimal as crate::models::Col>::from_sql(r.get_ref(2)?),
                        ))
                    })?
                    .collect::<Result<_, _>>()?;
                // Accounts without a snapshot on a given day keep their last known value.
                let active: BTreeSet<i64> = values.iter().map(|v| v.account.id).collect();
                let mut latest: Vec<(i64, Decimal)> = vec![];
                let mut by_day: std::collections::BTreeMap<NaiveDate, Decimal> = Default::default();
                for (day, account_id, value) in rows {
                    if active.contains(&account_id) {
                        match latest.iter_mut().find(|(i, _)| *i == account_id) {
                            Some(e) => e.1 = value,
                            None => latest.push((account_id, value)),
                        }
                        by_day.insert(day, latest.iter().fold(Decimal::ZERO, |a, (_, v)| a + v));
                    }
                }
                history = by_day.into_iter().collect();
            }
            Ok((values, totals, history))
        })?;
        self.flush_prices()?;
        let (values, totals, history) = out;
        let by_category = CATEGORIES.iter().filter_map(|(code, label)| totals.get(code).map(|t| (*code, *label, *t))).collect();
        let assets = values.iter().map(|v| v.total()).filter(|t| *t > Decimal::ZERO).fold(Decimal::ZERO, |a, b| a + b);
        let debts = values.iter().map(|v| v.total()).filter(|t| *t < Decimal::ZERO).fold(Decimal::ZERO, |a, b| a + b);
        Ok(Portfolio { accounts: values, total: assets + debts, by_category, assets, debts, history })
    }

    // ------------------------------------------------------------ accounts

    pub fn save_account(&self, account_id: Option<i64>, data: &HashMap<String, String>) -> AppResult<i64> {
        self.session(|c| {
            let (mut account, before) = match account_id.filter(|i| *i != 0) {
                Some(id) => {
                    access::check_edit(c, id)?;
                    let a = Account::get(c, id)?.ok_or_else(|| AppError::Other("'NoneType' object has no attribute 'name'".into()))?;
                    (a.clone(), Some(a))
                }
                None => (Account { owner_id: access::owner_for_new(c)?, name: String::new(), ..Default::default() }, None),
            };
            let get = |k: &str| data.get(k).cloned();
            account.name = get("name").filter(|s| !s.is_empty()).or_else(|| Some(account.name.clone()).filter(|s| !s.is_empty())).unwrap_or_else(|| "Compte".into());
            if let Some(t) = get("account_type").filter(|t| account_type(t).is_some()) {
                account.account_type = t;
            }
            account.institution = get("institution").filter(|s| !s.is_empty());
            account.currency = py::upper(&get("currency").filter(|s| !s.is_empty()).or_else(|| Some(account.currency.clone()).filter(|s| !s.is_empty())).unwrap_or_else(|| "EUR".into()));
            if let Some(b) = get("balance").filter(|s| !s.is_empty()) {
                let text = b.replace(',', ".").replace(' ', "");
                account.balance = crate::adapters::common::py_decimal(&text).ok_or_else(|| AppError::Other("[<class 'decimal.ConversionSyntax'>]".into()))?;
                account.balance_date = Some(utcnow());
            }
            let rate = py::strip(&get("interest_rate").unwrap_or_default().replace(',', ".")).to_string();
            account.interest_rate = if rate.is_empty() {
                None
            } else {
                Some(crate::adapters::common::py_decimal(&rate).ok_or_else(|| AppError::Other("[<class 'decimal.ConversionSyntax'>]".into()))?)
            };
            account.iban = Some(py::upper(&get("iban").unwrap_or_default().replace(' ', ""))).filter(|s| !s.is_empty());
            for (key, field) in [("bic", &mut account.bic), ("bank_code", &mut account.bank_code), ("branch_code", &mut account.branch_code), ("account_number", &mut account.account_number), ("rib_key", &mut account.rib_key)] {
                if let Some(v) = data.get(key) {
                    *field = Some(py::upper(&py::split_ws(v).concat())).filter(|s| !s.is_empty());
                }
            }
            for (key, field) in [("holder_name", &mut account.holder_name), ("holder_address", &mut account.holder_address)] {
                if let Some(v) = data.get(key) {
                    *field = Some(py::strip(v).to_string()).filter(|s| !s.is_empty());
                }
            }
            if let Some(people) = holders::from_form(data) {
                // the holders typed in the form (one block per person) are the truth; the single line is derived from them
                account.holders = if people.is_empty() { None } else { Some(holders::to_json(&people)) };
                account.holder_name = Some(holders::compose(&people)).filter(|s| !s.is_empty());
            }
            account.notes = get("notes").filter(|s| !s.is_empty());
            account.archived = get("archived").as_deref() == Some("on");
            match before {
                Some(b) => {
                    account.save(c, &b)?;
                    // its documents carry its institution, type and holders in their name
                    naming::rename_account_documents(c, account.id)?;
                }
                None => {
                    account.insert(c)?;
                }
            }
            Ok(account.id)
        })
    }

    pub fn delete_account(&self, account_id: i64) -> AppResult<()> {
        self.session(|c| {
            access::check_owner(c, account_id)?;
            if Account::get(c, account_id)?.is_some() {
                Account::delete(c, account_id)?;
            }
            Ok(())
        })
    }

    pub fn add_holding(&self, account_id: i64, data: &HashMap<String, String>) -> AppResult<()> {
        let dec = |s: &str| crate::adapters::common::py_decimal(s).ok_or_else(|| AppError::Other("[<class 'decimal.ConversionSyntax'>]".into()));
        let quantity = dec(&data.get("quantity").ok_or_else(|| AppError::Other("'quantity'".into()))?.replace(',', ".").replace(' ', ""))?;
        let symbol = Some(py::upper(py::strip(data.get("symbol").map(|s| s.as_str()).unwrap_or("")))).filter(|s| !s.is_empty());
        let isin = Some(py::upper(py::strip(data.get("isin").map(|s| s.as_str()).unwrap_or("")))).filter(|s| !s.is_empty());
        let value = data.get("value").cloned().unwrap_or_default().replace(',', ".").replace(' ', "");
        let asset_class = data.get("asset_class").filter(|s| !s.is_empty()).cloned().unwrap_or_else(|| if symbol.is_some() && isin.is_none() { "crypto".into() } else { "stock".into() });
        let value_dec = if value.is_empty() { None } else { Some(dec(&value)?) };
        self.session(|c| {
            access::check_edit(c, account_id)?;
            let extra = Some(data.get("yahoo").filter(|y| !y.is_empty()).map(|y| {
                let mut m = Map::new();
                m.insert("yahoo".into(), Value::String(py::strip(y).to_string()));
                Value::Object(m)
            }).unwrap_or(Value::Null));
            Holding {
                account_id,
                symbol: symbol.clone(),
                isin: isin.clone(),
                name: data.get("name").filter(|s| !s.is_empty()).cloned().or(symbol.clone()).or(isin.clone()),
                asset_class: asset_class.clone(),
                quantity,
                value: value_dec,
                value_currency: if value.is_empty() { None } else { Some("EUR".into()) },
                manual: true,
                extra,
                ..Default::default()
            }
            .insert(c)?;
            Ok(())
        })
    }

    pub fn delete_holding(&self, holding_id: i64) -> AppResult<()> {
        self.session(|c| {
            if let Some(h) = Holding::get(c, holding_id)? {
                access::check_edit(c, h.account_id)?;
                Holding::delete(c, h.id)?;
            }
            Ok(())
        })
    }

    // ----------------------------------------------------------- documents

    /// Encrypt and keep the file. Returns (document id, true if the very same file was already stored).
    pub fn store_document(&self, filename: &str, data: &[u8]) -> AppResult<(i64, bool)> {
        let digest = hex::encode(Sha256::digest(data));
        if let Some(existing) = self.session(|c| Ok(Document::query_one(c, "WHERE sha256 = ?", [&digest])?))? {
            return Ok((existing.id, true)); // (the web layer checks that the user may open it)
        }
        let blob_name = format!("{}.bin", token_hex(16));
        let path = self.settings.documents_dir().join(&blob_name);
        let sealed = self.vault.encrypt_file(data, &format!("document:{blob_name}"))?;
        {
            use std::os::unix::fs::OpenOptionsExt;
            let mut fh = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
            fh.write_all(&sealed)?;
        }
        let base = filename.rsplit('/').next().unwrap_or("");
        let name = py::take_chars(base, 300);
        self.session(|c| {
            let mut doc = Document {
                filename: if name.is_empty() { "document".into() } else { name.clone() },
                size: data.len() as i64,
                sha256: digest.clone(),
                blob_name: blob_name.clone(),
                status: "uploaded".into(),
                owner_id: access::owner_for_new(c)?,
                ..Default::default()
            };
            doc.insert(c)?;
            Ok((doc.id, false))
        })
    }

    /// What the adapters make of a file: the stored result, the status it leads to, the kind of document, the adapter used.
    fn read_source(doc_id: i64, adapter_id: Option<&str>, source: &SourceDocument) -> (Map<String, Value>, String, String, Option<String>) {
        let found = identify(source);
        let mut result = Map::new();
        result.insert("decision".into(), (if adapter_id.is_some() { "chosen" } else { found.decision }).into());
        result.insert("message".into(), (if adapter_id.is_some() { String::new() } else { found.message.clone() }).into());
        result.insert("institution".into(), found.institution.clone().map(Value::String).unwrap_or(Value::Null));
        result.insert("candidates".into(), Value::Array(found.candidates.iter().map(|c| c.to_json()).collect()));
        result.insert("statements".into(), Value::Array(vec![]));
        result.insert("reader".into(), reader_version().into());
        let chosen: Option<String> = adapter_id.map(|s| s.to_string()).or_else(|| found.chosen.as_ref().map(|c| c.adapter.id().to_string()));
        let (mut status, mut kind) = ("failed".to_string(), "statement".to_string());
        match &chosen {
            None if found.decision == "ask" => status = "needs_choice".into(),
            None => {
                result.insert("error".into(), found.message.clone().into());
            }
            Some(id) => {
                let parsed = get_adapter(id).and_then(|a| parse_with(a.id(), source).map(|s| (a, s)));
                match parsed {
                    Ok((a, statements)) => {
                        result.insert("adapter".into(), id.clone().into());
                        result.insert("statements".into(), Value::Array(statements.iter().map(|s| s.to_json()).collect()));
                        status = "parsed".into();
                        kind = a.kind().into();
                    }
                    Err(e) => {
                        log::warn!("Lecture de {doc_id} avec {id} impossible : {e}");
                        result.insert("adapter".into(), id.clone().into());
                        result.insert("error".into(), e.into());
                    }
                }
            }
        }
        (result, status, kind, chosen)
    }

    /// Find the adapter for a stored document (or use the one the user chose) and parse it. Returns its status.
    pub fn analyze_document(&self, doc_id: i64, adapter_id: Option<&str>, data: Option<Vec<u8>>) -> AppResult<String> {
        self.session(|c| self.doc(c, doc_id, true).map(|_| ()))?;
        let (filename, data) = match data {
            Some(d) => (self.filename(doc_id)?, d),
            None => self.read_document(doc_id)?,
        };
        let source = SourceDocument::new(&filename, data);
        let (result, status, kind, chosen) = Self::read_source(doc_id, adapter_id, &source);
        self.session(|c| {
            let mut doc = Document::get(c, doc_id)?.ok_or_else(|| AppError::Other("document".into()))?;
            let before = doc.clone();
            doc.status = status.clone();
            doc.parse_result = Some(Value::Object(result.clone()));
            doc.adapter_id = chosen.clone();
            doc.kind = kind.clone();
            doc.save(c, &before)?;
            Ok(())
        })?;
        Ok(status)
    }

    pub fn doc(&self, c: &Connection, doc_id: i64, edit: bool) -> AppResult<Document> {
        access::check_document(c, Document::get(c, doc_id)?, edit)
    }

    pub fn document_visible(&self, doc_id: i64) -> AppResult<bool> {
        self.session(|c| match Document::get(c, doc_id)? {
            Some(d) => access::document_visible(c, &d),
            None => Ok(false),
        })
    }

    fn filename(&self, doc_id: i64) -> AppResult<String> {
        self.session(|c| Ok(Document::get(c, doc_id)?.map(|d| d.filename).unwrap_or_default()))
    }

    pub fn read_document(&self, doc_id: i64) -> AppResult<(String, Vec<u8>)> {
        let doc = self.session(|c| self.doc(c, doc_id, false))?;
        let blob = std::fs::read(self.settings.documents_dir().join(&doc.blob_name))?;
        let data = self.vault.decrypt_file(&blob, &format!("document:{}", doc.blob_name)).map_err(AppError::Other)?;
        Ok((doc.filename, data))
    }

    pub fn delete_document(&self, doc_id: i64) -> AppResult<()> {
        let path = self.session(|c| {
            let Some(doc) = Document::get(c, doc_id)? else { return Ok(None) };
            self.doc(c, doc_id, true)?;
            c.execute("UPDATE transactions SET document_id = NULL WHERE document_id = ?", [doc_id])?;
            Document::delete(c, doc_id)?;
            Ok(Some(self.settings.documents_dir().join(&doc.blob_name)))
        })?;
        if let Some(p) = path {
            wipe(&p);
        }
        Ok(())
    }

    // ------------------------------------------------------------- duplicate files

    /// Does this file repeat one already imported (same account, same period)? "identical" when it changes nothing
    /// (the file is set aside), "differs" when it has extra or missing lines or other balances, None otherwise.
    pub fn check_duplicate(&self, doc_id: i64) -> AppResult<Option<&'static str>> {
        self.session(|c| {
            let mut doc = self.doc(c, doc_id, true)?;
            let sts = statements_of(&doc);
            if doc.status != "parsed" || sts.len() != 1 {
                return Ok(None);
            }
            let st = ParsedStatement::from_json(&sts[0]);
            if statement_service::document_only(st.document_kind.as_deref()) {
                return Ok(None); // a fees recap or an information letter: nothing to compare operation by operation
            }
            let found = statement_service::find_accounts(c, &st)?;
            if found.len() != 1 {
                return Ok(None);
            }
            let Some(dedup) = statement_service::plan_import(c, &st, Some(&found[0]))?.dedup else { return Ok(None) };
            let before = doc.clone();
            doc.duplicate_of = dedup.document.as_ref().map(|d| d.id);
            let identical = dedup.identical();
            if identical {
                doc.status = "duplicate".into();
            }
            doc.save(c, &before)?;
            Ok(Some(if identical { "identical" } else { "differs" }))
        })
    }

    /// Keep a duplicate that was set aside, as a copy next to the original (nothing is imported from it).
    pub fn keep_duplicate(&self, doc_id: i64) -> AppResult<()> {
        self.session(|c| {
            let mut doc = self.doc(c, doc_id, true)?;
            if doc.status == "duplicate" || doc.status == "parsed" {
                let before = doc.clone();
                doc.status = "copy".into();
                doc.save(c, &before)?;
            }
            Ok(())
        })
    }

    /// Delete the duplicates that were set aside and not kept within a day.
    pub fn purge_quarantine(&self) -> AppResult<usize> {
        let limit = utcnow() - Duration::hours(QUARANTINE_HOURS);
        access::unrestricted(|| {
            // housekeeping for everyone's files
            let old: Vec<i64> = self.session(|c| {
                let mut st = c.prepare("SELECT id FROM documents WHERE status = 'duplicate' AND uploaded_at < ?")?;
                let ids = st.query_map([fmt_datetime(&limit)], |r| r.get::<_, i64>(0))?.collect::<Result<_, _>>()?;
                Ok(ids)
            })?;
            for id in &old {
                self.delete_document(*id)?;
            }
            Ok(old.len())
        })
    }

    fn statement_at(doc: &Document, index: usize) -> AppResult<ParsedStatement> {
        let sts = statements_of(doc);
        sts.get(index).map(ParsedStatement::from_json).ok_or_else(|| AppError::Other("list index out of range".into()))
    }

    /// The one-click import (matching account, new lines, balance if newer), even if the period was imported before.
    pub fn import_default(&self, doc_id: i64, index: usize, force: bool) -> AppResult<Map<String, Value>> {
        self.session(|c| {
            let mut doc = self.doc(c, doc_id, true)?;
            let st = Self::statement_at(&doc, index)?;
            let found = statement_service::find_accounts(c, &st)?;
            if found.len() != 1 {
                return value_err("Compte introuvable ou ambigu : choisis-le dans le formulaire d'import");
            }
            let mut form = statement_service::default_import_form(&statement_service::plan_import(c, &st, Some(&found[0]))?);
            form.insert("force".into(), if force { "on".into() } else { String::new() });
            statement_service::import_statement(c, &mut doc, &st, &form)
        })
    }

    /// Replace the file already imported for this period by this one: the old file and what it brought are removed,
    /// this one is imported in full.
    pub fn replace_document(&self, doc_id: i64, index: usize) -> AppResult<Map<String, Value>> {
        let (result, old_path) = self.session(|c| {
            let mut doc = self.doc(c, doc_id, true)?;
            let st = Self::statement_at(&doc, index)?;
            let found = statement_service::find_accounts(c, &st)?;
            if found.len() != 1 {
                return value_err("Compte introuvable ou ambigu");
            }
            let Some(dedup) = statement_service::plan_import(c, &st, Some(&found[0]))?.dedup else {
                return value_err("Ce relevé ne remplace rien : aucune période identique n'a été importée");
            };
            let Some(mut old) = dedup.document.clone() else {
                return value_err("L'ancien fichier n'existe plus : impossible de le remplacer (garde les deux)");
            };
            let (old_name, old_path) = (old.filename.clone(), self.settings.documents_dir().join(&old.blob_name));
            let removed = statement_service::undo_import(c, &mut old)?;
            Document::delete(c, old.id)?;
            let fresh = Document::get(c, doc.id)?.unwrap_or(doc.clone());
            doc = fresh;
            let form = statement_service::default_import_form(&statement_service::plan_import(c, &st, Some(&found[0]))?);
            let mut result = statement_service::import_statement(c, &mut doc, &st, &form)?;
            let before = doc.clone();
            doc.duplicate_of = None;
            let mut pr = doc.parse_result.as_ref().and_then(|v| v.as_object()).cloned().unwrap_or_default();
            let mut rep = Map::new();
            rep.insert("filename".into(), old_name.clone().into());
            rep.insert("removed".into(), removed.into());
            pr.insert("replaced".into(), Value::Object(rep));
            doc.parse_result = Some(Value::Object(pr));
            doc.save(c, &before)?;
            result.insert("replaced".into(), old_name.into());
            result.insert("removed".into(), removed.into());
            Ok((result, old_path))
        })?;
        wipe(&old_path);
        Ok(result)
    }

    /// Read again a document that is still waiting (not imported) when the reading code changed since it was read:
    /// what it shows must come from the current reading, not from a past one. Returns true when it was redone.
    pub fn refresh_if_stale(&self, doc_id: i64) -> bool {
        let chosen = self.session(|c| {
            let Some(doc) = Document::get(c, doc_id)? else { return Ok(None) };
            if !access::document_visible(c, &doc)? || !["parsed", "needs_choice", "failed"].contains(&doc.status.as_str()) {
                return Ok(None);
            }
            if result_str(&doc, "reader").as_deref() == Some(reader_version()) {
                return Ok(None);
            }
            // the user's own choice stays
            let chosen = if result_str(&doc, "decision").as_deref() == Some("chosen") { result_str(&doc, "adapter") } else { None };
            Ok(Some(chosen))
        });
        let Ok(Some(chosen)) = chosen else { return false };
        match self.analyze_document(doc_id, chosen.as_deref(), None) {
            Ok(status) => {
                if status == "parsed" {
                    if let Err(e) = self.check_duplicate(doc_id) {
                        log::warn!("Relecture de {doc_id} impossible : {e}");
                        return false;
                    }
                }
                true
            }
            Err(e) => {
                log::warn!("Relecture de {doc_id} impossible : {e}");
                false
            }
        }
    }

    pub fn refresh_stale_waiting(&self) -> AppResult<usize> {
        let ids: Vec<i64> = self.session(|c| {
            Ok(access::visible_documents(c)?
                .into_iter()
                .filter(|d| ["parsed", "needs_choice", "failed"].contains(&d.status.as_str()) && result_str(d, "reader").as_deref() != Some(reader_version()))
                .map(|d| d.id)
                .collect())
        })?;
        Ok(ids.into_iter().filter(|i| self.refresh_if_stale(*i)).count())
    }

    pub fn reading_is_current(&self, doc: &Document) -> bool {
        result_str(doc, "reader").as_deref() == Some(reader_version())
    }

    /// Documents read by an older version of the reading code than the current one, validated or not (the duplicates
    /// set aside are about to disappear and are left out).
    pub fn stale_documents(&self) -> AppResult<Vec<Document>> {
        self.session(|c| {
            let mut out = vec![];
            for d in access::visible_documents(c)? {
                // only the ones the user may change
                if d.status != "duplicate" && result_str(&d, "reader").as_deref() != Some(reader_version()) && access::check_document(c, Some(d.clone()), true).is_ok() {
                    out.push(d);
                }
            }
            Ok(out)
        })
    }

    /// Bring one document to the latest version of the analysis. A waiting document is read again like a new one. A
    /// validated one is read again too, but keeps its status, its account and its operations: only what the reading
    /// says (and what is derived from it: the account's blanks, the standard file name) is brought up to date; to take
    /// the operations from the new reading, invalidate the import and validate it again. The user's choice of reading
    /// stays. Returns "relu" when the document was read again.
    pub fn refresh_document(&self, doc_id: i64) -> AppResult<&'static str> {
        let (status, chosen) = self.session(|c| {
            let doc = self.doc(c, doc_id, true)?;
            let chosen = if result_str(&doc, "decision").as_deref() == Some("chosen") { result_str(&doc, "adapter") } else { None };
            Ok((doc.status.clone(), chosen))
        })?;
        if ["parsed", "needs_choice", "failed", "uploaded"].contains(&status.as_str()) {
            if self.analyze_document(doc_id, chosen.as_deref(), None)? == "parsed" {
                self.check_duplicate(doc_id)?;
            }
            return Ok("relu");
        }
        let (filename, data) = self.read_document(doc_id)?;
        let source = SourceDocument::new(&filename, data);
        let (mut result, new_status, kind, adapter) = Self::read_source(doc_id, chosen.as_deref(), &source);
        if new_status != "parsed" {
            return value_err("La lecture actuelle n'arrive pas à relire ce document : l'ancienne lecture est conservée");
        }
        self.session(|c| {
            let mut doc = self.doc(c, doc_id, true)?;
            let before = doc.clone();
            if let Some(replaced) = doc.parse_result.as_ref().and_then(|r| r.get("replaced")).cloned() {
                result.insert("replaced".into(), replaced);
            }
            doc.parse_result = Some(Value::Object(result.clone()));
            doc.adapter_id = adapter.clone();
            doc.kind = kind.clone();
            doc.save(c, &before)?;
            if doc.status == "imported" {
                statement_service::refresh_identity(c, &mut doc)?;
            }
            Ok(())
        })?;
        Ok("relu")
    }

    /// Bring every stale document (or the given ones) to the latest version. Returns (done, failed).
    pub fn refresh_documents(&self, only: Option<&[i64]>) -> AppResult<(usize, usize)> {
        let ids: Vec<i64> = self.stale_documents()?.into_iter().map(|d| d.id).filter(|i| only.map(|o| o.contains(i)).unwrap_or(true)).collect();
        let (mut done, mut failed) = (0, 0);
        for id in ids {
            match self.refresh_document(id) {
                Ok(_) => done += 1,
                Err(e) => {
                    log::warn!("Actualisation de {id} impossible : {e}");
                    failed += 1;
                }
            }
        }
        Ok((done, failed))
    }

    /// Everything the review page shows: what was understood, and what importing it would do.
    pub fn review_document(&self, doc_id: i64) -> AppResult<Option<Review>> {
        self.refresh_if_stale(doc_id);
        self.session(|c| {
            let Some(doc) = Document::get(c, doc_id)? else { return Ok(None) };
            if !access::document_visible(c, &doc)? {
                return Ok(None);
            }
            let statements: Vec<ParsedStatement> = statements_of(&doc).iter().map(ParsedStatement::from_json).collect();
            let accounts = access::only_editable(c, Account::query(c, "ORDER BY institution, name", [])?)?;
            let mut plans = vec![];
            for st in &statements {
                let found = statement_service::find_accounts(c, st)?;
                let account = if found.len() == 1 { Some(&found[0]) } else { None };
                plans.push(statement_service::plan_import(c, st, account)?);
            }
            Ok(Some(Review { doc, plans, accounts }))
        })
    }

    pub fn import_statement(&self, doc_id: i64, index: usize, form: &Form) -> AppResult<Map<String, Value>> {
        self.session(|c| {
            let mut doc = self.doc(c, doc_id, true)?;
            let st = Self::statement_at(&doc, index)?;
            statement_service::import_statement(c, &mut doc, &st, form)
        })
    }

    /// Import without asking when there is nothing to ask: one clear adapter, one matching account, balances check
    /// out, nothing suspicious. Anything else is left for the user to review.
    pub fn auto_import(&self, doc_id: i64) -> AppResult<Option<Map<String, Value>>> {
        self.session(|c| {
            let mut doc = self.doc(c, doc_id, true)?;
            let sts = statements_of(&doc);
            if doc.status != "parsed" || result_str(&doc, "decision").as_deref() != Some("auto") || sts.len() != 1 {
                return Ok(None);
            }
            let st = ParsedStatement::from_json(&sts[0]);
            if st.document_kind.as_deref() == Some("info") {
                // nothing to ask: it is filed
                return statement_service::import_statement(c, &mut doc, &st, &Form::new()).map(Some);
            }
            let found = statement_service::find_accounts(c, &st)?;
            if found.len() != 1 {
                return Ok(None);
            }
            let plan = statement_service::plan_import(c, &st, Some(&found[0]))?;
            if !plan.clean() {
                return Ok(None);
            }
            statement_service::import_statement(c, &mut doc, &st, &statement_service::default_import_form(&plan)).map(Some)
        })
    }

    /// Documents read but not yet validated (the ones the user still has to look at), oldest period first.
    pub fn pending_documents(&self) -> AppResult<Vec<Document>> {
        self.session(|c| {
            let mut docs: Vec<Document> = access::visible_documents(c)?.into_iter().filter(|d| d.status == "parsed").collect();
            docs.sort_by(|a, b| (period_key(a), a.id).cmp(&(period_key(b), b.id)));
            Ok(docs)
        })
    }

    /// (next document to review, how many are waiting). Walks the same order as `pending_documents`; with `after_id`
    /// it continues after that document, even once that one has been validated.
    pub fn next_pending(&self, after_id: Option<i64>) -> AppResult<(Option<i64>, usize)> {
        let pending = self.pending_documents()?;
        let Some(after) = after_id.filter(|_| !pending.is_empty()) else { return Ok((pending.first().map(|d| d.id), pending.len())) };
        let key = self.session(|c| Ok(Document::get(c, after)?.map(|d| (period_key(&d), d.id))))?;
        let later: Vec<&Document> = pending.iter().filter(|d| key.as_ref().map(|k| (period_key(d), d.id) > *k).unwrap_or(true)).collect();
        Ok((later.first().map(|d| d.id), pending.len()))
    }

    /// For each document waiting for validation, what stops it from being validated in one click.
    pub fn pending_reasons(&self) -> AppResult<Vec<(i64, String)>> {
        let mut reasons = vec![];
        for d in self.pending_documents()? {
            let why = match self.check_obvious(d.id, false) {
                Ok(r) => r.unwrap_or_default(),
                // a document that cannot even be planned still deserves a line
                Err(e) => format!("Analyse impossible : {e}"),
            };
            reasons.push((d.id, why));
        }
        Ok(reasons)
    }

    /// None when the document can be validated without asking anything (and is, with `apply`); otherwise why not.
    pub fn check_obvious(&self, doc_id: i64, apply: bool) -> AppResult<Option<String>> {
        self.session(|c| {
            let mut doc = self.doc(c, doc_id, true)?;
            let sts = statements_of(&doc);
            if doc.status != "parsed" || sts.len() != 1 {
                return Ok(Some("Le fichier contient plusieurs documents ou n'a pas été lu".into()));
            }
            let st = ParsedStatement::from_json(&sts[0]);
            if st.document_kind.as_deref() == Some("info") {
                if apply {
                    statement_service::import_statement(c, &mut doc, &st, &Form::new())?;
                }
                return Ok(None);
            }
            let found = statement_service::find_accounts(c, &st)?;
            if found.len() > 1 {
                return Ok(Some("Plusieurs comptes pourraient correspondre : choisis le bon".into()));
            }
            let plan = statement_service::plan_import(c, &st, found.first())?;
            if plan.dedup.is_some() {
                return Ok(Some("Ce relevé existe déjà mais le fichier diffère : à trancher".into()));
            }
            if plan.ask_holder {
                return Ok(Some("Le nom du titulaire est coupé par la banque : à compléter une fois".into()));
            }
            if plan.account.is_none() {
                let kind = st.document_kind.as_deref();
                let creating = !statement_service::document_only(kind) || kind == Some("loan_offer");
                if !creating {
                    return Ok(Some("Aucun compte ne correspond encore : valide d'abord le relevé du compte".into()));
                }
                if st.confidence != "high" || !st.warnings.is_empty() || !plan.issues.is_empty() {
                    return Ok(Some(st.warnings.first().or(plan.issues.first()).cloned().unwrap_or_else(|| "Lecture à vérifier".into())));
                }
            } else if !plan.clean() {
                let why = st
                    .warnings
                    .first()
                    .or(plan.issues.first())
                    .cloned()
                    .or_else(|| plan.already_imported.as_ref().map(|_| "Relevé déjà importé pour cette période".to_string()))
                    .or_else(|| (!plan.overlaps.is_empty()).then(|| "Chevauche un relevé déjà importé".to_string()))
                    .or_else(|| (!plan.classified.probable.is_empty()).then(|| "Des opérations ressemblent à des doublons".to_string()))
                    .unwrap_or_else(|| "Lecture à vérifier".into());
                return Ok(Some(why));
            }
            if apply {
                statement_service::import_statement(c, &mut doc, &st, &statement_service::default_import_form(&plan))?;
            }
            Ok(None)
        })
    }

    /// Cancel every import, then read the files again with the current adapters and validate what is clearly fine.
    /// Files are kept; accounts keep their names, holders and sharing but lose what the documents brought. Whatever
    /// does not pass the checks any more is left waiting for a decision, with its reason: nothing is validated in
    /// silence.
    pub fn rebuild_imports(&self) -> AppResult<Map<String, Value>> {
        let ids: Vec<i64> = self.session(|c| {
            let docs: Vec<Document> = access::visible_documents(c)?.into_iter().filter(|d| d.status == "imported").collect();
            for d in &docs {
                access::check_document(c, Some(d.clone()), true)?;
            }
            let mut ordered = docs;
            ordered.sort_by(|a, b| (kind_rank(a), period_key(a), a.id).cmp(&(kind_rank(b), period_key(b), b.id)));
            let ids: Vec<i64> = ordered.iter().map(|d| d.id).collect();
            let mut touched: BTreeSet<i64> = BTreeSet::new();
            let mut with_holdings: BTreeSet<i64> = BTreeSet::new();
            for d in &ordered {
                let mut mine: BTreeSet<i64> = d.account_id.into_iter().collect();
                for sql in ["SELECT account_id FROM statements WHERE document_id = ?", "SELECT account_id FROM transactions WHERE document_id = ?"] {
                    let mut st = c.prepare(sql)?;
                    mine.extend(st.query_map([d.id], |r| r.get::<_, i64>(0))?.collect::<Result<Vec<_>, _>>()?);
                }
                touched.extend(mine.iter().copied());
                if statements_of(d).iter().any(|s| s.get("holdings").map(crate::models::truthy).unwrap_or(false)) {
                    with_holdings.extend(mine.iter().copied());
                }
            }
            for d in &ordered {
                let mut fresh = Document::get(c, d.id)?.unwrap_or(d.clone());
                statement_service::undo_import(c, &mut fresh)?;
            }
            // touched is a Python set: the order does not matter, every account is handled the same way
            for account_id in &touched {
                let Some(mut account) = Account::get(c, *account_id)? else { continue };
                if with_holdings.contains(account_id) {
                    c.execute("DELETE FROM holdings WHERE account_id = ?", [account_id])?;
                }
                let remaining: Option<i64> = c.query_row("SELECT id FROM transactions WHERE account_id = ? LIMIT 1", [account_id], |r| r.get(0)).ok();
                if remaining.is_none() {
                    // nothing but documents ever fed this account: its balance was theirs too
                    let before = account.clone();
                    account.balance = Decimal::ZERO;
                    account.balance_date = None;
                    account.save(c, &before)?;
                }
            }
            Ok(ids)
        })?;
        for id in &ids {
            self.analyze_document(*id, None, None)?;
        }
        let mut done_total = 0;
        let mut todo = ids.clone();
        for _ in 0..3 {
            let (mut progress, mut remaining) = (false, vec![]);
            for id in &todo {
                let ok = matches!(self.check_obvious(*id, true), Ok(None));
                if ok {
                    done_total += 1;
                    progress = true;
                } else {
                    remaining.push(*id);
                }
            }
            todo = remaining;
            if !progress || todo.is_empty() {
                break;
            }
        }
        let mut left = vec![];
        for id in &todo {
            let Some(doc) = self.session(|c| Ok(Document::get(c, *id)?))? else { continue };
            let why = match doc.status.as_str() {
                "parsed" => match self.check_obvious(*id, false) {
                    Ok(r) => r.unwrap_or_else(|| "Lecture à vérifier".into()),
                    Err(e) => format!("Analyse impossible : {e}"),
                },
                "needs_choice" => "Le type de document est à préciser".into(),
                _ => result_str(&doc, "error").filter(|s| !s.is_empty()).unwrap_or_else(|| "Document non reconnu par les lectures actuelles".into()),
            };
            let mut m = Map::new();
            m.insert("id".into(), doc.id.into());
            m.insert("filename".into(), doc.filename.clone().into());
            m.insert("status".into(), doc.status.clone().into());
            m.insert("reason".into(), why.into());
            left.push(Value::Object(m));
        }
        let mut out = Map::new();
        out.insert("total".into(), ids.len().into());
        out.insert("validated".into(), done_total.into());
        out.insert("left".into(), Value::Array(left));
        Ok(out)
    }

    /// Cancel the import of these documents (their operations leave the accounts, the files stay and wait for a new
    /// validation). Returns how many were cancelled; documents one may not change are left alone.
    pub fn invalidate_documents(&self, doc_ids: &[i64]) -> AppResult<usize> {
        let mut done = 0;
        for id in doc_ids {
            let r = self.session(|c| {
                let Some(mut doc) = Document::get(c, *id)? else { return Ok(false) };
                if doc.status != "imported" {
                    return Ok(false);
                }
                match self.doc(c, *id, true) {
                    Err(AppError::Denied(_)) => return Ok(false),
                    Err(e) => return Err(e),
                    Ok(_) => {}
                }
                statement_service::undo_import(c, &mut doc)?;
                Ok(true)
            })?;
            if r {
                done += 1;
            }
        }
        Ok(done)
    }

    pub fn undo_import(&self, doc_id: i64) -> AppResult<i64> {
        self.session(|c| {
            let Some(mut doc) = Document::get(c, doc_id)? else { return Ok(0) };
            self.doc(c, doc_id, true)?;
            statement_service::undo_import(c, &mut doc)
        })
    }

    /// Statements of an account (for the web layer).
    pub fn statements_of_account(c: &Connection, account_id: i64) -> AppResult<Vec<Statement>> {
        Ok(Statement::query(c, "WHERE account_id = ?", [account_id])?)
    }

    pub fn transactions_of_document(c: &Connection, doc_id: i64) -> AppResult<Vec<Transaction>> {
        Ok(Transaction::query(c, "WHERE document_id = ?", [doc_id])?)
    }

    pub fn documents_dir(&self) -> PathBuf {
        self.settings.documents_dir()
    }
}

/// `datetime.timestamp()` of a naive local time.
pub fn local_timestamp(at: NaiveDateTime) -> f64 {
    chrono::Local.from_local_datetime(&at).earliest().map(|d| d.timestamp_micros() as f64 / 1e6).unwrap_or(0.0)
}

/// Overwrite before unlinking (best effort: SSDs and copy-on-write filesystems may keep old blocks).
pub fn wipe(path: &std::path::Path) {
    if let Ok(meta) = std::fs::metadata(path) {
        let mut noise = vec![0u8; meta.len() as usize];
        rand::thread_rng().fill_bytes(&mut noise);
        if let Ok(mut fh) = std::fs::OpenOptions::new().write(true).open(path) {
            let _ = fh.write_all(&noise);
        }
        let _ = std::fs::remove_file(path);
    }
}
