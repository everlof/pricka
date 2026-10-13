use crate::client::browser_post;
use crate::login::SessionTokens;
use chromiumoxide::page::Page;
use chrono::NaiveDate;
use pricka_core::{DateRange, PrickaError, Result, Transaction};
use rust_decimal::Decimal;
use serde::Deserialize;
use tracing::{debug, info, trace};

const SERVLET_URL: &str = "https://secure.handelsbanken.se/bb/seip/servlet/ipko";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TransactionResponse {
    inla_account_transactions: Vec<RawTransaction>,
    #[allow(dead_code)]
    account_information: Option<AccountInfo>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct AccountInfo {
    book_balance: Option<String>,
    available_amount: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawTransaction {
    transaction_text: String,
    transaction_date: String,
    ledger_date: String,
    transaction_amount: String,
    #[allow(dead_code)]
    serial_number: Option<String>,
    #[allow(dead_code)]
    system_name: Option<String>,
}

fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}

fn parse_amount(s: &str) -> Option<Decimal> {
    let cleaned: String = s
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '\u{a0}')
        .collect();
    let normalized = cleaned.replace(',', ".");
    normalized.parse::<Decimal>().ok()
}

/// Max days per chunk — Handelsbanken IPKO returns empty for large ranges.
const CHUNK_DAYS: i64 = 90;

/// Fetch transactions for an account within a date range.
///
/// Uses the Espresso/IPKO servlet with `ShowAccountTransactions` action.
/// Large date ranges are split into 90-day chunks since the API silently
/// returns empty results for ranges that are too wide.
pub async fn get_transactions(
    page: &Page,
    tokens: &SessionTokens,
    account_id: &str,
    range: &DateRange,
) -> Result<Vec<Transaction>> {
    debug!(from = %range.from, to = %range.to, "Fetching transactions");

    let mut all_transactions = Vec::new();
    let mut chunk_start = range.from;

    while chunk_start <= range.to {
        let chunk_end = (chunk_start + chrono::Duration::days(CHUNK_DAYS - 1)).min(range.to);

        debug!(from = %chunk_start, to = %chunk_end, "Fetching chunk");
        let txs = fetch_chunk(page, tokens, account_id, chunk_start, chunk_end).await?;
        all_transactions.extend(txs);

        chunk_start = chunk_end + chrono::Duration::days(1);
    }

    debug!(total = all_transactions.len(), "Transactions parsed");

    Ok(all_transactions)
}

/// Fetch a single chunk of transactions.
async fn fetch_chunk(
    page: &Page,
    tokens: &SessionTokens,
    account_id: &str,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<Transaction>> {
    let body = serde_json::json!({
        "account": account_id,
        "dateFrom": from.to_string(),
        "dateTo": to.to_string(),
        "transactionType": "A",
        "amountFrom": "",
        "amountTo": "",
    });

    let url = format!("{SERVLET_URL}?appName=ipko&appAction=ShowAccountTransactions");

    let headers: Vec<(&str, &str)> = vec![
        ("accept", "application/json"),
        ("x-requested-with", "XMLHttpRequest"),
        ("accept-language", "sv-SE"),
        ("x-shb-synchronized-token", &tokens.synchronized_token),
        ("x-shb-characteristicskey", &tokens.characteristics_key),
        ("x-shb-featuretoggleskey", &tokens.feature_toggles_key),
    ];

    let resp = browser_post(page, &url, &headers, &body).await?;
    if resp.status != 200 {
        return Err(PrickaError::Http(format!(
            "Handelsbanken transactions returned {}",
            resp.status
        )));
    }

    trace!(status = resp.status, "Transaction response");

    trace!(body_len = resp.body.len(), "Transaction response body");

    let tx_resp: TransactionResponse = serde_json::from_str(&resp.body)
        .map_err(|_| PrickaError::Parse("Unexpected transaction response".into()))?;

    info!(
        count = tx_resp.inla_account_transactions.len(),
        %from, %to, "Got chunk"
    );

    parse_transactions(tx_resp)
}

fn parse_transactions(response: TransactionResponse) -> Result<Vec<Transaction>> {
    response
        .inla_account_transactions
        .into_iter()
        .map(|tx| {
            let transaction_date = parse_date(&tx.transaction_date)
                .ok_or_else(|| PrickaError::Parse("Invalid transaction date".into()))?;
            let amount = parse_amount(&tx.transaction_amount)
                .ok_or_else(|| PrickaError::Parse("Invalid transaction amount".into()))?;

            let ledger_date = if tx.ledger_date == "9999-12-31" {
                None
            } else {
                Some(
                    parse_date(&tx.ledger_date)
                        .ok_or_else(|| PrickaError::Parse("Invalid ledger date".into()))?,
                )
            };

            Ok(Transaction {
                date: ledger_date.unwrap_or(transaction_date),
                transaction_date: Some(transaction_date),
                description: tx.transaction_text,
                amount,
                balance: None,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn row() -> Value {
        serde_json::from_str(r#"{"transactionText":"Synthetic","transactionDate":"2026-01-01","ledgerDate":"2026-01-02","transactionAmount":"-1 234,56"}"#).unwrap()
    }

    #[test]
    fn synthetic_transactions_preserve_amounts_and_booking_dates() {
        let response = serde_json::from_value(json!({"inlaAccountTransactions": [row()]})).unwrap();
        let transactions = parse_transactions(response).unwrap();
        assert_eq!(transactions.len(), 1);
        assert_eq!(transactions[0].amount, Decimal::new(-123456, 2));
        assert_eq!(
            transactions[0].date,
            NaiveDate::from_ymd_opt(2026, 1, 2).unwrap()
        );
        assert_eq!(
            transactions[0].transaction_date,
            NaiveDate::from_ymd_opt(2026, 1, 1)
        );
    }

    #[test]
    fn malformed_rows_fail_the_entire_response_instead_of_being_skipped() {
        for field in ["transactionDate", "ledgerDate", "transactionAmount"] {
            let mut malformed = row();
            malformed[field] = json!("invalid");
            let response =
                serde_json::from_value(json!({"inlaAccountTransactions": [row(), malformed]}))
                    .unwrap();
            assert!(matches!(
                parse_transactions(response),
                Err(PrickaError::Parse(_))
            ));
        }
    }

    #[test]
    fn missing_list_fails_and_unbooked_date_uses_transaction_date() {
        assert!(serde_json::from_value::<TransactionResponse>(json!({})).is_err());
        let response = serde_json::from_value(json!({"inlaAccountTransactions": []})).unwrap();
        assert!(parse_transactions(response).unwrap().is_empty());
        let mut unbooked = row();
        unbooked["ledgerDate"] = json!("9999-12-31");
        let response =
            serde_json::from_value(json!({"inlaAccountTransactions": [unbooked]})).unwrap();
        let transactions = parse_transactions(response).unwrap();
        assert_eq!(
            transactions[0].date,
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()
        );
    }
}
