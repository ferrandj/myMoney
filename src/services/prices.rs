//! Free price sources for manually entered positions: ECB rates (Frankfurter), CoinGecko (crypto), Yahoo Finance
//! (stocks, ETFs, funds).

use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;
use std::time::Duration;

use rust_decimal::Decimal;
use serde_json::Value;

use crate::py;

pub const FIAT: &[&str] = &[
    "EUR", "USD", "GBP", "CHF", "JPY", "CAD", "AUD", "SEK", "NOK", "DKK", "PLN", "CZK", "HUF", "RON", "BGN", "TRY", "CNY", "HKD", "SGD", "NZD", "ZAR", "MXN",
    "BRL", "INR", "KRW", "ILS", "THB", "IDR", "MYR", "PHP", "ISK",
];

/// Symbols are ambiguous on CoinGecko (hundreds of tokens are called "ETH" something), so the main ones are pinned.
/// Others go through the search endpoint, ranked by market cap.
pub const COINGECKO_IDS: &[(&str, &str)] = &[
    ("BTC", "bitcoin"), ("ETH", "ethereum"), ("USDT", "tether"), ("USDC", "usd-coin"), ("BNB", "binancecoin"), ("SOL", "solana"), ("XRP", "ripple"),
    ("ADA", "cardano"), ("DOGE", "dogecoin"), ("TRX", "tron"), ("DOT", "polkadot"), ("MATIC", "matic-network"), ("POL", "polygon-ecosystem-token"),
    ("AVAX", "avalanche-2"), ("LINK", "chainlink"), ("ATOM", "cosmos"), ("LTC", "litecoin"), ("BCH", "bitcoin-cash"), ("XLM", "stellar"),
    ("ETC", "ethereum-classic"), ("XMR", "monero"), ("ALGO", "algorand"), ("NEAR", "near"), ("APT", "aptos"), ("ARB", "arbitrum"), ("OP", "optimism"),
    ("SUI", "sui"), ("TON", "the-open-network"), ("UNI", "uniswap"), ("AAVE", "aave"), ("DAI", "dai"), ("EURC", "euro-coin"), ("EURT", "tether-eurt"),
    ("FIL", "filecoin"), ("ICP", "internet-computer"), ("HBAR", "hedera-hashgraph"), ("VET", "vechain"), ("XTZ", "tezos"), ("EGLD", "elrond-erd-2"),
    ("KAS", "kaspa"), ("INJ", "injective-protocol"), ("TIA", "celestia"), ("NEXO", "nexo"), ("CRO", "crypto-com-chain"), ("KSM", "kusama"),
    ("GRT", "the-graph"), ("MKR", "maker"), ("SHIB", "shiba-inu"), ("PEPE", "pepe"), ("WBTC", "wrapped-bitcoin"), ("STETH", "staked-ether"),
    ("WETH", "weth"), ("CBBTC", "coinbase-wrapped-btc"), ("FLR", "flare-networks"), ("SEI", "sei-network"), ("PAXG", "pax-gold"), ("XAUT", "tether-gold"),
    ("RNDR", "render-token"), ("RENDER", "render-token"), ("FET", "fetch-ai"), ("HYPE", "hyperliquid"), ("ENA", "ethena"), ("JUP", "jupiter-exchange-solana"),
    ("BONK", "bonk"), ("WIF", "dogwifcoin"), ("STRK", "starknet"), ("MNT", "mantle"), ("IMX", "immutable-x"), ("SAND", "the-sandbox"),
    ("MANA", "decentraland"), ("AXS", "axie-infinity"), ("CRV", "curve-dao-token"), ("LDO", "lido-dao"), ("RPL", "rocket-pool"), ("SNX", "havven"),
    ("COMP", "compound-governance-token"), ("1INCH", "1inch"), ("ZEC", "zcash"), ("DASH", "dash"), ("EOS", "eos"), ("NEO", "neo"),
    ("QNT", "quant-network"), ("BAT", "basic-attention-token"), ("CHZ", "chiliz"), ("FTM", "fantom"), ("S", "sonic-3"), ("GNO", "gnosis"),
    ("XDAI", "xdai"), ("CELO", "celo"), ("KAVA", "kava"), ("OSMO", "osmosis"), ("RUNE", "thorchain"),
];

pub const FX_TTL: f64 = 6.0 * 3600.0;
pub const PRICE_TTL: f64 = 15.0 * 60.0;

#[derive(Debug, Clone)]
pub struct PriceError(pub String);

pub type CacheGet = Box<dyn Fn(&str) -> Option<(String, f64)> + Send + Sync>;
pub type CacheSet = Box<dyn Fn(&str, &str) + Send + Sync>;

