//! Types for streaming subscriptions — ISO 17978-3 §5.6 EventEnvelope.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

use crate::ErrorResponse;

/// Spec §5.6 Table 5 EventEnvelope.
///
/// `payload` carries the success body; `error` is the alternative
/// branch when the publisher emitted a `GenericError` (mutually
/// exclusive).  The cyclic-subscription `payload` shape is
/// `{seq: u64, values: {<param>: <value>, ...}}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamEvent {
    /// RFC 3339 UTC time the server emitted this event.
    pub timestamp: String,

    /// Conditional success payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<StreamPayload>,

    /// Conditional error payload (mutually exclusive with `payload`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorResponse>,
}

/// Shape of the success `payload` for cyclic-subscription events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamPayload {
    /// Sequence number, monotonically increasing within a subscription.
    pub seq: u64,
    /// Parameter values keyed by parameter id.
    #[serde(default)]
    pub values: HashMap<String, serde_json::Value>,
}

impl StreamEvent {
    /// Sequence number from the success payload, if any.
    pub fn sequence(&self) -> Option<u64> {
        self.payload.as_ref().map(|p| p.seq)
    }

    /// Iterate the success payload's parameter values, if any.
    pub fn values(&self) -> Option<&HashMap<String, serde_json::Value>> {
        self.payload.as_ref().map(|p| &p.values)
    }

    /// Get a parameter value as a specific type
    pub fn get<T: serde::de::DeserializeOwned>(&self, param: &str) -> Option<T> {
        self.payload
            .as_ref()?
            .values
            .get(param)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    /// Get a parameter value as f64 (common case for numeric sensors)
    pub fn get_f64(&self, param: &str) -> Option<f64> {
        self.payload
            .as_ref()?
            .values
            .get(param)
            .and_then(|v| v.as_f64())
    }

    /// Get a parameter value as i64
    pub fn get_i64(&self, param: &str) -> Option<i64> {
        self.payload
            .as_ref()?
            .values
            .get(param)
            .and_then(|v| v.as_i64())
    }

    /// Get a parameter value as string
    pub fn get_str(&self, param: &str) -> Option<&str> {
        self.payload
            .as_ref()?
            .values
            .get(param)
            .and_then(|v| v.as_str())
    }

    /// Check if a parameter is present
    pub fn has(&self, param: &str) -> bool {
        self.payload
            .as_ref()
            .map(|p| p.values.contains_key(param))
            .unwrap_or(false)
    }

    /// Get all parameter names in this event
    pub fn parameters(&self) -> Box<dyn Iterator<Item = &str> + '_> {
        match self.payload.as_ref() {
            Some(p) => Box::new(p.values.keys().map(|s| s.as_str())),
            None => Box::new(std::iter::empty()),
        }
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
