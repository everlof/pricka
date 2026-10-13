use std::collections::HashMap;

use chrono::NaiveDate;
use clap::Parser;
use pricka_core::{AuthPresenter, Bank, DateRange, ProxyConfig, ScrapeResult};

mod machine;

#[derive(Parser)]
#[command(name = "pricka", about = "Scrape Swedish bank transactions via BankID")]
struct Cli {
    /// Bank to connect to (handelsbanken-privat, handelsbanken-foretag, seb-foretag)
    #[arg(short, long)]
    bank: String,

    /// Swedish personal identity number (YYYYMMDDXXXX)
    #[arg(short, long)]
    personnummer: Option<String>,

    /// Emit JSON line events for applications; read private input from stdin
    #[arg(long)]
    machine: bool,

    /// SOCKS5 or HTTP proxy URL
    #[arg(long)]
    proxy: Option<String>,

    /// Use Tor (socks5://127.0.0.1:9050)
    #[arg(long)]
    tor: bool,

    /// Start date for transactions (YYYY-MM-DD)
    #[arg(long)]
    from: Option<String>,

    /// End date for transactions (YYYY-MM-DD)
    #[arg(long)]
    to: Option<String>,

    /// Output file (default: stdout)
    #[arg(short, long)]
    output: Option<String>,

    /// Show browser window (default: headless)
    #[arg(long)]
    headed: bool,

    /// Verbose logging (-v = debug, -vv = trace)
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
}

/// Terminal-based QR presenter that prints QR codes to stdout.
struct TerminalPresenter;

impl AuthPresenter for TerminalPresenter {
    fn present_qr(&self, qr_data: &str) {
        // Clear screen
        eprint!("\x1B[2J\x1B[H");
        eprintln!("=== BANKID QR-KOD ===");
        eprintln!("Scanna med BankID-appen:\n");
        print_qr_high_contrast(qr_data);
        eprintln!();
    }

    fn present_status(&self, status: &str) {
        eprintln!("[bankid] {status}");
    }
}

/// Render a QR code to stderr with forced black-on-white ANSI colors.
/// Uses Unicode half-block characters to pack 2 rows per terminal line.
fn print_qr_high_contrast(data: &str) {
    use qrcode::QrCode;

    let code = match QrCode::new(data) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("QR generation failed: {e}");
            return;
        }
    };

    let matrix = code.to_colors();
    let width = code.width();
    let rows: Vec<&[qrcode::Color]> = matrix.chunks(width).collect();

    // Add 1-module quiet zone around the QR
    let total_rows = rows.len() + 2;
    let total_cols = width + 2;

    let is_dark = |r: usize, c: usize| -> bool {
        if r == 0 || r > rows.len() || c == 0 || c > width {
            return false;
        }
        rows[r - 1][c - 1] == qrcode::Color::Dark
    };

    // Use 24-bit true color: pure white (#ffffff) and pure black (#000000)
    // This avoids theme-dependent palette colors (107/40) that some terminals remap.
    let mut r = 0;
    while r < total_rows {
        let mut line = String::new();
        for c in 0..total_cols {
            let upper = is_dark(r, c);
            let lower = if r + 1 < total_rows {
                is_dark(r + 1, c)
            } else {
                false
            };

            match (upper, lower) {
                // Both light → white bg, space
                (false, false) => line.push_str("\x1B[48;2;255;255;255m \x1B[0m"),
                // Upper dark, lower light → black fg, white bg, ▀
                (true, false) => line.push_str("\x1B[38;2;0;0;0;48;2;255;255;255m\u{2580}\x1B[0m"),
                // Upper light, lower dark → black fg, white bg, ▄
                (false, true) => line.push_str("\x1B[38;2;0;0;0;48;2;255;255;255m\u{2584}\x1B[0m"),
                // Both dark → black bg, space
                (true, true) => line.push_str("\x1B[48;2;0;0;0m \x1B[0m"),
            }
        }
        eprintln!("{line}");
        r += 2;
    }
}

fn parse_date(s: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|e| format!("Invalid date '{s}': {e}"))
}

fn create_bank(name: &str, proxy: ProxyConfig, headless: bool) -> Result<Box<dyn Bank>, String> {
    match name {
        "handelsbanken-privat" => Ok(pricka_handelsbanken::create(proxy, headless)),
        "handelsbanken-foretag" => Ok(pricka_handelsbanken::corporate::create(proxy, headless)),
        "seb-foretag" => Ok(pricka_seb::create(proxy, headless)),
        _ => Err(format!(
            "Unknown bank: {name}. Available: handelsbanken-privat, handelsbanken-foretag, seb-foretag"
        )),
    }
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(error) = run(&cli).await {
        if cli.machine {
            let _ = machine::emit(&serde_json::json!({ "type": "error", "code": "sync_failed" }));
        } else {
            eprintln!("Error: {error}");
        }
        std::process::exit(1);
    }
}

