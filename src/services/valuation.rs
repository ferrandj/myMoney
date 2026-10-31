//! Value every account in the base currency (EUR).

use std::collections::HashMap;

use rust_decimal::Decimal;

use crate::models::{Account, Holding};
use crate::py;
use crate::services::prices::{PriceError, PriceService};

pub const BASE: &str = "EUR";

#[derive(Clone, Debug)]
pub struct HoldingValue {
    pub holding: Holding,
    pub unit_price_eur: Option<Decimal>,
    pub value_eur: Option<Decimal>,
}

#[derive(Clone, Debug)]
pub struct AccountValue {
    pub account: Account,
    pub cash_eur: Decimal,
    pub holdings: Vec<HoldingValue>,
    pub warnings: Vec<String>,
}

impl AccountValue {
    pub fn total(&self) -> Decimal {
        let total = self.holdings.iter().fold(self.cash_eur, |a, h| a + py::or_zero(h.value_eur));
        if self.account.category() == "dettes" {
            return py::neg(total.abs());
        }
        total
    }
}

/// What the valuation needs from the price sources (the tests give it fixed prices).
pub trait PriceSource {
    fn convert(&self, amount: Decimal, from: &str, to: &str) -> Result<Decimal, PriceError>;
    fn crypto_prices(&self, symbols: &[String], currency: &str, ids: &HashMap<String, String>) -> HashMap<String, Decimal>;
    fn security_price(&self, isin: Option<&str>, symbol: Option<&str>) -> Option<(Decimal, String)>;
}

impl PriceSource for PriceService {
    fn convert(&self, amount: Decimal, from: &str, to: &str) -> Result<Decimal, PriceError> {
        PriceService::convert(self, amount, from, to)
    }
    fn crypto_prices(&self, symbols: &[String], currency: &str, ids: &HashMap<String, String>) -> HashMap<String, Decimal> {
        PriceService::crypto_prices(self, symbols, currency, ids)
    }
    fn security_price(&self, isin: Option<&str>, symbol: Option<&str>) -> Option<(Decimal, String)> {
        PriceService::security_price(self, isin, symbol)
    }
}

pub struct Valuator<'a> {
    pub prices: &'a dyn PriceSource,
    crypto: HashMap<String, Decimal>,
}

impl<'a> Valuator<'a> {
    pub fn new(prices: &'a dyn PriceSource) -> Self {
        Valuator { prices, crypto: HashMap::new() }
    }

    /// Fetch all crypto prices in as few calls as possible (the free CoinGecko tier is rate-limited).
    pub fn prefetch(&mut self, accounts: &[(Account, Vec<Holding>)]) {
        let mut symbols: Vec<String> = vec![];
        let mut ids: HashMap<String, String> = HashMap::new();
        for (account, holdings) in accounts {
            for h in holdings {
                if h.asset_class != "crypto" || h.value.is_some() {
                    continue;
                }
                let symbol = h.symbol.clone().filter(|s| !s.is_empty());
                if let (Some(id), Some(s)) = (h.extra.as_ref().and_then(|e| e.get("coingecko_id")).filter(|v| crate::models::truthy(v)), &symbol) {
                    ids.insert(py::upper(s), id.as_str().map(|x| x.to_string()).unwrap_or_else(|| id.to_string()));
                }
                if let Some(s) = symbol {
                    symbols.push(py::upper(&s));
                }
            }
            let cur = py::upper(&account.currency);
            if cur != "EUR" && cur != "USD" && py::char_len(&account.currency) > 3 {
                symbols.push(cur);
            }
        }
        if !symbols.is_empty() {
            self.crypto = self.prices.crypto_prices(&symbols, BASE, &ids);
        } else {
            self.crypto = HashMap::new();
        }
    }

    fn convert(&self, amount: Decimal, currency: Option<&str>) -> Option<Decimal> {
        self.prices.convert(amount, currency.filter(|c| !c.is_empty()).unwrap_or(BASE), BASE).ok()
    }

    pub fn holding_value(&self, h: &Holding) -> HoldingValue {
        let qty = py::or_zero(Some(h.quantity));
        let done = |price: Decimal| HoldingValue { holding: h.clone(), unit_price_eur: Some(price), value_eur: Some(py::py_mul(&price, &qty)) };
        if let Some(v) = h.value {
            let value = self.convert(v, h.value_currency.as_deref());
            let unit = match value {
                Some(x) if !qty.is_zero() => py::py_div(&x, &qty),
                _ => None,
            };
            return HoldingValue { holding: h.clone(), unit_price_eur: unit, value_eur: value };
        }
        let yahoo = h.extra.as_ref().and_then(|e| e.get("yahoo")).filter(|v| crate::models::truthy(v)).and_then(|v| v.as_str()).map(|s| s.to_string());
        let symbol = h.symbol.clone().filter(|s| !s.is_empty());
        if h.asset_class == "crypto" {
            let mut price = symbol.as_ref().and_then(|s| self.crypto.get(&py::upper(s)).copied());
            if let (None, Some(s)) = (price, &symbol) {
                if !self.crypto.contains_key(&py::upper(s)) {
                    price = self.prices.crypto_prices(&[s.clone()], BASE, &HashMap::new()).get(&py::upper(s)).copied();
                }
            }
            if let Some(p) = price {
                return done(p);
            }
        } else if h.isin.as_deref().map(|i| !i.is_empty()).unwrap_or(false) || yahoo.is_some() {
            if let Some((p, cur)) = self.prices.security_price(h.isin.as_deref(), yahoo.as_deref()) {
                if let Some(price) = self.convert(p, Some(&cur)) {
                    // a line rebuilt from papers knows its last price: a quote far from it is another class of the fund
                    let guarded = h.extra.as_ref().and_then(|e| e.get("guard")).map(crate::models::truthy).unwrap_or(false);
                    let far = match (guarded, h.unit_price) {
                        (true, Some(last)) if !last.is_zero() => ((price - last).abs() / last) > Decimal::new(3, 1),
                        _ => false,
                    };
                    if !far {
                        return done(price);
                    }
                }
            }
        }
        if let Some(u) = h.unit_price {
            if let Some(price) = self.convert(u, h.value_currency.as_deref()) {
                return done(price);
            }
        }
        HoldingValue { holding: h.clone(), unit_price_eur: None, value_eur: None }
    }

    pub fn account_value(&self, account: &Account, holdings: &[Holding]) -> AccountValue {
        let mut warnings = vec![];
        let cash = match self.convert(py::or_zero(Some(account.balance)), Some(&account.currency)) {
            Some(c) => c,
            None => {
                warnings.push(format!("Taux de change {} indisponible", account.currency));
                Decimal::ZERO
            }
        };
        let values: Vec<HoldingValue> = holdings.iter().map(|h| self.holding_value(h)).collect();
        let missing: Vec<String> = values
            .iter()
            .filter(|v| v.value_eur.is_none())
            .map(|v| v.holding.name.clone().filter(|s| !s.is_empty()).or(v.holding.symbol.clone().filter(|s| !s.is_empty())).unwrap_or_else(|| "?".into()))
            .collect();
        if !missing.is_empty() {
            warnings.push(format!("Prix introuvable : {}", missing.iter().take(5).cloned().collect::<Vec<_>>().join(", ")));
        }
        AccountValue { account: account.clone(), cash_eur: cash, holdings: values, warnings }
    }
}
