use std::io::{BufRead, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use pricka_core::{Account, AuthPresenter, Bank, DateRange};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const MAX_INPUT_BYTES: u64 = 4096;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MachineStart {
    pub personnummer: Option<String>,
    pub account_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MachineSelection {
    account_id: String,
}

#[derive(Default)]
pub(crate) struct MachinePresenter {
    failed: AtomicBool,
}

impl MachinePresenter {
    fn present(&self, event: Result<Value>) {
        if event.and_then(|event| emit(&event)).is_err() {
            self.failed.store(true, Ordering::Relaxed);
        }
    }

    pub(crate) fn check(&self) -> Result<()> {
        if self.failed.load(Ordering::Relaxed) {
            return Err("Authentication event could not be emitted".into());
        }
        Ok(())
    }
}

impl AuthPresenter for MachinePresenter {
    fn present_qr(&self, qr_data: &str) {
        self.present(qr_event(qr_data));
    }

    fn present_status(&self, status: &str) {
        self.present(Ok(json!({ "type": "status", "message": status })));
    }
}

fn qr_event(data: &str) -> Result<Value> {
    let code = qrcode::QrCode::new(data)?;
    let bits: String = code
        .to_colors()
        .iter()
        .map(|color| {
            if *color == qrcode::Color::Dark {
                '1'
            } else {
                '0'
            }
        })
        .collect();
    Ok(json!({ "type": "qr", "size": code.width(), "bits": bits }))
}

pub(crate) fn emit(value: &Value) -> Result<()> {
    write_event(&mut std::io::stdout().lock(), value)
}

fn write_event(output: &mut impl Write, value: &Value) -> Result<()> {
    serde_json::to_writer(&mut *output, value)?;
    writeln!(output)?;
    output.flush()?;
    Ok(())
}

pub(crate) fn read_input<T: DeserializeOwned>(input: &mut impl BufRead) -> Result<T> {
    let mut line = Vec::new();
    let bytes = input
        .take(MAX_INPUT_BYTES + 1)
        .read_until(b'\n', &mut line)?;
    if bytes == 0 || bytes as u64 > MAX_INPUT_BYTES {
        return Err("Missing or oversized machine input".into());
    }
    serde_json::from_slice(&line).map_err(|_| "Invalid machine input".into())
}

pub(crate) async fn run_session(
    bank: &mut dyn Bank,
    range: &DateRange,
    selected_id: Option<String>,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<usize> {
    let result = async {
        let accounts = bank.list_accounts().await?;
        run_sync(bank, accounts, range, selected_id, input, output).await
    }
    .await;
    // End the authenticated session even when selection or retrieval fails.
    let logout = bank.logout().await;
    let count = result?;
    logout?;
    write_event(
        output,
        &json!({
            "type": "complete",
            "count": count,
            "scrapedAt": chrono::Utc::now().to_rfc3339(),
        }),
    )?;
    Ok(count)
}

async fn run_sync(
    bank: &dyn Bank,
    accounts: Vec<Account>,
    range: &DateRange,
    selected_id: Option<String>,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<usize> {
    write_event(output, &json!({ "type": "accounts", "accounts": accounts }))?;
    let account_id = match selected_id {
        Some(id) => id,
        None => read_input::<MachineSelection>(input)?.account_id,
    };
    let account = accounts
        .into_iter()
        .find(|account| account.id == account_id)
        .ok_or("Selected account was not returned by the bank")?;
    write_event(output, &json!({ "type": "account", "account": account }))?;

    let mut count = 0;
    for chunk in date_chunks(range) {
        let transactions = bank.get_transactions(&account.id, &chunk).await?;
        count += transactions.len();
        for transaction in transactions {
            write_event(
                output,
                &json!({ "type": "transaction", "transaction": transaction }),
            )?;
        }
    }
    Ok(count)
}

fn date_chunks(range: &DateRange) -> Vec<DateRange> {
    let mut chunks = Vec::new();
    let mut start = range.from;
    while start <= range.to {
        let end = start
            .checked_add_signed(chrono::Duration::days(29))
            .unwrap_or(range.to)
            .min(range.to);
        chunks.push(DateRange {
            from: start,
            to: end,
        });
        if end == range.to {
            break;
        }
        start = end.succ_opt().expect("end is before range.to");
    }
    chunks
}

#[cfg(test)]
#[path = "machine_tests.rs"]
mod tests;
