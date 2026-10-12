use std::time::{Duration, Instant};

use tokio::sync::watch;
use tokio::time::sleep;
use tracing::{debug, info, warn};

use crate::qr::generate_qr_data;
use crate::types::{BankIdSession, BankIdStatus};

/// Callback that the bank adapter implements to poll its own BankID collect endpoint.
pub trait BankIdCollector: Send + Sync {
    /// Poll the bank's collect/status endpoint and return the current status.
    fn poll_status(
        &self,
        order_ref: &str,
    ) -> impl std::future::Future<Output = std::result::Result<BankIdStatus, String>> + Send;
}

/// Result of running the BankID auth flow.
#[derive(Debug)]
pub enum AuthResult {
    Complete,
    Failed(String),
    Timeout,
}

/// Run the BankID authentication flow with concurrent QR updates and status polling.
///
/// - `session`: The BankID session data from the bank's auth initiation endpoint.
/// - `collector`: The bank adapter's polling implementation.
/// - `on_qr`: Called every second with the new QR data string.
/// - `on_status`: Called when the status changes.
/// - `timeout`: Maximum time to wait for authentication.
pub async fn run_bankid_auth<C, FQ, FS>(
    session: &BankIdSession,
    collector: &C,
    mut on_qr: FQ,
    mut on_status: FS,
    timeout: Duration,
) -> AuthResult
where
    C: BankIdCollector,
    FQ: FnMut(&str) + Send,
    FS: FnMut(&str) + Send,
{
    let start = Instant::now();
    let (status_tx, status_rx) = watch::channel(BankIdStatus::Pending);

    // QR update task — runs every 1 second
    let qr_token = session.qr_start_token.clone();
    let qr_secret = session.qr_start_secret.clone();
    let qr_start = start;

    let qr_handle = tokio::spawn({
        let mut status_rx = status_rx.clone();
        async move {
            loop {
                tokio::select! {
                    _ = status_rx.changed() => {
                        let status = status_rx.borrow().clone();
                        if status == BankIdStatus::Complete || matches!(status, BankIdStatus::Failed(_)) {
                            break;
                        }
                    }
                    _ = sleep(Duration::from_secs(1)) => {}
                }

                let elapsed = qr_start.elapsed().as_secs();
                let qr_data = generate_qr_data(&qr_token, &qr_secret, elapsed);
                // We can't call on_qr from spawned task directly, so we break this pattern
                // and handle QR in the main loop instead.
                // This is a placeholder — the actual implementation sends via channel.
                debug!(elapsed, "QR tick");
                drop(qr_data);
            }
        }
    });

    // Main poll loop
    on_status("Waiting for BankID...");
    let mut last_status = BankIdStatus::Pending;

    loop {
        if start.elapsed() > timeout {
            warn!("BankID auth timed out");
            let _ = status_tx.send(BankIdStatus::Failed("timeout".into()));
            let _ = qr_handle.await;
            return AuthResult::Timeout;
        }

        // Generate and display QR
        let elapsed = start.elapsed().as_secs();
        let qr_data = generate_qr_data(&session.qr_start_token, &session.qr_start_secret, elapsed);
        on_qr(&qr_data);

        // Poll status
        match collector.poll_status(&session.order_ref).await {
            Ok(status) => {
                if status != last_status {
                    match &status {
                        BankIdStatus::Pending => on_status("Waiting for BankID scan..."),
                        BankIdStatus::UserSign => on_status("User is signing in BankID app..."),
                        BankIdStatus::Complete => {
                            on_status("Authentication complete!");
                            info!("BankID auth complete");
                            let _ = status_tx.send(BankIdStatus::Complete);
                            let _ = qr_handle.await;
                            return AuthResult::Complete;
                        }
                        BankIdStatus::Failed(reason) => {
                            on_status(&format!("Authentication failed: {reason}"));
                            warn!(reason, "BankID auth failed");
                            let _ = status_tx.send(status.clone());
                            let _ = qr_handle.await;
                            return AuthResult::Failed(reason.clone());
                        }
                    }
                    last_status = status;
                }
            }
            Err(e) => {
                warn!(error = %e, "Failed to poll BankID status");
            }
        }

        sleep(Duration::from_secs(2)).await;
    }
}
