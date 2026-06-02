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
///
/// Status codes restricted to ISO 17978-3 §5.8 set:
/// 200/201/202/204/400/401/404/405/406/409/415/500/501/503/504.
/// 403/412/502/429 are NOT in the spec set and were removed.
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
    /// 409 Conflict — `error-response` (UDS NRC).  The ECU answered
    /// but rejected the request given its current state; surface as
    /// a state-conflict to the client.
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
                tracing::debug!(nrc, sid, %message, "ECU error response");
                let body = GenericError::new(error_code::ERROR_RESPONSE, message)
                    .with_param("service", format!("0x{:02X}", sid))
                    .with_param("nrc", format!("0x{:02X}", nrc));
                // 409 (Conflict): ECU answered but rejected — the
                // resource is in a state incompatible with the request.
                (StatusCode::CONFLICT, body)
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
