//! Data parameter models

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Information about a data parameter
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParameterInfo {
    /// Unique identifier for this parameter
    pub id: String,
    /// Human-readable name
    pub name: String,
    /// Description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Unit of measurement (e.g., "rpm", "°C", "%")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// Data type (e.g., "uint16", "float", "string")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_type: Option<String>,
    /// Whether this parameter is read-only
    #[serde(default)]
    pub read_only: bool,
    /// Link to this parameter
    pub href: String,
    /// DID in hex format (e.g., "F40C") — populated by proxy backends
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub did: Option<String>,
}

/// A data value read from a parameter
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataValue {
    /// Parameter identifier
    pub id: String,
    /// Parameter name
    pub name: String,
    /// The value (JSON value to support various types)
    pub value: serde_json::Value,
    /// Unit of measurement
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// When this value was read
    pub timestamp: DateTime<Utc>,
    /// Raw hex-encoded bytes (populated by proxy backends from upstream response)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub raw: Option<String>,
    /// DID identifier in hex (populated by proxy backends from upstream response)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub did: Option<String>,
    /// Byte length of raw data
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub length: Option<usize>,
}

impl DataValue {
    /// Create a new DataValue with the current timestamp
    pub fn new(id: impl Into<String>, name: impl Into<String>, value: serde_json::Value) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            value,
            unit: None,
            timestamp: Utc::now(),
            raw: None,
            did: None,
            length: None,
        }
    }

    /// Add a unit to this value
    pub fn with_unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = Some(unit.into());
        self
    }

    /// Create from an integer value
    pub fn from_int(id: impl Into<String>, name: impl Into<String>, value: i64) -> Self {
        Self::new(id, name, serde_json::Value::Number(value.into()))
    }

    /// Create from a float value
    pub fn from_float(id: impl Into<String>, name: impl Into<String>, value: f64) -> Self {
        Self::new(
            id,
            name,
            serde_json::Number::from_f64(value)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
        )
    }

    /// Create from a string value
    pub fn from_string(
        id: impl Into<String>,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        Self::new(id, name, serde_json::Value::String(value.into()))
    }

    /// Create from a boolean value
    pub fn from_bool(id: impl Into<String>, name: impl Into<String>, value: bool) -> Self {
        Self::new(id, name, serde_json::Value::Bool(value))
    }
}

/// A data point in a stream/subscription
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataPoint {
    /// Parameter identifier
    pub id: String,
    /// The value
    pub value: serde_json::Value,
    /// Unit of measurement
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// Timestamp
    pub timestamp: DateTime<Utc>,
}
