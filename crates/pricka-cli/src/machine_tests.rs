use super::*;
use std::io::Cursor;
use std::sync::atomic::AtomicUsize;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::NaiveDate;
use pricka_core::{Balance, PrickaError, Transaction};
use rust_decimal::Decimal;

fn date(value: &str) -> NaiveDate {
    NaiveDate::parse_from_str(value, "%Y-%m-%d").unwrap()
}

fn account() -> Account {
    Account {
        id: "synthetic-account".into(),
        name: "Synthetic".into(),
        account_number: "0000".into(),
        balance: None,
        currency: "SEK".into(),
    }
}

#[derive(Default)]
struct FakeBank {
    calls: Mutex<Vec<(String, NaiveDate, NaiveDate)>>,
    fail_on_second_chunk: bool,
    fail_logout: bool,
    logouts: AtomicUsize,
}

#[async_trait]
impl Bank for FakeBank {
    fn name(&self) -> &str {
        "Synthetic bank"
    }

    async fn authenticate(&mut self, _: &str, _: &dyn AuthPresenter) -> pricka_core::Result<()> {
        panic!("Offline sync tests must not authenticate")
    }

    async fn list_accounts(&self) -> pricka_core::Result<Vec<Account>> {
        Ok(vec![account()])
    }

    async fn get_transactions(
        &self,
        id: &str,
        range: &DateRange,
    ) -> pricka_core::Result<Vec<Transaction>> {
        let mut calls = self.calls.lock().unwrap();
        calls.push((id.into(), range.from, range.to));
        if self.fail_on_second_chunk && calls.len() == 2 {
            return Err(PrickaError::Parse("Synthetic invalid row".into()));
        }
        Ok(vec![Transaction {
            date: range.from,
            transaction_date: None,
            description: "Synthetic\ntransaction".into(),
            amount: Decimal::new(-12345, 2),
            balance: None,
        }])
    }

    async fn get_balance(&self, _: &str) -> pricka_core::Result<Balance> {
        panic!("Not used")
    }

    async fn logout(&mut self) -> pricka_core::Result<()> {
        self.logouts.fetch_add(1, Ordering::Relaxed);
        if self.fail_logout {
            return Err(PrickaError::Bank("Synthetic logout failure".into()));
        }
        Ok(())
    }
}

fn events(output: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(output)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn inputs_are_bounded_and_preserve_the_next_line() {
    let mut input =
        Cursor::new(b"{\"personnummer\":null,\"accountId\":null}\n{\"accountId\":\"synthetic\"}\n");
    let start: MachineStart = read_input(&mut input).unwrap();
    assert!(start.personnummer.is_none() && start.account_id.is_none());
    let selection: MachineSelection = read_input(&mut input).unwrap();
    assert_eq!(selection.account_id, "synthetic");
    assert!(read_input::<MachineStart>(&mut input).is_err());
    let mut oversized = Cursor::new(vec![b' '; 10_000]);
    assert!(read_input::<MachineStart>(&mut oversized).is_err());
    assert_eq!(oversized.position(), MAX_INPUT_BYTES + 1);
    let error = read_input::<MachineStart>(&mut Cursor::new(b"private malformed input"))
        .err()
        .unwrap();
    assert_eq!(error.to_string(), "Invalid machine input");
}

#[test]
fn qr_event_contains_a_square_binary_matrix() {
    let event = qr_event("synthetic-qr-payload").unwrap();
    assert_eq!(event["type"], "qr");
    let size = event["size"].as_u64().unwrap() as usize;
    let bits = event["bits"].as_str().unwrap();
    assert_eq!(bits.len(), size * size);
    assert!(bits.bytes().all(|bit| bit == b'0' || bit == b'1'));
}

#[test]
fn chunks_cover_leap_days_and_the_maximum_date_without_overflow() {
    let range = DateRange {
        from: date("2024-02-01"),
        to: date("2024-04-02"),
    };
    let chunks = date_chunks(&range);
    assert_eq!(chunks.first().unwrap().from, range.from);
    assert_eq!(chunks.last().unwrap().to, range.to);
    for chunk in &chunks {
        assert!((chunk.to - chunk.from).num_days() < 30);
    }
    for adjacent in chunks.windows(2) {
        assert_eq!(adjacent[0].to.succ_opt().unwrap(), adjacent[1].from);
    }
    let edge = date_chunks(&DateRange {
        from: NaiveDate::MAX,
        to: NaiveDate::MAX,
    });
    assert_eq!(edge.len(), 1);
    assert_eq!(edge[0].to, NaiveDate::MAX);
}

#[tokio::test]
async fn sync_selects_an_account_and_emits_all_chunks_as_ndjson() {
    let mut bank = FakeBank::default();
    let range = DateRange {
        from: date("2026-01-01"),
        to: date("2026-03-05"),
    };
    let mut input = Cursor::new(b"{\"accountId\":\"synthetic-account\"}\n");
    let mut output = Vec::new();
    let count = run_session(&mut bank, &range, None, &mut input, &mut output)
        .await
        .unwrap();
    assert_eq!(count, 3);
    let parsed = events(&output);
    let types: Vec<_> = parsed
        .iter()
        .map(|event| event["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        types,
        [
            "accounts",
            "account",
            "transaction",
            "transaction",
            "transaction",
            "complete"
        ]
    );
    assert_eq!(parsed[2]["transaction"]["amount"], "-123.45");
    assert_eq!(parsed.last().unwrap()["count"], 3);
    assert_eq!(bank.logouts.load(Ordering::Relaxed), 1);
    assert_eq!(bank.calls.lock().unwrap()[0].0, "synthetic-account");
}

#[tokio::test]
async fn unknown_accounts_fail_before_fetching_transactions() {
    let mut bank = FakeBank::default();
    let range = DateRange {
        from: date("2026-01-01"),
        to: date("2026-01-01"),
    };
    let mut output = Vec::new();
    assert!(run_session(
        &mut bank,
        &range,
        Some("unknown".into()),
        &mut Cursor::new([]),
        &mut output
    )
    .await
    .is_err());
    assert!(bank.calls.lock().unwrap().is_empty());
    assert_eq!(bank.logouts.load(Ordering::Relaxed), 1);
    assert_eq!(events(&output).len(), 1);
}

#[tokio::test]
async fn later_chunk_failure_does_not_report_success() {
    let mut bank = FakeBank {
        fail_on_second_chunk: true,
        ..Default::default()
    };
    let range = DateRange {
        from: date("2026-01-01"),
        to: date("2026-02-01"),
    };
    let mut output = Vec::new();
    assert!(run_session(
        &mut bank,
        &range,
        Some(account().id),
        &mut Cursor::new([]),
        &mut output
    )
    .await
    .is_err());
    assert_eq!(bank.calls.lock().unwrap().len(), 2);
    assert_eq!(bank.logouts.load(Ordering::Relaxed), 1);
    assert!(events(&output)
        .iter()
        .all(|event| event["type"] != "complete"));
}

#[tokio::test]
async fn logout_failure_prevents_a_completion_event() {
    let mut bank = FakeBank {
        fail_logout: true,
        ..Default::default()
    };
    let range = DateRange {
        from: date("2026-01-01"),
        to: date("2026-01-01"),
    };
    let mut output = Vec::new();
    assert!(run_session(
        &mut bank,
        &range,
        Some(account().id),
        &mut Cursor::new([]),
        &mut output
    )
    .await
    .is_err());
    let parsed = events(&output);
    assert_eq!(parsed.last().unwrap()["type"], "transaction");
    assert!(parsed.iter().all(|event| event["type"] != "complete"));
}
