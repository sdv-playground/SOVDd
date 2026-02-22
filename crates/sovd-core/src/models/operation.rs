//! Operation (routine/command) models

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Information about an available operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationInfo {
    /// Operation identifier
    pub id: String,
    /// Human-readable name
    pub name: String,
    /// Description of what this operation does
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Parameters this operation accepts
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub parameters: Vec<OperationParam>,
    /// Whether this operation requires security access
    #[serde(default)]
    pub requires_security: bool,
    /// Required security level (0 = none)
    #[serde(default)]
    pub security_level: u8,
    /// Link to execute this operation
    pub href: String,
}

/// Parameter definition for an operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationParam {
    /// Parameter name
    pub name: String,
    /// Parameter type
    pub param_type: ParamType,
    /// Whether this parameter is required
    #[serde(default)]
    pub required: bool,
    /// Description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Parameter types for operations
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParamType {
    String,
    Integer,
    Float,
    Boolean,
    Bytes,
}

/// Result of starting an operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationExecution {
    /// Unique execution ID (for tracking async operations)
    pub execution_id: String,
    /// Operation that was executed
    pub operation_id: String,
    /// Current status
    pub status: OperationStatus,
    /// Result data (if completed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Error message (if failed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// When the operation started
    pub started_at: DateTime<Utc>,
    /// When the operation completed (if done)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
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

impl OperationExecution {
    /// Create a new pending operation
    pub fn pending(execution_id: impl Into<String>, operation_id: impl Into<String>) -> Self {
        Self {
            execution_id: execution_id.into(),
            operation_id: operation_id.into(),
            status: OperationStatus::Pending,
            result: None,
            error: None,
            started_at: Utc::now(),
            completed_at: None,
        }
    }

    /// Create a completed operation with result
    pub fn completed(
        execution_id: impl Into<String>,
        operation_id: impl Into<String>,
        result: serde_json::Value,
    ) -> Self {
        let now = Utc::now();
        Self {
            execution_id: execution_id.into(),
            operation_id: operation_id.into(),
            status: OperationStatus::Completed,
            result: Some(result),
            error: None,
            started_at: now,
            completed_at: Some(now),
        }
    }

    /// Create a completed operation with just a message
    pub fn completed_with_message(
        execution_id: impl Into<String>,
        operation_id: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::completed(
            execution_id,
            operation_id,
            serde_json::json!({ "message": message.into() }),
        )
    }

    /// Create a failed operation
    pub fn failed(
        execution_id: impl Into<String>,
        operation_id: impl Into<String>,
        error: impl Into<String>,
    ) -> Self {
        let now = Utc::now();
        Self {
            execution_id: execution_id.into(),
            operation_id: operation_id.into(),
            status: OperationStatus::Failed,
            result: None,
            error: Some(error.into()),
            started_at: now,
            completed_at: Some(now),
        }
    }
}
