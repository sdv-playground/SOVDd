//! sovd-test-cli — the external test runner over SOVD §7.15 scripts.
//!
//! A policy layer over diagnostics, deliberately SEPARATE from the generic
//! `sovd-cli`: it discovers a component's developer-registered tests, executes
//! each over SOVD, captures exactly that run's log window via the execution's
//! cursor bracket, judges verdict + logs, and reports. Its exit code IS its
//! result (0 = all passed, 1 = a test failed, 2 = a setup/transport error) —
//! a CI/rig gate keys off it. The protocol binding (the `scripts`/`logs` HTTP
//! methods) lives in `sovd-client`; only the test opinions live here.

mod harness;
mod output;

use anyhow::{Context, Result};
use clap::Parser;
use sovd_client::SovdClient;
use std::path::PathBuf;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use crate::output::{OutputContext, OutputFormat};

#[derive(Parser)]
#[command(name = "sovd-test-cli")]
#[command(
    author,
    version,
    about = "Run developer tests over SOVD and judge verdict + logs"
)]
#[command(propagate_version = true)]
struct Cli {
    /// ECU / component id (a guest VM exposing a test-agent).
    ecu: String,

    /// Run only this test id (positional). Omit to run all registered tests.
    test_id: Option<String>,

    /// Server URL.
    #[arg(
        short,
        long,
        env = "SOVD_SERVER",
        default_value = "http://localhost:8080"
    )]
    server: String,

    /// Bearer token (JWT) sent as `Authorization: Bearer <token>`. Mint it with
    /// the workshop minter (see examples/autoloader/sovd-get-logs.sh). Omit for
    /// a device serving tokenless reads.
    #[arg(long, env = "SOVD_TOKEN")]
    token: Option<String>,

    /// Pin this CA root (PEM) when verifying the server's TLS certificate.
    /// Takes precedence over `--insecure`.
    #[arg(long, env = "SOVD_CA_CERT")]
    ca_cert: Option<PathBuf>,

    /// Skip TLS certificate verification (`curl -k` equivalent). Ignored when
    /// `--ca-cert` is given. Dev rigs with a self-signed device leaf only.
    #[arg(long)]
    insecure: bool,

    /// Discovery filter: only tests carrying this tag (e.g. smoke).
    #[arg(long)]
    tag: Option<String>,

    /// Extra comma-separated substrings that mark a log line anomalous, on top
    /// of the built-in crash markers (case-insensitive).
    #[arg(long, value_delimiter = ',')]
    grep: Vec<String>,

    /// Judge on the framework verdict alone — skip the log-window scan.
    #[arg(long)]
    no_log_check: bool,

    /// Print anomalous log lines from each test's window.
    #[arg(long)]
    show_logs: bool,

    /// Write the full JSON report to this path.
    #[arg(long, short = 'r')]
    report: Option<String>,

    /// Output format.
    #[arg(short, long, value_enum, default_value = "table")]
    output: OutputFormat,

    /// Disable colored output.
    #[arg(long)]
    no_color: bool,

    /// Minimal output (for scripting).
    #[arg(short, long)]
    quiet: bool,

    /// Verbose logging.
    #[arg(short, long)]
    verbose: bool,
}

/// Exit codes — the tester's result is its status. Distinguish a test FAILURE
/// (1, the run completed and something failed) from a SETUP/transport error
/// (2, we couldn't run the tests at all) so a CI gate can tell them apart.
const EXIT_TEST_FAILED: i32 = 1;
const EXIT_SETUP_ERROR: i32 = 2;

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let filter = if cli.verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::new("warn")
    };
    tracing_subscriber::registry()
        .with(fmt::layer().with_target(false))
        .with(filter)
        .init();

    let ctx = OutputContext::new(cli.output, cli.no_color, cli.quiet);

    match run(&cli, &ctx).await {
        Ok(0) => std::process::exit(0),
        Ok(_failed) => std::process::exit(EXIT_TEST_FAILED),
        Err(e) => {
            // A setup/transport error — distinct from a test failing.
            ctx.error(&format!("error: {e:#}"));
            std::process::exit(EXIT_SETUP_ERROR);
        }
    }
}

