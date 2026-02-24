//! Example ECU configuration
//!
//! Fully data-driven configuration for ECU simulation.
//! Supports configurable service IDs for different OEM implementations
//! (e.g., standard UDS vs Vortex Motors-specific service IDs).

use serde::{Deserialize, Serialize};
use sovd_uds::uds::standard_did;

/// Complete example ECU configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EcuConfig {
    /// ECU identifier
    #[serde(default = "default_id")]
    pub id: String,

    /// ECU name
    #[serde(default = "default_name")]
    pub name: String,

    /// Transport configuration
    #[serde(default)]
    pub transport: TransportConfig,

    /// Service ID overrides for OEM-specific implementations
    #[serde(default)]
    pub service_ids: ServiceIdConfig,

    /// Security configuration
    #[serde(default)]
    pub security: SecurityConfig,

    /// Transfer data configuration
    #[serde(default)]
    pub transfer: TransferConfig,

    /// Parameter definitions (DIDs)
    #[serde(default)]
    pub parameters: Vec<ParameterDef>,

    /// DTC definitions
    #[serde(default)]
    pub dtcs: Vec<DtcDef>,

    /// I/O Output definitions
    #[serde(default)]
    pub outputs: Vec<OutputDef>,

    /// Routine definitions
    #[serde(default)]
    pub routines: Vec<RoutineDef>,
}

fn default_id() -> String {
    "example_ecu".to_string()
}

fn default_name() -> String {
    "Example ECU Simulator".to_string()
}

impl Default for EcuConfig {
    fn default() -> Self {
        Self {
            id: default_id(),
            name: default_name(),
            transport: TransportConfig::default(),
            service_ids: ServiceIdConfig::default(),
            security: SecurityConfig::default(),
            transfer: TransferConfig::default(),
            parameters: Vec::new(),
            dtcs: Vec::new(),
            outputs: Vec::new(),
            routines: Vec::new(),
        }
    }
}

impl EcuConfig {
    /// Load configuration from a TOML file
    pub fn load(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        let config: Self = toml::from_str(&content)?;
        Ok(config)
    }

    /// Load configuration from a YAML file
    #[allow(dead_code)]
    pub fn load_yaml(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        let config: Self = serde_yaml::from_str(&content)?;
        Ok(config)
    }

    /// Check if this config has custom definitions or should use defaults
    pub fn has_custom_definitions(&self) -> bool {
        !self.parameters.is_empty() || !self.dtcs.is_empty() || !self.outputs.is_empty()
    }
}

// =============================================================================
// Transport Configuration
// =============================================================================

/// Transport configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransportConfig {
    /// CAN interface
    #[serde(default = "default_interface")]
    pub interface: String,

    /// ECU's receive CAN ID (tester sends to this)
    #[serde(default = "default_rx_id")]
    pub rx_id: String,

    /// ECU's transmit CAN ID (ECU sends from this)
    #[serde(default = "default_tx_id")]
    pub tx_id: String,
}

fn default_interface() -> String {
    "vcan0".to_string()
}

fn default_rx_id() -> String {
    "0x18DA00F1".to_string()
}

fn default_tx_id() -> String {
    "0x18DAF100".to_string()
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            interface: default_interface(),
            rx_id: default_rx_id(),
            tx_id: default_tx_id(),
        }
    }
}

// =============================================================================
// Service ID Configuration (for OEM variants)
// =============================================================================

/// Service ID configuration
///
/// Allows overriding standard UDS service IDs for OEM-specific implementations.
///
/// # Example: Vortex Motors
/// ```toml
/// [service_ids]
/// read_data_by_periodic_id = 0xBB  # Standard: 0x2A
/// dynamically_define_data_id = 0xBA  # Standard: 0x2C
/// write_data_by_id = 0xBC  # Standard: 0x2E
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceIdConfig {
    /// DiagnosticSessionControl (standard: 0x10)
    #[serde(default = "default_diagnostic_session_control")]
    pub diagnostic_session_control: u8,

    /// ECUReset (standard: 0x11)
    #[serde(default = "default_ecu_reset")]
    pub ecu_reset: u8,

    /// ClearDiagnosticInformation (standard: 0x14)
    #[serde(default = "default_clear_diagnostic_info")]
    pub clear_diagnostic_info: u8,

    /// ReadDTCInformation (standard: 0x19)
    #[serde(default = "default_read_dtc_info")]
    pub read_dtc_info: u8,

    /// ReadDataByIdentifier (standard: 0x22)
    #[serde(default = "default_read_data_by_id")]
    pub read_data_by_id: u8,

    /// SecurityAccess (standard: 0x27)
    #[serde(default = "default_security_access")]
    pub security_access: u8,

    /// ReadDataByPeriodicIdentifier (standard: 0x2A, Vortex Motors: 0xBB)
    #[serde(default = "default_read_data_by_periodic_id")]
    pub read_data_by_periodic_id: u8,

    /// DynamicallyDefineDataIdentifier (standard: 0x2C, Vortex Motors: 0xBA)
    #[serde(default = "default_dynamically_define_data_id")]
    pub dynamically_define_data_id: u8,

    /// WriteDataByIdentifier (standard: 0x2E, Vortex Motors: 0xBC)
    #[serde(default = "default_write_data_by_id")]
    pub write_data_by_id: u8,

    /// InputOutputControlById (standard: 0x2F)
    #[serde(default = "default_io_control_by_id")]
    pub io_control_by_id: u8,

    /// RoutineControl (standard: 0x31)
    #[serde(default = "default_routine_control")]
    pub routine_control: u8,

    /// RequestDownload (standard: 0x34)
    #[serde(default = "default_request_download")]
    pub request_download: u8,

    /// RequestUpload (standard: 0x35)
    #[serde(default = "default_request_upload")]
    pub request_upload: u8,

    /// TransferData (standard: 0x36)
    #[serde(default = "default_transfer_data")]
    pub transfer_data: u8,

    /// RequestTransferExit (standard: 0x37)
    #[serde(default = "default_request_transfer_exit")]
    pub request_transfer_exit: u8,

    /// TesterPresent (standard: 0x3E)
    #[serde(default = "default_tester_present")]
    pub tester_present: u8,

    /// LinkControl (standard: 0x87)
    #[serde(default = "default_link_control")]
    pub link_control: u8,
}

