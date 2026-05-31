//! Spec-conforming error types — ISO 17978-3:2026.
//!
//! * `GenericError` — Table 16, the body of any error response.
//! * `DataError`    — Table 17, partial error embedded in a successful
//!   multi-element response (e.g. `data-lists` reads where some
//!   elements fail).
//! * [`error_code`] — Table 18 enumeration of canonical error codes.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Canonical SOVD error body — spec §5.8.3 Table 16.
///
/// `error_code` MUST be one of [`error_code::*`].  `vendor_code` is
/// required if `error_code == "vendor-specific"`.  `parameters` carries
/// structured context — values are arrays of strings, e.g.
/// `{"service": ["0x22"], "nrc": ["0x33"]}` for UDS negative
/// responses; or `{"http_code": ["404"]}` to surface the underlying
/// HTTP status when an HTTP-tier issue maps onto a spec-enum value.
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

    pub fn vendor(vendor_code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            error_code: error_code::VENDOR_SPECIFIC.to_string(),
            vendor_code: Some(vendor_code.into()),
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

/// Partial-error entry used inside multi-element responses — Table 17.
///
/// `path` is an RFC 6901 JSON pointer identifying which element of the
/// response is erroneous; `error` is the same `GenericError` shape as
/// a top-level error body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataError {
    /// RFC 6901 JSON pointer to the erroneous element.
    pub path: String,
    /// Conditional — present iff the slot represents a failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<GenericError>,
}

/// Table 18 ErrorCode enumeration.  Every `GenericError.error_code`
/// MUST be one of these (with `vendor-specific` as the escape hatch
/// for site-specific cases not covered by the enum).
pub mod error_code {
    /// Underlying component answered with an error (for UDS pair with
    /// `parameters: {service, nrc}`).
    pub const ERROR_RESPONSE: &str = "error-response";
    /// Missing required information (e.g. operation parameter values).
    pub const INCOMPLETE_REQUEST: &str = "incomplete-request";
    /// Client not authorized for the resource.
    pub const INSUFFICIENT_ACCESS_RIGHTS: &str = "insufficient-access-rights";
    /// Component response could not be processed (conversion mismatch).
    pub const INVALID_RESPONSE_CONTENT: &str = "invalid-response-content";
    /// Payload signature invalid.
    pub const INVALID_SIGNATURE: &str = "invalid-signature";
    /// Client's lock was broken by another client (server returns 409).
    pub const LOCK_BROKEN: &str = "lock-broken";
    /// Underlying component queried but did not respond.
    pub const NOT_RESPONDING: &str = "not-responding";
    /// Preconditions to execute the method not met.
    pub const PRECONDITION_NOT_FULFILLED: &str = "precondition-not-fulfilled";
    /// Server reachable but internal error.
    pub const SOVD_SERVER_FAILURE: &str = "sovd-server-failure";
    /// Server misconfigured; client SHALL treat as fatal.
    pub const SOVD_SERVER_MISCONFIGURED: &str = "sovd-server-misconfigured";
    /// Automated update not supported for this package.
    pub const UPDATE_AUTOMATED_NOT_SUPPORTED: &str = "update-automated-not-supported";
    /// Another update is executing.
    pub const UPDATE_EXECUTION_IN_PROGRESS: &str = "update-execution-in-progress";
    /// An update is already in preparation.
    pub const UPDATE_PREPARATION_IN_PROGRESS: &str = "update-preparation-in-progress";
    /// An update is already in progress.
    pub const UPDATE_PROCESS_IN_PROGRESS: &str = "update-process-in-progress";
    /// Details in `vendor_code` — escape hatch for cases not in Table 18.
    pub const VENDOR_SPECIFIC: &str = "vendor-specific";
}
