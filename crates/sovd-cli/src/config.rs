//! Configuration file handling for sovd-cli

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Configuration for the CLI tool
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    /// Default server URL
    pub server: Option<String>,
    /// Default output format
    pub output: Option<String>,
    /// Disable colored output
    pub no_color: Option<bool>,
}

impl Config {
    /// Load configuration from the default config file
    pub fn load() -> Result<Self> {
        let config_path = Self::config_path()?;
        if config_path.exists() {
            let content = std::fs::read_to_string(&config_path).with_context(|| {
                format!("Failed to read config file: {}", config_path.display())
            })?;
            toml::from_str(&content)
                .with_context(|| format!("Failed to parse config file: {}", config_path.display()))
        } else {
            Ok(Self::default())
        }
    }

    /// Load configuration from a specific path
    pub fn load_from(path: &PathBuf) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {}", path.display()))?;
        toml::from_str(&content)
            .with_context(|| format!("Failed to parse config file: {}", path.display()))
    }

    /// Get the default config file path
    pub fn config_path() -> Result<PathBuf> {
        let config_dir = dirs::config_dir()
            .context("Could not determine config directory")?
            .join("sovd-cli");

        Ok(config_dir.join("config.toml"))
    }

    /// Merge CLI arguments over config file values
    pub fn merge_with_args(
        &self,
        server: Option<&str>,
        output: Option<&str>,
        no_color: bool,
    ) -> MergedConfig {
        MergedConfig {
            server: server
                .map(String::from)
                .or_else(|| self.server.clone())
                .unwrap_or_else(|| "http://localhost:8080".to_string()),
            output: output
                .map(String::from)
                .or_else(|| self.output.clone())
                .unwrap_or_else(|| "table".to_string()),
            no_color: no_color || self.no_color.unwrap_or(false),
        }
    }
}

/// Fully resolved configuration after merging CLI args
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct MergedConfig {
    pub server: String,
    pub output: String,
    pub no_color: bool,
}