// Standard UDS service ID defaults
fn default_diagnostic_session_control() -> u8 {
    0x10
}
fn default_ecu_reset() -> u8 {
    0x11
}
fn default_clear_diagnostic_info() -> u8 {
    0x14
}
fn default_read_dtc_info() -> u8 {
    0x19
}
fn default_read_data_by_id() -> u8 {
    0x22
}
fn default_security_access() -> u8 {
    0x27
}
fn default_read_data_by_periodic_id() -> u8 {
    0x2A
}
fn default_dynamically_define_data_id() -> u8 {
    0x2C
}
fn default_write_data_by_id() -> u8 {
    0x2E
}
fn default_io_control_by_id() -> u8 {
    0x2F
}
fn default_routine_control() -> u8 {
    0x31
}
fn default_request_download() -> u8 {
    0x34
}
fn default_request_upload() -> u8 {
    0x35
}
fn default_transfer_data() -> u8 {
    0x36
}
fn default_request_transfer_exit() -> u8 {
    0x37
}
fn default_tester_present() -> u8 {
    0x3E
}
fn default_link_control() -> u8 {
    0x87
}

impl Default for ServiceIdConfig {
    fn default() -> Self {
        Self {
            diagnostic_session_control: default_diagnostic_session_control(),
            ecu_reset: default_ecu_reset(),
            clear_diagnostic_info: default_clear_diagnostic_info(),
            read_dtc_info: default_read_dtc_info(),
            read_data_by_id: default_read_data_by_id(),
            security_access: default_security_access(),
            read_data_by_periodic_id: default_read_data_by_periodic_id(),
            dynamically_define_data_id: default_dynamically_define_data_id(),
            write_data_by_id: default_write_data_by_id(),
            io_control_by_id: default_io_control_by_id(),
            routine_control: default_routine_control(),
            request_download: default_request_download(),
            request_upload: default_request_upload(),
            transfer_data: default_transfer_data(),
            request_transfer_exit: default_request_transfer_exit(),
            tester_present: default_tester_present(),
            link_control: default_link_control(),
        }
    }
}

impl ServiceIdConfig {
    /// Create Vortex Motors-specific service IDs
    #[allow(dead_code)]
    pub fn vortex() -> Self {
        Self {
            read_data_by_periodic_id: 0xBB,
            dynamically_define_data_id: 0xBA,
            write_data_by_id: 0xBC,
            ..Default::default()
        }
    }

    /// Check if this config uses non-standard service IDs
    pub fn is_non_standard(&self) -> bool {
        self.diagnostic_session_control != 0x10
            || self.ecu_reset != 0x11
            || self.clear_diagnostic_info != 0x14
            || self.read_dtc_info != 0x19
            || self.read_data_by_id != 0x22
            || self.security_access != 0x27
            || self.read_data_by_periodic_id != 0x2A
            || self.dynamically_define_data_id != 0x2C
            || self.write_data_by_id != 0x2E
            || self.io_control_by_id != 0x2F
            || self.routine_control != 0x31
            || self.request_download != 0x34
            || self.request_upload != 0x35
            || self.transfer_data != 0x36
            || self.request_transfer_exit != 0x37
            || self.tester_present != 0x3E
            || self.link_control != 0x87
    }
}

// =============================================================================
// Security Configuration
// =============================================================================

/// Security configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    /// Security access shared secret (hex string)
    #[serde(default = "default_secret")]
    pub secret: String,
}

fn default_secret() -> String {
    "ff".to_string()
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            secret: default_secret(),
        }
    }
}

// =============================================================================
// Transfer Configuration
// =============================================================================

/// Transfer data configuration for UDS 0x36
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferConfig {
    /// Block counter start value (typically 0 or 1)
    #[serde(default = "default_block_counter_start")]
    pub block_counter_start: u8,

    /// Block counter wrap value (what to use after 255)
    #[serde(default = "default_block_counter_wrap")]
    pub block_counter_wrap: u8,
}

fn default_block_counter_start() -> u8 {
    0
}

fn default_block_counter_wrap() -> u8 {
    0
}

impl Default for TransferConfig {
    fn default() -> Self {
        Self {
            block_counter_start: default_block_counter_start(),
            block_counter_wrap: default_block_counter_wrap(),
        }
    }
}