pub struct PriceService {
    agent: ureq::Agent,
    pub coingecko_api_key: Option<String>,
    mem: Mutex<HashMap<String, (String, f64)>>,
    cache_get: Option<CacheGet>,
    cache_set: Option<CacheSet>,
}

pub fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// `str(x)` of a JSON number as Python's json module reads it (an int, or a float).
pub fn number_str(v: &Value) -> Option<String> {
    match v {
        Value::Number(n) if n.is_i64() || n.is_u64() => Some(n.to_string()),
        Value::Number(n) => n.as_f64().map(py::float_repr),
        _ => None,
    }
}

fn dec(s: &str) -> Decimal {
    py::dec_parse(s).unwrap_or_default()
}

impl PriceService {
    pub fn new(coingecko_api_key: Option<String>, cache_get: Option<CacheGet>, cache_set: Option<CacheSet>) -> Self {
        let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(20)).user_agent("Mozilla/5.0 (mymoney)").try_proxy_from_env(true).build();
        PriceService { agent, coingecko_api_key, mem: Mutex::new(HashMap::new()), cache_get, cache_set }
    }

    // -- cache -----------------------------------------------------------
    fn cached(&self, key: &str, ttl: f64) -> Option<String> {
        let mut hit = self.mem.lock().unwrap().get(key).cloned();
        if hit.is_none() {
            if let Some(get) = &self.cache_get {
                hit = get(key);
                if let Some(h) = &hit {
                    self.mem.lock().unwrap().insert(key.to_string(), h.clone());
                }
            }
        }
        match hit {
            Some((v, t)) if now() - t < ttl => Some(v),
            _ => None,
        }
    }

    fn stale(&self, key: &str) -> Option<String> {
        let hit = self.mem.lock().unwrap().get(key).cloned().or_else(|| self.cache_get.as_ref().and_then(|g| g(key)));
        hit.map(|h| h.0)
    }

    fn store(&self, key: &str, value: &str) {
        self.mem.lock().unwrap().insert(key.to_string(), (value.to_string(), now()));
        if let Some(set) = &self.cache_set {
            set(key, value);
        }
    }

    fn get_json(&self, url: &str, params: &[(&str, &str)], headers: &[(&str, &str)]) -> Result<(u16, Value), String> {
        let mut req = self.agent.get(url);
        for (k, v) in params {
            req = req.query(k, v);
        }
        for (k, v) in headers {
            req = req.set(k, v);
        }
        match req.call() {
            Ok(r) => {
                let status = r.status();
                r.into_json::<Value>().map(|v| (status, v)).map_err(|e| e.to_string())
            }
            Err(ureq::Error::Status(code, _)) => Ok((code, Value::Null)),
            Err(ureq::Error::Transport(t)) => Err(transport_error(&t)),
        }
    }

    // -- FX --------------------------------------------------------------
    pub fn fx_rate(&self, from: &str, to: &str) -> Result<Decimal, PriceError> {
        let (from, to) = (py::upper(from), py::upper(to));
        if from == to {
            return Ok(Decimal::ONE);
        }
        if !FIAT.contains(&from.as_str()) {
            // Stablecoins and other crypto used as a "currency" (e.g. Binance balances in USDT).
            return self.crypto_prices(&[from.clone()], &to, &HashMap::new()).get(&from).copied().ok_or_else(|| PriceError(format!("'{from}'")));
        }
        let key = format!("fx:{from}:{to}");
        if let Some(c) = self.cached(&key, FX_TTL).filter(|c| !c.is_empty()) {
            return Ok(dec(&c));
        }
        let got = self.get_json("https://api.frankfurter.dev/v1/latest", &[("from", &from), ("to", &to)], &[]).and_then(|(status, v)| {
            if status >= 400 {
                return Err(status_error(status, &format!("https://api.frankfurter.dev/v1/latest?from={from}&to={to}")));
            }
            v.get("rates").and_then(|r| r.get(&to)).and_then(number_str).ok_or_else(|| "rates".to_string())
        });
        match got {
            Ok(value) => {
                self.store(&key, &value);
                Ok(dec(&value))
            }
            Err(_) => match self.stale(&key).filter(|s| !s.is_empty()) {
                Some(s) => {
                    log::warn!("FX {from}->{to} indisponible, dernier taux connu utilisé");
                    Ok(dec(&s))
                }
                None => Err(PriceError(format!("Taux de change {from}->{to} introuvable"))),
            },
        }
    }

    pub fn convert(&self, amount: Decimal, from: &str, to: &str) -> Result<Decimal, PriceError> {
        Ok(py::py_mul(&amount, &self.fx_rate(from, to)?))
    }

    // -- crypto ----------------------------------------------------------
    fn coingecko_get(&self, path: &str, params: &[(&str, &str)]) -> Result<Value, String> {
        let key = self.coingecko_api_key.clone().filter(|k| !k.is_empty());
        let headers: Vec<(&str, &str)> = key.as_deref().map(|k| vec![("x-cg-demo-api-key", k)]).unwrap_or_default();
        for attempt in 0..3 {
            let (status, v) = self.get_json(&format!("https://api.coingecko.com/api/v3{path}"), params, &headers)?;
            if status == 429 && attempt < 2 {
                std::thread::sleep(Duration::from_secs(20 * (attempt + 1)));
                continue;
            }
            if status >= 400 {
                let url = format!("https://api.coingecko.com/api/v3{path}?{}", py::urlencode(&params.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Vec<_>>()));
                return Err(status_error(status, &url));
            }
            return Ok(v);
        }
        Err("CoinGecko : trop de requêtes".into())
    }

    pub fn coingecko_id(&self, symbol: &str) -> Option<String> {
        let symbol = py::upper(symbol);
        if let Some((_, id)) = COINGECKO_IDS.iter().find(|(s, _)| *s == symbol) {
            return Some(id.to_string());
        }
        let key = format!("cgid:{symbol}");
        if let Some(c) = self.cached(&key, 30.0 * 86400.0) {
            return if c.is_empty() { None } else { Some(c) };
        }
        let coins = match self.coingecko_get("/search", &[("query", &symbol)]) {
            Ok(v) => v.get("coins").and_then(|c| c.as_array()).cloned().unwrap_or_default(),
            Err(_) => return None,
        };
        let mut matches: Vec<&Value> = coins.iter().filter(|c| py::upper(c.get("symbol").and_then(|s| s.as_str()).unwrap_or("")) == symbol).collect();
        matches.sort_by_key(|c| c.get("market_cap_rank").and_then(|r| r.as_i64()).filter(|r| *r != 0).unwrap_or(1_000_000_000));
        let id = matches.first().and_then(|c| c.get("id")).and_then(|i| i.as_str()).unwrap_or("").to_string();
        self.store(&key, &id);
        if id.is_empty() {
            None
        } else {
            Some(id)
        }
    }

    /// Prices by symbol. `ids` can force the CoinGecko id of some symbols.
    pub fn crypto_prices(&self, symbols: &[String], currency: &str, ids: &HashMap<String, String>) -> HashMap<String, Decimal> {
        let currency = py::lower(currency);
        let mut result = HashMap::new();
        let mut wanted: Vec<(String, String)> = vec![];
        let unique: BTreeSet<String> = symbols.iter().map(|s| py::upper(s)).collect();
        for symbol in unique {
            let Some(coin_id) = ids.get(&symbol).filter(|i| !i.is_empty()).cloned().or_else(|| self.coingecko_id(&symbol)) else { continue };
            match self.cached(&format!("cg:{coin_id}:{currency}"), PRICE_TTL).filter(|c| !c.is_empty()) {
                Some(c) => {
                    result.insert(symbol, dec(&c));
                }
                None => wanted.push((symbol, coin_id)),
            }
        }
        let unique_ids: Vec<String> = wanted.iter().map(|(_, i)| i.clone()).collect::<BTreeSet<_>>().into_iter().collect();
        for chunk in unique_ids.chunks(200) {
            let joined = chunk.join(",");
            let data = match self.coingecko_get("/simple/price", &[("ids", &joined), ("vs_currencies", &currency)]) {
                Ok(v) => v,
                Err(e) => {
                    log::warn!("CoinGecko indisponible : {e}");
                    Value::Null
                }
            };
            for coin_id in chunk {
                if let Some(p) = data.get(coin_id).and_then(|x| x.get(&currency)).and_then(number_str) {
                    self.store(&format!("cg:{coin_id}:{currency}"), &p);
                }
            }
        }
        for (symbol, coin_id) in wanted {
            if let Some(v) = self.stale(&format!("cg:{coin_id}:{currency}")) {
                result.insert(symbol, dec(&v));
            }
        }
        result
    }

    // -- stocks, ETFs, funds --------------------------------------------
    pub fn yahoo_symbol(&self, query: &str) -> Option<String> {
        let key = format!("yq:{query}");
        if let Some(c) = self.cached(&key, 30.0 * 86400.0) {
            return if c.is_empty() { None } else { Some(c) };
        }
        let mut quotes = match self.get_json("https://query1.finance.yahoo.com/v1/finance/search", &[("q", query), ("quotesCount", "5"), ("newsCount", "0")], &[]) {
            Ok((status, v)) if status < 400 => v.get("quotes").and_then(|q| q.as_array()).cloned().unwrap_or_default(),
            _ => return None,
        };
        // Prefer euro listings (Paris, Amsterdam, Xetra, Milan) for a French portfolio.
        let preferred = [".PA", ".AS", ".DE", ".MI", ".F"];
        let score = |q: &Value| q.get("score").and_then(|s| s.as_f64()).unwrap_or(0.0);
        quotes.sort_by(|a, b| {
            let pa = !preferred.iter().any(|p| a.get("symbol").and_then(|s| s.as_str()).unwrap_or("").ends_with(p));
            let pb = !preferred.iter().any(|p| b.get("symbol").and_then(|s| s.as_str()).unwrap_or("").ends_with(p));
            pa.cmp(&pb).then((-score(a)).partial_cmp(&-score(b)).unwrap_or(std::cmp::Ordering::Equal))
        });
        let symbol = quotes.first().and_then(|q| q.get("symbol")).and_then(|s| s.as_str()).unwrap_or("").to_string();
        self.store(&key, &symbol);
        if symbol.is_empty() {
            None
        } else {
            Some(symbol)
        }
    }

    pub fn security_price(&self, isin: Option<&str>, symbol: Option<&str>) -> Option<(Decimal, String)> {
        if cfg!(test) {
            return None; // the tests never reach for the network: a line then keeps its last known price
        }
        let symbol = match symbol.filter(|s| !s.is_empty()) {
            Some(s) => s.to_string(),
            None => self.yahoo_symbol(isin.filter(|i| !i.is_empty())?)?,
        };
        let key = format!("yp:{symbol}");
        let read = |s: &str| -> Option<(Decimal, String)> {
            let v: Value = serde_json::from_str(s).ok()?;
            Some((dec(v.get("p")?.as_str()?), v.get("c")?.as_str()?.to_string()))
        };
        if let Some(c) = self.cached(&key, PRICE_TTL).filter(|c| !c.is_empty()) {
            return read(&c);
        }
        let got = self.get_json(&format!("https://query1.finance.yahoo.com/v8/finance/chart/{symbol}"), &[("range", "1d"), ("interval", "1d")], &[]).ok().and_then(|(status, v)| {
            if status >= 400 {
                return None;
            }
            let meta = v.get("chart")?.get("result")?.get(0)?.get("meta")?.clone();
            let price = dec(&number_str(meta.get("regularMarketPrice")?)?);
            let currency = meta.get("currency").and_then(|c| c.as_str()).unwrap_or("EUR").to_string();
            Some((price, currency))
        });
        let Some((mut price, mut currency)) = got else {
            return self.stale(&key).and_then(|s| read(&s));
        };
        if currency == "GBp" {
            // London quotes in pence
            price = py::py_div(&price, &Decimal::from(100)).unwrap_or_default();
            currency = "GBP".into();
        }
        let mut m = serde_json::Map::new();
        m.insert("p".into(), py::dec_str(&price).into());
        m.insert("c".into(), currency.clone().into());
        self.store(&key, &py::json_dumps(&Value::Object(m)));
        Some((price, currency))
    }
}

