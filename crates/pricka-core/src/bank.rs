use async_trait::async_trait;

use crate::config::ProxyConfig;
use crate::error::Result;
use crate::types::{Account, Balance, DateRange, Transaction};

/// Callback interface for presenting BankID authentication to the user.
pub trait AuthPresenter: Send + Sync {
    /// Display a QR code string (the full bankid.xxx.N.hash payload).
    fn present_qr(&self, qr_data: &str);

    /// Display a status message (e.g., "Waiting for BankID...", "User authenticated").
    fn present_status(&self, status: &str);
}

/// Bank adapter trait. Each bank plugin implements this.
// async-trait marks generated futures as must_use; suppress the duplicate Clippy lint.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait Bank: Send + Sync {
    /// Human-readable bank name.
    fn name(&self) -> &str;

    /// Authenticate with the bank using BankID.
    ///
    /// The `personnummer` is the Swedish personal identity number (YYYYMMDDXXXX).
    /// The `presenter` is used to display QR codes and status updates.
    async fn authenticate(
        &mut self,
        personnummer: &str,
        presenter: &dyn AuthPresenter,
    ) -> Result<()>;

    /// List all accounts.
    async fn list_accounts(&self) -> Result<Vec<Account>>;

    /// Get transactions for an account within a date range.
    async fn get_transactions(
        &self,
        account_id: &str,
        range: &DateRange,
    ) -> Result<Vec<Transaction>>;

    /// Get current balance for an account.
    async fn get_balance(&self, account_id: &str) -> Result<Balance>;

    /// Log out / end the session.
    async fn logout(&mut self) -> Result<()>;
}

/// Create a bank adapter by name.
pub type BankFactory = fn(proxy: ProxyConfig) -> Box<dyn Bank>;
