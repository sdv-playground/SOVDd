//! Error types for SOVD client operations

use thiserror::Error;

/// Result type alias for SOVD client operations
pub type Result<T> = std::result::Result<T, SovdClientError>;

/// Errors that can occur during SOVD client operations
#[derive(Error, Debug)]
pub enum SovdClientError {
    /// HTTP request failed
    #[error("HTTP request failed: {0}")]
    HttpError(#[from] reqwest::Error),

    /// Invalid URL
    #[error("Invalid URL: {0}")]
    InvalidUrl(#[from] url::ParseError),

    /// IO error
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    /// Server returned an error response
    #[error("Server error {status}: {message}")]
    ServerError { status: u16, message: String },

    /// Failed to parse response
    #[error("Failed to parse response: {0}")]
    ParseError(String),

    /// Component not found
    #[error("Component not found: {0}")]
    ComponentNotFound(String),

    /// Parameter not found
    #[error("Parameter not found: {0}")]
    ParameterNotFound(String),

    /// Operation failed
    #[error("Operation failed: {0}")]
    OperationFailed(String),

    /// Security access denied
    #[error("Security access denied: {0}")]
    SecurityAccessDenied(String),

    /// Session error
    #[error("Session error: {0}")]
    SessionError(String),

    /// Timeout
    #[error("Request timed out")]
    Timeout,

    /// Connection failed
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    /// Streaming error
    #[error("Stream error: {0}")]
    StreamError(String),
}

impl SovdClientError {
    /// Create a server error from status code and message
    pub fn server_error(status: u16, message: impl Into<String>) -> Self {
        Self::ServerError {
            status,
            message: message.into(),
        }
    }
}
