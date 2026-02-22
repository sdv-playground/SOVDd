//! Types for streaming subscriptions

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

/// A streaming data event from an SSE subscription
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamEvent {
    /// Unix timestamp in milliseconds
    #[serde(rename = "ts")]
    pub timestamp: i64,

    /// Sequence number (monotonically increasing)
    #[serde(rename = "seq")]
    pub sequence: u64,

    /// Parameter values (parameter_name -> value)
    #[serde(flatten)]
    pub values: HashMap<String, serde_json::Value>,
}

impl StreamEvent {
    /// Get a parameter value as a specific type
    pub fn get<T: serde::de::DeserializeOwned>(&self, param: &str) -> Option<T> {
        self.values
            .get(param)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    /// Get a parameter value as f64 (common case for numeric sensors)
    pub fn get_f64(&self, param: &str) -> Option<f64> {
        self.values.get(param).and_then(|v| v.as_f64())
    }

    /// Get a parameter value as i64
    pub fn get_i64(&self, param: &str) -> Option<i64> {
        self.values.get(param).and_then(|v| v.as_i64())
    }

    /// Get a parameter value as string
    pub fn get_str(&self, param: &str) -> Option<&str> {
        self.values.get(param).and_then(|v| v.as_str())
    }

    /// Check if a parameter is present
    pub fn has(&self, param: &str) -> bool {
        self.values.contains_key(param)
    }

    /// Get all parameter names in this event
    pub fn parameters(&self) -> impl Iterator<Item = &str> {
        self.values.keys().map(|s| s.as_str())
    }
}

/// Errors that can occur during streaming
#[derive(Debug, Error)]
pub enum StreamError {
    /// HTTP/connection error
    #[error("Connection error: {0}")]
    Connection(#[from] reqwest::Error),

    /// Failed to parse SSE event
    #[error("Parse error: {0}")]
    Parse(String),

    /// Server returned an error
    #[error("Server error ({status}): {message}")]
    Server { status: u16, message: String },

    /// Stream was closed by the server
    #[error("Stream closed")]
    Closed,

    /// Subscription was cancelled
    #[error("Subscription cancelled")]
    Cancelled,
}

/// Result type for streaming operations
pub type StreamResult<T> = std::result::Result<T, StreamError>;
