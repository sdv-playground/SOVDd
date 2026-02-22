//! API error types and conversions

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use sovd_core::BackendError;

/// API error type that converts to HTTP responses
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
    /// 502 Bad Gateway - ECU returned negative response (SOVD compliant format)
    EcuErrorResponse { message: String, nrc: u8, sid: u8 },
    /// 503 Service Unavailable
    ServiceUnavailable(String),
    /// 504 Gateway Timeout
    GatewayTimeout(String),
    /// 500 Internal Server Error
    Internal(String),
}

/// Standard error response format
#[derive(Serialize)]
struct ErrorResponse {
    error: String,
    message: String,
}

/// SOVD-compliant ECU error response format
#[derive(Serialize)]
struct SovdErrorResponse {
    error_code: String,
    message: String,
    parameters: SovdErrorParameters,
    #[serde(rename = "x-errorsource")]
    error_source: String,
}

#[derive(Serialize)]
struct SovdErrorParameters {
    #[serde(rename = "NRC")]
    nrc: u8,
    #[serde(rename = "SID")]
    sid: u8,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // Handle ECU error response with SOVD-compliant format
        if let ApiError::EcuErrorResponse { message, nrc, sid } = self {
            tracing::debug!(nrc = nrc, sid = sid, %message, "ECU error response");

            let body = Json(SovdErrorResponse {
                error_code: "error-response".to_string(),
                message,
                parameters: SovdErrorParameters { nrc, sid },
                error_source: "ECU".to_string(),
            });

            return (StatusCode::BAD_GATEWAY, body).into_response();
        }

        let (status, error_type, message) = match self {
            ApiError::BadRequest(msg) => (StatusCode::BAD_REQUEST, "bad_request", msg),
            ApiError::NotFound(msg) => (StatusCode::NOT_FOUND, "not_found", msg),
            ApiError::Forbidden(msg) => (StatusCode::FORBIDDEN, "forbidden", msg),
            ApiError::Conflict(msg) => (StatusCode::CONFLICT, "conflict", msg),
            ApiError::PreconditionFailed(msg) => {
                (StatusCode::PRECONDITION_FAILED, "precondition_failed", msg)
            }
            ApiError::TooManyRequests(msg) => {
                (StatusCode::TOO_MANY_REQUESTS, "too_many_requests", msg)
            }
            ApiError::NotImplemented(msg) => (StatusCode::NOT_IMPLEMENTED, "not_implemented", msg),
            ApiError::BadGateway(msg) => (StatusCode::BAD_GATEWAY, "bad_gateway", msg),
            ApiError::EcuErrorResponse { .. } => unreachable!(), // Handled above
            ApiError::ServiceUnavailable(msg) => {
                (StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", msg)
            }
            ApiError::GatewayTimeout(msg) => (StatusCode::GATEWAY_TIMEOUT, "gateway_timeout", msg),
            ApiError::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error", msg),
        };

        // Log errors at appropriate levels
        if status.is_server_error() {
            tracing::error!(error = error_type, %message, "API error");
        } else if status.is_client_error() {
            tracing::debug!(error = error_type, %message, "API client error");
        }

        let body = Json(ErrorResponse {
            error: error_type.to_string(),
            message,
        });

        (status, body).into_response()
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
