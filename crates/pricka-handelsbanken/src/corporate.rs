use crate::client::{BrowserSession, browser_get};
use crate::corporate_login;
use async_trait::async_trait;
use chromiumoxide::page::Page;
use chrono::NaiveDate;
use pricka_core::{
    Account, AuthPresenter, Balance, Bank, DateRange, PrickaError, ProxyConfig, Result, Transaction,
};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::sync::Mutex;
use std::time::Duration;

const ORIGIN: &str = "https://secure.handelsbanken.se";
const OVERVIEW_URL: &str = "https://secure.handelsbanken.se/rsega/rykk/bu/accounts/v2/overview?displayReference=index/shb-seco-app-modules-accounts";
const TRANSACTION_URL: &str = "https://secure.handelsbanken.se/rsega/rykk/bu/accounts/v2/defaultAccount/transactions?displayReference=index%2Fshb-seco-app-modules-account-transactions";
const TRANSACTION_SETTINGS_URL: &str = "https://secure.handelsbanken.se/rsega/rykk/bu/accounts/v2/defaultAccount/transactions/settings?displayReference=index/shb-seco-app-modules-account-transactions";
// Booking-date bounds of the bank's full-history link.
const HISTORY_FROM_PARAMETER: &str = "BOKF_DAT_FOM";
const HISTORY_TO_PARAMETER: &str = "BOKF_DAT_TOM";
const HISTORY_ACCOUNT_PARAMETER: &str = "KONTO";
// On 2026-10-02 one 364-day window returned the same rows as four 91-day
// windows. The older IPKO service silently dropped rows for wide requests, so
// keep each page request within the verified window size. The bank served 24
// months of company history; older windows report `RangeUnavailable`.
const HISTORY_WINDOW_DAYS: i64 = 91;
// Applies to opening a window and, separately, to collecting its pages.
const HISTORY_PAGE_TIMEOUT: Duration = Duration::from_secs(30);
// Spaces consecutive history page loads, as a person paging through would.
const HISTORY_WINDOW_PAUSE: Duration = Duration::from_secs(1);
// Bounds the rows copied out of one history window across all of its pages.
const MAX_HISTORY_PAGE_ROWS: usize = 5_000;
// Columns per row after colspan expansion; the observed table has seven.
const MAX_HISTORY_COLUMNS: usize = 32;

// Bounds the page text and message copied for notice matching.
const MAX_HISTORY_PAGE_TEXT: usize = 200_000;
const MAX_HISTORY_MESSAGE: usize = 2_000;
// Word signatures of the bank's message box (element id "hhMsg"), observed on
// 2026-10-02. Every window without booked rows showed the empty message, and a
// window from 2000 showed the history-limit message. All words must appear.
const EMPTY_MESSAGE_WORDS: &[&str] = &["ej", "kontot", "period"];
const RANGE_LIMIT_WORDS: &[&str] = &["datum", "månader", "längre"];
const ERROR_NOTICES: &[&str] = &[
    "felaktig",
    "ogiltig",
    "tekniskt fel",
    "försök igen",
    "kan inte",
    "får inte",
    "för lång",
    "för många",
    "tidigare än",
    "äldre än",
    "överskrid",
    "error",
];

// Copies the open history page: every table row in document order (cells
// expanded by colspan), the visible text and message box for notice matching,
// and whether the history search form is present. The history table is a
// DataTables table, rendered by the page's scripts, that keeps only its current
// page in the document. Its body rows are collected page by page through the
// table's own length selector and "next" control, at the largest page size (or
// the smallest, which the probe uses to exercise paging), and the count is
// returned with the table's own total so a short read is detected.
const HISTORY_PAGE_SCRIPT: &str = r#"((mode) => {
  const text = (node) => (node.innerText || node.textContent || '').replace(/\s+/g, ' ').trim();
  const cells = (row) => Array.from(row.cells).flatMap((cell) => {
    const value = text(cell);
    return Array.from({ length: Math.min(Math.max(cell.colSpan || 1, 1), 32) }, (_, index) => index === 0 ? value : '');
  }).slice(0, 33);
  const table = document.getElementById('transactiontable') || document.querySelector('table.dataTable');
  const container = table && (table.closest('.dt-container') || table.closest('.dataTables_wrapper'));
  let paging = null;
  let bodyRows = null;
  if (table && container) {
    const select = container.querySelector('.dt-length select, .dataTables_length select');
    if (select) {
      const size = (option) => { const value = Number(option.value); return value < 0 ? Infinity : value; };
      const options = Array.from(select.options).filter((option) => !Number.isNaN(Number(option.value)));
      const chosen = options.reduce((best, option) =>
        (mode === 'smallest' ? size(option) < size(best) : size(option) > size(best)) ? option : best, options[0]);
      if (chosen && select.value !== chosen.value) {
        select.value = chosen.value;
        select.dispatchEvent(new Event('change', { bubbles: true }));
      }
    }
    const seen = new Set();
    let pages = 0;
    let stalled = false;
    bodyRows = [];
    for (let page = 0; page < 1000; page++) {
      const fresh = Array.from(table.tBodies).flatMap((body) => Array.from(body.rows)).filter((row) => !seen.has(row));
      if (page > 0 && fresh.length === 0) { stalled = true; break; }
      for (const row of fresh) { seen.add(row); bodyRows.push(cells(row)); }
      pages = page + 1;
      const next = container.querySelector('.dt-paging-button.next, .paginate_button.next');
      if (!next || next.disabled || next.classList.contains('disabled') || next.getAttribute('aria-disabled') === 'true') break;
      next.click();
    }
    const info = container.querySelector('.dt-info, .dataTables_info');
    const numbers = info ? (text(info).match(/\d+/g) || []) : [];
    paging = {
      pages,
      stalled,
      collected: bodyRows.length,
      expected: numbers.length ? Number(numbers[numbers.length - 1]) : null,
    };
  }
  const rows = [];
  let inserted = false;
  for (const row of document.querySelectorAll('tr')) {
    if (bodyRows && row.parentElement && row.parentElement.tagName === 'TBODY' && row.closest('table') === table) {
      if (!inserted) { rows.push(...bodyRows); inserted = true; }
      continue;
    }
    rows.push(cells(row));
  }
  if (bodyRows && !inserted) rows.push(...bodyRows);
  const message = document.getElementById('hhMsg');
  return {
    path: location.pathname,
    rows: rows.slice(0, 5001),
    text: document.body ? text(document.body).slice(0, 200001) : '',
    message: message ? text(message).slice(0, 2001) : null,
    historyForm: Boolean(document.querySelector('[name="BOKF_DAT_FOM"]') && document.querySelector('[name="BOKF_DAT_TOM"]')),
    paging,
  };
})"#;

