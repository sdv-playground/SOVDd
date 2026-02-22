//! Flash client configuration with YAML support

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Flash client configuration
///
/// Can be loaded from YAML, TOML, or constructed programmatically.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlashConfig {
    /// Connection settings
    pub connection: ConnectionConfig,

    /// Endpoint paths (configurable per server)
    #[serde(default)]
    pub endpoints: EndpointsConfig,

    /// Timeout settings
    #[serde(default)]
    pub timeouts: TimeoutsConfig,

    /// Component/ECU identifier (for SOVD paths)
    #[serde(default)]
    pub component_id: Option<String>,

    /// Gateway component ID (for sub-entity SOVD paths)
    /// When set together with component_id, produces sub-entity routes:
    /// `/vehicle/v1/components/{gateway_id}/apps/{component_id}/...`
    #[serde(default)]
    pub gateway_id: Option<String>,
}

/// Connection configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionConfig {
    /// Base URL of the server
    pub base_url: String,

    /// API key for authentication (optional)
    #[serde(default)]
    pub api_key: Option<String>,

    /// API key header name (default: X-API-Key)
    #[serde(default = "default_api_key_header")]
    pub api_key_header: String,
}

fn default_api_key_header() -> String {
    "X-API-Key".to_string()
}

/// Endpoint paths configuration
///
/// Allows different path schemes for different servers:
/// - Container: `/files`, `/flash`
/// - OpenSOVD: `/apps/sovd2uds/bulk-data/flashfiles`, `/components/{ecu}/x-sovd2uds-download`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndpointsConfig {
    /// Base path for file operations
    #[serde(default = "default_files_path")]
    pub files: String,

    /// Base path for flash operations
    #[serde(default = "default_flash_path")]
    pub flash: String,

    /// Detailed endpoint definitions (optional override)
    #[serde(default)]
    pub detail: Option<EndpointDetails>,
}

impl Default for EndpointsConfig {
    fn default() -> Self {
        Self {
            files: default_files_path(),
            flash: default_flash_path(),
            detail: None,
        }
    }
}

fn default_files_path() -> String {
    "/files".to_string()
}

fn default_flash_path() -> String {
    "/flash".to_string()
}

/// Detailed endpoint path definitions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndpointDetails {
    // File operations
    /// List files: GET
    #[serde(default)]
    pub files_list: Option<String>,
    /// Upload file: POST
    #[serde(default)]
    pub files_upload: Option<String>,
    /// Get upload/file status: GET {id}
    #[serde(default)]
    pub files_status: Option<String>,
    /// Verify file: POST {id}/verify
    #[serde(default)]
    pub files_verify: Option<String>,

    // Flash operations
    /// Start flash transfer: POST
    #[serde(default)]
    pub flash_transfer: Option<String>,
    /// Get transfer status: GET {id}
    #[serde(default)]
    pub flash_transfer_status: Option<String>,
    /// Transfer exit: PUT/DELETE
    #[serde(default)]
    pub flash_transfer_exit: Option<String>,
    /// ECU reset: POST
    #[serde(default)]
    pub flash_reset: Option<String>,
}

/// Timeout configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeoutsConfig {
    /// Upload timeout in milliseconds (default: 5 minutes)
    #[serde(default = "default_upload_timeout")]
    pub upload_ms: u64,

    /// Flash poll interval in milliseconds (default: 500ms)
    #[serde(default = "default_poll_interval")]
    pub flash_poll_ms: u64,

    /// General request timeout in milliseconds (default: 30s)
    #[serde(default = "default_request_timeout")]
    pub request_ms: u64,

    /// Connect timeout in milliseconds (default: 10s)
    #[serde(default = "default_connect_timeout")]
    pub connect_ms: u64,
}

impl Default for TimeoutsConfig {
    fn default() -> Self {
        Self {
            upload_ms: default_upload_timeout(),
            flash_poll_ms: default_poll_interval(),
            request_ms: default_request_timeout(),
            connect_ms: default_connect_timeout(),
        }
    }
}