// =============================================================================
// Parameter (DID) Definitions
// =============================================================================

/// Access level for parameters
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AccessLevel {
    /// Readable in default session (0x01), no security needed
    #[default]
    Public,
    /// Requires extended diagnostic session (0x03)
    Extended,
    /// Requires security access (0x27) to be unlocked
    Protected,
}

/// Data type for parameters
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DataType {
    /// Unsigned 8-bit integer
    #[default]
    Uint8,
    /// Unsigned 16-bit integer
    Uint16,
    /// Unsigned 32-bit integer
    Uint32,
    /// Signed 8-bit integer
    Int8,
    /// Signed 16-bit integer
    Int16,
    /// Signed 32-bit integer
    Int32,
    /// ASCII string
    String,
    /// Raw byte array
    Bytes,
}

impl DataType {
    /// Get the byte size for this data type (None for variable-length types)
    #[allow(dead_code)]
    pub fn byte_size(&self) -> Option<usize> {
        match self {
            DataType::Uint8 | DataType::Int8 => Some(1),
            DataType::Uint16 | DataType::Int16 => Some(2),
            DataType::Uint32 | DataType::Int32 => Some(4),
            DataType::String | DataType::Bytes => None,
        }
    }
}

/// Parameter (DID) definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParameterDef {
    /// DID (Data Identifier) - can be hex string "0xF190" or integer
    #[serde(deserialize_with = "deserialize_hex_u16")]
    pub did: u16,

    /// Human-readable identifier (e.g., "engine_rpm", "vin")
    pub id: String,

    /// Data type
    #[serde(default)]
    pub data_type: DataType,

    /// Initial value - interpretation depends on data_type
    /// - For numeric types: the number as string or integer
    /// - For string: the string value
    /// - For bytes: hex string "DEADBEEF" or array [0xDE, 0xAD, ...]
    #[serde(default)]
    pub value: ValueDef,

    /// Minimum value (for numeric types that vary)
    #[serde(default)]
    pub min: Option<u32>,

    /// Maximum value (for numeric types that vary)
    #[serde(default)]
    pub max: Option<u32>,

    /// Whether value varies over time (simulated sensor)
    #[serde(default)]
    pub varies: bool,

    /// Variation percentage (how much the value can change per update)
    #[serde(default = "default_variation")]
    pub variation_percent: u8,

    /// Access level
    #[serde(default)]
    pub access: AccessLevel,

    /// Whether this parameter can be written via WriteDataByIdentifier
    #[serde(default)]
    pub writable: bool,

    /// Optional description
    #[serde(default)]
    pub description: Option<String>,
}

fn default_variation() -> u8 {
    2
}

/// Value definition - flexible to support different formats
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(untagged)]
pub enum ValueDef {
    /// Integer value
    Integer(i64),
    /// String value (for string type or hex bytes)
    String(String),
    /// Byte array
    Bytes(Vec<u8>),
    /// No value specified
    #[default]
    None,
}

impl ValueDef {
    /// Convert to bytes based on data type
    pub fn to_bytes(&self, data_type: &DataType) -> Vec<u8> {
        match (self, data_type) {
            (ValueDef::Integer(n), DataType::Uint8) => vec![*n as u8],
            (ValueDef::Integer(n), DataType::Int8) => vec![*n as i8 as u8],
            (ValueDef::Integer(n), DataType::Uint16) => (*n as u16).to_be_bytes().to_vec(),
            (ValueDef::Integer(n), DataType::Int16) => (*n as i16).to_be_bytes().to_vec(),
            (ValueDef::Integer(n), DataType::Uint32) => (*n as u32).to_be_bytes().to_vec(),
            (ValueDef::Integer(n), DataType::Int32) => (*n as i32).to_be_bytes().to_vec(),
            (ValueDef::String(s), DataType::String) => s.as_bytes().to_vec(),
            (ValueDef::String(s), DataType::Bytes) => parse_hex_bytes(s).unwrap_or_default(),
            (ValueDef::String(s), _) => {
                // Try to parse as hex bytes, otherwise as number
                if let Some(bytes) = parse_hex_bytes(s) {
                    bytes
                } else if let Ok(n) = s.parse::<i64>() {
                    ValueDef::Integer(n).to_bytes(data_type)
                } else {
                    s.as_bytes().to_vec()
                }
            }
            (ValueDef::Bytes(b), _) => b.clone(),
            (ValueDef::None, DataType::Uint8 | DataType::Int8) => vec![0],
            (ValueDef::None, DataType::Uint16 | DataType::Int16) => vec![0, 0],
            (ValueDef::None, DataType::Uint32 | DataType::Int32) => vec![0, 0, 0, 0],
            (ValueDef::None, _) => Vec::new(),
            (ValueDef::Integer(n), DataType::String) => n.to_string().into_bytes(),
            (ValueDef::Integer(n), DataType::Bytes) => {
                // For bytes, treat integer as raw bytes
                let bytes = n.to_be_bytes();
                // Trim leading zeros
                bytes.iter().skip_while(|&&b| b == 0).copied().collect()
            }
        }
    }
}

// =============================================================================
// DTC Definitions
// =============================================================================

