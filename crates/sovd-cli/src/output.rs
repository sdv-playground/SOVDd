//! Output formatting for sovd-cli.
//!
//! Two audiences, two formats:
//! - **Table** — the human view: pretty, colourised, lossy (renamed columns,
//!   flattened fields). Not meant to be parsed.
//! - **Json** — the MACHINE view: the raw wire entity serialized verbatim
//!   (server field names, native types, full nested `fields`). Logs emit
//!   **NDJSON** (one object per line — streams, `--follow`-safe, jq/grep-able);
//!   bounded catalogs emit a JSON array.

use clap::ValueEnum;
use colored::Colorize;
use serde::Serialize;
use tabled::{Table, Tabled};

/// Output format options
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    /// ASCII table format (default) — the human view.
    #[default]
    Table,
    /// JSON — the raw wire entity (NDJSON for log streams). The machine view.
    Json,
}

/// Context for output rendering
#[allow(dead_code)]
pub struct OutputContext {
    pub format: OutputFormat,
    pub no_color: bool,
    pub quiet: bool,
}

impl OutputContext {
    pub fn new(format: OutputFormat, no_color: bool, quiet: bool) -> Self {
        if no_color {
            colored::control::set_override(false);
        }
        Self {
            format,
            no_color,
            quiet,
        }
    }

    /// Print a success message (unless in quiet mode)
    pub fn success(&self, msg: &str) {
        if !self.quiet {
            println!("{}", msg.green());
        }
    }

    /// Print an info message (unless in quiet mode)
    pub fn info(&self, msg: &str) {
        if !self.quiet {
            println!("{}", msg);
        }
    }

    /// Print a warning message
    #[allow(dead_code)]
    pub fn warn(&self, msg: &str) {
        eprintln!("{}", msg.yellow());
    }

    /// Print an error message
    pub fn error(&self, msg: &str) {
        eprintln!("{}", msg.red());
    }

    /// Print data in the configured format
    pub fn print<T: Tabled + Serialize>(&self, data: &[T]) {
        match self.format {
            OutputFormat::Table => {
                if data.is_empty() {
                    if !self.quiet {
                        println!("No data");
                    }
                } else {
                    let table = Table::new(data).to_string();
                    println!("{}", table);
                }
            }
            OutputFormat::Json => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(data).unwrap_or_else(|_| "[]".to_string())
                );
            }
        }
    }

    /// Print a collection where the human (Table) and machine (Json) views are
    /// DIFFERENT objects: `entities` are the raw wire values (serialized verbatim
    /// for Json — lossless, server field names), and `to_row` maps each to a
    /// pretty display row for the Table. Use this whenever the table columns are a
    /// lossy projection of a richer wire type (e.g. the log-source catalog).
    pub fn print_entities<E, R>(&self, entities: &[E], to_row: impl Fn(&E) -> R)
    where
        E: Serialize,
        R: Tabled + Serialize,
    {
        match self.format {
            OutputFormat::Table => {
                let rows: Vec<R> = entities.iter().map(&to_row).collect();
                self.print(&rows);
            }
            OutputFormat::Json => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(entities).unwrap_or_else(|_| "[]".to_string())
                );
            }
        }
    }

    /// Print a single item in the configured format
    pub fn print_one<T: Tabled + Serialize>(&self, data: &T) {
        match self.format {
            OutputFormat::Table => {
                let table = Table::new([data]).to_string();
                println!("{}", table);
            }
            OutputFormat::Json => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(data).unwrap_or_else(|_| "{}".to_string())
                );
            }
        }
    }

    /// Print key-value pairs (for info command)
    pub fn print_kv(&self, pairs: &[(&str, String)]) {
        match self.format {
            OutputFormat::Table => {
                for (key, value) in pairs {
                    println!("{}: {}", key.bold(), value);
                }
            }
            OutputFormat::Json => {
                let map: std::collections::HashMap<&str, &str> =
                    pairs.iter().map(|(k, v)| (*k, v.as_str())).collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&map).unwrap_or_else(|_| "{}".to_string())
                );
            }
        }
    }

    /// Print a batch of log entries. Table = LINE-per-entry (journalctl-style),
    /// NOT a boxed grid — logs are a stream, and a `+---+` box with a separator
    /// per row is unscannable and inflates to the widest message. Json = NDJSON:
    /// one RAW `LogEntry` per line (server field names, native types, full nested
    /// `fields` — machine/LLM-faithful, and the same shape a `--follow` stream
    /// appends incrementally).
    pub fn print_logs(&self, entries: &[sovd_client::LogEntry]) {
        for e in entries {
            self.print_log_line(e);
        }
    }

    /// Print ONE log entry — one Table line, or one NDJSON object. Because both
    /// formats are per-entry, `print_logs` and the `--follow` loop share this and
    /// stream identically (no buffered array to close).
    pub fn print_log_line(&self, e: &sovd_client::LogEntry) {
        match self.format {
            OutputFormat::Table => {
                let row = LogRow::from(e);
                // `TIME LEVEL SOURCE [emitter] message` — emitter only when present.
                let level = colorize_level(&row.level);
                let emitter = if row.emitter.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", row.emitter.cyan())
                };
                println!(
                    "{}  {:<7}  {}{}  {}",
                    row.time.dimmed(),
                    level,
                    row.source,
                    emitter,
                    row.message
                );
            }
            // NDJSON: the RAW wire entity, one compact object per line — NOT the
            // lossy display `LogRow` (which renames priority→level, drops
            // pid/status/href, and flattens `fields` to just the emitter).
            OutputFormat::Json => {
                println!(
                    "{}",
                    serde_json::to_string(e).unwrap_or_else(|_| "{}".to_string())
                );
            }
        }
    }
}

