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

    // Create output context
    let ctx = OutputContext::new(cli.output, merged.no_color, cli.quiet);

    // Execute command
    match &cli.command {
        Commands::List => {
            let client = create_client(&merged.server)?;
            commands::list(&client, &ctx).await?;
        }

        Commands::Info { ecu } => {
            let client = create_client(&merged.server)?;
            commands::info(&client, ecu, &ctx).await?;
        }

        Commands::Data { ecu } => {
            let client = create_client(&merged.server)?;
            commands::data(&client, ecu, &ctx).await?;
        }

        Commands::Read { ecu, params, all } => {
            let client = create_client(&merged.server)?;
            commands::read(&client, ecu, params, *all, &ctx).await?;
        }

        Commands::Write { ecu, param, value } => {
            let client = create_client(&merged.server)?;
            commands::write(&client, ecu, param, value, &ctx).await?;
        }

        Commands::Faults { ecu, active, clear } => {
            let client = create_client(&merged.server)?;
            commands::faults(&client, ecu, *active, *clear, &ctx).await?;
        }

        Commands::Monitor { ecu, params, rate } => {
            let client = create_client(&merged.server)?;
            commands::monitor(&client, ecu, params.clone(), *rate, &ctx).await?;
        }

        Commands::Session { ecu, session_type } => {
            let client = create_client(&merged.server)?;
            commands::session(&client, ecu, session_type, &ctx).await?;
        }

        Commands::Unlock { ecu, level, key } => {
            let client = create_client(&merged.server)?;
            commands::unlock(&client, ecu, *level, key.as_deref(), &ctx).await?;
        }

        Commands::Outputs { ecu } => {
            let client = create_client(&merged.server)?;
            commands::outputs(&client, ecu, &ctx).await?;
        }

        Commands::Actuate {
            ecu,
            output,
            action,
            value,
        } => {
            let client = create_client(&merged.server)?;
            commands::actuate(&client, ecu, output, action, value.as_deref(), &ctx).await?;
        }

        Commands::Flash { ecu, file } => {
            let flash_client = FlashClient::for_sovd(&merged.server, ecu)
                .context("Failed to create flash client")?;
            commands::flash(&flash_client, file, &ctx).await?;
        }

        Commands::Reset { ecu, reset_type } => {
            let client = create_client(&merged.server)?;
            commands::reset(&client, ecu, reset_type.as_deref(), &ctx).await?;
        }

        Commands::Ops { ecu } => {
            let client = create_client(&merged.server)?;
            commands::ops(&client, ecu, &ctx).await?;
        }

        Commands::Run {
            ecu,
            operation,
            action,
            params,
        } => {
            let client = create_client(&merged.server)?;
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
    }

    Ok(())
}

/// Create a SOVD client for the given server URL
fn create_client(server: &str) -> Result<SovdClient> {
    SovdClient::new(server).context("Failed to create SOVD client")
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
