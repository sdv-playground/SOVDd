//! Spec-conforming error response shape — ISO 17978-3 §5.8.3
//! (line 150, `GenericError`).
//!
//! All non-2xx SOVD responses MUST carry this body so client tooling
//! and 3rd-party gateways can interpret errors without inspecting
//! status codes alone.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Canonical SOVD error body.
///
/// `error_code` carries a spec-defined token (kebab-case).  The full
/// vocabulary is in [`error_code`]; `vendor-specific` is the escape
/// hatch and **requires** `vendor_code` to be set.
///
/// `parameters` carries structured context — values are arrays of
/// strings, e.g. `{"service": ["0x22"], "nrc": ["0x33"]}` for UDS
/// negative responses (spec line 161).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenericError {
    pub error_code: String,

    /// Required iff `error_code == "vendor-specific"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vendor_code: Option<String>,

    pub message: String,

    /// Optional client-side i18n key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub translation_id: Option<String>,

    /// Optional structured parameters — values are arrays of strings.
    #[serde(skip_serializing_if = "BTreeMap::is_empty", default)]
    pub parameters: BTreeMap<String, Vec<String>>,
}

impl GenericError {
    pub fn new(error_code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            error_code: error_code.into(),
            vendor_code: None,
            message: message.into(),
            translation_id: None,
            parameters: BTreeMap::new(),
        }
    }

    pub fn with_param(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.parameters
            .entry(key.into())
            .or_default()
            .push(value.into());
        self
    }
}

/// Canonical `error_code` tokens (kebab-case per spec convention).
pub mod error_code {
    pub const BAD_REQUEST: &str = "bad-request";
    pub const NOT_FOUND: &str = "not-found";
    pub const FORBIDDEN: &str = "forbidden";
    pub const CONFLICT: &str = "conflict";
    pub const PRECONDITION_FAILED: &str = "precondition-failed";
    pub const TOO_MANY_REQUESTS: &str = "too-many-requests";
    pub const NOT_IMPLEMENTED: &str = "not-implemented";
    pub const BAD_GATEWAY: &str = "bad-gateway";
    pub const SERVICE_UNAVAILABLE: &str = "service-unavailable";
    pub const GATEWAY_TIMEOUT: &str = "gateway-timeout";
    pub const INTERNAL_ERROR: &str = "internal-error";
    /// UDS negative response — pair with `parameters: {service, nrc}`.
    pub const ERROR_RESPONSE: &str = "error-response";
    /// Vendor-specific — pair with `vendor_code`.
    pub const VENDOR_SPECIFIC: &str = "vendor-specific";
}
