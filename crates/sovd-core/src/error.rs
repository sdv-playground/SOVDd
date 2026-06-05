//! Common error types for diagnostic backends

use thiserror::Error;

/// Result type for backend operations
pub type BackendResult<T> = Result<T, BackendError>;

/// Errors that can occur in diagnostic backends
#[derive(Debug, Error)]
pub enum BackendError {
    /// Entity (component) not found
    #[error("Entity not found: {0}")]
    EntityNotFound(String),

    /// Parameter not found
    #[error("Parameter not found: {0}")]
    ParameterNotFound(String),

    /// Operation not found
    #[error("Operation not found: {0}")]
    OperationNotFound(String),

    /// Output not found (for I/O control)
    #[error("Output not found: {0}")]
    OutputNotFound(String),

    /// Security access required
    #[error("Security access required: level {0}")]
    SecurityRequired(u8),

    /// Session change required
    #[error("Session change required: {0}")]
    SessionRequired(String),

    /// Operation not supported by this backend
    #[error("Operation not supported: {0}")]
    NotSupported(String),

    /// Protocol error (UDS NRC, etc.) - generic protocol errors
    #[error("Protocol error: {0}")]
    Protocol(String),

    /// ECU returned a negative response (NRC) - SOVD compliant error
    #[error("ECU error response: {message} (NRC 0x{nrc:02X}, SID 0x{sid:02X})")]
    EcuError {
        /// Negative Response Code from ECU
        nrc: u8,
        /// Service ID that was rejected
        sid: u8,
        /// Human-readable error message
        message: String,
    },

    /// Rate limited (exceeded attempts, time delay required)
    #[error("Rate limited: {0}")]
    RateLimited(String),

    /// Transport/communication error
    #[error("Transport error: {0}")]
    Transport(String),

    /// Invalid parameter or request
    #[error("Invalid request: {0}")]
    InvalidRequest(String),

    /// Timeout waiting for response
    #[error("Operation timed out")]
    Timeout,

    /// Resource busy (e.g., upload/download in progress)
    #[error("Resource busy: {0}")]
    Busy(String),

    /// Software update already in progress.  Distinct from `Busy` so
    /// the API layer can surface the spec-defined `update-process-in-progress`
    /// error_code (ISO 17978-3 Table 18) instead of the generic
    /// `precondition-not-fulfilled`.
    #[error("Update in progress: {0}")]
    UpdateInProgress(String),

    /// Payload type doesn't match the addressed component.  Maps to HTTP
    /// 415 Unsupported Media Type.  Used by the F.D3 dispatcher when a
    /// manifest's target doesn't match the component_id on the path —
    /// e.g. a `vm1` manifest POSTed to `/components/vm2/updates`.
    #[error("Unsupported media type: {0}")]
    UnsupportedMediaType(String),

    /// Internal error
    #[error("Internal error: {0}")]
    Internal(String),
}

impl BackendError {
    /// Returns the HTTP status code for this error.
    ///
    /// For `EcuError` (a UDS negative response) the status is derived from the
    /// NRC via [`nrc_to_status`] — the single source of truth for the
    /// NRC→HTTP mapping, shared with the `sovd-api` `IntoResponse` impl (which
    /// derives its `StatusCode` from this same function). Earlier this arm
    /// returned a blanket `502`, which contradicted the API layer's blanket
    /// `409`; both now agree (ISO 17978-3 §8.4, C-131).
    pub fn status_code(&self) -> u16 {
        match self {
            BackendError::EntityNotFound(_) => 404,
            BackendError::ParameterNotFound(_) => 404,
            BackendError::OperationNotFound(_) => 404,
            BackendError::OutputNotFound(_) => 404,
            BackendError::SecurityRequired(_) => 403,
            BackendError::SessionRequired(_) => 409,
            BackendError::NotSupported(_) => 501,
            BackendError::Protocol(_) => 502,
            BackendError::EcuError { nrc, .. } => nrc_to_status(*nrc),
            BackendError::RateLimited(_) => 429,
            BackendError::Transport(_) => 503,
            BackendError::InvalidRequest(_) => 400,
            BackendError::Timeout => 504,
            BackendError::Busy(_) => 409,
            BackendError::UpdateInProgress(_) => 409,
            BackendError::UnsupportedMediaType(_) => 415,
            BackendError::Internal(_) => 500,
        }
    }
}

