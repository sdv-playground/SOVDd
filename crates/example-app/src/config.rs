//! TOML configuration for the example app
//!
//! Allows the example app to declare its own public outputs, parameters,
//! and operations (with typed metadata) independently of the UDS gateway
//! config.  The app entity is the authority on its ECU's public interface.

use serde::Deserialize;
use sovd_uds::config::{OperationConfig, OutputConfig};

/// Parameter definition for the example app config.
///
/// Unlike UDS parameters (which come from DID YAML files via `DidStore`),
/// these are declared directly in the example-app TOML config so the
/// app entity controls which parameters are visible and their metadata.
#[derive(Debug, Clone, Deserialize)]
pub struct ParameterDef {
    pub id: String,
    pub name: String,
    /// UDS DID in hex format (e.g., "0xF190")
    pub did: String,
    #[serde(default)]
    pub data_type: Option<String>,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub writable: bool,
    #[serde(default)]
    pub description: Option<String>,
}

/// Configuration for a managed ECU sub-entity
#[derive(Debug, Clone, Deserialize)]
pub struct ManagedEcuConfig {
    /// Sub-entity identifier (e.g., "vtx_vx500")
    pub id: String,
    /// Human-readable name (e.g., "Vortex VX500 Engine ECU")
    pub name: String,
    /// Security secret for seed-key computation (hex string, e.g. "cc").
    /// Used internally by the app during flash — never exposed to external callers.
    #[serde(default)]
    pub secret: Option<String>,
    /// Parameter definitions exposed by this ECU
    #[serde(default)]
    pub parameters: Vec<ParameterDef>,
    /// Operation definitions exposed by this ECU
    #[serde(default)]
    pub operations: Vec<OperationConfig>,
    /// Output (I/O control) definitions exposed by this ECU
    #[serde(default)]
    pub outputs: Vec<OutputConfig>,
}

/// Top-level example app configuration
#[derive(Debug, Deserialize, Default)]
pub struct ExampleAppConfig {
    /// Output (I/O control) definitions exposed by this app entity
    #[serde(default)]
    pub outputs: Vec<OutputConfig>,
    /// Parameter definitions exposed by this app entity
    #[serde(default)]
    pub parameters: Vec<ParameterDef>,
    /// Operation definitions exposed by this app entity
    #[serde(default)]
    pub operations: Vec<OperationConfig>,
    /// Managed ECU sub-entity configuration (new format)
    #[serde(default)]
    pub managed_ecu: Option<ManagedEcuConfig>,
}

impl ExampleAppConfig {
    /// Load configuration from a TOML file
    pub fn load(path: &str) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read config file '{}': {}", path, e))?;
        toml::from_str(&content)
            .map_err(|e| format!("Failed to parse config file '{}': {}", path, e))
    }

    /// Normalize the config for backward compatibility.
    ///
    /// If top-level `[[parameters]]`, `[[operations]]`, or `[[outputs]]` exist
    /// but `[managed_ecu]` doesn't, migrate them into a `managed_ecu` section
    /// with a deprecation warning.  The `upstream_component` is used as the
    /// default ECU ID.
    pub fn normalize(&mut self, upstream_component: &str) {
        let has_top_level =
            !self.parameters.is_empty() || !self.operations.is_empty() || !self.outputs.is_empty();

        if has_top_level && self.managed_ecu.is_none() {
            tracing::warn!(
                "Deprecated: top-level [[parameters]], [[operations]], [[outputs]] in config. \
                 Migrate to [managed_ecu] section. Auto-migrating with id='{}'.",
                upstream_component
            );

            self.managed_ecu = Some(ManagedEcuConfig {
                id: upstream_component.to_string(),
                name: format!("Managed ECU ({})", upstream_component),
                secret: None,
                parameters: std::mem::take(&mut self.parameters),
                operations: std::mem::take(&mut self.operations),
                outputs: std::mem::take(&mut self.outputs),
            });
        }
    }
}
