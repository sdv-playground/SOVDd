//! Fault models (abstract representation for DTCs, errors, etc.)

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A diagnostic fault (DTC for ECU, error for HPC)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fault {
    /// Unique identifier for this fault
    pub id: String,
    /// Fault code (DTC code like "P0101" or error code)
    pub code: String,
    /// Severity level
    pub severity: FaultSeverity,
    /// Human-readable message/description
    pub message: String,
    /// Category/domain
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// When this fault was first detected
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_occurrence: Option<DateTime<Utc>>,
    /// When this fault was last seen
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_occurrence: Option<DateTime<Utc>>,
    /// Number of occurrences
    #[serde(skip_serializing_if = "Option::is_none")]
    pub occurrence_count: Option<u32>,
    /// Whether the fault is currently active
    #[serde(default)]
    pub active: bool,
    /// Additional status information (backend-specific)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<serde_json::Value>,
    /// Link to detailed fault information
    pub href: String,
}

/// Fault severity levels
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FaultSeverity {
    /// Informational only
    Info,
    /// Warning condition
    Warning,
    /// Error condition
    #[default]
    Error,
    /// Critical failure
    Critical,
}

/// Filter for querying faults
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FaultFilter {
    /// Filter by severity
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity: Option<FaultSeverity>,
    /// Filter by category
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Only active faults
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_only: Option<bool>,
    /// Faults since this time
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<DateTime<Utc>>,
    /// Maximum number of faults to return
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

/// Result of clearing faults
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClearFaultsResult {
    /// Whether the clear was successful
    pub success: bool,
    /// Number of faults cleared
    pub cleared_count: u32,
    /// Message describing the result
    pub message: String,
}

/// Result of getting faults (includes metadata)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaultsResult {
    /// List of faults
    pub faults: Vec<Fault>,
    /// Status availability mask (UDS-specific, indicates which status bits are supported)
    pub status_availability_mask: Option<u8>,
}
