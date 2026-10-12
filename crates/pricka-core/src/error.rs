use thiserror::Error;

#[derive(Debug, Error)]
pub enum PrickaError {
    #[error("Authentication failed: {0}")]
    AuthFailed(String),

    #[error("Authentication timed out")]
    AuthTimeout,

    #[error("HTTP error: {0}")]
    Http(String),

    #[error("Parse error: {0}")]
    Parse(String),

    #[error("Session expired")]
    SessionExpired,

    #[error("Bank error: {0}")]
    Bank(String),

    #[error("Not implemented: {0}")]
    NotImplemented(String),

    #[error("Browser error: {0}")]
    Browser(String),
}

pub type Result<T> = std::result::Result<T, PrickaError>;
