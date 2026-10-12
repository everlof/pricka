use serde::{Deserialize, Serialize};

/// Data returned by the bank when initiating a BankID authentication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BankIdSession {
    /// Token used in QR code generation (qrStartToken).
    pub qr_start_token: String,

    /// Secret used for HMAC in QR code generation (qrStartSecret).
    pub qr_start_secret: String,

    /// Order reference for polling the collect endpoint.
    pub order_ref: String,

    /// Auto-start token (for launching BankID app directly).
    pub auto_start_token: Option<String>,
}

/// Status from polling the bank's BankID collect endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BankIdStatus {
    /// Waiting for user to scan QR / open BankID app.
    Pending,

    /// User has scanned, waiting for them to authenticate.
    UserSign,

    /// Authentication complete.
    Complete,

    /// Authentication failed or was cancelled.
    Failed(String),
}
