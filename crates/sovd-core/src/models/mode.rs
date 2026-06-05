//! Mode-related models (session, security, link control)

use serde::{Deserialize, Serialize};

/// Session mode state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMode {
    /// Mode type (always "session")
    pub mode: String,
    /// Current session name
    pub session: String,
    /// Current session UDS ID
    pub session_id: u8,
}

/// Security access state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecurityState {
    /// Security is locked
    Locked,
    /// Seed has been requested, waiting for key
    SeedAvailable,
    /// Security is unlocked
    Unlocked,
}

/// Security mode state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityMode {
    /// Mode type (always "security")
    pub mode: String,
    /// Current security state
    pub state: SecurityState,
    /// Current security level (if unlocked or seed requested)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<u8>,
    /// Available security levels
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available_levels: Option<Vec<u8>>,
    /// Current seed (if state is SeedAvailable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<String>,
}

/// Communication-control mode (UDS CommunicationControl 0x28).
///
/// ISO 17978-3 §8.3.4 / Table 343: `<entity>/modes/comm-ctrl`. The `value`
/// is the currently-set subfunction (kebab-case), and `supported` is the
/// ECU-specific enum of subfunctions the ECU accepts. 0x28 is write-only on
/// the UDS wire, so `value` reflects the last successful PUT (or the initial
/// default) rather than a live read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommControlMode {
    /// Current subfunction, kebab-case (e.g. `enable-rx-tx`, `disable-rx-tx`).
    pub value: String,
    /// ECU-specific enumeration of accepted subfunction values.
    pub supported: Vec<String>,
}

/// DTC-setting mode (UDS ControlDTCSetting 0x85).
///
/// ISO 17978-3 §8.3.5 / Table 343: `<entity>/modes/dtcsetting`, an `on`/`off`
/// enum. Write-only on the wire, so `value` is the last-set state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DtcSettingMode {
    /// Current DTC-setting state: `on` or `off`.
    pub value: String,
}

/// Link control state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkMode {
    /// Current baud rate in bps
    pub current_baud_rate: u32,
    /// Pending baud rate (verified but not transitioned)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_baud_rate: Option<u32>,
    /// Link state description
    pub link_state: String,
}

/// Link control action result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkControlResult {
    /// Whether the action succeeded
    pub success: bool,
    /// Action that was performed
    pub action: String,
    /// Resulting baud rate
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baud_rate: Option<u32>,
    /// Human-readable message
    pub message: String,
}
