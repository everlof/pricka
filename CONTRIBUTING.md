# Contributing

Thanks for helping Pricka support more banks and login contexts. You can start with a bug report, a bank support request, documentation, or an adapter.

## Before you start

- Check the supported contexts in the README and existing issues. Personal and business banking often use different login and account APIs; name the context explicitly.
- For a new adapter, open a bank support request so the intended scope is clear. You can work without access to every account type; state which flows you have tested.
- Never post a real export, account number, personal identity number, BankID QR code, cookie, token, capture, or banking screenshot. Use fabricated values in examples and tests.

## Implement an adapter

Follow [Adding a bank](docs/adding-a-bank.md). Keep authentication, account parsing, and transaction parsing in focused modules. Return an error when a response is malformed; silently dropping rows can make an export look complete when it is not. Keep tokens and response bodies out of logs.

Add tests for parsing and mapping with synthetic data. Browser login depends on an external bank and BankID, so it is tested manually by a contributor with their own account. A PR can be useful before that manual test is complete if it clearly says what remains unverified.

## Check the change

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

In the pull request, include the bank and login context, the behavior covered, the commands you ran, and any untested paths. Confirm that every example and attachment is synthetic. Do not paste debug logs from a real session.

The CLI and library crates are currently developed together in this workspace; there is no dynamic plugin loader or published plugin API yet.
