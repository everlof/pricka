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
        .map_err(|_| PrickaError::Parse("Unexpected SEB transaction response".into()))?;

    debug!("SEB returned {} transactions", data.transactions.len());

    parse_transactions(data)
}

fn parse_transactions(data: SearchResponse) -> Result<Vec<Transaction>> {
    if data.transactions.len() >= 500 {
        return Err(PrickaError::Bank(
            "SEB returned a full page; a smaller range is required".into(),
        ));
    }
    data.transactions
        .into_iter()
        .map(|t| {
            let date = NaiveDate::parse_from_str(&t.booked_date, "%Y-%m-%d")
                .map_err(|_| PrickaError::Parse("Invalid booked date".into()))?;

            let transaction_date = t
                .value_date
                .as_deref()
                .map(|s| {
                    NaiveDate::parse_from_str(s, "%Y-%m-%d")
                        .map_err(|_| PrickaError::Parse("Invalid value date".into()))
                })
                .transpose()?;

            // SEB provides separate outgoing (negative) and ingoing (positive) amounts
            let amount = t
                .outgoing_amount
                .as_deref()
                .or(t.ingoing_amount.as_deref())
                .ok_or_else(|| PrickaError::Parse("Missing transaction amount".into()))?
                .parse::<Decimal>()
                .map_err(|_| PrickaError::Parse("Invalid transaction amount".into()))?;

            let balance = t
                .booked_balance
                .as_deref()
                .map(|s| {
                    s.parse::<Decimal>()
                        .map_err(|_| PrickaError::Parse("Invalid booked balance".into()))
                })
                .transpose()?;

            Ok(Transaction {
                date,
                transaction_date,
                description: t.text,
                amount,
                balance,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn row() -> Value {
        serde_json::from_str(r#"{"bookedDate":"2026-01-02","valueDate":"2026-01-01","text":"Synthetic","outgoingAmount":"-1234.56","bookedBalance":"3000.00"}"#).unwrap()
    }

    #[test]
    fn synthetic_transactions_preserve_amounts_and_booking_dates() {
        let response = serde_json::from_value(json!({"transactions": [row()]})).unwrap();
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
        for field in ["bookedDate", "valueDate", "outgoingAmount", "bookedBalance"] {
            let mut malformed = row();
            malformed[field] = json!("invalid");
            let response =
                serde_json::from_value(json!({"transactions": [row(), malformed]})).unwrap();
            assert!(matches!(
                parse_transactions(response),
                Err(PrickaError::Parse(_))
            ));
        }
    }

    #[test]
    fn missing_amount_fails_but_optional_dates_and_balances_can_be_absent() {
        let mut missing = row();
        missing["outgoingAmount"] = Value::Null;
        let response = serde_json::from_value(json!({"transactions": [missing]})).unwrap();
        assert!(parse_transactions(response).is_err());
        let mut incoming = row();
        incoming["outgoingAmount"] = Value::Null;
        incoming["ingoingAmount"] = json!("1234.56");
        incoming["valueDate"] = Value::Null;
        incoming["bookedBalance"] = Value::Null;
        let response = serde_json::from_value(json!({"transactions": [incoming]})).unwrap();
        let transactions = parse_transactions(response).unwrap();
        assert_eq!(transactions[0].amount, Decimal::new(123456, 2));
        assert!(transactions[0].transaction_date.is_none());
        assert!(transactions[0].balance.is_none());
    }

    #[test]
    fn a_full_page_cannot_be_reported_as_a_complete_export() {
        let response = serde_json::from_value(json!({"transactions": vec![row(); 500]})).unwrap();
        assert!(matches!(
            parse_transactions(response),
            Err(PrickaError::Bank(_))
        ));
    }
}
