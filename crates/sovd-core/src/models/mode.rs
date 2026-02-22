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
