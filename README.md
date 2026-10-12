# Pricka

Pricka is a Rust command-line tool that exports account and transaction data from Swedish banks as JSON. It opens Chrome or Chromium for BankID authentication and makes requests in the authenticated browser session.

The project is early and bank websites change. It currently has adapters for **Handelsbanken Privat**, **Handelsbanken Företag**, and **SEB Företag**. Support for other banks and login contexts is welcome.

| CLI value | Login context | Status |
| --- | --- | --- |
| `handelsbanken-privat` | Handelsbanken personal banking | Implemented |
| `handelsbanken-foretag` | Handelsbanken business banking | Implemented for the default account; see limits below |
| `seb-foretag` | SEB business banking | Implemented |

## Try it

You need a Rust toolchain, Chrome or Chromium, and access to the bank account you want to export. Authentication happens through BankID. Run from the repository root:

```bash
cargo run -p pricka-cli -- --bank handelsbanken-privat --personnummer YYYYMMDDXXXX --from 2026-01-01 --to 2026-01-31 -o /path/outside/repo/transactions.json
```

Use `--bank handelsbanken-foretag` for Handelsbanken business banking or `--bank seb-foretag` for SEB business banking. Run `cargo run -p pricka-cli -- --help` for the full option list. The output is JSON; [this example](examples/sample-output.json) is entirely synthetic.

Handelsbanken Företag currently supports the default company account. Company selection through `getMandates` and exporting more than one company account in one session are not yet supported. The adapter fails rather than returning a partial export when those flows are needed.

**The export contains personal and financial data.** Save it outside this repository. Do not attach real exports, banking screenshots, session captures, identity numbers, or credentials to issues or pull requests.

## Add a bank

A bank adapter is a Rust crate implementing the [`Bank` trait](crates/pricka-core/src/bank.rs). The CLI currently selects adapters at compile time. Start with [Adding a bank](docs/adding-a-bank.md) for the exact files to change, a small implementation checklist, and privacy guidance. [CONTRIBUTING.md](CONTRIBUTING.md) explains how to propose and test a change.

If you are considering a bank that is not listed above, [open a bank support request](https://github.com/everlof/pricka/issues/new?template=bank-support.yml) with the bank and login context. Please describe behavior without sharing real account data.

## Development

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

The `capture/` directory contains an optional local network capture utility for adapter development. Its output includes authentication and financial data and must remain private. The default test suite does not require a bank login.

[Architecture](docs/architecture.md) · [Contributing](CONTRIBUTING.md)

## License

Pricka is available under the [MIT License](LICENSE).
