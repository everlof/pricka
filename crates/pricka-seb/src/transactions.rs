use chromiumoxide::page::Page;
use chrono::NaiveDate;
use pricka_core::{DateRange, PrickaError, Result, Transaction};
use rust_decimal::Decimal;
use serde::Deserialize;
use tracing::debug;

use crate::client::browser_post;
use crate::login::SessionInfo;

const SEARCH_URL: &str =
    "https://apps.seb.se/ssc/accounts-web-service-corporate/search-transactions";

#[derive(Debug, Deserialize)]
struct SearchResponse {
    transactions: Vec<SebTransaction>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SebTransaction {
    booked_date: String,
    value_date: Option<String>,
    text: String,
    outgoing_amount: Option<String>,
    ingoing_amount: Option<String>,
    booked_balance: Option<String>,
}

pub async fn get_transactions(
    page: &Page,
    info: &SessionInfo,
    account_id: &str,
    range: &DateRange,
) -> Result<Vec<Transaction>> {
    let body = serde_json::json!({
        "accountIds": [account_id],
        "dateFrom": range.from.format("%Y-%m-%d").to_string(),
        "dateTo": range.to.format("%Y-%m-%d").to_string(),
        "paginationKey": null,
        "paginationSize": 500,
        "creditDebitIndicator": null,
        "proprietaryType": null
    });

    let resp = browser_post(
        page,
        SEARCH_URL,
        &[
            ("accept", "application/json, text/plain, */*"),
            ("content-type", "application/json"),
            ("client-id", &info.client_id),
            ("organization-id", &info.organization_id),
        ],
        &body,
    )
    .await?;

    if resp.status != 200 {
        return Err(PrickaError::Http(format!(
            "SEB transactions returned {}",
            resp.status
        )));
    }

    let data: SearchResponse = serde_json::from_str(&resp.body)
        .map_err(|e| PrickaError::Parse(format!("SEB transactions response: {e}")))?;

    debug!("SEB returned {} transactions", data.transactions.len());

    let transactions = data
        .transactions
        .into_iter()
        .filter_map(|t| {
            let date = NaiveDate::parse_from_str(&t.booked_date, "%Y-%m-%d").ok()?;

            let transaction_date = t
                .value_date
                .as_deref()
                .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());

            // SEB provides separate outgoing (negative) and ingoing (positive) amounts
            let amount = t
                .outgoing_amount
                .as_deref()
                .or(t.ingoing_amount.as_deref())
                .and_then(|s| s.parse::<Decimal>().ok())?;

            let balance = t
                .booked_balance
                .as_deref()
                .and_then(|s| s.parse::<Decimal>().ok());

            Some(Transaction {
                date,
                transaction_date,
                description: t.text,
                amount,
                balance,
            })
        })
        .collect();

    Ok(transactions)
}
