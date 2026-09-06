//! Configuration models — ISO 17978-3 §7.12.
//!
//! A configuration is a single resource that is always read and written *as a
//! whole* (§7.12.3.1 / §7.12.4.1) — individual parameters of a resource cannot
//! be addressed. Two representations exist (Table 145): `parameter` (a JSON
//! object of key/value settings) and `bulk` (an opaque byte stream whose
//! `version` + `content_type` say how to interpret it).

use serde::{Deserialize, Serialize};

/// Configuration representation — §7.12.2 Table 145.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum ConfigurationType {
    /// Provided as bulk data; `version` and `content_type` define how it is
    /// interpreted.
    Bulk,
    /// Provided as single attributes of a JSON object.
    Parameter,
}

/// One entry of `GET /{entity}/configurations` — §7.12.2 Table 144.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ConfigurationMetaData {
    /// Unique identifier for the configuration data.
    pub id: String,
    /// Name of the configuration data.
    pub name: String,
    /// Identifier for translating the configuration data name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub translation_id: Option<String>,
    /// Configuration type definition.
    #[serde(rename = "type")]
    pub configuration_type: ConfigurationType,
    /// C: mandatory for `bulk` configurations, not present for `parameter`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// MIME type telling the client how to interpret the bulk payload.
    /// C: mandatory for `bulk` configurations, not present for `parameter`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
}

/// Body of a parameter-configuration read — §7.12.3.3, the `ReadValue` of
/// Table 85.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ConfigurationValue {
    /// The `configuration-id` that was read.
    pub id: String,
    /// The configuration's parameters as a JSON object.
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub data: serde_json::Value,
    /// Schema of the returned data.
    /// C: only provided if the query parameter `include-schema` is `true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub schema: Option<serde_json::Value>,
}

/// Request body of a parameter-configuration write — §7.12.4.3 Table 152.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ConfigurationWrite {
    /// The value of the data; its type is defined by the resource.
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub data: serde_json::Value,
    /// C: required if a signature is required for this configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

/// A bulk configuration's payload plus the MIME type it is served with —
/// §7.12.3.2 / §7.12.4.2. The content is ExVe manufacturer specific
/// (`application/octet-stream`, `multipart/form-data`, `multipart/signed`, …).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BulkConfiguration {
    /// MIME type of `body` (goes out as the response `Content-Type`).
    pub content_type: String,
    /// The raw configuration payload.
    pub body: Vec<u8>,
}
