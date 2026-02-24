//! UDS backend configuration
//!
//! This module contains configuration types for the UDS backend,
//! including transport, parameters, operations, and session settings.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Configuration for a UDS backend
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UdsBackendConfig {
    /// ECU identifier
    pub id: String,
    /// Human-readable name
    pub name: String,
    /// Description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Transport configuration
    pub transport: TransportConfig,
    /// Operation definitions
    #[serde(default)]
    pub operations: Vec<OperationConfig>,
    /// Output (I/O control) definitions
    #[serde(default)]
    pub outputs: Vec<OutputConfig>,
    /// Service ID overrides for OEM variants
    #[serde(default)]
    pub service_overrides: ServiceOverrides,
    /// Session configuration
    #[serde(default)]
    pub sessions: SessionConfig,
    /// Flash commit/rollback configuration
    #[serde(default)]
    pub flash_commit: FlashCommitConfig,
}

/// Flash commit/rollback configuration for A/B bank firmware updates
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FlashCommitConfig {
    /// Whether this ECU supports firmware rollback
    #[serde(default)]
    pub supports_rollback: bool,
    /// UDS Routine ID for commit (e.g., "0xFF01")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_routine: Option<String>,
    /// UDS Routine ID for rollback (e.g., "0xFF02")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rollback_routine: Option<String>,
}

// =============================================================================
// Transport Configuration
// =============================================================================

/// Transport configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum TransportConfig {
    /// SocketCAN with ISO-TP (Linux only)
    SocketCan(SocketCanConfig),
    /// DoIP (Diagnostics over IP) - ISO 13400
    DoIp(DoIpConfig),
    /// Mock transport for testing
    Mock(MockConfig),
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self::Mock(MockConfig::default())
    }
}

/// SocketCAN configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocketCanConfig {
    /// CAN interface name (e.g., "can0")
    pub interface: String,
    /// CAN bus bitrate
    #[serde(default = "default_bitrate")]
    pub bitrate: u32,
    /// ISO-TP configuration
    pub isotp: IsoTpConfig,
}

fn default_bitrate() -> u32 {
    500000
}

/// ISO-TP addressing and options
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IsoTpConfig {
    /// Transmit CAN ID (tester -> ECU)
    pub tx_id: String,
    /// Receive CAN ID (ECU -> tester)
    pub rx_id: String,
    /// TX padding byte value
    #[serde(default = "default_padding")]
    pub tx_padding: u8,
    /// RX padding byte value
    #[serde(default = "default_padding")]
    pub rx_padding: u8,
    /// Block size for flow control
    #[serde(default)]
    pub block_size: u8,
    /// Separation time minimum (microseconds)
    #[serde(default)]
    pub st_min_us: u32,
    /// TX data length
    #[serde(default = "default_tx_dl")]
    pub tx_dl: u8,
}

fn default_padding() -> u8 {
    0xCC
}

fn default_tx_dl() -> u8 {
    8
}

/// DoIP (Diagnostics over IP) configuration per ISO 13400
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoIpConfig {
    /// Gateway IP address or hostname
    pub gateway_host: String,
    /// Gateway TCP port (default: 13400)
    #[serde(default = "default_doip_port")]
    pub gateway_port: u16,
    /// DoIP source address (tester logical address, e.g., 0x0E80)
    pub source_address: u16,
    /// DoIP target address (ECU logical address)
    pub target_address: u16,
    /// Activation type (default: 0x00)
    #[serde(default)]
    pub activation_type: u8,
    /// Connection timeout in milliseconds
    #[serde(default = "default_doip_connect_timeout")]
    pub connect_timeout_ms: u64,
    /// Routing activation timeout in milliseconds
    #[serde(default = "default_doip_activation_timeout")]
    pub activation_timeout_ms: u64,
    /// Response timeout in milliseconds
    #[serde(default = "default_doip_response_timeout")]
    pub response_timeout_ms: u64,
    /// Keep-alive interval in seconds (0 to disable)
    #[serde(default = "default_doip_keepalive")]
    pub keepalive_interval_secs: u64,
    /// Enable vehicle discovery via UDP broadcast
    #[serde(default)]
    pub auto_discover: bool,
    /// UDP discovery port (default: 13400)
    #[serde(default = "default_doip_port")]
    pub discovery_port: u16,
}

fn default_doip_port() -> u16 {
    13400
}

fn default_doip_connect_timeout() -> u64 {
    5000
}

fn default_doip_activation_timeout() -> u64 {
    2000
}