/// DTC (Diagnostic Trouble Code) definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DtcDef {
    /// 3-byte DTC number - can be hex string "010100" or array [0x01, 0x01, 0x00]
    #[serde(deserialize_with = "deserialize_dtc_bytes")]
    pub bytes: [u8; 3],

    /// Initial status byte
    #[serde(
        default = "default_dtc_status",
        deserialize_with = "deserialize_hex_u8"
    )]
    pub status: u8,

    /// Optional snapshot data (freeze frame)
    #[serde(default, deserialize_with = "deserialize_optional_hex_bytes")]
    pub snapshot: Option<Vec<u8>>,

    /// Optional extended data
    #[serde(default, deserialize_with = "deserialize_optional_hex_bytes")]
    pub extended_data: Option<Vec<u8>>,

    /// Optional description
    #[serde(default)]
    pub description: Option<String>,
}

fn default_dtc_status() -> u8 {
    0x09 // test_failed + confirmed
}

// =============================================================================
// I/O Output Definitions
// =============================================================================

/// I/O Output definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputDef {
    /// Output identifier
    #[serde(deserialize_with = "deserialize_hex_u16")]
    pub id: u16,

    /// Human-readable name
    pub name: String,

    /// Size in bytes
    #[serde(default = "default_output_size")]
    pub size: usize,

    /// Default value
    #[serde(default, deserialize_with = "deserialize_hex_bytes_vec")]
    pub default: Vec<u8>,

    /// Whether security access is required
    #[serde(default)]
    pub requires_security: bool,

    /// Optional description
    #[serde(default)]
    pub description: Option<String>,
}

fn default_output_size() -> usize {
    1
}

// =============================================================================
// Routine Definitions
// =============================================================================

/// Routine definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutineDef {
    /// Routine identifier
    #[serde(deserialize_with = "deserialize_hex_u16")]
    pub id: u16,

    /// Human-readable name
    pub name: String,

    /// Whether security access is required
    #[serde(default)]
    pub requires_security: bool,

    /// Minimum session required (0x01=default, 0x02=programming, 0x03=extended)
    /// Defaults to 0x03 (extended) — most routines require non-default session
    #[serde(default = "default_extended_session")]
    pub required_session: u8,

    /// Default result data (for simulated completion)
    #[serde(default, deserialize_with = "deserialize_hex_bytes_vec")]
    pub result: Vec<u8>,

    /// Whether this routine completes instantly or runs async
    #[serde(default = "default_true")]
    pub instant: bool,

    /// Optional description
    #[serde(default)]
    pub description: Option<String>,
}

fn default_extended_session() -> u8 {
    0x03
}

fn default_true() -> bool {
    true
}

// =============================================================================
// Hex Parsing Helpers
// =============================================================================

/// Parse hex string to bytes (supports "DEADBEEF" or "0xDEADBEEF")
fn parse_hex_bytes(s: &str) -> Option<Vec<u8>> {
    let s = s.trim().strip_prefix("0x").unwrap_or(s);
    let s = s.strip_prefix("0X").unwrap_or(s);

    if s.is_empty() || !s.len().is_multiple_of(2) {
        return None;
    }

    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// Deserialize a hex u16 (supports "0xF190" or 61840)
fn deserialize_hex_u16<'de, D>(deserializer: D) -> Result<u16, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum HexOrInt {
        Hex(String),
        Int(u16),
    }

    match HexOrInt::deserialize(deserializer)? {
        HexOrInt::Int(n) => Ok(n),
        HexOrInt::Hex(s) => {
            let s = s.trim().strip_prefix("0x").unwrap_or(&s);
            let s = s.strip_prefix("0X").unwrap_or(s);
            u16::from_str_radix(s, 16).map_err(|e| D::Error::custom(e.to_string()))
        }
    }
}

/// Deserialize a hex u8 (supports "0x09" or 9)
fn deserialize_hex_u8<'de, D>(deserializer: D) -> Result<u8, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum HexOrInt {
        Hex(String),
        Int(u8),
    }

    match HexOrInt::deserialize(deserializer)? {
        HexOrInt::Int(n) => Ok(n),
        HexOrInt::Hex(s) => {
            let s = s.trim().strip_prefix("0x").unwrap_or(&s);
            let s = s.strip_prefix("0X").unwrap_or(s);
            u8::from_str_radix(s, 16).map_err(|e| D::Error::custom(e.to_string()))
        }
    }
}

/// Deserialize DTC bytes (supports "010100" or [1, 1, 0])
fn deserialize_dtc_bytes<'de, D>(deserializer: D) -> Result<[u8; 3], D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum DtcBytes {
        Hex(String),
        Array(Vec<u8>),
    }

    match DtcBytes::deserialize(deserializer)? {
        DtcBytes::Array(arr) => {
            if arr.len() != 3 {
                return Err(D::Error::custom("DTC bytes must be exactly 3 bytes"));
            }
            Ok([arr[0], arr[1], arr[2]])
        }
        DtcBytes::Hex(s) => {
            let bytes = parse_hex_bytes(&s)
                .ok_or_else(|| D::Error::custom("Invalid hex string for DTC bytes"))?;
            if bytes.len() != 3 {
                return Err(D::Error::custom("DTC bytes must be exactly 3 bytes"));
            }
            Ok([bytes[0], bytes[1], bytes[2]])
        }
    }
}

