use chromiumoxide::page::Page;
use pricka_core::{Account, PrickaError, Result};
use rust_decimal::Decimal;
use serde::Deserialize;
use tracing::debug;

use crate::client::browser_get;
use crate::login::SessionInfo;

const ACCOUNTS_URL: &str = "https://apps.seb.se/ssc/accounts-web-service-corporate/accounts";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountsResponse {
    grouped_accounts: Vec<SebAccount>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SebAccount {
    id: String,
    name: String,
    account_number: String,
    currency: String,
    available_balance: Option<String>,
}

pub async fn list_accounts(page: &Page, info: &SessionInfo) -> Result<Vec<Account>> {
    let resp = browser_get(
        page,
        ACCOUNTS_URL,
        &[
            ("accept", "application/json, text/plain, */*"),
            ("content-type", "application/json"),
            ("client-id", &info.client_id),
            ("organization-id", &info.organization_id),
        ],
    )
    .await?;

    if resp.status != 200 {
        return Err(PrickaError::Http(format!(
            "SEB accounts returned {}",
            resp.status
        )));
    }

    let data: AccountsResponse = serde_json::from_str(&resp.body)
        .map_err(|e| PrickaError::Parse(format!("SEB accounts response: {e}")))?;

    debug!("SEB returned {} accounts", data.grouped_accounts.len());

    let accounts = data
        .grouped_accounts
        .into_iter()
        .map(|a| {
            let balance = a
                .available_balance
                .as_deref()
                .and_then(|s| s.parse::<Decimal>().ok());

            Account {
                id: a.id,
                name: a.name,
                account_number: a.account_number,
                balance,
                currency: a.currency,
            }
        })
        .collect();

    Ok(accounts)
}
