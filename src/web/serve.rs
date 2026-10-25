//! `mymoney serve`: the web interface, with the start-up of the Python version (installation code in the logs,
//! optional unattended unlock, background auto-lock).

use std::sync::Arc;

use super::app::Web;
use crate::app::MyMoney;
use crate::config::Settings;

/// Python's logging line: "2024-05-01 10:00:00,123 WARNING mymoney.web.app: message".
struct Logger {
    level: log::LevelFilter,
}

fn python_name(target: &str) -> String {
    let t = target.strip_prefix("mymoney::").unwrap_or(target);
    let name = match t {
        "app" => "core".to_string(),
        "web::app" | "web::serve" | "web::http" => "web.app".to_string(),
        "web::cloudflare" => "web.cloudflare".to_string(),
        other => other.replace("::", "."),
    };
    format!("mymoney.{name}")
}

impl log::Log for Logger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= self.level && m.target().starts_with("mymoney")
    }
    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let now = chrono::Local::now();
        let level = match record.level() {
            log::Level::Error => "ERROR",
            log::Level::Warn => "WARNING",
            log::Level::Info => "INFO",
            log::Level::Debug | log::Level::Trace => "DEBUG",
        };
        eprintln!("{},{:03} {level} {}: {}", now.format("%Y-%m-%d %H:%M:%S"), now.timestamp_subsec_millis(), python_name(record.target()), record.args());
    }
    fn flush(&self) {}
}

pub fn init_logging() {
    let level = match std::env::var("MYMONEY_LOG_LEVEL").unwrap_or_else(|_| "INFO".into()).to_uppercase().as_str() {
        "DEBUG" => log::LevelFilter::Debug,
        "WARNING" | "WARN" => log::LevelFilter::Warn,
        "ERROR" | "CRITICAL" => log::LevelFilter::Error,
        _ => log::LevelFilter::Info,
    };
    if log::set_boxed_logger(Box::new(Logger { level })).is_ok() {
        log::set_max_level(level);
    }
}

fn usage() -> ! {
    eprintln!("usage: mymoney [-h] {{serve}} ...");
    eprintln!("mymoney: error: the following arguments are required: command");
    std::process::exit(2);
}

/// `mymoney serve [--host HOST] [--port PORT]`.
pub fn main(args: &[String]) -> i32 {
    let mut host = std::env::var("MYMONEY_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let mut port: String = std::env::var("MYMONEY_PORT").unwrap_or_else(|_| "8000".into());
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--host" if i + 1 < args.len() => {
                host = args[i + 1].clone();
                i += 2;
            }
            "--port" if i + 1 < args.len() => {
                port = args[i + 1].clone();
                i += 2;
            }
            a if a.starts_with("--host=") => {
                host = a[7..].to_string();
                i += 1;
            }
            a if a.starts_with("--port=") => {
                port = a[7..].to_string();
                i += 1;
            }
            "-h" | "--help" => {
                println!("usage: mymoney serve [-h] [--host HOST] [--port PORT]");
                return 0;
            }
            _ => usage(),
        }
    }
    let Ok(port) = port.trim().parse::<u16>() else {
        eprintln!("mymoney serve: error: argument --port: invalid int value: '{port}'");
        return 2;
    };
    init_logging();
    let settings = Settings::from_env();
    let core = match MyMoney::new(settings.clone()) {
        Ok(c) => c,
        Err(e) => {
            log::error!("Démarrage impossible : {e}");
            return 1;
        }
    };
    let web = match Web::new(core.clone()) {
        Ok(w) => w,
        Err(e) => {
            log::error!("Démarrage impossible : {e}");
            return 1;
        }
    };
    if !core.vault.initialized() {
        // Whoever reaches the page first could otherwise create the vault: require a code from the logs.
        log::warn!("Première installation : code d'installation = {}", core.setup_token);
    }
    if let Err(e) = core.auto_unlock() {
        log::error!("Déverrouillage automatique impossible : {e}");
    }
    let sessions = web.sessions.clone();
    core.start_background(Arc::new(move || sessions.active()));
    let max_body = ((settings.max_upload_mb.max(1) as usize) * 1024 * 1024 * 12).min(1 << 30);
    let addr = format!("{host}:{port}");
    log::info!("myMoney écoute sur http://{addr}");
    let handler = {
        let web = web.clone();
        Arc::new(move |req| web.handle(req))
    };
    match super::http::serve(&addr, 32, max_body, handler) {
        Ok(()) => 0,
        Err(e) => {
            log::error!("Écoute sur {addr} impossible : {e}");
            1
        }
    }
}