/// Deserialize hex bytes vec (supports "DEADBEEF" or [0xDE, 0xAD, ...])
fn deserialize_hex_bytes_vec<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum HexBytes {
        Hex(String),
        Array(Vec<u8>),
    }

    match HexBytes::deserialize(deserializer)? {
        HexBytes::Array(arr) => Ok(arr),
        HexBytes::Hex(s) => {
            parse_hex_bytes(&s).ok_or_else(|| D::Error::custom("Invalid hex string"))
        }
    }
}

/// Deserialize optional hex bytes
fn deserialize_optional_hex_bytes<'de, D>(deserializer: D) -> Result<Option<Vec<u8>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OptionalHexBytes {
        Hex(String),
        Array(Vec<u8>),
        None,
    }

    match Option::<OptionalHexBytes>::deserialize(deserializer)? {
        None => Ok(None),
        Some(OptionalHexBytes::None) => Ok(None),
        Some(OptionalHexBytes::Array(arr)) => Ok(Some(arr)),
        Some(OptionalHexBytes::Hex(s)) => {
            if s.is_empty() {
                Ok(None)
            } else {
                parse_hex_bytes(&s)
                    .map(Some)
                    .ok_or_else(|| D::Error::custom("Invalid hex string"))
            }
        }
    }
}

// =============================================================================
// Default ECU Definitions (fallback when no config provided)
// =============================================================================