/// Resolve auth, build the client, run the harness. Returns the failed-test
/// count (Ok) or a setup error (Err) — main maps both to exit codes.
async fn run(cli: &Cli, ctx: &OutputContext) -> Result<usize> {
    let auth = ClientAuth::from_cli(cli)?;
    let client = create_client(&cli.server, &auth)?;
    let args = harness::TestArgs {
        only: cli.test_id.clone(),
        tag: cli.tag.clone(),
        grep: cli.grep.clone(),
        no_log_check: cli.no_log_check,
        show_logs: cli.show_logs,
        report: cli.report.clone(),
    };
    harness::run(&client, &cli.ecu, &args, ctx).await
}

/// Client-auth inputs resolved once from the CLI flags: an optional bearer token
/// plus the TLS trust decision (pinned CA root PEM, or skip-verify).
struct ClientAuth {
    token: Option<String>,
    ca_cert_pem: Option<Vec<u8>>,
    insecure: bool,
}

impl ClientAuth {
    /// Read the CA PEM off disk (if `--ca-cert` was given) so a bad path fails
    /// once, up front, rather than on the first request.
    fn from_cli(cli: &Cli) -> Result<Self> {
        let ca_cert_pem = match &cli.ca_cert {
            Some(path) => Some(
                std::fs::read(path)
                    .with_context(|| format!("Failed to read --ca-cert {}", path.display()))?,
            ),
            None => None,
        };
        Ok(Self {
            token: cli.token.clone(),
            ca_cert_pem,
            insecure: cli.insecure,
        })
    }
}

/// Build a SOVD client honouring the resolved auth: a bearer token when present,
/// otherwise unauthenticated — both verifying against the pinned CA (or skipping
/// verification when `--insecure`).
fn create_client(server: &str, auth: &ClientAuth) -> Result<SovdClient> {
    let ca = auth.ca_cert_pem.as_deref();
    match &auth.token {
        Some(token) => SovdClient::with_bearer_token_verifying(server, token, auth.insecure, ca),
        None => SovdClient::new_verifying(server, auth.insecure, ca),
    }
    .context("Failed to create SOVD client")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The clap layer is wired up correctly (no overlapping short flags, valid
    /// arg spec).
    #[test]
    fn cli_arg_spec_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    /// `ecu` is required and positional; `test_id` is the optional second
    /// positional. `sovd-test-cli vm1` runs all; `… vm1 hsm.sign` runs one.
    #[test]
    fn positional_ecu_and_optional_test_id() {
        let all = Cli::try_parse_from(["sovd-test-cli", "vm1"]).expect("parse all");
        assert_eq!(all.ecu, "vm1");
        assert!(all.test_id.is_none());

        let one = Cli::try_parse_from(["sovd-test-cli", "vm1", "hsm.sign"]).expect("parse one");
        assert_eq!(one.ecu, "vm1");
        assert_eq!(one.test_id.as_deref(), Some("hsm.sign"));
    }

    /// `--grep a,b` splits on commas into multiple markers.
    #[test]
    fn grep_splits_on_comma() {
        let cli =
            Cli::try_parse_from(["sovd-test-cli", "vm1", "--grep", "watchdog,brownout"]).unwrap();
        assert_eq!(cli.grep, vec!["watchdog", "brownout"]);
    }

    /// A `--ca-cert` path that doesn't exist fails up front (at auth resolution).
    #[test]
    fn ca_cert_missing_file_errors_early() {
        let cli =
            Cli::try_parse_from(["sovd-test-cli", "vm1", "--ca-cert", "/no/such/ca.pem"]).unwrap();
        match ClientAuth::from_cli(&cli) {
            Ok(_) => panic!("missing CA file must error"),
            Err(e) => assert!(e.to_string().contains("--ca-cert")),
        }
    }

    /// Client builds with and without a token (no network I/O at build).
    #[test]
    fn create_client_selects_by_token_presence() {
        let with_token = ClientAuth {
            token: Some("jwt.abc.def".to_string()),
            ca_cert_pem: None,
            insecure: false,
        };
        assert!(create_client("http://localhost:8080", &with_token).is_ok());

        let no_token = ClientAuth {
            token: None,
            ca_cert_pem: None,
            insecure: true,
        };
        assert!(create_client("http://localhost:8080", &no_token).is_ok());
    }
}
