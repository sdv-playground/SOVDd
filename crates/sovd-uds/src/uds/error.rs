//! UDS protocol errors

use thiserror::Error;

use super::NegativeResponseCode;

#[derive(Debug, Error, Clone)]
pub enum UdsError {
    #[error("Negative response: {nrc} (0x{nrc:02X}) for service 0x{service_id:02X}")]
    NegativeResponse {
        service_id: u8,
        nrc: NegativeResponseCode,
    },

    #[error("Invalid response: {0}")]
    InvalidResponse(String),

    #[error("Response timeout")]
    Timeout,

    #[error("Transport error: {0}")]
    Transport(String),

    #[error("Security access failed: {0}")]
    SecurityAccessFailed(String),

    #[error("Session transition failed: {0}")]
    SessionTransitionFailed(String),
}