impl EcuConfig {
    /// Create default VTX ECM configuration with all standard parameters
    pub fn default_vtx_ecm() -> Self {
        Self {
            id: "vtx_ecm".to_string(),
            name: "VTX ECM Simulator".to_string(),
            parameters: vec![
                // === PUBLIC DIDs (Standard Boot / Software Identification) ===
                ParameterDef {
                    did: standard_did::BOOT_SOFTWARE_ID,
                    id: "boot_sw_id".to_string(),
                    data_type: DataType::String,
                    value: ValueDef::String("VTX-BOOT-1.0.3".to_string()),
                    access: AccessLevel::Public,
                    ..Default::default()
                },
                ParameterDef {
                    did: standard_did::APPLICATION_SOFTWARE_ID,
                    id: "app_sw_id".to_string(),
                    data_type: DataType::String,
                    value: ValueDef::String("VTX-APP-1.4.2".to_string()),
                    access: AccessLevel::Public,
                    ..Default::default()
                },
                ParameterDef {
                    did: standard_did::APPLICATION_DATA_ID,
                    id: "app_data_id".to_string(),
                    data_type: DataType::String,
                    value: ValueDef::String("VTX-DATA-2024-01".to_string()),
                    access: AccessLevel::Public,
                    ..Default::default()
                },
                ParameterDef {
                    did: standard_did::BOOT_SOFTWARE_FINGERPRINT,
                    id: "boot_sw_fingerprint".to_string(),
                    data_type: DataType::String,
                    value: ValueDef::String("20230115-ACME".to_string()),
                    access: AccessLevel::Public,
                    ..Default::default()
                },
                ParameterDef {
                    did: standard_did::APP_SOFTWARE_FINGERPRINT,
                    id: "app_sw_fingerprint".to_string(),
                    data_type: DataType::String,
                    value: ValueDef::String("20240115-ACME".to_string()),
                    access: AccessLevel::Public,
                    ..Default::default()
                },
                ParameterDef {
                    did: standard_did::APP_DATA_FINGERPRINT,
                    id: "app_data_fingerprint".to_string(),
                    data_type: DataType::String,
                    value: ValueDef::String("20240115-ACME".to_string()),
                    access: AccessLevel::Public,
                    ..Default::default()
                },
                // === PUBLIC DIDs (Standard Identification) ===
                ParameterDef {
                    did: standard_did::VIN,
                    id: "vin".to_string(),
                    data_type: DataType::String,
                    value: ValueDef::String("WF0XXXGCDX1234567".to_string()),
                    access: AccessLevel::Public,
                    ..Default::default()
                },
                ParameterDef {
                    did: standard_did::SPARE_PART_NUMBER,
                    id: "part_number".to_string(),
                    data_type: DataType::String,
                    value: ValueDef::String("VTX-ECM-2024-001".to_string()),
                    access: AccessLevel::Public,
                    ..Default::default()
                },
                ParameterDef {
                    did: standard_did::ECU_SERIAL_NUMBER,
                    id: "serial_number".to_string(),
                    data_type: DataType::String,
                    value: ValueDef::String("SN123456789".to_string()),
                    access: AccessLevel::Public,
                    ..Default::default()
                },
                ParameterDef {
                    did: standard_did::ECU_SOFTWARE_NUMBER,
                    id: "sw_number".to_string(),
                    data_type: DataType::String,
                    value: ValueDef::String("ECM-SW-2024-1042".to_string()),
                    access: AccessLevel::Public,
                    ..Default::default()
                },
                ParameterDef {
                    did: standard_did::ECU_SOFTWARE_VERSION,
                    id: "ecu_sw_version".to_string(),
                    data_type: DataType::String,
                    value: ValueDef::String("1.4.2".to_string()),
                    access: AccessLevel::Public,
                    ..Default::default()
                },
                ParameterDef {
                    did: standard_did::SYSTEM_SUPPLIER_ID,
                    id: "supplier".to_string(),
                    data_type: DataType::String,
                    value: ValueDef::String("ACME Automotive GmbH".to_string()),
                    access: AccessLevel::Public,
                    ..Default::default()
                },
                ParameterDef {
                    did: standard_did::ECU_MANUFACTURING_DATE,
                    id: "mfg_date".to_string(),
                    data_type: DataType::String,
                    value: ValueDef::String("20231201".to_string()),
                    access: AccessLevel::Public,
                    ..Default::default()
                },
                ParameterDef {
                    did: standard_did::ECU_HARDWARE_NUMBER,
                    id: "hardware_number".to_string(),
                    data_type: DataType::String,
                    value: ValueDef::String("HW-REV-C".to_string()),
                    access: AccessLevel::Public,
                    ..Default::default()
                },
                ParameterDef {
                    did: standard_did::SUPPLIER_HW_NUMBER,
                    id: "supplier_hw_number".to_string(),
                    data_type: DataType::String,
                    value: ValueDef::String("SUP-HW-ECM-003".to_string()),
                    access: AccessLevel::Public,
                    ..Default::default()
                },
                ParameterDef {
                    did: standard_did::SUPPLIER_HW_VERSION,
                    id: "hardware_version".to_string(),
                    data_type: DataType::String,
                    value: ValueDef::String("3.2.1".to_string()),
                    access: AccessLevel::Public,
                    ..Default::default()
                },
                ParameterDef {
                    did: standard_did::SUPPLIER_SW_NUMBER,
                    id: "supplier_sw_number".to_string(),
                    data_type: DataType::String,
                    value: ValueDef::String("SUP-SW-ECM-142".to_string()),
                    access: AccessLevel::Public,
                    ..Default::default()
                },
                ParameterDef {
                    did: standard_did::SUPPLIER_SW_VERSION,
                    id: "supplier_sw_version".to_string(),
                    data_type: DataType::String,
                    value: ValueDef::String("1.4.2-release".to_string()),
                    access: AccessLevel::Public,
                    ..Default::default()
                },
                ParameterDef {
                    did: standard_did::SYSTEM_NAME,
                    id: "system_name".to_string(),
                    data_type: DataType::String,
                    value: ValueDef::String("VTX Engine Control Module".to_string()),
                    access: AccessLevel::Public,
                    ..Default::default()
                },
                ParameterDef {
                    did: standard_did::TESTER_SERIAL_NUMBER,
                    id: "tester_serial".to_string(),
                    data_type: DataType::String,
                    value: ValueDef::String("TST-2024-00042".to_string()),
                    access: AccessLevel::Public,
                    ..Default::default()
                },
                // === PUBLIC DIDs (Sensor / Runtime) ===
                ParameterDef {
                    did: 0xF40E,
                    id: "vehicle_speed".to_string(),
                    data_type: DataType::Uint8,
                    value: ValueDef::Integer(65),
                    min: Some(0),
                    max: Some(255),
                    varies: true,
                    access: AccessLevel::Public,
                    ..Default::default()
                },
                ParameterDef {
                    did: 0xF404,
                    id: "engine_load".to_string(),
                    data_type: DataType::Uint8,
                    value: ValueDef::Integer(127),
                    min: Some(0),
                    max: Some(255),
                    varies: true,
                    access: AccessLevel::Public,
                    ..Default::default()
                },
                ParameterDef {
                    did: 0xF405,
                    id: "coolant_temp".to_string(),
                    data_type: DataType::Uint8,
                    value: ValueDef::Integer(132),
                    min: Some(0),
                    max: Some(255),
                    varies: true,
                    access: AccessLevel::Public,
                    ..Default::default()
                },
                // === EXTENDED DIDs ===
                ParameterDef {
                    did: 0xF40C,
                    id: "engine_rpm".to_string(),
                    data_type: DataType::Uint16,
                    value: ValueDef::Integer(7400),
                    min: Some(0),
                    max: Some(32000),
                    varies: true,
                    access: AccessLevel::Extended,
                    ..Default::default()
                },
                ParameterDef {
                    did: 0xF48A,
                    id: "oil_pressure".to_string(),
                    data_type: DataType::Uint16,
                    value: ValueDef::Integer(4500),
                    min: Some(0),
                    max: Some(10000),
                    varies: true,
                    access: AccessLevel::Extended,
                    ..Default::default()
                },
                ParameterDef {
                    did: 0xF40D,
                    id: "fuel_rate".to_string(),
                    data_type: DataType::Uint16,
                    value: ValueDef::Integer(600),
                    min: Some(0),
                    max: Some(10000),
                    varies: true,
                    access: AccessLevel::Extended,
                    ..Default::default()
                },
                ParameterDef {
                    did: 0xF406,
                    id: "intake_temp".to_string(),
                    data_type: DataType::Uint8,
                    value: ValueDef::Integer(75),
                    min: Some(0),
                    max: Some(255),
                    varies: true,
                    access: AccessLevel::Extended,
                    ..Default::default()
                },
                // === PROTECTED DIDs ===
                ParameterDef {
                    did: 0xF42F,
                    id: "boost_pressure".to_string(),
                    data_type: DataType::Uint16,
                    value: ValueDef::Integer(1500),
                    min: Some(0),
                    max: Some(5000),
                    varies: true,
                    access: AccessLevel::Protected,
                    ..Default::default()
                },
                ParameterDef {
                    did: 0xF478,
                    id: "exhaust_temp".to_string(),
                    data_type: DataType::Uint16,
                    value: ValueDef::Integer(4500),
                    min: Some(0),
                    max: Some(10000),
                    varies: true,
                    access: AccessLevel::Protected,
                    ..Default::default()
                },
                ParameterDef {
                    did: 0xF411,
                    id: "throttle_position".to_string(),
                    data_type: DataType::Uint8,
                    value: ValueDef::Integer(76),
                    min: Some(0),
                    max: Some(255),
                    varies: true,
                    access: AccessLevel::Protected,
                    ..Default::default()
                },
                // === WRITABLE DIDs ===
                ParameterDef {
                    did: standard_did::PROGRAMMING_DATE,
                    id: "programming_date".to_string(),
                    data_type: DataType::Bytes,
                    value: ValueDef::String("20240115".to_string()),
                    access: AccessLevel::Extended,
                    writable: true,
                    ..Default::default()
                },
                ParameterDef {
                    did: 0xF19D,
                    id: "installation_date".to_string(),
                    data_type: DataType::Bytes,
                    value: ValueDef::String("20230620".to_string()),
                    access: AccessLevel::Protected,
                    writable: true,
                    ..Default::default()
                },
            ],
            dtcs: vec![
                DtcDef {
                    bytes: [0x01, 0x01, 0x00],
                    status: 0x09,
                    snapshot: Some(vec![0x01, 0xF4, 0x05, 0x84, 0xF4, 0x0E, 0x40]),
                    extended_data: Some(vec![0x01, 0x05, 0x00, 0x03]),
                    description: Some(
                        "P0101 - Mass Air Flow Circuit Range/Performance".to_string(),
                    ),
                },
                DtcDef {
                    bytes: [0x03, 0x00, 0x00],
                    status: 0x24,
                    snapshot: None,
                    extended_data: None,
                    description: Some("P0300 - Random/Multiple Cylinder Misfire".to_string()),
                },
                DtcDef {
                    bytes: [0x44, 0x20, 0x00],
                    status: 0x28,
                    snapshot: None,
                    extended_data: Some(vec![0x01, 0x02, 0x00, 0x10]),
                    description: Some("C0420 - Steering Angle Sensor Circuit".to_string()),
                },
                DtcDef {
                    bytes: [0x92, 0x34, 0x00],
                    status: 0x89,
                    snapshot: None,
                    extended_data: None,
                    description: Some("B1234 - Airbag Warning Lamp Circuit".to_string()),
                },
                DtcDef {
                    bytes: [0xC1, 0x00, 0x00],
                    status: 0x28,
                    snapshot: None,
                    extended_data: None,
                    description: Some("U0100 - Lost Communication with ECM/PCM".to_string()),
                },
            ],
            outputs: vec![
                OutputDef {
                    id: 0xF000,
                    name: "LED Status".to_string(),
                    size: 1,
                    default: vec![0x00],
                    requires_security: false,
                    description: Some("Status LED on/off control".to_string()),
                },
                OutputDef {
                    id: 0xF001,
                    name: "Fan Speed".to_string(),
                    size: 2,
                    default: vec![0x00, 0x00],
                    requires_security: false,
                    description: Some("Cooling fan motor speed".to_string()),
                },
                OutputDef {
                    id: 0xF002,
                    name: "Relay 1".to_string(),
                    size: 1,
                    default: vec![0x00],
                    requires_security: false,
                    description: Some("General purpose relay 1".to_string()),
                },
                OutputDef {
                    id: 0xF003,
                    name: "Relay 2".to_string(),
                    size: 1,
                    default: vec![0x00],
                    requires_security: true,
                    description: Some("General purpose relay 2 (secured)".to_string()),
                },
                OutputDef {
                    id: 0xF004,
                    name: "PWM Output".to_string(),
                    size: 1,
                    default: vec![0x80],
                    requires_security: false,
                    description: Some("Pulse-width modulated output duty cycle".to_string()),
                },
            ],
            routines: vec![
                RoutineDef {
                    id: 0x0203,
                    name: "Check Programming Preconditions".to_string(),
                    requires_security: false,
                    required_session: 0x03,
                    result: vec![0x00],
                    instant: true,
                    description: None,
                },
                RoutineDef {
                    id: 0xFF00,
                    name: "Erase Memory".to_string(),
                    requires_security: true,
                    required_session: 0x02,
                    result: vec![0x00],
                    instant: true,
                    description: None,
                },
                RoutineDef {
                    id: 0xFF01,
                    name: "Firmware Commit".to_string(),
                    requires_security: true,
                    required_session: 0x03,
                    result: vec![0x00],
                    instant: true,
                    description: Some("Commit activated firmware (A/B bank)".to_string()),
                },
                RoutineDef {
                    id: 0xFF02,
                    name: "Firmware Rollback".to_string(),
                    requires_security: true,
                    required_session: 0x03,
                    result: vec![0x00],
                    instant: true,
                    description: Some(
                        "Rollback to previous firmware version (A/B bank)".to_string(),
                    ),
                },
            ],
            ..Default::default()
        }
    }