/// Map a UDS Negative Response Code (NRC, ISO 14229-1) to the HTTP status the
/// SOVD server returns when an ECU rejects a request (e.g. a `0x2E`
/// WriteDataByIdentifier).
///
/// This is the **single source of truth** for the NRC→HTTP mapping. Both
/// [`BackendError::status_code`] (for the `EcuError` arm) and the `sovd-api`
/// `ApiError::EcuErrorResponse` `IntoResponse` derive their status from here,
/// so the two layers can never disagree.
///
/// The spec (ISO 17978-3 §8.4, conformance C-131) mandates *that* the error is
/// surfaced with `service` + `nrc` + `http_code`, but leaves the specific HTTP
/// status per NRC as an engineering choice. This table is that choice:
///
/// | HTTP | NRCs                                                              |
/// |------|-------------------------------------------------------------------|
/// | 400  | `0x13` incorrectMessageLengthOrInvalidFormat, `0x31` requestOutOfRange |
/// | 403  | `0x33` securityAccessDenied, `0x35` invalidKey, `0x36` exceedNumberOfAttempts |
/// | 502  | `0x10` generalReject, `0x11` serviceNotSupported, `0x12` subFunctionNotSupported, `0x14` responseTooLong, `0x26` failurePreventsExecutionOfRequestedAction, `0x72` generalProgrammingFailure |
/// | 503  | `0x21` busyRepeatRequest                                          |
/// | 409  | everything else (state conflict — DEFAULT): `0x22` conditionsNotCorrect, `0x24` requestSequenceError, `0x37` requiredTimeDelayNotExpired, `0x70`/`0x71`/`0x73`, `0x7E`/`0x7F`, … |
///
/// 400/403/502/503 are deliberate departures from the default; `409 Conflict`
/// models "the ECU answered but its current state is incompatible with the
/// request".
pub fn nrc_to_status(nrc: u8) -> u16 {
    match nrc {
        // 400 Bad Request — the request itself was malformed / out of range.
        0x13 | 0x31 => 400,
        // 403 Forbidden — security access gate (denied / bad key / locked out).
        0x33 | 0x35 | 0x36 => 403,
        // 502 Bad Gateway — ECU-side failure to service the (well-formed) request.
        0x10 | 0x11 | 0x12 | 0x14 | 0x26 | 0x72 => 502,
        // 503 Service Unavailable — transiently busy, retry.
        0x21 => 503,
        // 409 Conflict — state conflict; the default for every other NRC
        // (0x22, 0x24, 0x37, 0x70/0x71/0x73, 0x7E/0x7F, and all unlisted).
        _ => 409,
    }
}

#[cfg(test)]
mod tests {
    use super::nrc_to_status;

    #[test]
    fn nrc_to_status_representatives() {
        // 400 Bad Request
        assert_eq!(nrc_to_status(0x13), 400);
        assert_eq!(nrc_to_status(0x31), 400);
        // 403 Forbidden
        assert_eq!(nrc_to_status(0x33), 403);
        assert_eq!(nrc_to_status(0x35), 403);
        assert_eq!(nrc_to_status(0x36), 403);
        // 502 Bad Gateway (ECU-side failure)
        assert_eq!(nrc_to_status(0x10), 502);
        assert_eq!(nrc_to_status(0x11), 502);
        assert_eq!(nrc_to_status(0x12), 502);
        assert_eq!(nrc_to_status(0x14), 502);
        assert_eq!(nrc_to_status(0x26), 502);
        assert_eq!(nrc_to_status(0x72), 502);
        // 503 Service Unavailable
        assert_eq!(nrc_to_status(0x21), 503);
        // 409 Conflict (DEFAULT)
        assert_eq!(nrc_to_status(0x22), 409);
        assert_eq!(nrc_to_status(0x24), 409);
        assert_eq!(nrc_to_status(0x37), 409);
        assert_eq!(nrc_to_status(0x70), 409);
        assert_eq!(nrc_to_status(0x71), 409);
        assert_eq!(nrc_to_status(0x73), 409);
        assert_eq!(nrc_to_status(0x7E), 409);
        assert_eq!(nrc_to_status(0x7F), 409);
        // An unlisted NRC falls through to the 409 default.
        assert_eq!(nrc_to_status(0x99), 409);
    }
}
