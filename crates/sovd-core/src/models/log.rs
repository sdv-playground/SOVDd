//! Log entry models (primarily for HPC backends)
//!
//! This module supports both traditional text logs (journald-style) and
//! binary data dumps for the message passing pattern (container → cloud).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A log entry - supports both text logs and binary dumps
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    /// Unique identifier for this entry
    pub id: String,
    /// Timestamp of the log entry
    pub timestamp: DateTime<Utc>,
    /// Log priority/level
    pub priority: LogPriority,
    /// Log message content (for text logs)
    pub message: String,
    /// Source of the log (service name, container, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Process/PID that generated the log
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// Additional fields (journald fields, labels, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<serde_json::Value>,
    /// Log type for categorization (e.g., "engine_dump", "diagnostic", "system")
    #[serde(skip_serializing_if = "Option::is_none", rename = "type")]
    pub log_type: Option<String>,
    /// Size of content in bytes (for binary logs)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Retrieval status (pending, retrieved, processed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<LogStatus>,
    /// URL to download content (for large binary data)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
    /// Additional metadata (trigger, fault codes, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Status of a log entry for message passing pattern
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogStatus {
    /// Log is available for retrieval
    #[default]
    Pending,
    /// Log content has been downloaded at least once
    Retrieved,
    /// Log has been processed (kept for audit)
    Processed,
}

/// Log priority levels (aligned with syslog priorities)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogPriority {
    /// Emergency: system is unusable
    Emergency = 0,
    /// Alert: action must be taken immediately
    Alert = 1,
    /// Critical: critical conditions
    Critical = 2,
    /// Error: error conditions
    Error = 3,
    /// Warning: warning conditions
    Warning = 4,
    /// Notice: normal but significant condition
    Notice = 5,
    /// Info: informational messages
    #[default]
    Info = 6,
    /// Debug: debug-level messages
    Debug = 7,
}

impl LogPriority {
    /// Convert from syslog priority number
    pub fn from_syslog(priority: u8) -> Self {
        match priority {
            0 => Self::Emergency,
            1 => Self::Alert,
            2 => Self::Critical,
            3 => Self::Error,
            4 => Self::Warning,
            5 => Self::Notice,
            6 => Self::Info,
            _ => Self::Debug,
        }
    }
}

/// Filter for querying logs
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LogFilter {
    /// Filter by priority (this level and above)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<LogPriority>,
    /// Filter by source (service/unit name)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Logs since this time
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<DateTime<Utc>>,
    /// Logs until this time
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<DateTime<Utc>>,
    /// Text pattern to search for
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    /// Maximum number of entries to return
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// Return last N entries (tail)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tail: Option<usize>,
    /// Filter by log type (e.g., "engine_dump", "diagnostic")
    #[serde(skip_serializing_if = "Option::is_none", rename = "type")]
    pub log_type: Option<String>,
    /// Filter by retrieval status (pending, retrieved)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<LogStatus>,
}