fn default_upload_timeout() -> u64 {
    300_000 // 5 minutes
}

fn default_poll_interval() -> u64 {
    500 // 500ms
}

fn default_request_timeout() -> u64 {
    30_000 // 30 seconds
}

fn default_connect_timeout() -> u64 {
    10_000 // 10 seconds
}

impl FlashConfig {
    /// Load configuration from a YAML file
    pub fn from_yaml_file(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let content = std::fs::read_to_string(path.as_ref())
            .map_err(|e| ConfigError::IoError(e.to_string()))?;
        Self::from_yaml(&content)
    }

    /// Parse configuration from YAML string
    pub fn from_yaml(yaml: &str) -> Result<Self, ConfigError> {
        serde_yaml::from_str(yaml).map_err(|e| ConfigError::ParseError(e.to_string()))
    }

    /// Parse configuration from JSON string
    pub fn from_json(json: &str) -> Result<Self, ConfigError> {
        serde_json::from_str(json).map_err(|e| ConfigError::ParseError(e.to_string()))
    }

    /// Serialize configuration to YAML
    pub fn to_yaml(&self) -> Result<String, ConfigError> {
        serde_yaml::to_string(self).map_err(|e| ConfigError::SerializeError(e.to_string()))
    }

    /// Serialize configuration to JSON (for discovery endpoint)
    pub fn to_json(&self) -> Result<String, ConfigError> {
        serde_json::to_string_pretty(self).map_err(|e| ConfigError::SerializeError(e.to_string()))
    }

    /// Create a builder for programmatic configuration
    pub fn builder(base_url: impl Into<String>) -> FlashConfigBuilder {
        FlashConfigBuilder::new(base_url)
    }

    /// Get the base path prefix (includes component_id for SOVD-style paths)
    ///
    /// When both `gateway_id` and `component_id` are set, produces sub-entity
    /// paths per SOVD §6.5:
    /// `/vehicle/v1/components/{gateway}/apps/{app}`
    fn base_prefix(&self) -> String {
        match (&self.gateway_id, &self.component_id) {
            (Some(gw), Some(app)) => format!(
                "/vehicle/v1/components/{}/apps/{}",
                gw,
                app.replace('/', "%2F")
            ),
            (None, Some(id)) => format!("/vehicle/v1/components/{}", id),
            _ => String::new(),
        }
    }

    /// Get the resolved endpoint path for file listing
    pub fn files_list_path(&self) -> String {
        self.endpoints
            .detail
            .as_ref()
            .and_then(|d| d.files_list.clone())
            .unwrap_or_else(|| format!("{}{}", self.base_prefix(), self.endpoints.files))
    }

    /// Get the resolved endpoint path for file upload
    pub fn files_upload_path(&self) -> String {
        self.endpoints
            .detail
            .as_ref()
            .and_then(|d| d.files_upload.clone())
            .unwrap_or_else(|| format!("{}{}", self.base_prefix(), self.endpoints.files))
    }

    /// Get the resolved endpoint path for file/upload status
    pub fn files_status_path(&self, id: &str) -> String {
        self.endpoints
            .detail
            .as_ref()
            .and_then(|d| d.files_status.clone())
            .map(|p| p.replace("{id}", id))
            .unwrap_or_else(|| format!("{}{}/{}", self.base_prefix(), self.endpoints.files, id))
    }

    /// Get the resolved endpoint path for file verification
    pub fn files_verify_path(&self, id: &str) -> String {
        self.endpoints
            .detail
            .as_ref()
            .and_then(|d| d.files_verify.clone())
            .map(|p| p.replace("{id}", id))
            .unwrap_or_else(|| {
                format!(
                    "{}{}/{}/verify",
                    self.base_prefix(),
                    self.endpoints.files,
                    id
                )
            })
    }