// The page's scripts draw the history table, or the message box for a window
// without rows, after the load event. Wait until one of them is complete; in
// production a read straight after load found neither.
const HISTORY_READY_SCRIPT: &str = r#"(async () => {
  const deadline = Date.now() + 15000;
  const visible = (node) => node && node.getClientRects().length > 0 && getComputedStyle(node).display !== 'none';
  while (Date.now() < deadline) {
    const table = document.getElementById('transactiontable') || document.querySelector('table.dataTable');
    const container = table && (table.closest('.dt-container') || table.closest('.dataTables_wrapper'));
    const info = container && container.querySelector('.dt-info, .dataTables_info');
    const processing = container && container.querySelector('.dt-processing, .dataTables_processing');
    if (info && (info.innerText || '').trim() && !visible(processing)) return 'table';
    const message = document.getElementById('hhMsg');
    if (message && (message.innerText || '').trim()) return 'message';
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  return 'timeout';
})()"#;

#[derive(Deserialize)]
struct AccountOverview {
    accounts: Vec<CorporateAccount>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CorporateAccount {
    account_number_raw: String,
    account_number: String,
    arrangement_name: String,
    // camelCase would expect "arrangementIsoCurrencyCode"; the bank sends "ISO".
    #[serde(rename = "arrangementISOCurrencyCode")]
    arrangement_iso_currency_code: String,
}

fn parse_accounts(body: &str) -> Result<Vec<Account>> {
    let parsed: AccountOverview = serde_json::from_str(body)
        .map_err(|_| PrickaError::Parse("Unexpected company account overview".into()))?;
    if parsed.accounts.is_empty() {
        return Err(PrickaError::Bank(
            "No authorized company accounts were returned".into(),
        ));
    }
    parsed
        .accounts
        .into_iter()
        .map(|account| {
            if account.account_number_raw.is_empty()
                || account.account_number_raw.len() > 128
                || account.account_number.len() > 128
                || account.arrangement_name.len() > 256
                || account.arrangement_iso_currency_code.len() != 3
            {
                return Err(PrickaError::Parse("Invalid company account shape".into()));
            }
            Ok(Account {
                id: account.account_number_raw,
                name: account.arrangement_name,
                account_number: account.account_number,
                balance: None,
                currency: account.arrangement_iso_currency_code,
            })
        })
        .collect()
}

#[derive(Deserialize)]
struct TransactionOverview {
    #[serde(rename = "transactionArrangement")]
    arrangement: TransactionArrangement,
}

#[derive(Deserialize)]
struct TransactionArrangement {
    #[serde(rename = "_links")]
    links: TransactionLinks,
}

#[derive(Deserialize)]
struct TransactionLinks {
    transactions: BankLink,
}

#[derive(Deserialize)]
struct BankLink {
    href: String,
}

/// Fixed failure classes of a history page read. Messages never include page
/// content, because cells hold customer values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HistoryError {
    NavigationFailed,
    ExtractionFailed,
    PageTooLarge,
    TableNotFound,
    InvalidDate,
    InvalidAmount,
    MissingDescription,
    OutsideWindow,
    UnexpectedRow,
    AmbiguousTable,
    UnsupportedLayout,
    UnexpectedResponse,
    RangeUnavailable,
    PagingFailed,
    UnrecognizedMessage,
}

impl HistoryError {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::NavigationFailed => "navigation_failed",
            Self::ExtractionFailed => "extraction_failed",
            Self::PageTooLarge => "page_too_large",
            Self::TableNotFound => "table_not_found",
            Self::InvalidDate => "invalid_date",
            Self::InvalidAmount => "invalid_amount",
            Self::MissingDescription => "missing_description",
            Self::OutsideWindow => "outside_window",
            Self::UnexpectedRow => "unexpected_row",
            Self::AmbiguousTable => "ambiguous_table",
            Self::UnsupportedLayout => "unsupported_layout",
            Self::UnexpectedResponse => "unexpected_response",
            Self::RangeUnavailable => "range_unavailable",
            Self::PagingFailed => "paging_failed",
            Self::UnrecognizedMessage => "unrecognized_message",
        }
    }
}

impl From<HistoryError> for PrickaError {
    fn from(error: HistoryError) -> Self {
        match error {
            HistoryError::NavigationFailed | HistoryError::ExtractionFailed => {
                PrickaError::Browser(format!("Company history page failed ({})", error.code()))
            }
            HistoryError::UnexpectedResponse => PrickaError::Http(format!(
                "Company history page was not returned ({})",
                error.code()
            )),
            HistoryError::RangeUnavailable => PrickaError::Bank(
                "Handelsbanken does not provide company history that far back".into(),
            ),
            _ => PrickaError::Parse(format!(
                "Unexpected company history page ({})",
                error.code()
            )),
        }
    }
}

/// Label words found in one history heading cell. A label can carry several,
/// such as a booked balance or a combined deposit and withdrawal column, so
/// roles are decided from all of them rather than the first match.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HeadingWords {
    pub(crate) booking: bool,
    pub(crate) ledger: bool,
    pub(crate) transaction: bool,
    pub(crate) value: bool,
    pub(crate) date: bool,
    pub(crate) amount: bool,
    pub(crate) credit: bool,
    pub(crate) debit: bool,
    pub(crate) balance: bool,
    pub(crate) text: bool,
}

impl HeadingWords {
    pub(crate) fn of(heading: &str) -> Self {
        let heading = heading.trim().to_lowercase();
        let has = |words: &[&str]| words.iter().any(|word| heading.contains(word));
        Self {
            booking: has(&["bokför", "booking"]),
            ledger: has(&["reskontra", "ledger"]),
            transaction: has(&["transaktion", "transaction"]),
            value: has(&["valuta", "valuter", "value"]),
            date: has(&["datum", "date"])
                || heading
                    .split(|character: char| !character.is_alphanumeric())
                    .any(|word| word.ends_with("dag")),
            amount: has(&["belopp", "amount"]),
            credit: has(&["insättning", "deposit"]),
            debit: has(&["uttag", "withdrawal"]),
            balance: has(&["saldo", "balance"]),
            text: has(&[
                "text",
                "meddelande",
                "referens",
                "beskrivning",
                "mottagare",
                "avsändare",
                "description",
            ]),
        }
    }

