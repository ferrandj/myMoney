//! The errors the application distinguishes (the Python exceptions it catches).

#[derive(Debug, Clone)]
pub enum AppError {
    /// The vault is locked (VaultLocked).
    Locked,
    /// The user may not do this (AccessDenied): the web layer answers 404.
    Denied(String),
    /// A wrong password or recovery code (InvalidPassword).
    InvalidPassword,
    /// Bad input or a refused action (ValueError): the message is shown to the user.
    Value(String),
    /// Too many attempts (PermissionError).
    Throttled(String),
    /// Anything else (a bug, a database error).
    Other(String),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppError::Locked => write!(f, "Coffre verrouillé"),
            AppError::Denied(m) | AppError::Value(m) | AppError::Throttled(m) | AppError::Other(m) => write!(f, "{m}"),
            AppError::InvalidPassword => write!(f, "Mot de passe invalide"),
        }
    }
}

impl std::error::Error for AppError {}

impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        AppError::Other(format!("Base de données : {e}"))
    }
}

impl From<crate::security::vault::VaultLocked> for AppError {
    fn from(_: crate::security::vault::VaultLocked) -> Self {
        AppError::Locked
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError::Other(e.to_string())
    }
}

pub type AppResult<T> = Result<T, AppError>;

pub fn value_err<T>(msg: impl Into<String>) -> AppResult<T> {
    Err(AppError::Value(msg.into()))
}
