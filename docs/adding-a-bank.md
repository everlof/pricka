# Adding a bank

Each bank and login context needs its own adapter. For example, personal and business banking at the same bank may have different authentication and account APIs.

## 1. Decide the scope

Name the CLI value `bank-context`, such as `example-privat`. Note which login, account listing, transaction date range, and logout flows the adapter will support. Open a [bank support request](https://github.com/everlof/pricka/issues/new?template=bank-support.yml) to coordinate work. Keep all account details out of the issue.

## 2. Create the crate

Add `crates/pricka-<bank>-<context>/` and register it in the root `Cargo.toml` workspace. Use [Handelsbanken Privat](../crates/pricka-handelsbanken/src/lib.rs) or [SEB Företag](../crates/pricka-seb/src/lib.rs) as a guide. Implement [`Bank`](../crates/pricka-core/src/bank.rs):

- `authenticate`: open the browser session, present BankID status through `AuthPresenter`, and retain only the session state needed for later calls.
- `list_accounts`: map bank records to `Account`, including a stable account ID, display name, account number, balance when available, and currency.
- `get_transactions`: honor the requested `DateRange` and map every returned row to `Transaction`. Report malformed or missing rows as errors.
- `get_balance`: return `Balance` if supported; otherwise return a clear `NotImplemented` error.
- `logout`: close the browser session and clear in-memory state.

Expose a `create(proxy: ProxyConfig, headless: bool) -> Box<dyn Bank>` function. Add a branch for the new CLI value in `crates/pricka-cli/src/main.rs` and update its help text and list of available banks. Existing adapters use Chrome through `chromiumoxide`; an adapter can use the transport its bank requires while preserving the `Bank` contract.

## 3. Test without publishing financial data

Write parser and mapping tests with invented account numbers, names, dates, and amounts. Test empty results, malformed responses, multiple pages or date windows, and sign handling where relevant. Use [the synthetic export](../examples/sample-output.json) as a format reference.

If you need to inspect a live flow, `capture/` can record browser traffic locally. Captures may contain cookies, authentication context, identity numbers, and transactions. Keep them outside Git and never attach them to issues or pull requests. Reduce findings to endpoint shapes and synthetic fixtures before sharing code.

Run the checks in [CONTRIBUTING.md](../CONTRIBUTING.md). If you manually test with your own account, describe which flow worked without posting the account data or session logs.
