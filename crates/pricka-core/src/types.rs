use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub name: String,
    pub account_number: String,
    pub balance: Option<Decimal>,
    pub currency: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    pub date: NaiveDate,
    pub transaction_date: Option<NaiveDate>,
    pub description: String,
    pub amount: Decimal,
    pub balance: Option<Decimal>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Balance {
    pub available: Decimal,
    pub booked: Option<Decimal>,
    pub currency: String,
}

#[derive(Debug, Clone)]
pub struct DateRange {
    pub from: NaiveDate,
    pub to: NaiveDate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrapeResult {
    pub scraped_at: String,
    pub accounts: Vec<Account>,
    pub transactions: std::collections::HashMap<String, Vec<Transaction>>,
}

/// Parse a Swedish-formatted amount string into a Decimal.
///
/// Handles formats like "3 000,00", "-1\u{a0}234,56", "3000.00"
pub fn parse_swedish_amount(s: &str) -> Option<Decimal> {
    let cleaned: String = s
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '\u{a0}') // remove spaces and NBSP
        .collect();

    let normalized = cleaned.replace(',', ".");

    normalized.parse::<Decimal>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_swedish_amount() {
        assert_eq!(
            parse_swedish_amount("3 000,00"),
            Some(Decimal::new(300000, 2))
        );
        assert_eq!(
            parse_swedish_amount("-1\u{a0}234,56"),
            Some(Decimal::new(-123456, 2))
        );
        assert_eq!(parse_swedish_amount("100,50"), Some(Decimal::new(10050, 2)));
        assert_eq!(
            parse_swedish_amount("3000.00"),
            Some(Decimal::new(300000, 2))
        );
    }
}
