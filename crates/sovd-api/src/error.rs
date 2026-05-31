//! API error types and conversions — ISO 17978-3 §5.8.3 GenericError.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use sovd_core::{error_code, BackendError, GenericError};

/// API error type that converts to HTTP responses.
#[derive(Debug)]
pub enum ApiError {
    /// 400 Bad Request
    BadRequest(String),
    /// 404 Not Found
    NotFound(String),
    /// 403 Forbidden
    Forbidden(String),
    /// 409 Conflict
    Conflict(String),
    /// 412 Precondition Failed (session/mode requirements not met)
    PreconditionFailed(String),
    /// 429 Too Many Requests (rate limited)
    TooManyRequests(String),
    /// 501 Not Implemented
    NotImplemented(String),
    /// 502 Bad Gateway (backend protocol error)
    BadGateway(String),
    /// 502 Bad Gateway — ECU returned a negative response (UDS NRC).
    EcuErrorResponse { message: String, nrc: u8, sid: u8 },
    /// 503 Service Unavailable
    ServiceUnavailable(String),
    /// 504 Gateway Timeout
    GatewayTimeout(String),
    /// 500 Internal Server Error
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
                GenericError::new(error_code::BAD_REQUEST, msg),
            ),
            ApiError::NotFound(msg) => (
                StatusCode::NOT_FOUND,
                GenericError::new(error_code::NOT_FOUND, msg),
            ),
            ApiError::Forbidden(msg) => (
                StatusCode::FORBIDDEN,
                GenericError::new(error_code::FORBIDDEN, msg),
            ),
            ApiError::Conflict(msg) => (
                StatusCode::CONFLICT,
                GenericError::new(error_code::CONFLICT, msg),
            ),
            ApiError::PreconditionFailed(msg) => (
                StatusCode::PRECONDITION_FAILED,
                GenericError::new(error_code::PRECONDITION_FAILED, msg),
            ),
            ApiError::TooManyRequests(msg) => (
                StatusCode::TOO_MANY_REQUESTS,
                GenericError::new(error_code::TOO_MANY_REQUESTS, msg),
            ),
            ApiError::NotImplemented(msg) => (
                StatusCode::NOT_IMPLEMENTED,
                GenericError::new(error_code::NOT_IMPLEMENTED, msg),
            ),
            ApiError::BadGateway(msg) => (
                StatusCode::BAD_GATEWAY,
                GenericError::new(error_code::BAD_GATEWAY, msg),
            ),
            ApiError::ServiceUnavailable(msg) => (
                StatusCode::SERVICE_UNAVAILABLE,
                GenericError::new(error_code::SERVICE_UNAVAILABLE, msg),
            ),
            ApiError::GatewayTimeout(msg) => (
                StatusCode::GATEWAY_TIMEOUT,
                GenericError::new(error_code::GATEWAY_TIMEOUT, msg),
            ),
            ApiError::Internal(msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                GenericError::new(error_code::INTERNAL_ERROR, msg),
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
