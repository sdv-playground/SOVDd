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

    /// Internal error
    #[error("Internal error: {0}")]
    Internal(String),
}

impl BackendError {
    /// Returns the HTTP status code for this error
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
            BackendError::EcuError { .. } => 502,
            BackendError::RateLimited(_) => 429,
            BackendError::Transport(_) => 503,
            BackendError::InvalidRequest(_) => 400,
            BackendError::Timeout => 504,
            BackendError::Busy(_) => 409,
            BackendError::Internal(_) => 500,
        }
    }
}
