//! Request and response types for SOVD client

use serde::{Deserialize, Serialize};

// =============================================================================
// Component Types
// =============================================================================

/// Component information returned by the server
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Component {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "type")]
    pub component_type: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub href: Option<String>,
    /// Component capabilities (returned by detail endpoint)
    #[serde(default)]
    pub capabilities: Option<ComponentCapabilities>,
    /// Data endpoint URL
    #[serde(default)]
    pub data: Option<String>,
    /// Faults endpoint URL
    #[serde(default)]
    pub faults: Option<String>,
    /// Operations endpoint URL
    #[serde(default)]
    pub operations: Option<String>,
    /// Logs endpoint URL (if supported)
    #[serde(default)]
    pub logs: Option<String>,
    /// Sub-entities/apps endpoint URL (if supported)
    #[serde(default)]
    pub apps: Option<String>,
}

/// Component capabilities
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ComponentCapabilities {
    #[serde(default)]
    pub read_data: bool,
    #[serde(default)]
    pub write_data: bool,
    #[serde(default)]
    pub faults: bool,
    #[serde(default)]
    pub clear_faults: bool,
    #[serde(default)]
    pub logs: bool,
    #[serde(default)]
    pub operations: bool,
    #[serde(default)]
    pub software_update: bool,
    #[serde(default)]
    pub io_control: bool,
    #[serde(default)]
    pub sessions: bool,
    #[serde(default)]
    pub security: bool,
    #[serde(default)]
    pub sub_entities: bool,
    #[serde(default)]
    pub subscriptions: bool,
}

/// List of components response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentList {
    pub items: Vec<Component>,
}

// =============================================================================
// Data/Parameter Types
// =============================================================================

/// Parameter/DID information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParameterInfo {
    /// SOVD-compliant parameter identifier (semantic name or DID fallback)
    /// Use this in API calls: /data/{id}
    pub id: String,
    /// DID in hex format (for UDS debugging)
    pub did: String,
    /// Display name
    #[serde(default)]
    pub name: Option<String>,
    /// Data type (e.g., "uint8", "uint16")
    #[serde(default)]
    pub data_type: Option<String>,
    /// Unit
    #[serde(default)]
    pub unit: Option<String>,
    /// Whether this parameter supports writing
    #[serde(default)]
    pub writable: bool,
    /// API endpoint
    pub href: String,
}

/// Parameters list response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParametersResponse {
    pub count: usize,
    pub items: Vec<ParameterInfo>,
}

/// Data read response (DID response format)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataResponse {
    /// DID in hex format
    pub did: Option<String>,
    /// Decoded value (or raw hex if no conversion)
    pub value: serde_json::Value,
    /// Unit (if conversion registered)
    #[serde(default)]
    pub unit: Option<String>,
    /// Raw hex bytes
    #[serde(default)]
    pub raw: Option<String>,
    /// Byte length
    #[serde(default)]
    pub length: Option<usize>,
    /// Whether conversion was applied
    #[serde(default)]
    pub converted: Option<bool>,
    /// Timestamp (milliseconds)
    #[serde(default)]
    pub timestamp: Option<i64>,
}

impl DataResponse {
    /// Get raw bytes from the response
    ///
    /// Useful for client-side conversion of private data when the server
    /// doesn't have the DID definition.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let response = client.read_data_raw("ecu", "F405").await?;
    /// let raw_bytes = response.raw_bytes()?;
    /// // Apply your own conversion: value = raw * scale + offset
    /// let temp = raw_bytes[0] as f64 * 1.0 + (-40.0);
    /// ```
    pub fn raw_bytes(&self) -> Result<Vec<u8>, String> {
        self.raw
            .as_ref()
            .ok_or_else(|| "No raw bytes in response".to_string())
            .and_then(|hex_str| {
                hex::decode(hex_str).map_err(|e| format!("Invalid hex in raw field: {}", e))
            })
    }

    /// Check if this response has server-side conversion applied
    pub fn is_converted(&self) -> bool {
        self.converted.unwrap_or(false)
    }

    /// Get value as f64 (for numeric values)
    pub fn as_f64(&self) -> Option<f64> {
        self.value.as_f64()
    }

    /// Get value as i64 (for integer values)
    pub fn as_i64(&self) -> Option<i64> {
        self.value.as_i64()
    }

    /// Get value as string
    pub fn as_str(&self) -> Option<&str> {
        self.value.as_str()
    }
}

/// Multiple data values response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataListResponse {
    pub data: Vec<DataResponse>,
}

/// Write data request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteDataRequest {
    pub value: serde_json::Value,
}

// =============================================================================
// Fault Types
// =============================================================================

/// Fault/DTC information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaultInfo {
    pub id: String,
    #[serde(alias = "dtc_code")]
    pub code: String,
    pub message: String,
    pub severity: String,
    #[serde(default)]
    pub category: Option<String>,
    pub active: bool,
    pub href: String,
}

