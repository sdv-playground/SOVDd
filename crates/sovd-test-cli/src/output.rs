//! Output rendering for sovd-test-cli — a lean subset of sovd-cli's output
//! layer (table / json), carrying only what the tester needs: message helpers
//! and `print`/`print_one` over `Tabled + Serialize` rows. No CSV, no per-domain
//! Row zoo — the tester defines its own report rows.

use clap::ValueEnum;
use colored::Colorize;
use serde::Serialize;
use tabled::{Table, Tabled};

/// Output format — table for humans, json for a CI/rig consumer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    /// ASCII table (default).
    #[default]
    Table,
    /// JSON.
    Json,
}

/// Context for output rendering.
pub struct OutputContext {
    pub format: OutputFormat,
    pub quiet: bool,
}

impl OutputContext {
    pub fn new(format: OutputFormat, no_color: bool, quiet: bool) -> Self {
        if no_color {
            colored::control::set_override(false);
        }
        Self { format, quiet }
    }

    /// Success line (suppressed in quiet mode).
    pub fn success(&self, msg: &str) {
        if !self.quiet {
            println!("{}", msg.green());
        }
    }

    /// Info line (suppressed in quiet mode).
    pub fn info(&self, msg: &str) {
        if !self.quiet {
            println!("{}", msg);
        }
    }

    /// Warning line — always to stderr (a diagnostic, not the result).
    pub fn warn(&self, msg: &str) {
        eprintln!("{}", msg.yellow());
    }

    /// Error line — always to stderr.
    pub fn error(&self, msg: &str) {
        eprintln!("{}", msg.red());
    }

    /// Print a slice in the configured format.
    pub fn print<T: Tabled + Serialize>(&self, data: &[T]) {
        match self.format {
            OutputFormat::Table => {
                if data.is_empty() {
                    if !self.quiet {
                        println!("No data");
                    }
                } else {
                    println!("{}", Table::new(data));
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

    /// Print a single item in the configured format.
    pub fn print_one<T: Tabled + Serialize>(&self, data: &T) {
        match self.format {
            OutputFormat::Table => println!("{}", Table::new([data])),
            OutputFormat::Json => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(data).unwrap_or_else(|_| "{}".to_string())
                );
            }
        }
    }
}