    fn money(self) -> bool {
        !self.balance && (self.amount || self.credit || self.debit)
    }

    fn dated(self) -> bool {
        !self.balance
            && !self.money()
            && (self.date || self.booking || self.ledger || self.transaction)
    }
}

/// Column positions of the history table, derived from its heading row.
#[derive(Debug, PartialEq)]
pub(crate) struct HistoryLayout {
    pub(crate) booking: usize,
    pub(crate) transaction: Option<usize>,
    /// Absent when deposits and withdrawals are split into separate columns,
    /// a layout that has not been observed or verified.
    pub(crate) amount: Option<usize>,
    pub(crate) balance: Option<usize>,
    pub(crate) description: Vec<usize>,
    pub(crate) width: usize,
}

impl HistoryLayout {
    pub(crate) fn from_heading(cells: &[String]) -> Option<Self> {
        // A heading holds labels only; this keeps a data row whose text happens
        // to contain a label word from starting a new table.
        if cells
            .iter()
            .any(|cell| parse_iso_date(cell).is_some() || parse_amount(cell).is_some())
        {
            return None;
        }
        let words = cells
            .iter()
            .map(|cell| HeadingWords::of(cell))
            .collect::<Vec<_>>();
        if !words.iter().any(|word| word.money()) {
            return None;
        }
        let find = |test: &dyn Fn(HeadingWords) -> bool| words.iter().position(|word| test(*word));
        // The history link filters on booking date, so prefer that column.
        let booking = find(&|word| word.dated() && word.booking)
            .or_else(|| find(&|word| word.dated() && word.ledger))
            .or_else(|| find(&|word| word.dated() && !word.transaction && !word.value))?;
        Some(Self {
            booking,
            transaction: find(&|word| word.dated() && word.transaction),
            amount: find(&|word| word.money() && (word.amount || (word.credit && word.debit))),
            balance: find(&|word| word.balance),
            description: words
                .iter()
                .enumerate()
                .filter(|(_, word)| word.text && !word.dated() && !word.money() && !word.balance)
                .map(|(index, _)| index)
                .collect(),
            width: cells.len(),
        })
    }
}

pub(crate) fn parse_iso_date(value: &str) -> Option<NaiveDate> {
    let value = value.trim();
    (value.len() == 10)
        .then(|| NaiveDate::parse_from_str(value, "%Y-%m-%d").ok())
        .flatten()
}

pub(crate) fn parse_amount(value: &str) -> Option<Decimal> {
    let normalized = value
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>()
        .replace('\u{2212}', "-")
        .replace(',', ".");
    if normalized.is_empty()
        || !normalized
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'-' | b'+' | b'.'))
    {
        return None;
    }
    normalized.parse().ok()
}

fn has_iso_date(cells: &[String]) -> bool {
    cells.iter().any(|cell| parse_iso_date(cell).is_some())
}

/// Booked transactions of one history page, and how many preliminary rows
/// (no booking date and no running balance yet) were left for a later read.
#[derive(Debug)]
pub(crate) struct ParsedHistory {
    pub(crate) transactions: Vec<Transaction>,
    pub(crate) pending: usize,
}

/// Parses history rows in document order. A recognized heading row starts the
/// history table. Rows without a booking date (opening balances, spacing) and
/// preliminary rows are skipped: the bank lists reserved items above booked
/// ones with text where the booking date and balance would be, and they are
/// read once booked. Every other surprise fails the read instead of importing
/// a partial or misaligned ledger, including a second dated table.
pub(crate) fn parse_history_rows(
    rows: &[Vec<String>],
    range: &DateRange,
) -> std::result::Result<ParsedHistory, HistoryError> {
    if rows.len() > MAX_HISTORY_PAGE_ROWS {
        return Err(HistoryError::PageTooLarge);
    }
    let headings = rows
        .iter()
        .enumerate()
        .filter_map(|(index, cells)| {
            HistoryLayout::from_heading(cells).map(|layout| (index, layout))
        })
        .collect::<Vec<_>>();
    let Some((first, _)) = headings.first() else {
        return Err(HistoryError::TableNotFound);
    };
    let sections = headings
        .iter()
        .enumerate()
        .map(|(position, (start, layout))| {
            let end = headings
                .get(position + 1)
                .map_or(rows.len(), |(next, _)| *next);
            (layout, &rows[start + 1..end])
        })
        // A filled booking cell marks a data row even when its date no longer
        // parses, so a changed format fails below instead of reading as empty.
        .filter(|(layout, body)| {
            body.iter().any(|cells| {
                has_iso_date(cells)
                    || (cells.len() == layout.width && !cells[layout.booking].is_empty())
            })
        })
        .collect::<Vec<_>>();
    if sections.len() > 1 || rows[..*first].iter().any(|cells| has_iso_date(cells)) {
        return Err(HistoryError::AmbiguousTable);
    }
    let mut parsed = ParsedHistory {
        transactions: Vec::new(),
        pending: 0,
    };
    let Some((layout, body)) = sections.into_iter().next() else {
        return Ok(parsed);
    };
    let (Some(amount_column), false) = (layout.amount, layout.description.is_empty()) else {
        return Err(HistoryError::UnsupportedLayout);
    };
    for cells in body {
        if cells.len() != layout.width {
            if has_iso_date(cells) {
                return Err(HistoryError::UnexpectedRow);
            }
            continue;
        }
        let Some(date) = parse_iso_date(&cells[layout.booking]) else {
            if has_iso_date(cells) {
                return Err(HistoryError::InvalidDate);
            }
            let has_amount = !cells[amount_column].is_empty();
            let booked_balance = layout
                .balance
                .is_some_and(|index| parse_amount(&cells[index]).is_some());
            match (has_amount, booked_balance) {
                // Spacing, captions and the opening balance.
                (false, _) => continue,
                // Reserved items carry an amount but no booking date or balance.
                (true, false) => {
                    parsed.pending += 1;
                    continue;
                }
                // Looks booked but has no readable booking date.
                (true, true) => return Err(HistoryError::InvalidDate),
            }
        };
        if date < range.from || date > range.to {
            return Err(HistoryError::OutsideWindow);
        }
        let transaction_date = match layout.transaction {
            Some(index) if !cells[index].is_empty() => {
                Some(parse_iso_date(&cells[index]).ok_or(HistoryError::InvalidDate)?)
            }
            _ => None,
        };
        let amount = parse_amount(&cells[amount_column]).ok_or(HistoryError::InvalidAmount)?;
        let balance = match layout.balance {
            Some(index) if !cells[index].is_empty() => {
                Some(parse_amount(&cells[index]).ok_or(HistoryError::InvalidAmount)?)
            }
            _ => None,
        };
        let description = layout
            .description
            .iter()
            .map(|index| cells[*index].as_str())
            .filter(|cell| !cell.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if description.is_empty() {
            return Err(HistoryError::MissingDescription);
        }
        parsed.transactions.push(Transaction {
            date,
            transaction_date,
            description,
            amount,
            balance,
        });
    }
    Ok(parsed)
}

/// One history window as copied from the page. The text holds customer values
/// and is only matched against fixed notice wording.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HistoryPage {
    path: String,
    pub(crate) rows: Vec<Vec<String>>,
    text: String,
    pub(crate) message: Option<String>,
    pub(crate) history_form: bool,
    pub(crate) paging: Option<HistoryPaging>,
}