/// Faults list response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaultsResponse {
    pub items: Vec<FaultInfo>,
    #[serde(default)]
    pub total_count: usize,
}

/// Clear faults response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClearFaultsResponse {
    pub success: bool,
    #[serde(default)]
    pub cleared_count: Option<u32>,
    #[serde(default)]
    pub message: Option<String>,
}

// =============================================================================
// Operation Types
// =============================================================================

/// Operation information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationInfo {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub requires_security: bool,
    #[serde(default)]
    pub security_level: u8,
    pub href: String,
}

/// Operations list response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationsResponse {
    pub items: Vec<OperationInfo>,
}

/// Operation execution request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationRequest {
    /// Action: "start", "stop", or "result"
    #[serde(default = "default_action")]
    pub action: String,
    /// Optional parameters (hex string)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<String>,
}

fn default_action() -> String {
    "start".to_string()
}

/// Status of an operation execution
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    /// Operation is pending/queued
    Pending,
    /// Operation is currently running
    Running,
    /// Operation completed successfully
    Completed,
    /// Operation failed
    Failed,
    /// Operation was cancelled
    Cancelled,
}

impl std::fmt::Display for OperationStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OperationStatus::Pending => write!(f, "pending"),
            OperationStatus::Running => write!(f, "running"),
            OperationStatus::Completed => write!(f, "completed"),
            OperationStatus::Failed => write!(f, "failed"),
            OperationStatus::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// Operation execution response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationResponse {
    pub operation_id: String,
    pub action: String,
    pub status: OperationStatus,
    #[serde(default)]
    pub result_data: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    pub timestamp: i64,
}

// =============================================================================
// Log Types (for message passing pattern and HPC logs)
// =============================================================================

/// Log entry - supports both text logs and binary dumps
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    /// Unique identifier for this log entry
    pub id: String,
    /// Timestamp of the log entry
    pub timestamp: String,
    /// Log priority/level
    #[serde(alias = "level")]
    pub priority: String,
    /// Log message content
    pub message: String,
    /// Source of the log (service name, container, etc.)
    #[serde(default)]
    pub source: Option<String>,
    /// Process/PID that generated the log
    #[serde(default)]
    pub pid: Option<u32>,
    /// Log type for categorization (e.g., "engine_dump", "diagnostic")
    #[serde(default, rename = "type")]
    pub log_type: Option<String>,
    /// Size of content in bytes (for binary logs)
    #[serde(default)]
    pub size: Option<u64>,
    /// Retrieval status (pending, retrieved, processed)
    #[serde(default)]
    pub status: Option<String>,
    /// URL to download content (for large binary data)
    #[serde(default)]
    pub href: Option<String>,
    /// Additional metadata (trigger, fault codes, etc.)
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

/// Logs response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogsResponse {
    /// Log entries (server returns "items")
    #[serde(alias = "logs")]
    pub items: Vec<LogEntry>,
    /// Total count of logs
    #[serde(default, alias = "total")]
    pub total_count: Option<usize>,
}

/// Log filter for querying logs
#[derive(Debug, Clone, Default, Serialize)]
pub struct LogFilter {
    /// Filter by log type (e.g., "engine_dump", "diagnostic")
    #[serde(skip_serializing_if = "Option::is_none", rename = "type")]
    pub log_type: Option<String>,
    /// Filter by retrieval status (pending, retrieved)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Filter by priority level
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    /// Filter by source
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Maximum number of entries to return
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

// =============================================================================
// Sub-entity (App) Types
// =============================================================================

/// App/sub-entity information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppInfo {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default, rename = "type")]
    pub app_type: Option<String>,
    #[serde(default)]
    pub href: Option<String>,
    /// Capabilities (present in detail responses, absent in list responses)
    #[serde(default)]
    pub capabilities: Option<ComponentCapabilities>,
}

/// Apps list response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppsResponse {
    pub items: Vec<AppInfo>,
}

// =============================================================================
// Subscription/Stream Types
// =============================================================================

/// Subscription request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscriptionRequest {
    pub parameters: Vec<String>,
    #[serde(default = "default_rate")]
    pub rate_hz: u32,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub duration_secs: Option<u64>,
}

fn default_rate() -> u32 {
    1
}

/// Subscription response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscriptionResponse {
    pub subscription_id: String,
    pub stream_url: String,
}

// =============================================================================
// Admin/Definition Types
// =============================================================================

/// DID definition info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefinitionInfo {
    /// SOVD-compliant semantic identifier
    pub id: String,
    /// DID in hex format
    pub did: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, rename = "type")]
    pub data_type: Option<String>,
    #[serde(default)]
    pub scale: Option<f64>,
    #[serde(default)]
    pub offset: Option<f64>,
    #[serde(default)]
    pub unit: Option<String>,
}