    /// Get the resolved endpoint path for starting flash transfer
    pub fn flash_transfer_path(&self) -> String {
        self.endpoints
            .detail
            .as_ref()
            .and_then(|d| d.flash_transfer.clone())
            .unwrap_or_else(|| format!("{}{}/transfer", self.base_prefix(), self.endpoints.flash))
    }

    /// Get the resolved endpoint path for flash transfer status
    pub fn flash_transfer_status_path(&self, id: &str) -> String {
        self.endpoints
            .detail
            .as_ref()
            .and_then(|d| d.flash_transfer_status.clone())
            .map(|p| p.replace("{id}", id))
            .unwrap_or_else(|| {
                format!(
                    "{}{}/transfer/{}",
                    self.base_prefix(),
                    self.endpoints.flash,
                    id
                )
            })
    }

    /// Get the resolved endpoint path for transfer exit
    pub fn flash_transfer_exit_path(&self) -> String {
        self.endpoints
            .detail
            .as_ref()
            .and_then(|d| d.flash_transfer_exit.clone())
            .unwrap_or_else(|| {
                format!(
                    "{}{}/transferexit",
                    self.base_prefix(),
                    self.endpoints.flash
                )
            })
    }

    /// Get the resolved endpoint path for flash commit
    pub fn flash_commit_path(&self) -> String {
        format!("{}{}/commit", self.base_prefix(), self.endpoints.flash)
    }

    /// Get the resolved endpoint path for flash rollback
    pub fn flash_rollback_path(&self) -> String {
        format!("{}{}/rollback", self.base_prefix(), self.endpoints.flash)
    }

    /// Get the resolved endpoint path for flash activation state
    pub fn flash_activation_path(&self) -> String {
        format!("{}{}/activation", self.base_prefix(), self.endpoints.flash)
    }

    /// Get the resolved endpoint path for ECU reset
    /// Note: SOVD reset is at /vehicle/v1/components/:id/reset, not under /flash/
    pub fn flash_reset_path(&self) -> String {
        self.endpoints
            .detail
            .as_ref()
            .and_then(|d| d.flash_reset.clone())
            .unwrap_or_else(|| format!("{}/reset", self.base_prefix()))
    }
}

/// Builder for FlashConfig
pub struct FlashConfigBuilder {
    config: FlashConfig,
}

impl FlashConfigBuilder {
    /// Create a new builder with the given base URL
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            config: FlashConfig {
                connection: ConnectionConfig {
                    base_url: base_url.into(),
                    api_key: None,
                    api_key_header: default_api_key_header(),
                },
                endpoints: EndpointsConfig::default(),
                timeouts: TimeoutsConfig::default(),
                component_id: None,
                gateway_id: None,
            },
        }
    }

    /// Set the API key
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.config.connection.api_key = Some(key.into());
        self
    }

    /// Set the API key header name
    pub fn api_key_header(mut self, header: impl Into<String>) -> Self {
        self.config.connection.api_key_header = header.into();
        self
    }

    /// Set the files endpoint base path
    pub fn files_path(mut self, path: impl Into<String>) -> Self {
        self.config.endpoints.files = path.into();
        self
    }

    /// Set the flash endpoint base path
    pub fn flash_path(mut self, path: impl Into<String>) -> Self {
        self.config.endpoints.flash = path.into();
        self
    }

    /// Set the component ID (for SOVD-style paths)
    pub fn component_id(mut self, id: impl Into<String>) -> Self {
        self.config.component_id = Some(id.into());
        self
    }

    /// Set the gateway ID (for sub-entity SOVD paths)
    pub fn gateway_id(mut self, id: impl Into<String>) -> Self {
        self.config.gateway_id = Some(id.into());
        self
    }

    /// Set upload timeout in milliseconds
    pub fn upload_timeout_ms(mut self, ms: u64) -> Self {
        self.config.timeouts.upload_ms = ms;
        self
    }

    /// Set flash poll interval in milliseconds
    pub fn flash_poll_ms(mut self, ms: u64) -> Self {
        self.config.timeouts.flash_poll_ms = ms;
        self
    }

    /// Set request timeout in milliseconds
    pub fn request_timeout_ms(mut self, ms: u64) -> Self {
        self.config.timeouts.request_ms = ms;
        self
    }

    /// Build the configuration
    pub fn build(self) -> FlashConfig {
        self.config
    }
}