/// How the DataTables body was collected.
#[derive(Debug, Clone, Copy, Deserialize)]
pub(crate) struct HistoryPaging {
    pub(crate) stalled: bool,
    pub(crate) collected: f64,
    pub(crate) expected: Option<f64>,
}

impl HistoryPaging {
    fn complete(self) -> bool {
        !self.stalled
            && self
                .expected
                .is_some_and(|expected| expected == self.collected)
    }
}

// ASCII tokens for words of the bank's message box. A failure detail names
// which of these appear, never the message itself, so logs can show wording
// the adapter does not recognize without carrying bank text.
const MESSAGE_TOKENS: &[(&str, &str)] = &[
    ("ej", "ej"),
    ("inga", "inga"),
    ("ingen", "ingen"),
    ("finns", "finns"),
    ("saknas", "saknas"),
    ("hittades", "hittades"),
    ("transaktioner", "transaktioner"),
    ("bokförda", "bokforda"),
    ("reserverade", "reserverade"),
    ("preliminära", "preliminara"),
    ("kontot", "kontot"),
    ("period", "period"),
    ("perioden", "perioden"),
    ("vald", "vald"),
    ("angiven", "angiven"),
    ("datum", "datum"),
    ("dagar", "dagar"),
    ("månader", "manader"),
    ("längre", "langre"),
    ("fel", "fel"),
    ("försök", "forsok"),
    ("sökning", "sokning"),
];
// Words that can appear anywhere on a history page that drew no table:
// errors, sign-out, blocking or maintenance notices, and page chrome.
const PAGE_TOKENS: &[(&str, &str)] = &[
    ("fel", "fel"),
    ("tekniskt", "tekniskt"),
    ("försök", "forsok"),
    ("senare", "senare"),
    ("vänta", "vanta"),
    ("tillfälligt", "tillfalligt"),
    ("underhåll", "underhall"),
    ("logga", "logga"),
    ("inloggad", "inloggad"),
    ("utloggad", "utloggad"),
    ("sessionen", "sessionen"),
    ("spärrad", "sparrad"),
    ("nekad", "nekad"),
    ("behörighet", "behorighet"),
    ("många", "manga"),
    ("kontohändelser", "kontohandelser"),
    ("transaktioner", "transaktioner"),
];
// Room for the detail within the machine protocol's 120-character limit.
const MAX_FAILURE_DETAIL: usize = 120;