fn default_doip_response_timeout() -> u64 {
    5000
}

fn default_doip_keepalive() -> u64 {
    30
}

/// Mock transport configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MockConfig {
    /// Simulated latency in milliseconds
    #[serde(default)]
    pub latency_ms: u64,
}

// =============================================================================
// Parameter Configuration
// =============================================================================

/// Parameter configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParameterConfig {
    /// Parameter identifier
    pub id: String,
    /// Human-readable name
    pub name: String,
    /// UDS Data Identifier (DID) in hex format (e.g., "0xF190")
    pub did: String,
    /// Data type
    #[serde(default)]
    pub data_type: DataType,
    /// Byte length (optional, inferred from data_type if not specified)
    #[serde(default)]
    pub byte_length: Option<usize>,
    /// Unit of measurement
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// Scale factor (physical = raw * scale + offset)
    #[serde(default = "default_scale")]
    pub scale: f64,
    /// Offset (physical = raw * scale + offset)
    #[serde(default)]
    pub offset: f64,
    /// Whether this parameter is writable
    #[serde(default)]
    pub writable: bool,
    /// Required security level (0 = none)
    #[serde(default)]
    pub security_level: u8,
}

fn default_scale() -> f64 {
    1.0
}

/// Data types for parameters
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DataType {
    #[default]
    Uint8,
    Uint16,
    Uint32,
    Int8,
    Int16,
    Int32,
    Float,
    String,
    Bytes,
}

impl std::fmt::Display for DataType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            DataType::Uint8 => "uint8",
            DataType::Uint16 => "uint16",
            DataType::Uint32 => "uint32",
            DataType::Int8 => "int8",
            DataType::Int16 => "int16",
            DataType::Int32 => "int32",
            DataType::Float => "float",
            DataType::String => "string",
            DataType::Bytes => "bytes",
        };
        f.write_str(s)
    }
}

impl DataType {
    /// Get the byte size for this data type (None for variable-length types)
    pub fn byte_size(&self) -> Option<usize> {
        match self {
            DataType::Uint8 | DataType::Int8 => Some(1),
            DataType::Uint16 | DataType::Int16 => Some(2),
            DataType::Uint32 | DataType::Int32 | DataType::Float => Some(4),
            DataType::String | DataType::Bytes => None, // Variable length
        }
    }
}

// =============================================================================
// Operation Configuration
// =============================================================================

/// Operation (routine) configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationConfig {
    /// Operation identifier
    pub id: String,
    /// Human-readable name
    pub name: String,
    /// UDS Routine ID (RID) in hex format (e.g., "0xFF00")
    pub rid: String,
    /// Description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Required security level
    #[serde(default)]
    pub security_level: u8,
}

// =============================================================================
// Output (I/O Control) Configuration
// =============================================================================

/// Output (I/O control) configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputConfig {
    /// Output identifier
    pub id: String,
    /// Human-readable name
    pub name: String,
    /// UDS Output ID (IOID) in hex format (e.g., "0xF000")
    pub ioid: String,
    /// Default value (hex string)
    #[serde(default)]
    pub default_value: String,
    /// Description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Required security level
    #[serde(default)]
    pub security_level: u8,
    /// Data type for typed value conversion
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_type: Option<DataType>,
    /// Unit of measurement
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// Scale factor (physical = raw * scale + offset)
    #[serde(default = "default_scale")]
    pub scale: f64,
    /// Offset (physical = raw * scale + offset)
    #[serde(default)]
    pub offset: f64,
    /// Minimum allowed physical value
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    /// Maximum allowed physical value
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// Allowed string values for enum-like outputs (index maps to raw integer value)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed: Vec<String>,
}

// =============================================================================
// Session Configuration
// =============================================================================

