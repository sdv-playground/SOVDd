//! API error types and conversions — ISO 17978-3 §5.8.3 GenericError
//! + Table 18 ErrorCode vocabulary.
//!
//! `error_code` MUST be one of the spec-defined tokens (Table 18); HTTP-
//! status-only mappings (e.g. "bad-request", "not-found") were rejected
//! in conformance review — they're not in Table 18.  Where the spec
//! doesn't cover an HTTP-tier issue, fall back to `vendor-specific`
//! with a `vendor_code` and surface the underlying HTTP status via
//! `parameters.http_code`.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use sovd_core::error::nrc_to_status;
use sovd_core::{error_code, BackendError, GenericError};

/// HTTP status for a UDS Negative Response Code (NRC).
///
/// Thin axum-typed wrapper over [`sovd_core::error::nrc_to_status`], which is
/// the single source of truth for the NRC→HTTP table (ISO 17978-3 §8.4,
/// C-131). Keeping the table in one place guarantees this `IntoResponse`
/// status and [`BackendError::status_code`] never diverge.
pub fn nrc_status(nrc: u8) -> StatusCode {
    StatusCode::from_u16(nrc_to_status(nrc)).unwrap_or(StatusCode::CONFLICT)
}

/// API error type that converts to HTTP responses.
///
/// Most arms stay within the ISO 17978-3 §5.8 status set:
/// 200/201/202/204/400/401/404/405/406/409/415/500/501/503/504.
/// The one exception is `EcuErrorResponse`, whose status is the NRC→HTTP
/// mapping (ISO 17978-3 §8.4, C-131) — §8.4 may add per-method codes
/// (403/502 for security / ECU-side-failure NRCs) on top of §5.8's set.
#[derive(Debug)]
pub enum ApiError {
    /// 400 Bad Request — `incomplete-request`
    BadRequest(String),
    /// 404 Not Found — `incomplete-request`
    NotFound(String),
    /// 401 Unauthorized — `insufficient-access-rights`.
    /// Spec §5.8 401 covers "authentication required / missing /
    /// insufficient" — i.e. both authn AND authz issues route here.
    Unauthorized(String),
    /// 409 Conflict — `precondition-not-fulfilled` (generic).
    Conflict(String),
    /// 409 Conflict — `update-process-in-progress`.
    UpdateInProgress(String),
    /// 409 Conflict — `update-preparation-in-progress`.
    UpdatePreparationInProgress(String),
    /// 409 Conflict — `update-execution-in-progress`.
    UpdateExecutionInProgress(String),
    /// 409 Conflict — `update-automated-not-supported`.
    UpdateAutomatedNotSupported(String),
    /// 409 Conflict — `precondition-not-fulfilled` (mode/session/lock
    /// gate failed).  Same status as Conflict; kept distinct for
    /// telemetry/log clarity.
    PreconditionFailed(String),
    /// 503 Service Unavailable — rate-limited or backpressured.
    /// Spec §5.8 503 may include a `Retry-After` header.
    Throttled(String),
    /// 501 Not Implemented — `sovd-server-misconfigured`.
    NotImplemented(String),
    /// 503 Service Unavailable — upstream protocol problem
    /// (`not-responding`, no parseable answer from the ECU).
    ServiceUnavailable(String),
    /// 504 Gateway Timeout — upstream didn't respond in time.
    GatewayTimeout(String),
    /// `error-response` (UDS NRC).  The ECU answered but rejected the
    /// request; the HTTP status is the NRC→HTTP mapping ([`nrc_status`],
    /// C-131) — 400/403/502/503 for specific NRC classes, else 409
    /// (state conflict).
    EcuErrorResponse { message: String, nrc: u8, sid: u8 },
    /// 415 Unsupported Media Type — F.D3 dispatcher rejects a payload
    /// whose target doesn't match the addressed component.  Carries
    /// `vendor-specific` error_code with vendor `wrong-target`.
    UnsupportedMediaType(String),
    /// 500 Internal Server Error — `sovd-server-failure`.
    Internal(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, body) = match self {
            ApiError::EcuErrorResponse { message, nrc, sid } => {
                // NRC→HTTP per the single-source table (ISO 17978-3 §8.4,
                // C-131): the ECU answered but rejected — map the NRC to the
                // status that best models *why* (400 malformed, 403 security,
                // 502 ECU-side failure, 503 busy, else 409 state conflict).
                let status = nrc_status(nrc);
                tracing::debug!(nrc, sid, status = status.as_u16(), %message, "ECU error response");
                // error-response body MUST carry service + nrc; add http_code
                // (yaml:156 lists it as a parameter) so the client sees the
                // mapped status in the body too.
                let body = GenericError::new(error_code::ERROR_RESPONSE, message)
                    .with_param("service", format!("0x{:02X}", sid))
                    .with_param("nrc", format!("0x{:02X}", nrc))
                    .with_param("http_code", status.as_u16().to_string());
                (status, body)
            }
            ApiError::BadRequest(msg) => (
                StatusCode::BAD_REQUEST,
                GenericError::new(error_code::INCOMPLETE_REQUEST, msg),
            ),
            ApiError::NotFound(msg) => (
                StatusCode::NOT_FOUND,
                GenericError::new(error_code::INCOMPLETE_REQUEST, msg)
                    .with_param("http_code", "404"),
            ),
            ApiError::Unauthorized(msg) => (
                StatusCode::UNAUTHORIZED,
                GenericError::new(error_code::INSUFFICIENT_ACCESS_RIGHTS, msg),
            ),
            ApiError::Conflict(msg) => (
                StatusCode::CONFLICT,
                GenericError::new(error_code::PRECONDITION_NOT_FULFILLED, msg),
            ),
            ApiError::UpdateInProgress(msg) => (
                StatusCode::CONFLICT,
                GenericError::new(error_code::UPDATE_PROCESS_IN_PROGRESS, msg),
            ),
            ApiError::UpdatePreparationInProgress(msg) => (
                StatusCode::CONFLICT,
                GenericError::new(error_code::UPDATE_PREPARATION_IN_PROGRESS, msg),
            ),
            ApiError::UpdateExecutionInProgress(msg) => (
                StatusCode::CONFLICT,
                GenericError::new(error_code::UPDATE_EXECUTION_IN_PROGRESS, msg),
            ),
            ApiError::UpdateAutomatedNotSupported(msg) => (
                StatusCode::CONFLICT,
                GenericError::new(error_code::UPDATE_AUTOMATED_NOT_SUPPORTED, msg),
            ),
            ApiError::PreconditionFailed(msg) => (
                StatusCode::CONFLICT,
                GenericError::new(error_code::PRECONDITION_NOT_FULFILLED, msg),
            ),
            ApiError::Throttled(msg) => (
                StatusCode::SERVICE_UNAVAILABLE,
                GenericError::vendor("rate-limited", msg),
            ),
            ApiError::NotImplemented(msg) => (
                StatusCode::NOT_IMPLEMENTED,
                GenericError::new(error_code::SOVD_SERVER_MISCONFIGURED, msg),
            ),
            ApiError::ServiceUnavailable(msg) => (
                StatusCode::SERVICE_UNAVAILABLE,
                GenericError::new(error_code::NOT_RESPONDING, msg),
            ),
            ApiError::GatewayTimeout(msg) => (
                StatusCode::GATEWAY_TIMEOUT,
                GenericError::new(error_code::NOT_RESPONDING, msg),
            ),
            ApiError::UnsupportedMediaType(msg) => (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                GenericError::vendor("wrong-target", msg),
            ),
            ApiError::Internal(msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                GenericError::new(error_code::SOVD_SERVER_FAILURE, msg),
            ),
        };