    /// Ensure all standard UDS identification DIDs are present.
    ///
    /// Any standard DID not already defined in the config is automatically added
    /// with sensible defaults (string type, read-only, public, non-varying).
    /// TOML configs only need to define sensor/runtime DIDs and optionally
    /// override standard DID *values* — the type, access, and writability
    /// are known from the spec.
    pub fn ensure_standard_dids(&mut self) {
        let existing_dids: std::collections::HashSet<u16> =
            self.parameters.iter().map(|p| p.did).collect();

        for &(did, key, _label) in standard_did::IDENTIFICATION_DIDS {
            if existing_dids.contains(&did) {
                continue;
            }

            // Only fingerprints, programming date, and tester serial are writable
            // per ISO 14229-1 (set during the programming procedure).
            // Everything else (VIN, serial, SW version, HW number, …) is read-only.
            let writable = matches!(
                did,
                standard_did::BOOT_SOFTWARE_FINGERPRINT
                    | standard_did::APP_SOFTWARE_FINGERPRINT
                    | standard_did::APP_DATA_FINGERPRINT
                    | standard_did::PROGRAMMING_DATE
                    | standard_did::TESTER_SERIAL_NUMBER
            );

            self.parameters.push(ParameterDef {
                did,
                id: key.to_string(),
                data_type: DataType::String,
                value: ValueDef::String(self.default_did_value(did)),
                access: AccessLevel::Public,
                writable,
                ..Default::default()
            });
        }
    }

