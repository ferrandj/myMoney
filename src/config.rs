//! Runtime configuration, read from environment variables (see .env.example).

use std::path::PathBuf;

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

fn env_bool(key: &str, default: bool) -> bool {
    match env(key) {
        None => default,
        Some(v) => matches!(v.trim().to_lowercase().as_str(), "1" | "true" | "yes" | "on"),
    }
}

fn env_int(key: &str, default: i64) -> i64 {
    env(key).and_then(|v| v.trim().parse().ok()).unwrap_or(default)
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub data_dir: PathBuf,
    /// Mark the session cookie "Secure". Enable it as soon as the app is served over HTTPS.
    pub cookie_secure: bool,
    /// Minutes of inactivity after which the vault is locked and the key wiped from memory.
    pub auto_lock_minutes: i64,
    /// Opt-in unattended unlock from a Docker secret file. Lowers security: see SECURITY.md.
    pub unlock_password_file: Option<String>,
    /// E-mail of the user whose password is in that file (needed once there are several users).
    pub unlock_email: Option<String>,
    /// Cloudflare Access: with both set, a request carrying a valid signed Access token signs in the user of its e-mail.
    pub cf_team_domain: Option<String>,
    pub cf_audience: Option<String>,
    /// Extra origins accepted for form posts (the public address behind a tunnel), separated by commas.
    pub trusted_origins: String,
    pub max_upload_mb: i64,
    /// Argon2id cost. Defaults fit a modest NAS; raise memory if the NAS has RAM to spare.
    pub argon2_memory_kib: u32,
    pub argon2_time_cost: u32,
}

impl Settings {
    pub fn from_env() -> Settings {
        let data_dir = PathBuf::from(env("MYMONEY_DATA_DIR").unwrap_or_else(|| "./data".into()));
        let data_dir = std::path::absolute(&data_dir).unwrap_or(data_dir);
        Settings {
            data_dir,
            cookie_secure: env_bool("MYMONEY_COOKIE_SECURE", false),
            auto_lock_minutes: env_int("MYMONEY_AUTO_LOCK_MINUTES", 15),
            unlock_password_file: env("MYMONEY_UNLOCK_PASSWORD_FILE"),
            unlock_email: env("MYMONEY_UNLOCK_EMAIL"),
            cf_team_domain: env("MYMONEY_CF_TEAM_DOMAIN"),
            cf_audience: env("MYMONEY_CF_AUD"),
            trusted_origins: env("MYMONEY_TRUSTED_ORIGINS").unwrap_or_default(),
            max_upload_mb: env_int("MYMONEY_MAX_UPLOAD_MB", 25),
            argon2_memory_kib: env_int("MYMONEY_ARGON2_MEMORY_KIB", 128 * 1024) as u32,
            argon2_time_cost: env_int("MYMONEY_ARGON2_TIME_COST", 3) as u32,
        }
    }

    pub fn keystore_path(&self) -> PathBuf {
        self.data_dir.join("keystore.json")
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("mymoney.db")
    }

    pub fn documents_dir(&self) -> PathBuf {
        self.data_dir.join("documents")
    }

    /// Files created by the app (database, WAL, documents) are readable by its user only.
    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        #[cfg(unix)]
        unsafe {
            libc_umask(0o077);
        }
        for p in [self.data_dir.clone(), self.documents_dir()] {
            std::fs::create_dir_all(&p)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700))?;
            }
        }
        Ok(())
    }
}

#[cfg(unix)]
extern "C" {
    #[link_name = "umask"]
    fn libc_umask(mask: u32) -> u32;
}
