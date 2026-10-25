pub mod adapters;
pub mod app;
pub mod config;
pub mod db;
pub mod error;
pub mod models;
pub mod pdf;
pub mod py;
pub mod scenario;
pub mod schema;
pub mod security;
pub mod services;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 3 && args[1] == "pdf-words" {
        let data = std::fs::read(&args[2]).expect("read");
        let tol: f64 = args.get(3).map(|s| s.parse().unwrap()).unwrap_or(1.0);
        let pages = pdf::extract_words(&data, tol).expect("pdf");
        println!("{}", serde_json::to_string(&pages).unwrap());
    } else if args.len() >= 5 && (args[1] == "debug-db" || args[1] == "debug-create") {
        let settings = config::Settings { data_dir: args[2].clone().into(), ..config::Settings::from_env() };
        settings.ensure_dirs().unwrap();
        let ks = security::keystore::Keystore { path: settings.keystore_path(), time_cost: settings.argon2_time_cost, memory_kib: settings.argon2_memory_kib };
        let vault = security::vault::Vault::new(ks);
        let slot = services::users::slot_id(&args[3]).unwrap();
        let db = db::Database::new(settings.db_path());
        if args[1] == "debug-create" {
            let rec = vault.setup(&args[4], Some(&slot)).unwrap();
            db.open(&vault.subkey("database").unwrap()).unwrap();
            db.session(|c| { services::users::create(c, &args[3], Some("Rust"), true)?; let mut a = models::Account { name: "Compte Rust".into(), balance: "12.50".parse().unwrap(), ..Default::default() }; a.insert(c)?; Ok(()) }).unwrap();
            println!("recovery {rec}");
        } else {
            vault.unlock(&args[4], Some(&slot)).unwrap();
            db.open(&vault.subkey("database").unwrap()).unwrap();
            db.session(|c| {
                for u in services::users::all_users(c)? { println!("user {} {} admin={} created={}", u.id, u.email, u.is_admin, models::fmt_datetime(&u.created_at)); }
                for a in models::Account::query(c, "", [])? { println!("account {} {} {} {:?}", a.id, a.name, py::dec_str(&a.balance), a.owner_id); }
                Ok(())
            }).unwrap();
        }
    } else if args.len() >= 4 && args[1] == "scenario" {
        std::process::exit(scenario::main(&args[2], &args[3]));
    } else if args.len() >= 3 && args[1] == "parse" {
        // identify + parse_with, as JSON, for comparing with the Python reader
        let data = std::fs::read(&args[2]).expect("read");
        let name = args.get(3).cloned().unwrap_or_else(|| std::path::Path::new(&args[2]).file_name().unwrap().to_string_lossy().into_owned());
        let doc = adapters::SourceDocument::new(&name, data);
        let found = adapters::registry::identify(&doc);
        let mut out = serde_json::Map::new();
        out.insert("decision".into(), found.decision.into());
        out.insert("message".into(), found.message.clone().into());
        out.insert("institution".into(), found.institution.clone().map(serde_json::Value::String).unwrap_or(serde_json::Value::Null));
        out.insert("candidates".into(), serde_json::Value::Array(found.candidates.iter().map(|c| c.to_json()).collect()));
        let mut parsed = serde_json::Map::new();
        for c in &found.candidates {
            let v = match adapters::registry::parse_with(c.adapter.id(), &doc) {
                Ok(sts) => serde_json::Value::Array(sts.iter().map(|s| s.to_json()).collect()),
                Err(e) => serde_json::json!({"error": e}),
            };
            parsed.insert(c.adapter.id().into(), v);
        }
        out.insert("parsed".into(), serde_json::Value::Object(parsed));
        println!("{}", py::json_dumps(&serde_json::Value::Object(out)));
    } else if args.len() >= 3 && args[1] == "pdf-text" {
        let data = std::fs::read(&args[2]).expect("read");
        print!("{}", pdf::extract_text(&data).expect("pdf"));
    }
}
