//! SOVD CLI - Command-line tool for SOVD vehicle diagnostics
//!
//! A comprehensive CLI for interacting with SOVD-compliant diagnostic servers.

mod commands;
mod config;
mod output;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use sovd_client::flash::FlashClient;
use sovd_client::SovdClient;
use std::path::PathBuf;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use crate::config::Config;
use crate::output::{OutputContext, OutputFormat};

#[derive(Parser)]
#[command(name = "sovd-cli")]
#[command(author, version, about = "SOVD Vehicle Diagnostics CLI")]
#[command(propagate_version = true)]
struct Cli {
    /// Server URL
    #[arg(
        short,
        long,
        env = "SOVD_SERVER",
        default_value = "http://localhost:8080"
    )]
    server: String,

    /// Configuration file path
    #[arg(short, long, env = "SOVD_CONFIG")]
    config: Option<PathBuf>,

    /// Bearer token (JWT) sent as `Authorization: Bearer <token>`. Mint it with
    /// the workshop minter (see examples/autoloader/sovd-get-logs.sh). When
    /// omitted the client is unauthenticated — fine for a device serving
    /// tokenless reads, rejected (401) otherwise.
    #[arg(long, env = "SOVD_TOKEN")]
    token: Option<String>,

    /// Pin this CA root (PEM file) when verifying the server's TLS certificate —
    /// the tower identity root for dialling a device's self-signed leaf. Takes
    /// precedence over `--insecure`.
    #[arg(long, env = "SOVD_CA_CERT")]
    ca_cert: Option<PathBuf>,

    /// Skip TLS certificate verification (the `curl -k` equivalent). Ignored when
    /// `--ca-cert` is given. For dev rigs with a self-signed device leaf only.
    #[arg(long)]
    insecure: bool,

    /// Output format
    #[arg(short, long, value_enum, default_value = "table")]
    output: OutputFormat,

    /// Disable colored output
    #[arg(long)]
    no_color: bool,

    /// Minimal output (for scripting)
    #[arg(short, long)]
    quiet: bool,

    /// Verbose logging
    #[arg(short, long)]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// List all ECU components
    List,

    /// Show ECU component details
    Info {
        /// ECU component ID
        ecu: String,
    },

    /// List available data parameters
    Data {
        /// ECU component ID
        ecu: String,
    },

    /// Read data parameter(s)
    Read {
        /// ECU component ID
        ecu: String,

        /// Parameter ID(s) to read
        #[arg(required_unless_present = "all")]
        params: Vec<String>,

        /// Read all available parameters
        #[arg(long)]
        all: bool,
    },

    /// Write a data parameter
    Write {
        /// ECU component ID
        ecu: String,

        /// Parameter ID
        param: String,

        /// Value to write (string, number, or JSON)
        value: String,
    },

    /// List and manage faults/DTCs
    Faults {
        /// ECU component ID
        ecu: String,

        /// Show only active faults
        #[arg(long)]
        active: bool,

        /// Clear all faults
        #[arg(long)]
        clear: bool,
    },

    /// Monitor parameters in real-time (SSE streaming)
    Monitor {
        /// ECU component ID
        ecu: String,

        /// Parameter ID(s) to monitor
        #[arg(required = true)]
        params: Vec<String>,

        /// Update rate in Hz
        #[arg(long, default_value = "1")]
        rate: u32,
    },

    /// Change diagnostic session
    Session {
        /// ECU component ID
        ecu: String,

        /// Session type: default, extended, programming, engineering
        #[arg(value_name = "TYPE")]
        session_type: String,
    },

    /// Security access (unlock ECU)
    Unlock {
        /// ECU component ID
        ecu: String,

        /// Security level (1 or 3)
        #[arg(long, default_value = "1")]
        level: Option<u8>,

        /// Security key (hex string, e.g., "0102030405")
        #[arg(long)]
        key: Option<String>,
    },

    /// List available I/O outputs
    Outputs {
        /// ECU component ID
        ecu: String,
    },

    /// Control an I/O output
    Actuate {
        /// ECU component ID
        ecu: String,

        /// Output ID
        output: String,

        /// Control action: adjust, reset, freeze, shortterm
        action: String,

        /// Value for adjust action
        value: Option<String>,
    },

    /// Flash firmware to ECU
    Flash {
        /// ECU component ID
        ecu: String,

        /// Firmware file path
        file: PathBuf,
    },

    /// Reset ECU
    Reset {
        /// ECU component ID
        ecu: String,

        /// Reset type: hard, soft, key_off_on
        #[arg(long, default_value = "hard")]
        reset_type: Option<String>,
    },

    /// List available operations/routines
    Ops {
        /// ECU component ID
        ecu: String,
    },

    /// Execute an operation/routine
    Run {
        /// ECU component ID
        ecu: String,

        /// Operation ID
        operation: String,

        /// Action: start, stop, result
        #[arg(long, default_value = "start")]
        action: Option<String>,

        /// Parameters as JSON object
        #[arg(long)]
        params: Option<String>,
    },

    /// Access SOVD §7.21 logs. Point at a vehicle gateway to get the whole
    /// vehicle's logs (merged + timestamp-sorted server-side).
    Logs {
        /// ECU / component id (a gateway id gives the merged vehicle view).
        ecu: String,

        /// Action (positional): list (default), get, content, delete.
        #[arg(default_value = "list")]
        action: String,

        /// Log entry id (positional) — required for get / content / delete.
        id: Option<String>,

        /// Only priority this level and above (emergency|alert|critical|error|warning|notice|info|debug).
        #[arg(long)]
        priority: Option<String>,

        /// Filter by source (service/unit name).
        #[arg(long)]
        source: Option<String>,

        /// Filter by log type (e.g. engine_dump, diagnostic).
        #[arg(long = "type")]
        log_type: Option<String>,

        /// Filter by retrieval status (pending|retrieved).
        #[arg(long)]
        status: Option<String>,

        /// Substring the message must contain (client-side).
        #[arg(long)]
        grep: Option<String>,

        /// Show only the last N entries.
        #[arg(long)]
        tail: Option<usize>,

        /// Server-side max entries to fetch.
        #[arg(long)]
        limit: Option<u32>,

        /// For `content`: write bytes to this file instead of stdout.
        #[arg(long, short = 'o')]
        out: Option<String>,

        /// Follow: poll the list and print new entries (tail -f).
        #[arg(long, short = 'f')]
        follow: bool,

        /// Poll interval (seconds) for --follow.
        #[arg(long, default_value = "1.0")]
        interval: f64,

        /// Page the whole log via the server cursor (oldest→newest), following
        /// `next_cursor` until exhausted. The interactive "get all" over the
        /// inline JSON surface — for a WHOLE-FILE download use `bulk-data`.
        #[arg(long)]
        all: bool,

        /// Lower time bound: RFC 3339, or a position sentinel BEGIN (oldest) /
        /// END (now) / END-<N>{s,m,h,d} (e.g. END-10m = last 10 min of this
        /// boot). Resolved server-side. Precise on journald, coarse (file mtime)
        /// on host files; a cursor is the reliable resume token across reboots.
        #[arg(long)]
        since: Option<String>,

        /// Upper time bound: RFC 3339 or BEGIN/END/END-<N>… (see --since).
        #[arg(long)]
        until: Option<String>,
    },

    /// Access §7.20 bulk-data (log files / large payloads). The spec-native
    /// "get all logs": `bulk-data get-all <ecu> logs -d <dir>` downloads every
    /// item in a category.
    BulkData {
        /// ECU / component id.
        ecu: String,

        /// Action (positional): categories (default) | list | download | get-all.
        #[arg(default_value = "categories")]
        action: String,

        /// Category id — required for list / download / get-all (e.g. `logs`).
        category: Option<String>,

        /// Item id — required for `download`.
        id: Option<String>,

        /// Only items created after this RFC 3339 time (list / get-all).
        #[arg(long)]
        created_after: Option<String>,

        /// Only items created before this RFC 3339 time (list / get-all).
        #[arg(long)]
        created_before: Option<String>,

        /// For `download`: write bytes to this file (default: stdout).
        #[arg(long, short = 'o')]
        out: Option<String>,

        /// For `get-all`: write each item to this directory (one file per id).
        #[arg(long, short = 'd')]
        dir: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Set up logging
    let filter = if cli.verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::new("warn")
    };

    tracing_subscriber::registry()
        .with(fmt::layer().with_target(false))
        .with(filter)
        .init();

    // Load config file
    let config = if let Some(config_path) = &cli.config {
        Config::load_from(config_path)?
    } else {
        Config::load().unwrap_or_default()
    };

    // Merge CLI args with config
    let merged = config.merge_with_args(Some(&cli.server), Some(cli.output.into()), cli.no_color);

    // Resolve client auth (bearer token + TLS trust) once for every command.
    let auth = ClientAuth::from_cli(&cli)?;

    // Create output context
    let ctx = OutputContext::new(cli.output, merged.no_color, cli.quiet);

    // Execute command
    match &cli.command {
        Commands::List => {
            let client = create_client(&merged.server, &auth)?;
            commands::list(&client, &ctx).await?;
        }

        Commands::Info { ecu } => {
            let client = create_client(&merged.server, &auth)?;
            commands::info(&client, ecu, &ctx).await?;
        }

        Commands::Data { ecu } => {
            let client = create_client(&merged.server, &auth)?;
            commands::data(&client, ecu, &ctx).await?;
        }

        Commands::Read { ecu, params, all } => {
            let client = create_client(&merged.server, &auth)?;
            commands::read(&client, ecu, params, *all, &ctx).await?;
        }

        Commands::Write { ecu, param, value } => {
            let client = create_client(&merged.server, &auth)?;
            commands::write(&client, ecu, param, value, &ctx).await?;
        }

        Commands::Faults { ecu, active, clear } => {
            let client = create_client(&merged.server, &auth)?;
            commands::faults(&client, ecu, *active, *clear, &ctx).await?;
        }

        Commands::Monitor { ecu, params, rate } => {
            let client = create_client(&merged.server, &auth)?;
            commands::monitor(&client, ecu, params.clone(), *rate, &ctx).await?;
        }

        Commands::Session { ecu, session_type } => {
            let client = create_client(&merged.server, &auth)?;
            commands::session(&client, ecu, session_type, &ctx).await?;
        }

        Commands::Unlock { ecu, level, key } => {
            let client = create_client(&merged.server, &auth)?;
            commands::unlock(&client, ecu, *level, key.as_deref(), &ctx).await?;
        }

        Commands::Outputs { ecu } => {
            let client = create_client(&merged.server, &auth)?;
            commands::outputs(&client, ecu, &ctx).await?;
        }

        Commands::Actuate {
            ecu,
            output,
            action,
            value,
        } => {
            let client = create_client(&merged.server, &auth)?;
            commands::actuate(&client, ecu, output, action, value.as_deref(), &ctx).await?;
        }

        Commands::Flash { ecu, file } => {
            // Flash is destructive, so it honours the same global auth flags as
            // every other command (bearer token + TLS trust), threaded through
            // the flash config builder.
            let mut builder = sovd_client::flash::FlashConfig::builder(&merged.server)
                .component_id(ecu)
                .insecure(auth.insecure)
                .ca_cert_pem(auth.ca_cert_pem.clone());
            if let Some(token) = &auth.token {
                builder = builder.bearer(token);
            }
            let flash_client =
                FlashClient::new(builder.build()).context("Failed to create flash client")?;
            commands::flash(&flash_client, file, &ctx).await?;
        }

        Commands::Reset { ecu, reset_type } => {
            let client = create_client(&merged.server, &auth)?;
            commands::reset(&client, ecu, reset_type.as_deref(), &ctx).await?;
        }

        Commands::Ops { ecu } => {
            let client = create_client(&merged.server, &auth)?;
            commands::ops(&client, ecu, &ctx).await?;
        }

        Commands::Run {
            ecu,
            operation,
            action,
            params,
        } => {
            let client = create_client(&merged.server, &auth)?;
            commands::run(
                &client,
                ecu,
                operation,
                action.as_deref(),
                params.as_deref(),
                &ctx,
            )
            .await?;
        }

        Commands::Logs {
            ecu,
            action,
            id,
            priority,
            source,
            log_type,
            status,
            grep,
            tail,
            limit,
            out,
            follow,
            interval,
            all,
            since,
            until,
        } => {
            let client = create_client(&merged.server, &auth)?;
            match action.as_str() {
                "list" => {
                    let args = commands::logs::LogArgs {
                        priority: priority.clone(),
                        source: source.clone(),
                        log_type: log_type.clone(),
                        status: status.clone(),
                        limit: *limit,
                        pattern: grep.clone(),
                        tail: *tail,
                        follow: *follow,
                        interval_secs: *interval,
                        all: *all,
                        since: since.clone(),
                        until: until.clone(),
                    };
                    commands::logs::list(&client, ecu, &args, &ctx).await?;
                }
                "get" | "content" | "delete" => {
                    let Some(id) = id else {
                        anyhow::bail!("a log id is required: `logs <ecu> {action} <id>`");
                    };
                    match action.as_str() {
                        "get" => commands::logs::get(&client, ecu, id, &ctx).await?,
                        "content" => {
                            commands::logs::content(&client, ecu, id, out.as_deref(), &ctx).await?
                        }
                        "delete" => commands::logs::delete(&client, ecu, id, &ctx).await?,
                        _ => unreachable!(),
                    }
                }
                other => anyhow::bail!(
                    "unknown logs action `{other}` (expected: list, get, content, delete)"
                ),
            }
        }

        Commands::BulkData {
            ecu,
            action,
            category,
            id,
            created_after,
            created_before,
            out,
            dir,
        } => {
            let client = create_client(&merged.server, &auth)?;
            commands::bulk_data::run(
                &client,
                ecu,
                action,
                category.as_deref(),
                id.as_deref(),
                created_after.as_deref(),
                created_before.as_deref(),
                out.as_deref(),
                dir.as_deref(),
                &ctx,
            )
            .await?;
        }
    }

    Ok(())
}

