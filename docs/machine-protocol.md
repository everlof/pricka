# Machine protocol

Start Pricka as a subprocess with stdin, stdout and stderr pipes:

```sh
pricka --machine --bank seb-foretag --from 2026-01-01 --to 2026-01-31
```

Write one JSON line to stdin and flush it:

```json
{"personnummer":null,"accountId":null}
```

Both Handelsbanken contexts require a 12-digit `personnummer`. SEB does not.
Prefer stdin to command-line arguments for private input. Each input line,
including its newline, is limited to 4096 bytes. `--output` cannot be combined
with `--machine`.

After authentication, Pricka emits the available accounts. If `accountId` was
null or omitted, write and flush a second JSON line selecting one of their IDs:

```json
{"accountId":"synthetic-account"}
```

If the first line already supplied an ID, no second line is needed. Pricka
still emits the account list and checks that the requested ID was returned by
the bank. Close stdin only after supplying all required input.

## Events

Each stdout line is one JSON object with a `type` field. BankID QR and status
events can repeat during authentication; applications should replace the
displayed QR code with the latest one.

| Type | Fields | Meaning |
| --- | --- | --- |
| `qr` | `size`, `bits` | Square QR matrix; `bits` contains `size * size` row-major `0`/`1` characters, with `1` dark. Add a quiet zone when rendering. |
| `status` | `message` | Authentication status for display. |
| `accounts` | `accounts` | Available accounts using Pricka's account JSON schema. |
| `account` | `account` | The selected account. |
| `transaction` | `transaction` | One transaction using Pricka's transaction JSON schema. Decimal values are strings. |
| `complete` | `count`, `scrapedAt` | Successful export and logout; count is the number of transaction events, timestamp is UTC. |
| `error` | `code` | Runtime failure, currently `sync_failed`, followed by a nonzero process exit. Private error details are omitted. |

Transaction retrieval uses consecutive, non-overlapping date windows of at
most 30 days. Bank-specific restrictions still apply, including Handelsbanken
Företag's default-account limitation and SEB's 500-row limit per request.

## Import completion

Stage transaction events until **both** `complete` and a zero process exit
have been received. Check that its count matches the received transactions.
Discard the staged import on an error event, nonzero exit, missing completion,
invalid event or interrupted pipe. A later date window can fail after earlier
transaction events have been emitted; those events alone do not prove a
complete export. Keep reading stdout while waiting for process exit.

Machine mode disables tracing, including `RUST_LOG` overrides. Argument parsing
errors and help output follow normal CLI behavior; callers must still check
the exit status. All account, transaction and QR events contain sensitive
data. Keep them out of logs and Git. These examples are synthetic.

## Browser configuration

`PRICKA_CHROME_BIN` selects a Chrome/Chromium executable. The browser sandbox
remains enabled by default. `PRICKA_CHROME_NO_SANDBOX=1` explicitly disables it
for deployments where it cannot start; use this only in an appropriately
isolated, unprivileged environment.

The consuming application owns its ledger mapping, staging and import logic.
Pricka owns bank authentication, account selection, transaction validation and
the event stream. No application-specific configuration or credentials are
part of this protocol.
