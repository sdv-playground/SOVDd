//! Data parameter models

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// ISO 17978-3 §7.9 (Table 70) `DataCategory` — the kind of a data value.
///
/// SOVD does not differentiate identification, measurement, and parameter
/// data on the wire by path; instead each data resource is *tagged* with a
/// category so a client can filter (`GET /data?categories=…`) and enumerate
/// (`GET /data-categories`).
///
/// Only the four standard values are modelled here. Custom categories
/// (prefix `x-<ext>-…`, Table 70) are out of scope — they would round-trip
/// as their own string but no backend currently produces one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DataCategory {
    /// Identifications — fixed parameters that identify an entity (VIN, part
    /// number, software version). UDS identification DIDs (`0xF180..=0xF19E`).
    #[serde(rename = "identData")]
    IdentData,
    /// Measurements — dynamically changing values (e.g. battery voltage).
    #[serde(rename = "currentData")]
    CurrentData,
    /// Parameters — read/write parameters stored in the ExVe.
    #[serde(rename = "storedData")]
    StoredData,
    /// System information — dynamic system resources (e.g. CPU load).
    #[serde(rename = "sysInfo")]
    SysInfo,
}

impl DataCategory {
    /// Default category for a UDS Data Identifier when no explicit category is
    /// configured: the ISO 14229-1 identification range `0xF180..=0xF19E`
    /// (boot/app software IDs through the tester serial number) maps to
    /// [`DataCategory::IdentData`]; everything else is a measurement
    /// ([`DataCategory::CurrentData`]).
    pub fn from_did(did: u16) -> Self {
        if (0xF180..=0xF19E).contains(&did) {
            DataCategory::IdentData
        } else {
            DataCategory::CurrentData
        }
    }

    /// Default category from a hex DID string (e.g. `"F190"`, `"0xF190"`).
    /// Applies the same `0xF180..=0xF19E → identData` rule as [`from_did`]
    /// after parsing; a string that is not a valid 16-bit hex DID falls back
    /// to [`DataCategory::CurrentData`].
    ///
    /// [`from_did`]: DataCategory::from_did
    pub fn from_did_str(s: &str) -> Self {
        let trimmed = s.trim_start_matches("0x").trim_start_matches("0X");
        match u16::from_str_radix(trimmed, 16) {
            Ok(did) => Self::from_did(did),
            Err(_) => DataCategory::CurrentData,
        }
    }

    /// The spec wire token (camelCase, e.g. `"identData"`).
    pub fn as_wire(&self) -> &'static str {
        match self {
            DataCategory::IdentData => "identData",
            DataCategory::CurrentData => "currentData",
            DataCategory::StoredData => "storedData",
            DataCategory::SysInfo => "sysInfo",
        }
    }

    /// Parse a spec wire token back into a `DataCategory`.
    /// Returns `None` for unknown / custom (`x-<ext>-…`) tokens.
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "identData" => Some(DataCategory::IdentData),
            "currentData" => Some(DataCategory::CurrentData),
            "storedData" => Some(DataCategory::StoredData),
            "sysInfo" => Some(DataCategory::SysInfo),
            _ => None,
        }
    }
}

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
    /// ISO 17978-3 §7.9 data category (Table 70). `ValueMetaData.category`
    /// is M in the `GET /data` list; populated by backends from the resolved
    /// category. `None` only when a backend has not classified the parameter.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub category: Option<DataCategory>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_category_serializes_to_spec_wire_tokens() {
        // The four standard Table 70 tokens must be exact camelCase.
        assert_eq!(
            serde_json::to_string(&DataCategory::IdentData).unwrap(),
            "\"identData\""
        );
        assert_eq!(
            serde_json::to_string(&DataCategory::CurrentData).unwrap(),
            "\"currentData\""
        );
        assert_eq!(
            serde_json::to_string(&DataCategory::StoredData).unwrap(),
            "\"storedData\""
        );
        assert_eq!(
            serde_json::to_string(&DataCategory::SysInfo).unwrap(),
            "\"sysInfo\""
        );
        // Round-trips via serde and the explicit helpers.
        for c in [
            DataCategory::IdentData,
            DataCategory::CurrentData,
            DataCategory::StoredData,
            DataCategory::SysInfo,
        ] {
            assert_eq!(DataCategory::from_wire(c.as_wire()), Some(c));
            let json = serde_json::to_value(c).unwrap();
            let back: DataCategory = serde_json::from_value(json).unwrap();
            assert_eq!(back, c);
        }
        assert_eq!(DataCategory::from_wire("x-acme-foo"), None);
    }

    #[test]
    fn data_category_from_did_uses_identification_range() {
        assert_eq!(DataCategory::from_did(0xF180), DataCategory::IdentData);
        assert_eq!(DataCategory::from_did(0xF190), DataCategory::IdentData);
        assert_eq!(DataCategory::from_did(0xF19E), DataCategory::IdentData);
        assert_eq!(DataCategory::from_did(0xF17F), DataCategory::CurrentData);
        assert_eq!(DataCategory::from_did(0xF19F), DataCategory::CurrentData);
        assert_eq!(DataCategory::from_did(0xF40C), DataCategory::CurrentData);
    }

    #[test]
    fn data_category_from_did_str_parses_hex() {
        assert_eq!(DataCategory::from_did_str("F190"), DataCategory::IdentData);
        assert_eq!(
            DataCategory::from_did_str("0xF190"),
            DataCategory::IdentData
        );
        assert_eq!(
            DataCategory::from_did_str("F40C"),
            DataCategory::CurrentData
        );
        // Non-DID / unparseable → measurement default.
        assert_eq!(
            DataCategory::from_did_str("not-a-did"),
            DataCategory::CurrentData
        );
    }

    #[test]
    fn parameter_info_omits_category_when_none() {
        let p = ParameterInfo {
            id: "x".into(),
            name: "X".into(),
            description: None,
            unit: None,
            data_type: None,
            read_only: true,
            href: String::new(),
            did: None,
            category: None,
        };
        let json = serde_json::to_value(&p).unwrap();
        assert!(json.get("category").is_none());
    }
}
