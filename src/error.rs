use std::fmt;

/// Unified error type for merlint
#[derive(Debug)]
pub enum MerlintError {
    /// Failed to parse a session or trace file
    Parse(String),
    /// IO errors (file read/write)
    Io(std::io::Error),
    /// JSON serialization/deserialization errors
    Json(serde_json::Error),
    /// Database errors
    Db(String),
    /// Proxy/network errors
    Proxy(String),
    /// Configuration errors
    Config(String),
}

impl fmt::Display for MerlintError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MerlintError::Parse(msg) => write!(f, "parse error: {}", msg),
            MerlintError::Io(err) => write!(f, "io error: {}", err),
            MerlintError::Json(err) => write!(f, "json error: {}", err),
            MerlintError::Db(msg) => write!(f, "database error: {}", msg),
            MerlintError::Proxy(msg) => write!(f, "proxy error: {}", msg),
            MerlintError::Config(msg) => write!(f, "config error: {}", msg),
        }
    }
}

impl std::error::Error for MerlintError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            MerlintError::Io(err) => Some(err),
            MerlintError::Json(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for MerlintError {
    fn from(err: std::io::Error) -> Self {
        MerlintError::Io(err)
    }
}

impl From<serde_json::Error> for MerlintError {
    fn from(err: serde_json::Error) -> Self {
        MerlintError::Json(err)
    }
}

impl From<rusqlite::Error> for MerlintError {
    fn from(err: rusqlite::Error) -> Self {
        MerlintError::Db(err.to_string())
    }
}

pub type Result<T> = std::result::Result<T, MerlintError>;
