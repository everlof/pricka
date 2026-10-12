use crate::client::browser_get;
use crate::login::SessionTokens;
use chromiumoxide::page::Page;
use pricka_core::{Account, PrickaError, Result};
use rust_decimal::Decimal;
use serde::Deserialize;
use tracing::{debug, info, trace};

const BASE_URL: &str = "https://secure.handelsbanken.se";
const SERVLET_URL: &str = "https://secure.handelsbanken.se/bb/seip/servlet/ipko";
const REFERER: &str = "https://secure.handelsbanken.se/se/private/sv/";

// -- ShowAccountOverview (IPKO GET) response types --

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountOverviewResponse {
    #[serde(default)]
    account_list: Vec<OverviewAccount>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OverviewAccount {
    account_number: String,
    account_name: String,
    currency: String,
    #[serde(default)]
    amount_available: String,
    #[serde(default)]
    balance: String,
    /// "020" = real account, "010" = separator, "030" = total row
    row_id: Option<String>,
}

// -- Minerva REST API response types --

#[derive(Debug, Deserialize)]
struct MinervaAccountsResponse {
    #[serde(default)]
    agreements: Vec<MinervaAgreement>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MinervaAgreement {
    identifier: MinervaIdentifier,
    name: Option<String>,
    #[serde(default)]
    iso_currency_code: Option<String>,
    available_amount: Option<MinervaAmount>,
    current_balance: Option<MinervaAmount>,
    product_segment_type: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MinervaIdentifier {
    #[allow(dead_code)]
    value: Option<String>,
    value_raw: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MinervaAmount {
    #[allow(dead_code)]
    value: Option<String>,
    value_raw: Option<serde_json::Value>,
}

fn value_to_decimal(v: &serde_json::Value) -> Option<Decimal> {
    match v {
        serde_json::Value::Number(n) => n
            .as_f64()
            .and_then(|f| Decimal::try_from(f).ok())
            .or_else(|| n.as_i64().map(Decimal::from)),
        serde_json::Value::String(s) => parse_swedish_decimal(s),
        _ => None,
    }
}

/// Parse Swedish-formatted decimal strings like "6 000,00" or "205 358,95".
fn parse_swedish_decimal(s: &str) -> Option<Decimal> {
    let cleaned: String = s
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '\u{a0}')
        .collect();
    cleaned.replace(',', ".").parse::<Decimal>().ok()
}

/// Fetch accounts — tries ShowAccountOverview first (returns all accounts),
/// falls back to Minerva myAccounts (limited to startpage selection).
pub async fn list_accounts(page: &Page, tokens: &SessionTokens) -> Result<Vec<Account>> {
    debug!("Fetching accounts...");

    // Strategy 1: IPKO ShowAccountOverview (returns ALL accounts, not just startpage)
    match overview_accounts(page, tokens).await {
        Ok(accounts) if !accounts.is_empty() => {
            info!(
                count = accounts.len(),
                "Got accounts from ShowAccountOverview"
            );
            return Ok(accounts);
        }
        Ok(_) => debug!("ShowAccountOverview returned empty account list"),
        Err(e) => debug!(error = %e, "ShowAccountOverview failed, trying Minerva fallback"),
    }

    // Strategy 2: Minerva REST API (limited to ~8 startpage accounts, but has structured data)
    match minerva_accounts(page, tokens).await {
        Ok(accounts) if !accounts.is_empty() => {
            info!(count = accounts.len(), "Got accounts from Minerva API");
            return Ok(accounts);
        }
        Ok(_) => debug!("Minerva returned empty account list"),
        Err(e) => debug!(error = %e, "Minerva API also failed"),
    }

    Err(PrickaError::Bank(
        "Could not retrieve account list from either ShowAccountOverview or Minerva API.".into(),
    ))
}

/// Fetch all accounts via IPKO ShowAccountOverview (GET request).
/// This returns all accounts, not limited to the startpage selection.
async fn overview_accounts(page: &Page, tokens: &SessionTokens) -> Result<Vec<Account>> {
    let url = format!("{SERVLET_URL}?appAction=ShowAccountOverview&appName=ipko");
    debug!(url, "Trying ShowAccountOverview endpoint");

    let headers: Vec<(&str, &str)> = vec![
        ("accept", "application/json"),
        ("x-requested-with", "XMLHttpRequest"),
        ("accept-language", "sv-SE"),
        ("referer", REFERER),
        ("x-shb-synchronized-token", &tokens.synchronized_token),
        ("x-shb-characteristicskey", &tokens.characteristics_key),
        ("x-shb-featuretoggleskey", &tokens.feature_toggles_key),
    ];

    let resp = browser_get(page, &url, &headers).await?;

    trace!(
        status = resp.status,
        body_len = resp.body.len(),
        "ShowAccountOverview response"
    );

    if resp.status != 200 {
        return Err(PrickaError::Http(format!(
            "ShowAccountOverview returned {}",
            resp.status
        )));
    }

    let overview: AccountOverviewResponse = serde_json::from_str(&resp.body)
        .map_err(|e| PrickaError::Parse(format!("ShowAccountOverview parse: {e}")))?;

    Ok(overview
        .account_list
        .iter()
        .filter(|a| {
            // Filter out separator rows (010) and total rows (030)
            // Only keep real account rows (020)
            let row_id = a.row_id.as_deref().unwrap_or("");
            let is_real = row_id == "020" || (!a.account_number.is_empty() && row_id.is_empty());
            if !is_real {
                trace!(row_id, "Skipping non-account row");
            }
            is_real
        })
        .map(|a| {
            // Account numbers can contain spaces; strip them before building the ID.
            let account_number: String = a
                .account_number
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();

            let name = if a.account_name.is_empty() {
                account_number.clone()
            } else {
                a.account_name.clone()
            };

            let balance = parse_swedish_decimal(&a.amount_available)
                .or_else(|| parse_swedish_decimal(&a.balance));

            let currency = if a.currency.is_empty() {
                "SEK".to_string()
            } else {
                a.currency.clone()
            };

            // Build IPKO-compatible ID for transaction fetching
            let id = format!("{account_number}~INLÅ~N");

            Account {
                id,
                name,
                account_number,
                balance,
                currency,
            }
        })
        .collect())
}

/// Try the Minerva REST API for account listing (fallback — limited to startpage).
async fn minerva_accounts(page: &Page, tokens: &SessionTokens) -> Result<Vec<Account>> {
    let url = format!("{BASE_URL}/rseda/rykk/bu/accounts/v1/myAccounts");
    debug!(url, "Trying Minerva myAccounts endpoint");

    let headers: Vec<(&str, &str)> = vec![
        ("accept", "application/json, application/problem+json"),
        ("accept-language", "sv"),
        ("x-requested-with", "fetch"),
        ("content-type", "application/json;charset=UTF-8"),
        ("referer", REFERER),
        ("x-shb-synchronized-token", &tokens.synchronized_token),
        ("x-shb-characteristicskey", &tokens.characteristics_key),
        ("x-shb-featuretoggleskey", &tokens.feature_toggles_key),
    ];

    let resp = browser_get(page, &url, &headers).await?;

    trace!(
        status = resp.status,
        body_len = resp.body.len(),
        "Minerva myAccounts response"
    );

    if resp.status != 200 {
        return Err(PrickaError::Http(format!(
            "Minerva myAccounts returned {}",
            resp.status
        )));
    }

    let minerva: MinervaAccountsResponse = serde_json::from_str(&resp.body)
        .map_err(|e| PrickaError::Parse(format!("Minerva parse: {e}")))?;

    Ok(minerva
        .agreements
        .iter()
        .filter_map(|a| {
            let account_number = match &a.identifier.value_raw {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(serde_json::Value::Number(n)) => n.to_string(),
                _ => return None,
            };

            let name = a.name.clone().unwrap_or_else(|| account_number.clone());

            let balance = a
                .available_amount
                .as_ref()
                .and_then(|amt| amt.value_raw.as_ref())
                .and_then(value_to_decimal)
                .or_else(|| {
                    a.current_balance
                        .as_ref()
                        .and_then(|amt| amt.value_raw.as_ref())
                        .and_then(value_to_decimal)
                });

            let currency = a
                .iso_currency_code
                .clone()
                .unwrap_or_else(|| "SEK".to_string());

            // Build IPKO-compatible ID for transaction fetching
            let segment = a.product_segment_type.as_deref().unwrap_or("account");
            let system = if segment == "account" {
                "INLÅ"
            } else {
                segment
            };
            let id = format!("{account_number}~{system}~N");

            Some(Account {
                id,
                name,
                account_number,
                balance,
                currency,
            })
        })
        .collect())
}
