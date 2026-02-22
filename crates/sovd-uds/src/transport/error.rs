//! Transport layer errors

use thiserror::Error;

#[derive(Debug, Error, Clone)]
pub enum TransportError {
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Connection closed")]
    ConnectionClosed,

    #[error("Send failed: {0}")]
    SendFailed(String),

    #[error("Receive failed: {0}")]
    ReceiveFailed(String),

    #[error("Timeout: {0}")]
    Timeout(String),

    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("Transport not supported: {0}")]
    Unsupported(String),

    #[error("Protocol error: {0}")]
    ProtocolError(String),

    #[error("TLS connection required")]
    TlsRequired,
}