/// Configuration errors
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("IO error: {0}")]
    IoError(String),

    #[error("Parse error: {0}")]
    ParseError(String),

    #[error("Serialize error: {0}")]
    SerializeError(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_yaml_parsing() {
        let yaml = r#"
connection:
  base_url: "http://localhost:8080"
  api_key: "secret123"

endpoints:
  files: "/files"
  flash: "/flash"

timeouts:
  upload_ms: 60000
  flash_poll_ms: 250
"#;

        let config = FlashConfig::from_yaml(yaml).unwrap();
        assert_eq!(config.connection.base_url, "http://localhost:8080");
        assert_eq!(config.connection.api_key, Some("secret123".to_string()));
        assert_eq!(config.endpoints.files, "/files");
        assert_eq!(config.timeouts.upload_ms, 60000);
        assert_eq!(config.timeouts.flash_poll_ms, 250);
    }

    #[test]
    fn test_builder() {
        let config = FlashConfig::builder("http://localhost:9080")
            .api_key("my-secret")
            .files_path("/apps/sovd2uds/bulk-data/flashfiles")
            .flash_path("/components/ecu/x-sovd2uds-download")
            .upload_timeout_ms(120_000)
            .build();

        assert_eq!(config.connection.base_url, "http://localhost:9080");
        assert_eq!(config.connection.api_key, Some("my-secret".to_string()));
        assert_eq!(
            config.endpoints.files,
            "/apps/sovd2uds/bulk-data/flashfiles"
        );
        assert_eq!(config.timeouts.upload_ms, 120_000);
    }

    #[test]
    fn test_path_resolution() {
        let config = FlashConfig::builder("http://localhost:8080")
            .files_path("/files")
            .flash_path("/flash")
            .build();

        assert_eq!(config.files_list_path(), "/files");
        assert_eq!(config.files_upload_path(), "/files");
        assert_eq!(config.files_status_path("abc123"), "/files/abc123");
        assert_eq!(config.files_verify_path("abc123"), "/files/abc123/verify");
        assert_eq!(config.flash_transfer_path(), "/flash/transfer");
        assert_eq!(
            config.flash_transfer_status_path("xyz"),
            "/flash/transfer/xyz"
        );
        assert_eq!(config.flash_transfer_exit_path(), "/flash/transferexit");
        // Reset is at component level, not under /flash/ (SOVD standard)
        assert_eq!(config.flash_reset_path(), "/reset");

        // With component_id set (SOVD mode)
        let sovd_config = FlashConfig::builder("http://localhost:8080")
            .component_id("vtx_ecm")
            .build();
        assert_eq!(
            sovd_config.flash_reset_path(),
            "/vehicle/v1/components/vtx_ecm/reset"
        );

        // With gateway_id + component_id set (sub-entity SOVD mode)
        let sub_config = FlashConfig::builder("http://localhost:8080")
            .gateway_id("uds_gw")
            .component_id("engine_ecu")
            .build();
        assert_eq!(
            sub_config.files_list_path(),
            "/vehicle/v1/components/uds_gw/apps/engine_ecu/files"
        );
        assert_eq!(
            sub_config.flash_transfer_path(),
            "/vehicle/v1/components/uds_gw/apps/engine_ecu/flash/transfer"
        );
        assert_eq!(
            sub_config.flash_reset_path(),
            "/vehicle/v1/components/uds_gw/apps/engine_ecu/reset"
        );
    }

    #[test]
    fn test_to_yaml() {
        let config = FlashConfig::builder("http://localhost:8080")
            .api_key("test")
            .build();

        let yaml = config.to_yaml().unwrap();
        assert!(yaml.contains("base_url"));
        assert!(yaml.contains("http://localhost:8080"));
    }
}