/// Colorize a syslog level name for the line log view (no-op when colour is off,
/// which `OutputContext::new` already globally disables via `--no-color`).
fn colorize_level(level: &str) -> colored::ColoredString {
    match level {
        "emergency" | "alert" | "critical" | "error" => level.red().bold(),
        "warning" => level.yellow(),
        "notice" | "info" => level.normal(),
        "debug" | "trace" => level.dimmed(),
        _ => level.normal(),
    }
}

// =============================================================================
// Display types for various commands
// =============================================================================

/// Component display for list command
#[derive(Debug, Tabled, Serialize)]
pub struct ComponentRow {
    #[tabled(rename = "ID")]
    pub id: String,
    #[tabled(rename = "Name")]
    pub name: String,
    #[tabled(rename = "Status")]
    pub status: String,
}

/// A log source in the `logs <ecu> sources` catalog.
#[derive(Debug, Tabled, Serialize)]
pub struct LogSourceRow {
    #[tabled(rename = "SOURCE")]
    pub name: String,
    #[tabled(rename = "KIND")]
    pub kind: String,
    #[tabled(rename = "CURSOR")]
    pub cursor: String,
    #[tabled(rename = "EMITTERS")]
    pub emitters: String,
}

/// Parameter display for data command
#[derive(Debug, Tabled, Serialize)]
pub struct ParameterRow {
    #[tabled(rename = "ID")]
    pub id: String,
    #[tabled(rename = "DID")]
    pub did: String,
    #[tabled(rename = "Name")]
    pub name: String,
    #[tabled(rename = "Type")]
    pub data_type: String,
    #[tabled(rename = "Unit")]
    pub unit: String,
}

/// Data value display for read command
#[derive(Debug, Tabled, Serialize)]
pub struct DataRow {
    #[tabled(rename = "Parameter")]
    pub parameter: String,
    #[tabled(rename = "Value")]
    pub value: String,
    #[tabled(rename = "Unit")]
    pub unit: String,
    #[tabled(rename = "Raw")]
    pub raw: String,
}

/// Fault display for faults command
#[derive(Debug, Tabled, Serialize)]
pub struct FaultRow {
    #[tabled(rename = "Code")]
    pub code: String,
    #[tabled(rename = "Fault")]
    pub fault_name: String,
    #[tabled(rename = "Severity")]
    pub severity: String,
    #[tabled(rename = "Active")]
    pub active: String,
    #[tabled(rename = "Category")]
    pub category: String,
}

/// Output display for the logs command (SOVD §7.21 entries).
#[derive(Debug, Tabled, Serialize)]
pub struct LogRow {
    #[tabled(rename = "Time")]
    pub time: String,
    #[tabled(rename = "Level")]
    pub level: String,
    #[tabled(rename = "Source")]
    pub source: String,
    /// The emitter within a multi-emitter source (the slog2 buffer name:
    /// supernova / vhsm-ssd / devb_sdmmc / …). Blank for single-emitter sources
    /// (files, guest agent) that don't carry `fields.emitter`.
    #[tabled(rename = "Emitter")]
    pub emitter: String,
    #[tabled(rename = "Message")]
    pub message: String,
    #[tabled(rename = "ID")]
    pub id: String,
}

impl From<&sovd_client::LogEntry> for LogRow {
    fn from(e: &sovd_client::LogEntry) -> Self {
        LogRow {
            // The client models timestamp/priority as the raw wire strings.
            time: e.timestamp.clone(),
            level: e.priority.clone(),
            source: e.source.clone().unwrap_or_default(),
            // Pull the emitter out of the structured fields (slog2 records set
            // fields.emitter to the buffer name); blank when absent.
            emitter: e
                .fields
                .as_ref()
                .and_then(|f| f.get("emitter"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            message: e.message.clone(),
            id: e.id.clone(),
        }
    }
}

/// Output display for outputs command
#[derive(Debug, Tabled, Serialize)]
pub struct OutputRow {
    #[tabled(rename = "ID")]
    pub id: String,
    #[tabled(rename = "Name")]
    pub name: String,
    #[tabled(rename = "Type")]
    pub data_type: String,
    #[tabled(rename = "Controls")]
    pub controls: String,
}

/// Operation display for ops command
#[derive(Debug, Tabled, Serialize)]
pub struct OperationRow {
    #[tabled(rename = "ID")]
    pub id: String,
    #[tabled(rename = "Name")]
    pub name: String,
    #[tabled(rename = "Description")]
    pub description: String,
    #[tabled(rename = "Security")]
    pub requires_security: String,
}

/// Stream event display for monitor command
#[derive(Debug, Tabled, Serialize)]
pub struct StreamRow {
    #[tabled(rename = "Time")]
    pub timestamp: String,
    #[tabled(rename = "Seq")]
    pub sequence: String,
    #[tabled(rename = "Parameter")]
    pub parameter: String,
    #[tabled(rename = "Value")]
    pub value: String,
}