async fn run(cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    if cli.machine && cli.output.is_some() {
        return Err("--output cannot be combined with --machine".into());
    }
    let machine_start: Option<machine::MachineStart> = cli
        .machine
        .then(|| machine::read_input(&mut std::io::stdin().lock()))
        .transpose()?;
    let personnummer = machine_start
        .as_ref()
        .and_then(|input| input.personnummer.as_deref())
        .or(cli.personnummer.as_deref())
        .unwrap_or("");
    if cli.bank.starts_with("handelsbanken-")
        && (personnummer.len() != 12 || !personnummer.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err("Handelsbanken requires a 12-digit personnummer".into());
    }
    let range = date_range(cli)?;

    // Log levels: default=info, -v=debug, -vv=trace
    // RUST_LOG env var overrides if set
    let default_filter = match cli.verbose {
        0 => "pricka=info,pricka_handelsbanken=info,pricka_bankid=info",
        1 => "pricka=debug,pricka_handelsbanken=debug,pricka_bankid=debug",
        _ => "pricka=trace,pricka_handelsbanken=trace,pricka_bankid=trace",
    };

    let filter = if cli.machine {
        tracing_subscriber::EnvFilter::new("off")
    } else {
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| default_filter.parse().unwrap())
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_writer(std::io::stderr)
        .init();

    // Resolve proxy config
    let proxy = if cli.tor {
        ProxyConfig::Socks5("socks5://127.0.0.1:9050".into())
    } else if let Some(url) = &cli.proxy {
        if url.starts_with("socks") {
            ProxyConfig::Socks5(url.clone())
        } else {
            ProxyConfig::Http(url.clone())
        }
    } else {
        ProxyConfig::None
    };

    // Create bank adapter
    let mut bank = create_bank(&cli.bank, proxy, !cli.headed)?;
    if !cli.machine {
        eprintln!("Connecting to {}...", bank.name());
    }

    // Authenticate
    let terminal_presenter = TerminalPresenter;
    let machine_presenter = machine::MachinePresenter::default();
    let presenter: &dyn AuthPresenter = if cli.machine {
        &machine_presenter
    } else {
        &terminal_presenter
    };
    bank.authenticate(personnummer, presenter).await?;

    if cli.machine {
        if let Err(error) = machine_presenter.check() {
            let _ = bank.logout().await;
            return Err(error);
        }
        let selected_id = machine_start.and_then(|input| input.account_id);
        machine::run_session(
            &mut *bank,
            &range,
            selected_id,
            &mut std::io::stdin().lock(),
            &mut std::io::stdout().lock(),
        )
        .await?;
        return Ok(());
    }

    // Fetch accounts
    let accounts = bank.list_accounts().await?;
    eprintln!("Found {} accounts", accounts.len());

    // Fetch transactions for each account
    let mut transactions: HashMap<String, Vec<pricka_core::Transaction>> = HashMap::new();
    for account in &accounts {
        eprintln!("Fetching transactions for {}...", account.name);
        let txs = bank.get_transactions(&account.id, &range).await?;
        eprintln!("  {} transactions", txs.len());
        transactions.insert(account.name.clone(), txs);
    }

    // Build result
    let result = ScrapeResult {
        scraped_at: chrono::Utc::now().to_rfc3339(),
        accounts,
        transactions,
    };

    // Output
    let json = serde_json::to_string_pretty(&result)?;
    if let Some(path) = &cli.output {
        std::fs::write(path, &json)?;
        eprintln!("Results saved to {path}");
    } else {
        println!("{json}");
    }

    // Logout
    bank.logout().await?;

    Ok(())
}

fn date_range(cli: &Cli) -> Result<DateRange, Box<dyn std::error::Error>> {
    let today = chrono::Local::now().date_naive();
    let from = cli
        .from
        .as_deref()
        .map(parse_date)
        .transpose()?
        .unwrap_or(today - chrono::Duration::days(90));
    let to = cli
        .to
        .as_deref()
        .map(parse_date)
        .transpose()?
        .unwrap_or(today);
    if from > to {
        return Err("--from must be on or before --to".into());
    }
    Ok(DateRange { from, to })
}
