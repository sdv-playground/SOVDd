//! Output formatting for sovd-cli (table, json, csv)

use clap::ValueEnum;
use colored::Colorize;
use serde::Serialize;
use tabled::{Table, Tabled};

/// Output format options
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    /// ASCII table format (default)
    Table,
    /// JSON format
    Json,
    /// CSV format
    Csv,
}

impl Default for OutputFormat {
    fn default() -> Self {
        Self::Table
    }
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
            OutputFormat::Csv => {
                print_csv(data);
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
            OutputFormat::Csv => {
                print_csv(&[data]);
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
            OutputFormat::Csv => {
                // Header
                let keys: Vec<&str> = pairs.iter().map(|(k, _)| *k).collect();
                println!("{}", keys.join(","));
                // Values
                let values: Vec<&str> = pairs.iter().map(|(_, v)| v.as_str()).collect();
                println!("{}", values.join(","));
            }
        }
    }
}

/// Print data as CSV
fn print_csv<T: Serialize>(data: &[T]) {
    if data.is_empty() {
        return;
    }

    // Get field names from the first item
    let first = serde_json::to_value(&data[0]).unwrap_or_default();
    if let serde_json::Value::Object(map) = &first {
        // Print header
        let headers: Vec<&str> = map.keys().map(|s| s.as_str()).collect();
        println!("{}", headers.join(","));

        // Print rows
        for item in data {
            if let Ok(serde_json::Value::Object(row)) = serde_json::to_value(item) {
                let values: Vec<String> = headers
                    .iter()
                    .map(|h| {
                        row.get(*h)
                            .map(|v| match v {
                                serde_json::Value::String(s) => escape_csv(s),
                                other => escape_csv(&other.to_string()),
                            })
                            .unwrap_or_default()
                    })
                    .collect();
                println!("{}", values.join(","));
            }
        }
    }
}

/// Escape a value for CSV output
fn escape_csv(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
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
    #[tabled(rename = "Message")]
    pub message: String,
    #[tabled(rename = "Severity")]
    pub severity: String,
    #[tabled(rename = "Active")]
    pub active: String,
    #[tabled(rename = "Category")]
    pub category: String,
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