        if status.is_server_error() {
            tracing::error!(
                error_code = %body.error_code,
                message = %body.message,
                "API error"
            );
        } else if status.is_client_error() {
            tracing::debug!(
                error_code = %body.error_code,
                message = %body.message,
                "API client error"
            );
        }

        (status, Json(body)).into_response()
    }
}

impl From<BackendError> for ApiError {
    fn from(err: BackendError) -> Self {
        match err {
            BackendError::EntityNotFound(msg) => ApiError::NotFound(msg),
            BackendError::ParameterNotFound(msg) => ApiError::NotFound(msg),
            BackendError::OperationNotFound(msg) => ApiError::NotFound(msg),
            BackendError::OutputNotFound(msg) => ApiError::NotFound(msg),
            BackendError::SecurityRequired(level) => {
                ApiError::Unauthorized(format!("Security access level {} required", level))
            }
            BackendError::SessionRequired(session) => {
                ApiError::PreconditionFailed(format!("Session change required: {}", session))
            }
            BackendError::NotSupported(op) => {
                ApiError::NotImplemented(format!("Operation not supported: {}", op))
            }
            BackendError::Protocol(msg) => ApiError::ServiceUnavailable(msg),
            BackendError::EcuError { nrc, sid, message } => {
                ApiError::EcuErrorResponse { message, nrc, sid }
            }
            BackendError::RateLimited(msg) => ApiError::Throttled(msg),
            BackendError::Transport(msg) => ApiError::ServiceUnavailable(msg),
            BackendError::InvalidRequest(msg) => ApiError::BadRequest(msg),
            BackendError::Timeout => ApiError::GatewayTimeout("Operation timed out".to_string()),
            BackendError::Busy(msg) => ApiError::Conflict(msg),
            BackendError::UpdateInProgress(msg) => ApiError::UpdateInProgress(msg),
            BackendError::UnsupportedMediaType(msg) => ApiError::UnsupportedMediaType(msg),
            BackendError::Internal(msg) => ApiError::Internal(msg),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::nrc_status;
    use axum::http::StatusCode;

    #[test]
    fn nrc_status_representatives() {
        // The axum-typed wrapper agrees with the C-131 table (single source
        // of truth in sovd_core::error::nrc_to_status).
        assert_eq!(nrc_status(0x31), StatusCode::BAD_REQUEST); // 400
        assert_eq!(nrc_status(0x13), StatusCode::BAD_REQUEST); // 400
        assert_eq!(nrc_status(0x33), StatusCode::FORBIDDEN); // 403
        assert_eq!(nrc_status(0x35), StatusCode::FORBIDDEN); // 403
        assert_eq!(nrc_status(0x36), StatusCode::FORBIDDEN); // 403
        assert_eq!(nrc_status(0x10), StatusCode::BAD_GATEWAY); // 502
        assert_eq!(nrc_status(0x72), StatusCode::BAD_GATEWAY); // 502
        assert_eq!(nrc_status(0x21), StatusCode::SERVICE_UNAVAILABLE); // 503
        assert_eq!(nrc_status(0x22), StatusCode::CONFLICT); // 409 default
        assert_eq!(nrc_status(0x99), StatusCode::CONFLICT); // 409 default
    }
}