/// How httpx words a failed connection (the Python version logs it): "[Errno 111] Connection refused", never the URL.
fn transport_error(t: &ureq::Transport) -> String {
    use std::error::Error;
    let mut source = t.source();
    while let Some(e) = source {
        if let Some(io) = e.downcast_ref::<std::io::Error>() {
            if matches!(io.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock) {
                return "timed out".into();
            }
            if let Some(n) = io.raw_os_error() {
                let text = std::io::Error::from_raw_os_error(n).to_string();
                let text = text.split(" (os error").next().unwrap_or(&text).to_string();
                return format!("[Errno {n}] {text}");
            }
        }
        source = e.source();
    }
    match t.kind() {
        ureq::ErrorKind::Dns => "[Errno -2] Name or service not known".into(),
        _ => t.message().map(|m| m.to_string()).unwrap_or_else(|| t.kind().to_string()),
    }
}

/// httpx's `raise_for_status()` message.
fn status_error(code: u16, url: &str) -> String {
    let kind = match code {
        100..=199 => "Informational response",
        300..=399 => "Redirect response",
        400..=499 => "Client error",
        _ => "Server error",
    };
    let reason = match code {
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        408 => "Request Timeout",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "",
    };
    format!("{kind} '{code} {reason}' for url '{url}'\nFor more information check: https://developer.mozilla.org/en-US/docs/Web/HTTP/Status/{code}")
}
