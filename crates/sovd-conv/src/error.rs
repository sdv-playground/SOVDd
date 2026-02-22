//! Error types for DID conversion

use thiserror::Error;

/// Errors that can occur during DID conversion
#[derive(Debug, Error)]
pub enum ConvError {
    /// DID not found in store
    #[error("unknown DID: 0x{0:04X}")]
    UnknownDid(u16),

    /// Invalid DID string format
    #[error("invalid DID format: {0}")]
    InvalidDidFormat(String),

    /// Data too short for the expected type
    #[error("data too short: expected {expected} bytes, got {actual}")]
    DataTooShort { expected: usize, actual: usize },

    /// Invalid data for the type
    #[error("invalid data: {0}")]
    InvalidData(String),

    /// Value out of range for encoding
    #[error("value out of range: {value} not in [{min}, {max}]")]
    ValueOutOfRange { value: f64, min: f64, max: f64 },

    /// YAML parsing error
    #[error("YAML parse error: {0}")]
    YamlError(#[from] serde_yaml::Error),

    /// IO error
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    /// JSON error
    #[error("JSON error: {0}")]
    JsonError(#[from] serde_json::Error),
}

/// Result type for DID conversion operations
pub type ConvResult<T> = Result<T, ConvError>;

/// Parse a DID string (hex) to u16
///
/// Accepts formats: "F405", "0xF405", "0XF405", "f405"
pub fn parse_did(s: &str) -> ConvResult<u16> {
    let s = s.trim().trim_start_matches("0x").trim_start_matches("0X");
    u16::from_str_radix(s, 16).map_err(|_| ConvError::InvalidDidFormat(s.to_string()))
}

/// Format a DID as hex string (uppercase, no prefix)
pub fn format_did(did: u16) -> String {
    format!("{:04X}", did)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_did() {
        assert_eq!(parse_did("F405").unwrap(), 0xF405);
        assert_eq!(parse_did("0xF405").unwrap(), 0xF405);
        assert_eq!(parse_did("0XF405").unwrap(), 0xF405);
        assert_eq!(parse_did("f405").unwrap(), 0xF405);
        assert_eq!(parse_did("  F405  ").unwrap(), 0xF405);
        assert!(parse_did("invalid").is_err());
        assert!(parse_did("FFFFF").is_err()); // Too large
    }

    #[test]
    fn test_format_did() {
        assert_eq!(format_did(0xF405), "F405");
        assert_eq!(format_did(0x0001), "0001");
        assert_eq!(format_did(0xFFFF), "FFFF");
    }
}
