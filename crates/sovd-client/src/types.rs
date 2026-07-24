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
    /// §7.20 bulk-data collection (log-file download / large payloads).
    #[serde(default)]
    pub bulk_data: bool,
}

/// List of components response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentList {
    pub items: Vec<Component>,
}

// =============================================================================
// Bulk-data (SOVD §7.20) — the spec-native large-payload / log-file download.
// =============================================================================

/// `GET /{entity}/bulk-data` — one category.
#[derive(Debug, Clone, Deserialize)]
pub struct BulkCategoryRef {
    pub id: String,
    #[serde(default)]
    pub href: Option<String>,
}

/// `GET /{entity}/bulk-data/{category}` — one downloadable item's metadata.
#[derive(Debug, Clone, Deserialize)]
pub struct BulkItemRef {
    pub id: String,
    #[serde(default)]
    pub size: u64,
    /// RFC 3339 UTC creation/mtime.
    #[serde(default)]
    pub created: Option<String>,
    #[serde(default)]
    pub mime: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub href: Option<String>,
}

/// Envelope for the category list.
#[derive(Debug, Clone, Deserialize)]
pub struct BulkCategoriesResponse {
    #[serde(default)]
    pub items: Vec<BulkCategoryRef>,
}

/// Envelope for the item list.
#[derive(Debug, Clone, Deserialize)]
pub struct BulkItemsResponse {
    #[serde(default)]
    pub items: Vec<BulkItemRef>,
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
    /// ISO 17978-3 §7.9 data category (Table 70 `ValueMetaData.category`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<sovd_core::DataCategory>,
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
    /// Server-side read time, RFC 3339 (ISO 17978-3 C-050).
    #[serde(default)]
    pub timestamp: Option<String>,
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

/// Fault/DTC information per ISO 17978-3 §7.8 Table 61.
/// `severity` is integer 1..4 (1=Critical, 2=Error, 3=Warning, 4=Info).
///
/// The non-spec `id`, `category`, `active` extras were dropped in
/// Phase F.6.  Clients derive "active" from `status.testFailed`; the
/// id is implicit in the resource path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaultInfo {
    pub code: String,
    pub fault_name: String,
    pub severity: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symptom: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fault_translation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symptom_translation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<serde_json::Value>,
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

/// Operation information.
///
/// Carries the spec routine fields plus IO control fields (per
/// ISO 17978-3 C-133: UDS InputOutputControl folds under /operations).
/// IO control fields are `Some(_)` only when the server marked the
/// operation as an output (sets `output_id`).
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

    // i18n
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description_translation_id: Option<String>,

    // IO control (UDS 0x2F) — present only for outputs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_type: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub control_types: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controlled_by_tester: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frozen: Option<bool>,
}

/// Operations list response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationsResponse {
    pub items: Vec<OperationInfo>,
}

/// Operation start request body — ISO 17978-3 §7.14.
///
/// `parameters` is polymorphic: a hex string for RoutineControl (0x31)
/// or a JSON object `{ "action": "...", "value"?: ... }` for
/// InputOutputControl (0x2F).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartExecutionRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
}

/// Status of an operation execution — ISO 17978-3 §7.14 line 387.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    /// Operation is currently running.
    Running,
    /// Operation completed successfully.
    Completed,
    /// Operation failed.
    Failed,
    /// Operation was stopped (UDS RoutineControl 0x31 0x02).
    Stopped,
}

impl std::fmt::Display for OperationStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OperationStatus::Running => write!(f, "running"),
            OperationStatus::Completed => write!(f, "completed"),
            OperationStatus::Failed => write!(f, "failed"),
            OperationStatus::Stopped => write!(f, "stopped"),
        }
    }
}

/// Operation execution resource — ISO 17978-3 §7.14 (mirror of
/// `sovd_core::OperationExecution`).
///
/// The server allocates `execution_id` on POST and returns it in the
/// `Location` header.  Clients re-use it for `GET .../executions/{exec_id}`
/// polls and `DELETE .../executions/{exec_id}` stop calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationExecution {
    pub execution_id: String,
    pub operation_id: String,
    pub status: OperationStatus,
    /// Result payload (if completed) — opaque JSON, schema is operation-defined.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Error message (if status == failed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// RFC 3339 start time.
    pub started_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
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
    /// Opaque cursor for the next page; feed back as `LogFilter::after`. `None`
    /// once the head is reached — a paging loop stops here. Absent when the
    /// backend doesn't paginate.
    #[serde(default)]
    pub next_cursor: Option<String>,
    /// Oldest position still available (gap detection). Absent when unknown.
    #[serde(default)]
    pub oldest_cursor: Option<String>,
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
    /// Opaque pagination cursor — return entries strictly after this position.
    /// From a prior response's `next_cursor`; never constructed by hand.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    /// Lower time bound (RFC 3339). Coarse on the host tier (file mtime), precise
    /// on journald. Caveat: CVC wall-clock is non-monotonic across reboots — a
    /// cursor is the reliable resume token; time filters are a convenience.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    /// Upper time bound (RFC 3339). Same caveats as `since`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
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
// Cyclic-subscription Types — ISO 17978-3 §7.10
// =============================================================================

/// Spec line 358 — coarse polling cadence.  Server maps these to
/// concrete rates (fast=20 Hz / normal=5 Hz / slow=1 Hz on SOVDd).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubscriptionInterval {
    Fast,
    Normal,
    Slow,
}

/// Request body for `POST .../cyclic-subscriptions`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CyclicSubscriptionRequest {
    /// URI-reference to the subscribed parameter.
    pub resource: String,
    pub interval: SubscriptionInterval,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<u32>,
}

/// Created cyclic subscription (mirror of the server's
/// `CyclicSubscription`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CyclicSubscription {
    pub subscription_id: String,
    pub component_id: String,
    pub resource: String,
    pub interval: SubscriptionInterval,
    pub protocol: String,
    pub status: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

/// List response for `GET .../cyclic-subscriptions`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CyclicSubscriptionsResponse {
    pub items: Vec<CyclicSubscription>,
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

/// Error response from server — ISO 17978-3 §5.8.3 (`GenericError`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error_code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translation_id: Option<String>,
    #[serde(default)]
    pub parameters: std::collections::BTreeMap<String, Vec<String>>,
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

/// Response from ECU reset — ISO 17978-3 §7.19 execution shape.
///
/// Server returns 202 + `Location` header to a status sub-resource;
/// this body lives at that sub-resource too.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EcuResetResponse {
    /// Execution status (always `completed` for reset — fire-and-forget).
    pub status: String,
    /// Server-allocated execution id (also embedded in `href`).
    pub exec_id: String,
    /// Type of reset performed ("hard", "soft", "key_off_on", "custom").
    pub reset_type: String,
    /// Human-readable message.
    pub message: String,
    /// Power-down time in seconds (if provided by ECU).
    #[serde(default)]
    pub power_down_time: Option<u8>,
    /// HATEOAS link to the status sub-resource.
    pub href: String,
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