/// Definitions list response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefinitionsResponse {
    pub count: usize,
    pub dids: Vec<DefinitionInfo>,
    #[serde(default)]
    pub meta: Option<serde_json::Value>,
}

/// Upload definitions response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadDefinitionsResponse {
    pub status: String,
    pub loaded: usize,
}

// =============================================================================
// Error Types
// =============================================================================

/// Error response from server
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub details: Option<serde_json::Value>,
}

// =============================================================================
// Session/Security Types
// =============================================================================

/// UDS session types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionType {
    Default,
    Extended,
    Programming,
    Engineering,
}

impl SessionType {
    /// Get the UDS session byte value
    pub fn as_uds_byte(&self) -> u8 {
        match self {
            SessionType::Default => 0x01,
            SessionType::Programming => 0x02,
            SessionType::Extended => 0x03,
            SessionType::Engineering => 0x60,
        }
    }

    /// Get the session name for the API
    pub fn as_name(&self) -> &'static str {
        match self {
            SessionType::Default => "default",
            SessionType::Programming => "programming",
            SessionType::Extended => "extended",
            SessionType::Engineering => "engineering",
        }
    }
}

impl std::fmt::Display for SessionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_name())
    }
}

impl std::str::FromStr for SessionType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "default" => Ok(SessionType::Default),
            "programming" => Ok(SessionType::Programming),
            "extended" => Ok(SessionType::Extended),
            "engineering" => Ok(SessionType::Engineering),
            _ => Err(format!("Invalid session type: {}", s)),
        }
    }
}

/// Security access level
///
/// In UDS, security access uses odd numbers for seed requests
/// and even numbers (odd + 1) for key responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityLevel(pub u8);

impl SecurityLevel {
    /// Security level 1 (seed request 0x01, key send 0x02)
    pub const LEVEL_1: SecurityLevel = SecurityLevel(0x01);
    /// Security level 2 (seed request 0x03, key send 0x04)
    pub const LEVEL_3: SecurityLevel = SecurityLevel(0x03);
    /// Programming security (seed request 0x11, key send 0x12)
    pub const PROGRAMMING: SecurityLevel = SecurityLevel(0x11);

    /// Get the seed request sub-function (odd number)
    pub fn seed_request(&self) -> u8 {
        self.0
    }

    /// Get the key send sub-function (even number = seed + 1)
    pub fn key_send(&self) -> u8 {
        self.0 + 1
    }

    /// Get the level number (1-based) for the API
    pub fn as_level_number(&self) -> u8 {
        // Level 1 = 0x01, Level 2 = 0x03, etc.
        self.0.div_ceil(2)
    }
}

// =============================================================================
// Health Check
// =============================================================================

/// Health check response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub uptime_secs: Option<u64>,
}

// =============================================================================
// Software Download (UDS 0x34/0x36/0x37)
// =============================================================================

/// Response from starting a download session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartDownloadResponse {
    /// Unique session identifier
    pub session_id: String,
    /// Maximum bytes per transfer block
    pub max_block_size: u32,
    /// Expected number of blocks
    #[serde(default)]
    pub expected_blocks: u32,
    /// URL for transferring data blocks
    #[serde(default)]
    pub transfer_url: Option<String>,
    /// URL for finalizing the download
    #[serde(default)]
    pub finalize_url: Option<String>,
}

/// Response from transferring a data block
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferDataResponse {
    /// Current block counter
    pub block_counter: u8,
    /// Total bytes transferred so far
    pub bytes_transferred: u32,
    /// Remaining bytes to transfer
    #[serde(default)]
    pub remaining_bytes: u32,
    /// Progress percentage (0-100)
    #[serde(default)]
    pub progress_percent: f64,
}

/// Response from finalizing a download session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinalizeDownloadResponse {
    /// Whether the download completed successfully
    pub success: bool,
    /// Total bytes transferred
    #[serde(default)]
    pub total_bytes: u32,
    /// Number of blocks transferred
    #[serde(default)]
    pub blocks_transferred: u32,
    /// Duration in milliseconds
    #[serde(default)]
    pub duration_ms: u64,
    /// CRC32 checksum (hex string)
    #[serde(default)]
    pub crc32: Option<String>,
}

/// Response from ECU reset
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EcuResetResponse {
    /// Whether the reset was initiated successfully
    pub success: bool,
    /// Type of reset performed ("hard", "soft", "key_off_on", "custom")
    pub reset_type: String,
    /// Human-readable message
    pub message: String,
    /// Power-down time in seconds (if provided by ECU)
    #[serde(default)]
    pub power_down_time: Option<u8>,
}

// =============================================================================
// Mode Types
// =============================================================================