    /// Generate a sensible default value for a standard identification DID.
    fn default_did_value(&self, did: u16) -> String {
        match did {
            standard_did::VIN => "UNSET".to_string(),
            standard_did::SYSTEM_NAME => self.name.clone(),
            standard_did::ECU_SERIAL_NUMBER => format!("SN-{}", self.id),
            standard_did::SPARE_PART_NUMBER => format!("PN-{}", self.id),
            standard_did::ECU_SOFTWARE_NUMBER => format!("SW-{}", self.id),
            standard_did::ECU_SOFTWARE_VERSION => "1.0.0".to_string(),
            standard_did::SYSTEM_SUPPLIER_ID => "Simulated".to_string(),
            standard_did::ECU_MANUFACTURING_DATE => "20240101".to_string(),
            standard_did::ECU_HARDWARE_NUMBER => format!("HW-{}", self.id),
            standard_did::SUPPLIER_HW_NUMBER => format!("SUPHW-{}", self.id),
            standard_did::SUPPLIER_HW_VERSION => "1.0".to_string(),
            standard_did::SUPPLIER_SW_NUMBER => format!("SUPSW-{}", self.id),
            standard_did::SUPPLIER_SW_VERSION => "1.0.0".to_string(),
            standard_did::PROGRAMMING_DATE => "00000000".to_string(),
            standard_did::BOOT_SOFTWARE_ID => format!("BOOT-{}-1.0", self.id),
            standard_did::APPLICATION_SOFTWARE_ID => format!("APP-{}-1.0", self.id),
            standard_did::APPLICATION_DATA_ID => format!("DATA-{}-1.0", self.id),
            _ => String::new(),
        }
    }
}

impl Default for ParameterDef {
    fn default() -> Self {
        Self {
            did: 0,
            id: String::new(),
            data_type: DataType::Uint8,
            value: ValueDef::None,
            min: None,
            max: None,
            varies: false,
            variation_percent: 2,
            access: AccessLevel::Public,
            writable: false,
            description: None,
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hex_u16() {
        let yaml = r#"
did: "0xF190"
id: test
"#;
        let def: ParameterDef = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(def.did, 0xF190);
    }

    #[test]
    fn test_parse_int_did() {
        let yaml = r#"
did: 61840
id: test
"#;
        let def: ParameterDef = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(def.did, 61840);
    }

    #[test]
    fn test_parse_dtc_hex() {
        let yaml = r#"
bytes: "010100"
status: "0x09"
"#;
        let def: DtcDef = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(def.bytes, [0x01, 0x01, 0x00]);
        assert_eq!(def.status, 0x09);
    }

    #[test]
    fn test_parse_dtc_array() {
        let yaml = r#"
bytes: [1, 1, 0]
status: 9
"#;
        let def: DtcDef = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(def.bytes, [0x01, 0x01, 0x00]);
        assert_eq!(def.status, 0x09);
    }

    #[test]
    fn test_value_to_bytes() {
        assert_eq!(
            ValueDef::Integer(1850).to_bytes(&DataType::Uint16),
            vec![0x07, 0x3A]
        );
        assert_eq!(
            ValueDef::String("TEST".to_string()).to_bytes(&DataType::String),
            vec![0x54, 0x45, 0x53, 0x54]
        );
    }

    #[test]
    fn test_default_vtx_ecm() {
        let config = EcuConfig::default_vtx_ecm();
        assert_eq!(config.id, "vtx_ecm");
        assert!(!config.parameters.is_empty());
        assert!(!config.dtcs.is_empty());
        assert!(!config.outputs.is_empty());
        assert!(!config.routines.is_empty());
    }
}
