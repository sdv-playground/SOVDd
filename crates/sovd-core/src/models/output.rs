//! I/O control output models (UDS 0x2F)

use serde::{Deserialize, Serialize};

/// Information about an I/O output
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputInfo {
    /// Output identifier
    pub id: String,
    /// Human-readable name
    pub name: String,
    /// Numeric output ID (hex format)
    pub output_id: String,
    /// Whether this output requires security access
    pub requires_security: bool,
    /// Required security level (0 = none)
    #[serde(default)]
    pub security_level: u8,
    /// Link to detailed output information
    pub href: String,
    /// Data type hint (e.g., "uint8", "uint16")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_type: Option<String>,
    /// Unit of measurement
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

/// Detailed output information with current state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputDetail {
    /// Output identifier
    pub id: String,
    /// Human-readable name
    pub name: String,
    /// Numeric output ID (hex format)
    pub output_id: String,
    /// Current value (hex string)
    pub current_value: String,
    /// Default value (hex string)
    pub default_value: String,
    /// Whether the output is currently controlled by tester
    pub controlled_by_tester: bool,
    /// Whether the output value is frozen
    pub frozen: bool,
    /// Whether this output requires security access
    pub requires_security: bool,
    /// Required security level (0 = none)
    #[serde(default)]
    pub security_level: u8,
    /// Typed current value (decoded from raw bytes using type metadata)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    /// Typed default value (decoded from raw bytes using type metadata)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    /// Data type hint (e.g., "uint8", "uint16")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_type: Option<String>,
    /// Unit of measurement
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// Minimum allowed physical value
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    /// Maximum allowed physical value
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// Allowed string values for enum-like outputs
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed: Vec<String>,
}

/// I/O control action types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IoControlAction {
    /// Return control to ECU (0x00)
    ReturnToEcu,
    /// Reset to default (0x01)
    ResetToDefault,
    /// Freeze current value (0x02)
    Freeze,
    /// Short-term adjustment (0x03)
    ShortTermAdjust,
}

impl IoControlAction {
    /// Convert to UDS control option byte
    pub fn to_uds_option(self) -> u8 {
        match self {
            IoControlAction::ReturnToEcu => 0x00,
            IoControlAction::ResetToDefault => 0x01,
            IoControlAction::Freeze => 0x02,
            IoControlAction::ShortTermAdjust => 0x03,
        }
    }

    /// Parse from string representation
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "return_to_ecu" | "returncontrol" | "return" => Some(IoControlAction::ReturnToEcu),
            "reset_to_default" | "resettodefault" | "reset" => {
                Some(IoControlAction::ResetToDefault)
            }
            "freeze" | "freezecurrent" => Some(IoControlAction::Freeze),
            "short_term_adjust" | "adjust" | "shorttermadjust" => {
                Some(IoControlAction::ShortTermAdjust)
            }
            _ => None,
        }
    }
}

/// Result of I/O control operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IoControlResult {
    /// Output identifier
    pub output_id: String,
    /// Action that was performed
    pub action: String,
    /// Whether the operation succeeded
    pub success: bool,
    /// Whether the output is now controlled by the tester
    pub controlled_by_tester: bool,
    /// Whether the output value is frozen
    pub frozen: bool,
    /// New value after control (hex string)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_value: Option<String>,
    /// Typed new value (decoded from raw bytes using type metadata)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    /// Error message if failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