fn tokens_in(text: &str, vocabulary: &[(&str, &'static str)]) -> Vec<&'static str> {
    let lower = text.to_lowercase();
    let present = lower
        .split(|character: char| !character.is_alphabetic())
        .collect::<Vec<_>>();
    vocabulary
        .iter()
        .filter(|(word, _)| present.contains(word))
        .map(|(_, token)| *token)
        .collect()
}

/// A history failure with the window's length and age, whether a table and the
/// search form were drawn, and fixed tokens from the message box (or, without
/// one, from the page). Codes and counts only.
fn history_failure(
    error: HistoryError,
    page: &HistoryPage,
    window: &DateRange,
    today: NaiveDate,
) -> PrickaError {
    if !matches!(
        error,
        HistoryError::TableNotFound
            | HistoryError::UnrecognizedMessage
            | HistoryError::PagingFailed
    ) {
        return error.into();
    }
    let (source, mut tokens) = match page.message.as_deref() {
        Some(message) => ("message", tokens_in(message, MESSAGE_TOKENS)),
        None => ("page", tokens_in(&page.text, PAGE_TOKENS)),
    };
    let days = (window.to - window.from).num_days() + 1;
    let ago = (today - window.to).num_days().clamp(0, 999);
    let table = u8::from(page.paging.is_some());
    let form = u8::from(page.history_form);
    loop {
        let words = if tokens.is_empty() {
            "none".to_owned()
        } else {
            tokens.join(" ")
        };
        // Compact: "30d end-6d" is the window length and how many days before
        // today it ends; table/form are 0 or 1.
        let detail = format!(
            "Unexpected company history page ({} {days}d end-{ago}d table{table} form{form} {source}: {words})",
            error.code()
        );
        if detail.len() <= MAX_FAILURE_DETAIL || tokens.is_empty() {
            return PrickaError::Parse(detail);
        }
        tokens.pop();
    }
}

fn has_words(text: &str, required: &[&str]) -> bool {
    let lower = text.to_lowercase();
    let words = lower
        .split(|character: char| !character.is_alphabetic())
        .collect::<Vec<_>>();
    required.iter().all(|word| words.contains(word))
}

impl HistoryPage {
    /// Error phrases present anywhere on the page.
    pub(crate) fn error_notices(&self) -> Vec<&'static str> {
        let text = self.text.to_lowercase();
        ERROR_NOTICES
            .iter()
            .copied()
            .filter(|notice| text.contains(notice))
            .collect()
    }

    /// Whether the bank's message box says the dates are outside its history.
    pub(crate) fn range_limit_message(&self) -> bool {
        self.message
            .as_deref()
            .is_some_and(|message| has_words(message, RANGE_LIMIT_WORDS))
    }

    /// Whether the bank's message box reports a window without transactions.
    pub(crate) fn empty_message(&self) -> bool {
        self.message.as_deref().is_some_and(|message| {
            has_words(message, EMPTY_MESSAGE_WORDS) && !has_words(message, RANGE_LIMIT_WORDS)
        })
    }

    fn empty_window(&self) -> bool {
        self.history_form
            && !self.rows.iter().any(|cells| has_iso_date(cells))
            && self.empty_message()
            && self.error_notices().is_empty()
    }
}

/// Parses a history window. The bank renders no table for a window without
/// booked transactions, so a missing table is accepted only with the bank's
/// empty notice on its history form; anything else stays a failure.
pub(crate) fn parse_history_page(
    page: &HistoryPage,
    expected_path: &str,
    range: &DateRange,
) -> std::result::Result<ParsedHistory, HistoryError> {
    if page.path != expected_path {
        return Err(HistoryError::UnexpectedResponse);
    }
    if page.paging.is_some_and(|paging| !paging.complete()) {
        return Err(HistoryError::PagingFailed);
    }
    if page.range_limit_message() {
        return Err(HistoryError::RangeUnavailable);
    }
    match parse_history_rows(&page.rows, range) {
        Err(HistoryError::TableNotFound) if page.empty_window() => Ok(ParsedHistory {
            transactions: Vec::new(),
            pending: 0,
        }),
        // Separates a message the adapter does not know from a page that drew nothing.
        Err(HistoryError::TableNotFound) if page.message.is_some() => {
            Err(HistoryError::UnrecognizedMessage)
        }
        result => result,
    }
}

/// The bank's own history link with only its booking-date bounds replaced.
pub(crate) fn history_window_url(href: &str, from: NaiveDate, to: NaiveDate) -> Option<String> {
    let mut url = url::Url::parse(href).ok()?;
    let mut replaced = 0;
    let pairs = url
        .query_pairs()
        .map(|(key, value)| {
            let value = match key.as_ref() {
                HISTORY_FROM_PARAMETER => {
                    replaced += 1;
                    from.to_string()
                }
                HISTORY_TO_PARAMETER => {
                    replaced += 1;
                    to.to_string()
                }
                _ => value.into_owned(),
            };
            (key.into_owned(), value)
        })
        .collect::<Vec<_>>();
    if replaced != 2 {
        return None;
    }
    url.query_pairs_mut().clear().extend_pairs(pairs);
    Some(url.into())
}

/// Opens one history window and copies it. Callers parse it with
/// [`parse_history_page`] before returning transactions.
pub(crate) async fn read_history_page(
    page: &Page,
    href: &str,
    from: NaiveDate,
    to: NaiveDate,
) -> std::result::Result<HistoryPage, HistoryError> {
    let url = history_window_url(href, from, to).ok_or(HistoryError::NavigationFailed)?;
    tokio::time::timeout(HISTORY_PAGE_TIMEOUT, page.goto(url))
        .await
        .map_err(|_| HistoryError::NavigationFailed)?
        .map_err(|_| HistoryError::NavigationFailed)?;
    // A timeout here is not an error by itself: the read below decides.
    let _ = tokio::time::timeout(HISTORY_PAGE_TIMEOUT, page.evaluate(HISTORY_READY_SCRIPT)).await;
    let history = tokio::time::timeout(
        HISTORY_PAGE_TIMEOUT,
        page.evaluate(format!("{HISTORY_PAGE_SCRIPT}('largest')")),
    )
    .await
    .map_err(|_| HistoryError::ExtractionFailed)?
    .map_err(|_| HistoryError::ExtractionFailed)?
    .into_value::<HistoryPage>()
    .map_err(|_| HistoryError::ExtractionFailed)?;
    if history.rows.len() > MAX_HISTORY_PAGE_ROWS
        || history
            .rows
            .iter()
            .any(|cells| cells.len() > MAX_HISTORY_COLUMNS)
        || history.text.chars().count() > MAX_HISTORY_PAGE_TEXT
        || history
            .message
            .as_ref()
            .is_some_and(|message| message.chars().count() > MAX_HISTORY_MESSAGE)
    {
        return Err(HistoryError::PageTooLarge);
    }
    Ok(history)
}

/// Path of the history route, to detect a redirect to another page.
pub(crate) fn history_path(href: &str) -> Option<String> {
    url::Url::parse(href).ok().map(|url| url.path().to_owned())
}

fn account_digits(value: &str) -> String {
    value.chars().filter(char::is_ascii_digit).collect()
}

fn validated_history_link(href: &str, account_id: &str) -> Result<String> {
    let url = url::Url::parse(href)
        .or_else(|_| url::Url::parse(ORIGIN).and_then(|origin| origin.join(href)))
        .map_err(|_| PrickaError::Parse("Invalid company history link".into()))?;
    if url.scheme() != "https"
        || url.host_str() != Some("secure.handelsbanken.se")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(PrickaError::Parse(
            "Company history link left bank origin".into(),
        ));
    }
    let linked_account = url
        .query_pairs()
        .find(|(key, _)| key == HISTORY_ACCOUNT_PARAMETER)
        .map(|(_, value)| account_digits(&value))
        .ok_or_else(|| PrickaError::Parse("Company history link has no account".into()))?;
    // The link formats the account differently from the overview; the trial
    // found the same digits in both.
    if linked_account.is_empty() || linked_account != account_digits(account_id) {
        return Err(PrickaError::NotImplemented(
            "Only the company's default account history is supported".into(),
        ));
    }
    let has = |name: &str| url.query_pairs().any(|(key, _)| key == name);
    if !has(HISTORY_FROM_PARAMETER) || !has(HISTORY_TO_PARAMETER) {
        return Err(PrickaError::Parse(
            "Company history link has no date range".into(),
        ));
    }
    Ok(url.into())
}

/// Reads made through the JSON account routes. The bank answers those with
/// 403 once the session has loaded a legacy history page (navigated or
/// fetched), and on 2026-10-02 the history link read also answered 403 when it
/// waited for the founder's account choice after login. The trial verified the
/// overview, transaction settings and history link read together right after
/// login, so all three happen then and are reused for the session.
#[derive(Default)]
struct SessionReads {
    accounts: Option<Vec<Account>>,
    history_href: Option<String>,
    history: Option<(String, String)>,
}

pub struct HandelsbankenCorporate {
    proxy: ProxyConfig,
    headless: bool,
    session: Option<BrowserSession>,
    request_headers: Vec<(&'static str, String)>,
    reads: Mutex<SessionReads>,
}

impl HandelsbankenCorporate {
    pub fn new(proxy: ProxyConfig, headless: bool) -> Self {
        Self {
            proxy,
            headless,
            session: None,
            request_headers: Vec::new(),
            reads: Mutex::new(SessionReads::default()),
        }
    }

    fn page(&self) -> Result<&Page> {
        self.session
            .as_ref()
            .map(|session| &session.page)
            .ok_or(PrickaError::SessionExpired)
    }

    fn reads(&self) -> std::sync::MutexGuard<'_, SessionReads> {
        self.reads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn reset(&mut self) {
        self.request_headers.clear();
        *self
            .reads
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = SessionReads::default();
    }

    fn headers(&self) -> Vec<(&str, &str)> {
        let mut headers = vec![
            ("accept", "application/json, application/problem+json"),
            ("accept-language", "sv"),
            ("x-requested-with", "fetch"),
            // Matches the requests the trial verified.
            ("content-type", "application/json;charset=UTF-8"),
        ];
        headers.extend(
            self.request_headers
                .iter()
                .map(|(name, value)| (*name, value.as_str())),
        );
        headers
    }

    /// The account overview and the default account's history link, read
    /// back to back in the order the bank's own transaction module uses.
    async fn read_session(&self) -> Result<(Vec<Account>, String)> {
        let page = self.page()?;
        let headers = self.headers();
        let response = browser_get(page, OVERVIEW_URL, &headers).await?;
        if response.status != 200 {
            return Err(PrickaError::Http(format!(
                "Company account overview returned {}",
                response.status
            )));
        }
        let accounts = parse_accounts(&response.body)?;
        // Only the request matters: the module reads its settings before its list.
        let _ = browser_get(page, TRANSACTION_SETTINGS_URL, &headers).await;
        let response = browser_get(page, TRANSACTION_URL, &headers).await?;
        if response.status != 200 {
            return Err(PrickaError::Http(format!(
                "Company transactions returned {}",
                response.status
            )));
        }
        let overview: TransactionOverview = serde_json::from_str(&response.body)
            .map_err(|_| PrickaError::Parse("Unexpected company transactions response".into()))?;
        Ok((accounts, overview.arrangement.links.transactions.href))
    }

    async fn history_link(&self, account_id: &str) -> Result<String> {
        if let Some((account, link)) = &self.reads().history {
            return if account == account_id {
                Ok(link.clone())
            } else {
                Err(PrickaError::NotImplemented(
                    "Only the company's default account history is supported".into(),
                ))
            };
        }
        // Confirm that the requested account is visible in this session.
        if !self
            .list_accounts()
            .await?
            .iter()
            .any(|account| account.id == account_id)
        {
            return Err(PrickaError::Bank(
                "Selected company account is unavailable".into(),
            ));
        }
        let href = self
            .reads()
            .history_href
            .clone()
            .ok_or(PrickaError::SessionExpired)?;
        let link = validated_history_link(&href, account_id)?;
        self.reads().history = Some((account_id.to_owned(), link.clone()));
        Ok(link)
    }
}

// async-trait marks generated futures as must_use; suppress the duplicate Clippy lint.
#[allow(clippy::double_must_use)]
#[async_trait]
impl Bank for HandelsbankenCorporate {
    fn name(&self) -> &str {
        "Handelsbanken Företag"
    }

    async fn authenticate(
        &mut self,
        personnummer: &str,
        presenter: &dyn AuthPresenter,
    ) -> Result<()> {
        if let Some(old) = self.session.take() {
            old.close().await;
        }
        self.reset();
        let session = BrowserSession::launch(&self.proxy, self.headless).await?;
        match corporate_login::open_application(&session.page, personnummer, presenter).await {
            Ok(headers) => {
                self.request_headers = headers;
                self.session = Some(session);
                Ok(())
            }
            Err(error) => {
                session.close().await;
                Err(error)
            }
        }
    }

    async fn list_accounts(&self) -> Result<Vec<Account>> {
        if let Some(accounts) = &self.reads().accounts {
            return Ok(accounts.clone());
        }
        let (accounts, href) = self.read_session().await?;
        let mut reads = self.reads();
        reads.accounts = Some(accounts.clone());
        reads.history_href = Some(href);
        Ok(accounts)
    }

    async fn get_transactions(
        &self,
        account_id: &str,
        range: &DateRange,
    ) -> Result<Vec<Transaction>> {
        if range.from > range.to || account_id.is_empty() || account_id.len() > 128 {
            return Err(PrickaError::Parse("Invalid company history request".into()));
        }
        let link = self.history_link(account_id).await?;
        let path = history_path(&link)
            .ok_or_else(|| PrickaError::Parse("Invalid company history link".into()))?;
        let page = self.page()?;
        let today = chrono::Local::now().date_naive();
        let mut transactions = Vec::new();
        let mut from = range.from;
        while from <= range.to {
            if from > range.from {
                tokio::time::sleep(HISTORY_WINDOW_PAUSE).await;
            }
            let to = (from + chrono::Duration::days(HISTORY_WINDOW_DAYS - 1)).min(range.to);
            let window = DateRange { from, to };
            let history = read_history_page(page, &link, from, to).await?;
            let parsed = parse_history_page(&history, &path, &window)
                .map_err(|error| history_failure(error, &history, &window, today))?;
            transactions.extend(parsed.transactions);
            from = to + chrono::Duration::days(1);
        }
        Ok(transactions)
    }

    async fn get_balance(&self, _account_id: &str) -> Result<Balance> {
        Err(PrickaError::NotImplemented(
            "Company balance is not available separately".into(),
        ))
    }

    async fn logout(&mut self) -> Result<()> {
        self.reset();
        if let Some(session) = self.session.take() {
            session.close().await;
        }
        Ok(())
    }
}

pub fn create(proxy: ProxyConfig, headless: bool) -> Box<dyn Bank> {
    Box::new(HandelsbankenCorporate::new(proxy, headless))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(cells: &[&str]) -> Vec<String> {
        cells.iter().map(|cell| (*cell).to_owned()).collect()
    }

    fn range() -> DateRange {
        DateRange {
            from: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
            to: NaiveDate::from_ymd_opt(2026, 9, 30).unwrap(),
        }
    }

    // Synthetic labels with the word pattern the live trial reported: two
    // booking-type dates, a value date, a combined deposit and withdrawal
    // column, and a booked balance.
    const HEADING: &[&str] = &[
        "Nr",
        "Reskontradag",
        "Bokföringsdag",
        "Valutadag",
        "Text",
        "Insättning/Uttag",
        "Bokfört saldo",
    ];

    fn parse(rows: &[Vec<String>]) -> std::result::Result<ParsedHistory, HistoryError> {
        parse_history_rows(rows, &range())
    }

    #[test]
    fn heading_words_assign_combined_labels_to_one_role_each() {
        let layout = HistoryLayout::from_heading(&row(HEADING)).unwrap();
        assert_eq!(
            layout,
            HistoryLayout {
                booking: 2,
                transaction: None,
                amount: Some(5),
                balance: Some(6),
                description: vec![4],
                width: 7,
            }
        );
        let with_transaction_date = HistoryLayout::from_heading(&row(&[
            "Reskontradatum",
            "Transaktionsdatum",
            "Referens",
            "Belopp",
            "Saldo",
        ]))
        .unwrap();
        assert_eq!(with_transaction_date.booking, 0);
        assert_eq!(with_transaction_date.transaction, Some(1));
        assert_eq!(with_transaction_date.amount, Some(3));
        assert!(HistoryLayout::from_heading(&row(&["Saldo", "Bokfört"])).is_none());
    }

    #[test]
    fn history_rows_skip_preliminary_and_undated_rows() {
        let rows = vec![
            row(&["Synthetic company", "Konto"]),
            row(HEADING),
            row(&[
                "1",
                "Prel",
                "",
                "",
                "Synthetic card purchase",
                "\u{2212}50,00",
                "Prel",
            ]),
            row(&["", "", "", "", "Ingående saldo", "", "6 234,50"]),
            row(&[
                "2",
                "2026-09-30",
                "2026-09-30",
                "2026-10-01",
                "Synthetic payment",
                "\u{2212}1 234,50",
                "5 000,00",
            ]),
        ];
        let parsed = parse(&rows).unwrap();
        assert_eq!(parsed.pending, 1);
        let transactions = parsed.transactions;
        assert_eq!(transactions.len(), 1);
        assert_eq!(transactions[0].date.to_string(), "2026-09-30");
        assert_eq!(transactions[0].transaction_date, None);
        assert_eq!(transactions[0].description, "Synthetic payment");
        assert_eq!(transactions[0].amount.to_string(), "-1234.50");
        assert_eq!(
            transactions[0].balance.map(|balance| balance.to_string()),
            Some("5000.00".into())
        );
    }

    #[test]
    fn history_rows_fail_instead_of_importing_misaligned_values() {
        let dated = |amount: &str, text: &str, date: &str| {
            vec![
                row(HEADING),
                row(&["1", date, date, date, text, amount, "1,00"]),
            ]
        };
        let error = |rows: &[Vec<String>]| parse(rows).err();
        assert_eq!(
            error(&dated("12 kr", "Synthetic", "2026-09-30")),
            Some(HistoryError::InvalidAmount)
        );
        assert_eq!(
            error(&dated("1,00", "", "2026-09-30")),
            Some(HistoryError::MissingDescription)
        );
        assert_eq!(
            error(&dated("1,00", "Synthetic", "2026-10-01")),
            Some(HistoryError::OutsideWindow)
        );
        // A changed date format with a booked balance is not a preliminary row.
        assert_eq!(
            error(&dated("1,00", "Synthetic", "30.09.2026")),
            Some(HistoryError::InvalidDate)
        );
        assert_eq!(
            error(&[row(&["2026-09-30", "Synthetic", "1,00"])]),
            Some(HistoryError::TableNotFound)
        );
        let mut short = dated("1,00", "Synthetic", "2026-09-30");
        short[1].pop();
        assert_eq!(error(&short), Some(HistoryError::UnexpectedRow));
        let mut twice = dated("1,00", "Synthetic", "2026-09-30");
        twice.extend(dated("1,00", "Synthetic", "2026-09-30"));
        assert_eq!(error(&twice), Some(HistoryError::AmbiguousTable));
        let split = vec![
            row(&["Bokföringsdag", "Text", "Insättning", "Uttag", "Saldo"]),
            row(&["2026-09-30", "Synthetic", "1,00", "", "1,00"]),
        ];
        assert_eq!(error(&split), Some(HistoryError::UnsupportedLayout));
        // A data row mentioning a label word must not be read as a heading.
        let labelled = vec![
            row(HEADING),
            row(&[
                "1",
                "2026-09-30",
                "2026-09-30",
                "",
                "Belopp fredag",
                "1,00",
                "2,00",
            ]),
        ];
        assert_eq!(
            parse(&labelled).map(|parsed| parsed.transactions.len()),
            Ok(1)
        );
        assert_eq!(
            parse(&[row(HEADING)]).map(|parsed| parsed.transactions.len()),
            Ok(0)
        );
    }

    const HISTORY_PATH: &str = "/bb/synthetic/history";

    fn history_page(message: Option<&str>, text: &str, rows: Vec<Vec<String>>) -> HistoryPage {
        HistoryPage {
            path: HISTORY_PATH.into(),
            rows,
            text: text.into(),
            message: message.map(str::to_owned),
            history_form: true,
            paging: None,
        }
    }

    fn read(page: &HistoryPage) -> std::result::Result<usize, HistoryError> {
        parse_history_page(page, HISTORY_PATH, &range()).map(|parsed| parsed.transactions.len())
    }

    #[test]
    fn missing_table_is_empty_only_with_the_bank_message_on_its_history_form() {
        // Synthetic wording with the observed word signatures.
        let message = "Det finns ej några transaktioner på kontot för vald period";
        let limit = "Datum får ej ligga längre tillbaka än 24 månader";
        assert_eq!(
            read(&history_page(Some(message), "Synthetic AB", Vec::new())),
            Ok(0)
        );
        assert_eq!(
            read(&history_page(Some(limit), "Synthetic AB", Vec::new())),
            Err(HistoryError::RangeUnavailable)
        );
        let no_form = HistoryPage {
            history_form: false,
            ..history_page(Some(message), "Synthetic AB", Vec::new())
        };
        assert_eq!(read(&no_form), Err(HistoryError::UnrecognizedMessage));
        assert_eq!(
            read(&history_page(None, "Synthetic AB", Vec::new())),
            Err(HistoryError::TableNotFound)
        );
        for unclear in ["Synthetic notice", "Kontot är ej aktivt"] {
            assert_eq!(
                read(&history_page(Some(unclear), "Synthetic AB", Vec::new())),
                Err(HistoryError::UnrecognizedMessage)
            );
        }
        assert_eq!(
            read(&history_page(
                Some(message),
                "Tekniskt fel, försök igen",
                Vec::new()
            )),
            Err(HistoryError::UnrecognizedMessage)
        );
        let dated = vec![row(&["2026-09-30", "Synthetic", "1,00"])];
        assert_eq!(
            read(&history_page(Some(message), "Synthetic AB", dated)),
            Err(HistoryError::UnrecognizedMessage)
        );
        let redirected = HistoryPage {
            path: "/logon/se/corp/sv/".into(),
            ..history_page(Some(message), "Synthetic AB", Vec::new())
        };
        assert_eq!(read(&redirected), Err(HistoryError::UnexpectedResponse));
    }

    #[test]
    fn history_failures_name_window_and_message_tokens_only() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 2).unwrap();
        let page = history_page(
            Some("Det finns ej några bokförda transaktioner för Synthetic AB 1234567890"),
            "Synthetic AB",
            Vec::new(),
        );
        let PrickaError::Parse(detail) =
            history_failure(HistoryError::UnrecognizedMessage, &page, &range(), today)
        else {
            panic!("expected a parse failure");
        };
        assert_eq!(
            detail,
            "Unexpected company history page (unrecognized_message 92d end-2d table0 form1 message: ej finns transaktioner bokforda)"
        );
        assert!(detail.len() <= MAX_FAILURE_DETAIL);
        assert!(
            detail.chars().all(
                |character| character.is_ascii_alphanumeric() || " _:().,-".contains(character)
            )
        );
        // Without a message box the page's own fixed words are named instead.
        let blank = history_page(
            None,
            "Synthetic AB Tekniskt fel. Försök igen senare.",
            Vec::new(),
        );
        let PrickaError::Parse(blank) =
            history_failure(HistoryError::TableNotFound, &blank, &range(), today)
        else {
            panic!("expected a parse failure");
        };
        assert_eq!(
            blank,
            "Unexpected company history page (table_not_found 92d end-2d table0 form1 page: fel tekniskt forsok senare)"
        );
        assert!(matches!(
            history_failure(HistoryError::RangeUnavailable, &page, &range(), today),
            PrickaError::Bank(_)
        ));
    }

    #[test]
    fn paged_tables_must_report_every_row_they_count() {
        let rows = vec![
            row(HEADING),
            row(&[
                "1",
                "2026-09-30",
                "2026-09-30",
                "2026-09-30",
                "Synthetic",
                "1,00",
                "2,00",
            ]),
            row(&[
                "2",
                "2026-09-29",
                "2026-09-29",
                "2026-09-29",
                "Synthetic",
                "1,00",
                "1,00",
            ]),
        ];
        let paged = |collected: f64, expected: Option<f64>, stalled: bool| HistoryPage {
            paging: Some(HistoryPaging {
                stalled,
                collected,
                expected,
            }),
            ..history_page(None, "Synthetic AB", rows.clone())
        };
        assert_eq!(read(&paged(2.0, Some(2.0), false)), Ok(2));
        for (collected, expected, stalled) in [
            (2.0, Some(3.0), false),
            (2.0, None, false),
            (2.0, Some(2.0), true),
        ] {
            assert_eq!(
                read(&paged(collected, expected, stalled)),
                Err(HistoryError::PagingFailed)
            );
        }
    }

    #[test]
    fn account_overview_uses_the_field_names_the_bank_sends() {
        // Field set reported by the 2026-10-02 trial; values are synthetic.
        let body = serde_json::json!({ "accounts": [{
            "accountNumber": "1234 567 890",
            "accountNumberRaw": "1234567890",
            "arrangementName": "Synthetic company account",
            "arrangementISOCurrencyCode": "SEK",
            "arrangementISOCountryCode": "SE",
            "involvedPartyFullName": "Synthetic AB",
        }]})
        .to_string();
        let accounts = parse_accounts(&body).unwrap();
        assert_eq!(accounts[0].id, "1234567890");
        assert_eq!(accounts[0].currency, "SEK");
        assert!(matches!(
            parse_accounts(
                &body.replace("arrangementISOCurrencyCode", "arrangementIsoCurrencyCode")
            ),
            Err(PrickaError::Parse(_))
        ));
        assert!(matches!(
            parse_accounts(r#"{"accounts":[]}"#),
            Err(PrickaError::Bank(_))
        ));
    }

    #[test]
    fn history_link_must_stay_on_bank_and_name_the_selected_account() {
        let href = "/bb/seok/servlet/x?appName=a&KONTO=12%2034-56&BOKF_DAT_FOM=2026-09-01&BOKF_DAT_TOM=2026-09-30";
        let link = validated_history_link(href, "123456").unwrap();
        assert!(link.starts_with("https://secure.handelsbanken.se/bb/"));
        assert!(matches!(
            validated_history_link(href, "999999"),
            Err(PrickaError::NotImplemented(_))
        ));
        assert!(
            validated_history_link(
                "https://example.test/x?KONTO=123456&BOKF_DAT_FOM=a&BOKF_DAT_TOM=b",
                "123456"
            )
            .is_err()
        );
        assert!(validated_history_link("/bb/x?KONTO=123456", "123456").is_err());

        let window = history_window_url(
            &link,
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(),
        )
        .unwrap();
        let pairs = url::Url::parse(&window)
            .unwrap()
            .query_pairs()
            .into_owned()
            .collect::<Vec<_>>();
        assert!(pairs.contains(&("KONTO".into(), "12 34-56".into())));
        assert!(pairs.contains(&("BOKF_DAT_FOM".into(), "2026-01-01".into())));
        assert!(pairs.contains(&("BOKF_DAT_TOM".into(), "2026-03-31".into())));
    }
}
