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
use sovd_core::{error_code, BackendError, GenericError};

/// API error type that converts to HTTP responses.
#[derive(Debug)]
pub enum ApiError {
    /// 400 Bad Request — `incomplete-request`
    BadRequest(String),
    /// 404 Not Found — `incomplete-request` (caller referenced a
    /// non-existent resource path; spec has no dedicated "not-found"
    /// enum value, treat as a malformed request)
    NotFound(String),
    /// 403 Forbidden — `insufficient-access-rights`
    Forbidden(String),
    /// 409 Conflict — `precondition-not-fulfilled` (generic resource
    /// busy / state-machine conflict).  For lock-broken specifically
    /// use the dedicated `LockBroken` variant; for in-progress flash
    /// use `UpdateInProgress`.
    Conflict(String),
    /// 409 Conflict — `lock-broken`.
    LockBroken(String),
    /// 409 Conflict — `update-process-in-progress`.
    UpdateInProgress(String),
    /// 412 Precondition Failed — `precondition-not-fulfilled`
    PreconditionFailed(String),
    /// 429 Too Many Requests — `vendor-specific` (no Table 18 mapping)
    TooManyRequests(String),
    /// 501 Not Implemented — `sovd-server-misconfigured`
    NotImplemented(String),
    /// 502 Bad Gateway — `not-responding` (upstream protocol error)
    BadGateway(String),
    /// 502 Bad Gateway — `error-response` (UDS NRC).
    EcuErrorResponse { message: String, nrc: u8, sid: u8 },
    /// 503 Service Unavailable — `not-responding` (transport unavailable).
    ServiceUnavailable(String),
    /// 504 Gateway Timeout — `not-responding`.
    GatewayTimeout(String),
    /// 500 Internal Server Error — `sovd-server-failure`.
    Internal(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, body) = match self {
            ApiError::EcuErrorResponse { message, nrc, sid } => {
                tracing::debug!(nrc, sid, %message, "ECU error response");
                let body = GenericError::new(error_code::ERROR_RESPONSE, message)
                    .with_param("service", format!("0x{:02X}", sid))
                    .with_param("nrc", format!("0x{:02X}", nrc));
                (StatusCode::BAD_GATEWAY, body)
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
            ApiError::Forbidden(msg) => (
                StatusCode::FORBIDDEN,
                GenericError::new(error_code::INSUFFICIENT_ACCESS_RIGHTS, msg),
            ),
            ApiError::Conflict(msg) => (
                StatusCode::CONFLICT,
                GenericError::new(error_code::PRECONDITION_NOT_FULFILLED, msg)
                    .with_param("http_code", "409"),
            ),
            ApiError::LockBroken(msg) => (
                StatusCode::CONFLICT,
                GenericError::new(error_code::LOCK_BROKEN, msg),
            ),
            ApiError::UpdateInProgress(msg) => (
                StatusCode::CONFLICT,
                GenericError::new(error_code::UPDATE_PROCESS_IN_PROGRESS, msg),
            ),
            ApiError::PreconditionFailed(msg) => (
                StatusCode::PRECONDITION_FAILED,
                GenericError::new(error_code::PRECONDITION_NOT_FULFILLED, msg),
            ),
            ApiError::TooManyRequests(msg) => (
                StatusCode::TOO_MANY_REQUESTS,
                GenericError::vendor("rate-limited", msg).with_param("http_code", "429"),
            ),
            ApiError::NotImplemented(msg) => (
                StatusCode::NOT_IMPLEMENTED,
                GenericError::new(error_code::SOVD_SERVER_MISCONFIGURED, msg)
                    .with_param("http_code", "501"),
            ),
            ApiError::BadGateway(msg) => (
                StatusCode::BAD_GATEWAY,
                GenericError::new(error_code::NOT_RESPONDING, msg).with_param("http_code", "502"),
            ),
            ApiError::ServiceUnavailable(msg) => (
                StatusCode::SERVICE_UNAVAILABLE,
                GenericError::new(error_code::NOT_RESPONDING, msg).with_param("http_code", "503"),
            ),
            ApiError::GatewayTimeout(msg) => (
                StatusCode::GATEWAY_TIMEOUT,
                GenericError::new(error_code::NOT_RESPONDING, msg).with_param("http_code", "504"),
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
                ApiError::Forbidden(format!("Security access level {} required", level))
            }
            BackendError::SessionRequired(session) => {
                ApiError::PreconditionFailed(format!("Session change required: {}", session))
            }
            BackendError::NotSupported(op) => {
                ApiError::NotImplemented(format!("Operation not supported: {}", op))
            }
            BackendError::Protocol(msg) => ApiError::BadGateway(msg),
            BackendError::EcuError { nrc, sid, message } => {
                ApiError::EcuErrorResponse { message, nrc, sid }
            }
            BackendError::RateLimited(msg) => ApiError::TooManyRequests(msg),
            BackendError::Transport(msg) => ApiError::ServiceUnavailable(msg),
            BackendError::InvalidRequest(msg) => ApiError::BadRequest(msg),
            BackendError::Timeout => ApiError::GatewayTimeout("Operation timed out".to_string()),
            BackendError::Busy(msg) => ApiError::Conflict(msg),
            BackendError::Internal(msg) => ApiError::Internal(msg),
        }
    }
}