/// The client-auth inputs resolved once from the global CLI flags and threaded
/// into every `create_client` call: an optional bearer token plus the TLS trust
/// decision (pinned CA root PEM, or skip-verify).
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

/// Create a SOVD client for the given server URL, honouring the resolved auth:
/// a bearer token when present, otherwise an unauthenticated client — both
/// verifying against the pinned CA (or skipping verification when `--insecure`).
fn create_client(server: &str, auth: &ClientAuth) -> Result<SovdClient> {
    let ca = auth.ca_cert_pem.as_deref();
    match &auth.token {
        Some(token) => {
            SovdClient::with_bearer_token_verifying(server, token, auth.insecure, ca)
        }
        None => SovdClient::new_verifying(server, auth.insecure, ca),
    }
    .context("Failed to create SOVD client")
}

// Implement conversion for OutputFormat to string (for config merge)
impl From<OutputFormat> for &str {
    fn from(format: OutputFormat) -> Self {
        match format {
            OutputFormat::Table => "table",
            OutputFormat::Json => "json",
            OutputFormat::Csv => "csv",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The clap layer is wired up correctly (no overlapping short flags, valid
    /// arg spec) — a debug_assert clap runs only in this test.
    #[test]
    fn cli_arg_spec_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    /// With a token, we build the bearer client; without, the unauthenticated
    /// one. Both must construct successfully (client-build does no network I/O).
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

    /// A `--ca-cert` path that doesn't exist fails up front (at auth resolution),
    /// not deferred to the first request.
    #[test]
    fn ca_cert_missing_file_errors_early() {
        let cli = Cli::try_parse_from([
            "sovd-cli",
            "--ca-cert",
            "/no/such/ca-cert.pem",
            "list",
        ])
        .expect("args parse");
        // Not `.expect_err`: ClientAuth deliberately has no Debug (it holds the
        // bearer token — keep it out of any log/panic output).
        match ClientAuth::from_cli(&cli) {
            Ok(_) => panic!("missing CA file must error"),
            Err(e) => assert!(e.to_string().contains("--ca-cert")),
        }
    }

    /// `logs` action + id are positional: `logs <ecu>` defaults to list,
    /// `logs <ecu> get <id>` fills both.
    #[test]
    fn logs_action_and_id_are_positional() {
        let list = Cli::try_parse_from(["sovd-cli", "logs", "supernova"]).expect("parse list");
        match list.command {
            Commands::Logs { ecu, action, id, .. } => {
                assert_eq!(ecu, "supernova");
                assert_eq!(action, "list"); // default
                assert!(id.is_none());
            }
            _ => panic!("expected Logs"),
        }

        let get = Cli::try_parse_from(["sovd-cli", "logs", "vm1", "get", "line:x:abc"])
            .expect("parse get");
        match get.command {
            Commands::Logs { ecu, action, id, .. } => {
                assert_eq!(ecu, "vm1");
                assert_eq!(action, "get");
                assert_eq!(id.as_deref(), Some("line:x:abc"));
            }
            _ => panic!("expected Logs"),
        }
    }
}
