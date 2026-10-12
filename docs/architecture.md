# Architecture

Pricka is a Rust workspace with a shared bank interface and a CLI. The CLI selects an adapter by `--bank`, authenticates through BankID, lists accounts, fetches transactions for a date range, and writes JSON to stdout or a file.

- `pricka-core` defines `Bank`, `Account`, `Transaction`, `Balance`, and shared errors.
- `pricka-handelsbanken` implements Handelsbanken personal banking.
- `pricka-seb` implements SEB business banking.
- `pricka-bankid` contains a QR helper for flows that require local QR generation; current adapters use bank-provided QR data.
- `pricka-cli` handles options, QR display, adapter selection, and JSON output.
- `capture/` is an optional local development tool, not part of the CLI runtime.

The current adapters use Chrome via the DevTools Protocol so requests run in the authenticated browser context. Bank APIs and authentication flows vary, so each adapter owns its login, account, and transaction code. Shared result types make the exported JSON consistent.

An export, a capture, or a verbose browser log can disclose personal and financial data. Keep those files outside the repository and use only synthetic data in tests and documentation.