/// Mode response (generic for session, security, link, etc.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModeResponse {
    /// Mode identifier
    pub id: String,
    /// Human-readable name
    #[serde(default)]
    pub name: Option<String>,
    /// Current value
    #[serde(default)]
    pub value: Option<serde_json::Value>,
    /// Seed for security access (if applicable)
    #[serde(default)]
    pub seed: Option<serde_json::Value>,
    /// Human-readable description
    #[serde(default)]
    pub description: Option<String>,
    /// Error message (if failed)
    #[serde(default)]
    pub error: Option<String>,
    /// Current baud rate (for link mode)
    #[serde(default)]
    pub current_baud_rate: Option<u32>,
    /// Link state (for link mode)
    #[serde(default)]
    pub link_state: Option<String>,
}

// =============================================================================
// Output/I/O Control Types
// =============================================================================

/// Output/actuator information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputInfo {
    /// Output identifier
    pub id: String,
    /// Display name
    #[serde(default)]
    pub name: Option<String>,
    /// Description
    #[serde(default)]
    pub description: Option<String>,
    /// Data type
    #[serde(default)]
    pub data_type: Option<String>,
    /// Supported control types
    #[serde(default)]
    pub control_types: Vec<String>,
    /// API endpoint
    #[serde(default)]
    pub href: Option<String>,
    /// Current raw value (hex string, e.g. "00")
    #[serde(default)]
    pub current_value: Option<String>,
    /// Default raw value (hex string, e.g. "00")
    #[serde(default)]
    pub default_value: Option<String>,
    /// Current value label (e.g. "off") or number
    #[serde(default)]
    pub value: Option<serde_json::Value>,
    /// Default value label (e.g. "off") or number
    #[serde(default)]
    pub default: Option<serde_json::Value>,
    /// Allowed value labels (e.g. ["off", "slow", "fast"])
    #[serde(default)]
    pub allowed: Option<Vec<serde_json::Value>>,
    /// Whether the output is currently controlled by a tester
    #[serde(default)]
    pub controlled_by_tester: Option<bool>,
    /// Whether the output is currently frozen
    #[serde(default)]
    pub frozen: Option<bool>,
    /// Whether the output requires security unlock
    #[serde(default)]
    pub requires_security: Option<bool>,
    /// Required security level (0 = none)
    #[serde(default)]
    pub security_level: Option<u8>,
}

/// Outputs list response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputsResponse {
    pub items: Vec<OutputInfo>,
}

/// Output control response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputControlResponse {
    /// Output identifier
    pub output_id: String,
    /// Action that was performed
    pub action: String,
    /// Whether the action succeeded
    pub success: bool,
    /// Whether the output is controlled by tester
    pub controlled_by_tester: bool,
    /// Whether the output is frozen
    pub frozen: bool,
    /// New value (if applicable, hex string)
    #[serde(default)]
    pub new_value: Option<String>,
    /// Typed new value (decoded from raw bytes using type metadata)
    #[serde(default)]
    pub value: Option<serde_json::Value>,
    /// Error message (if failed)
    #[serde(default)]
    pub error: Option<String>,
}

// =============================================================================
// Subscription List Response
// =============================================================================

/// Subscription list response (component-level)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscriptionListResponse {
    pub subscriptions: Vec<SubscriptionResponse>,
}

// =============================================================================
// Global Subscription Types (flat namespace)
// =============================================================================

/// Global subscription response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalSubscriptionResponse {
    pub subscription_id: String,
    /// Stream URL for connecting to the subscription
    pub stream_url: String,
    /// Component ID
    #[serde(default)]
    pub component_id: Option<String>,
    /// Parameters
    #[serde(default)]
    pub parameters: Option<Vec<String>>,
    #[serde(default)]
    pub rate_hz: Option<u32>,
    #[serde(default)]
    pub status: Option<String>,
}

/// Global subscription list response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalSubscriptionListResponse {
    pub items: Vec<GlobalSubscriptionResponse>,
}

// =============================================================================
// Dynamic Data Identifier Types
// =============================================================================

/// Source DID for dynamic data definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataDefinitionSource {
    /// Source DID in hex format
    pub did: String,
    /// Byte position in source DID (1-based, serializes as "position" for server)
    #[serde(default, rename = "position", alias = "start_byte")]
    pub start_byte: Option<usize>,
    /// Number of bytes to extract
    #[serde(default)]
    pub size: Option<usize>,
}

/// Data definition response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataDefinitionResponse {
    /// DDID in hex format
    pub ddid: String,
    /// Status message (or status field for backwards compat)
    #[serde(alias = "message", default)]
    pub status: String,
    /// HATEOAS link
    #[serde(default)]
    pub href: Option<String>,
    /// Source DIDs included (legacy)
    #[serde(default)]
    pub source_dids: Vec<String>,
    /// Error message (if failed)
    #[serde(default)]
    pub error: Option<String>,
}