/// Session configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    /// Tester present interval in milliseconds
    #[serde(default = "default_tester_present_interval")]
    pub tester_present_interval_ms: u64,
    /// Whether to suppress positive response for tester present
    #[serde(default = "default_true")]
    pub tester_present_suppress_response: bool,
    /// Custom session types (e.g., "telematics" = 0x40)
    #[serde(default)]
    pub custom_sessions: HashMap<String, u8>,
    /// TransferData block counter start (0 or 1)
    #[serde(default = "default_block_counter_start")]
    pub transfer_data_block_counter_start: u8,
    /// TransferData block counter wrap (what to use after 255, typically same as start)
    #[serde(default = "default_block_counter_wrap")]
    pub transfer_data_block_counter_wrap: u8,
    /// Default session sub-function (0x01)
    #[serde(default = "default_session")]
    pub default_session: u8,
    /// Programming session sub-function (0x02)
    #[serde(default = "programming_session")]
    pub programming_session: u8,
    /// Extended session sub-function (0x03)
    #[serde(default = "extended_session")]
    pub extended_session: u8,
    /// Engineering session sub-function (0x60)
    #[serde(default = "engineering_session")]
    pub engineering_session: u8,
    /// Security access configuration
    #[serde(default)]
    pub security: Option<SecurityConfig>,
    /// Keepalive configuration
    #[serde(default)]
    pub keepalive: KeepaliveConfig,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            tester_present_interval_ms: default_tester_present_interval(),
            tester_present_suppress_response: default_true(),
            custom_sessions: HashMap::new(),
            transfer_data_block_counter_start: default_block_counter_start(),
            transfer_data_block_counter_wrap: default_block_counter_wrap(),
            default_session: default_session(),
            programming_session: programming_session(),
            extended_session: extended_session(),
            engineering_session: engineering_session(),
            security: None,
            keepalive: KeepaliveConfig::default(),
        }
    }
}

fn default_tester_present_interval() -> u64 {
    2000
}

fn default_true() -> bool {
    true
}

fn default_block_counter_start() -> u8 {
    1
}

fn default_block_counter_wrap() -> u8 {
    1
}

fn default_session() -> u8 {
    0x01
}

fn programming_session() -> u8 {
    0x02
}

fn extended_session() -> u8 {
    0x03
}

fn engineering_session() -> u8 {
    0x60
}

/// Security access configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    pub enabled: bool,
    pub level: u8,
}

/// Keepalive configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeepaliveConfig {
    #[serde(default = "default_keepalive_enabled")]
    pub enabled: bool,
    #[serde(default = "default_keepalive_interval")]
    pub interval_ms: u64,
    #[serde(default = "default_suppress_response")]
    pub suppress_response: bool,
}

fn default_keepalive_enabled() -> bool {
    true
}

fn default_keepalive_interval() -> u64 {
    2000
}

fn default_suppress_response() -> bool {
    true
}

impl Default for KeepaliveConfig {
    fn default() -> Self {
        Self {
            enabled: default_keepalive_enabled(),
            interval_ms: default_keepalive_interval(),
            suppress_response: default_suppress_response(),
        }
    }
}

// =============================================================================
// Service Overrides
// =============================================================================

/// Service ID overrides for OEM variants
///
/// Some manufacturers (e.g., Vortex Motors) use non-standard service IDs.
/// This configuration allows mapping standard SOVD operations to
/// manufacturer-specific service IDs.
///
/// # Example (Vortex Motors)
/// ```yaml
/// service_overrides:
///   dynamically_define_data_id: 0xBA   # Standard: 0x2C
///   read_data_by_periodic_id: 0xBB     # Standard: 0x2A
///   write_data_by_id: 0xBC             # Standard: 0x2E
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServiceOverrides {
    /// DiagnosticSessionControl (standard: 0x10)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic_session_control: Option<u8>,
    /// ECUReset (standard: 0x11)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ecu_reset: Option<u8>,
    /// ClearDiagnosticInformation (standard: 0x14)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clear_diagnostic_info: Option<u8>,
    /// ReadDTCInformation (standard: 0x19)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_dtc_info: Option<u8>,
    /// ReadDataByIdentifier (standard: 0x22)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_data_by_id: Option<u8>,
    /// SecurityAccess (standard: 0x27)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub security_access: Option<u8>,
    /// ReadDataByPeriodicIdentifier (standard: 0x2A)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_data_by_periodic_id: Option<u8>,
    /// DynamicallyDefineDataIdentifier (standard: 0x2C)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamically_define_data_id: Option<u8>,
    /// WriteDataByIdentifier (standard: 0x2E)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub write_data_by_id: Option<u8>,
    /// InputOutputControlById (standard: 0x2F)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub io_control_by_id: Option<u8>,
    /// RoutineControl (standard: 0x31)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routine_control: Option<u8>,
    /// RequestDownload (standard: 0x34)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_download: Option<u8>,
    /// RequestUpload (standard: 0x35)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_upload: Option<u8>,
    /// TransferData (standard: 0x36)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transfer_data: Option<u8>,
    /// RequestTransferExit (standard: 0x37)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_transfer_exit: Option<u8>,
    /// TesterPresent (standard: 0x3E)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tester_present: Option<u8>,
    /// LinkControl (standard: 0x87)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_control: Option<u8>,
}
