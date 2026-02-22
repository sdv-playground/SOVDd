//! UDS backend errors

use sovd_core::BackendError;
use thiserror::Error;

use crate::uds::UdsError;

/// UDS-specific backend errors
#[derive(Debug, Error)]
pub enum UdsBackendError {
    /// Transport error (CAN bus issues)
    #[error("Transport error: {0}")]
    Transport(String),

    /// UDS protocol error (negative response)
    #[error("UDS error: service 0x{service:02X}, NRC 0x{nrc:02X} - {message}")]
    Protocol {
        service: u8,
        nrc: u8,
        message: String,
    },

    /// Configuration error
    #[error("Configuration error: {0}")]
    Config(String),

    /// Parameter not found
    #[error("Parameter not found: {0}")]
    ParameterNotFound(String),

    /// Timeout waiting for response
    #[error("Timeout waiting for ECU response")]
    Timeout,
}

impl From<UdsBackendError> for BackendError {
    fn from(err: UdsBackendError) -> Self {
        match err {
            UdsBackendError::Transport(msg) => BackendError::Transport(msg),
            UdsBackendError::Protocol {
                service,
                nrc,
                message,
            } => map_nrc_to_backend_error(service, nrc, &message),
            UdsBackendError::Config(msg) => BackendError::Internal(msg),
            UdsBackendError::ParameterNotFound(id) => BackendError::ParameterNotFound(id),
            UdsBackendError::Timeout => BackendError::Timeout,
        }
    }
}

/// Convert UdsError to BackendError, preserving NRC information
impl From<UdsError> for BackendError {
    fn from(err: UdsError) -> Self {
        match err {
            UdsError::NegativeResponse { service_id, nrc } => {
                let nrc_byte: u8 = nrc.into();
                map_nrc_to_backend_error(service_id, nrc_byte, &nrc.to_string())
            }
            UdsError::Timeout => BackendError::Timeout,
            UdsError::Transport(msg) => BackendError::Transport(msg),
            UdsError::InvalidResponse(msg) => {
                BackendError::Protocol(format!("Invalid response: {}", msg))
            }
            UdsError::SecurityAccessFailed(_) => BackendError::SecurityRequired(1),
            UdsError::SessionTransitionFailed(msg) => {
                BackendError::Protocol(format!("Session transition failed: {}", msg))
            }
        }
    }
}

/// Convert UdsError to BackendError (public helper for explicit conversions)
pub fn convert_uds_error(err: UdsError) -> BackendError {
    err.into()
}

/// Map NRC to appropriate backend error
///
/// This function maps UDS Negative Response Codes (NRCs) to appropriate
/// backend errors, following the SOVD specification for HTTP status codes:
///
/// - 0x11, 0x12: Service/subfunction not supported → 501 Not Implemented
/// - 0x13, 0x31: Message format/range errors → 400 Bad Request
/// - 0x22, 0x7E, 0x7F: Session-related conditions → 409 Conflict
/// - 0x33: Security access denied → 403 Forbidden
/// - 0x36, 0x37: Exceeded attempts / time delay → 429 Too Many Requests
/// - All others: ECU error response → 502 Bad Gateway (with SOVD format)
fn map_nrc_to_backend_error(service: u8, nrc: u8, message: &str) -> BackendError {
    match nrc {
        // Service not supported → 501 Not Implemented
        0x11 | 0x12 => BackendError::NotSupported(format!(
            "Service not supported: {} (NRC 0x{:02X})",
            message, nrc
        )),

        // Message format/range errors → 400 Bad Request
        0x13 => {
            BackendError::InvalidRequest(format!("Incorrect message length or format: {}", message))
        }
        0x31 => BackendError::InvalidRequest(format!("Request out of range: {}", message)),

        // Session-related conditions → 409 Conflict (preconditions)
        0x22 => BackendError::SessionRequired("appropriate session".to_string()),
        0x7E => BackendError::SessionRequired(
            "subfunction not supported in current session".to_string(),
        ),
        0x7F => {
            BackendError::SessionRequired("service not supported in current session".to_string())
        }

        // Security access denied → 403 Forbidden
        0x33 => BackendError::SecurityRequired(1),

        // Rate limiting → 429 Too Many Requests
        0x36 => BackendError::RateLimited("Exceeded number of attempts".to_string()),
        0x37 => BackendError::RateLimited("Required time delay not expired".to_string()),

        // All other NRCs → ECU error response (502 with SOVD-compliant format)
        _ => BackendError::EcuError {
            nrc,
            sid: service,
            message: format!("Negative response: {} (NRC 0x{:02X})", message, nrc),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uds::NegativeResponseCode;

    #[test]
    fn test_security_access_denied_conversion() {
        let uds_err = UdsError::NegativeResponse {
            service_id: 0x22,
            nrc: NegativeResponseCode::SecurityAccessDenied,
        };

        let backend_err: BackendError = uds_err.into();

        match backend_err {
            BackendError::SecurityRequired(level) => {
                assert_eq!(level, 1);
            }
            other => panic!("Expected SecurityRequired, got {:?}", other),
        }
    }
}
