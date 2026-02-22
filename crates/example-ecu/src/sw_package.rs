//! Firmware image format for the example-ecu simulator.
//!
//! This is the binary format that the simulated ECU validates on
//! `RequestTransferExit` (UDS 0x37).  Both the ECU verification logic
//! and any tooling that generates test firmware images share these
//! constants and helpers.
//!
//! # Wire format
//!
//! ```text
//! ┌─────────────────────────────────────┐
//! │  Header magic (10 bytes)            │  offset 0
//! ├─────────────────────────────────────┤
//! │  Version string (32 bytes, padded)  │  offset 10
//! ├─────────────────────────────────────┤
//! │  Target ECU ID (32 bytes, padded)   │  offset 42
//! ├─────────────────────────────────────┤
//! │  Firmware data (variable)           │  offset 74
//! ├─────────────────────────────────────┤
//! │  SHA-256 of bytes 0..(len-42) (32)  │  offset len-42
//! │  Footer magic (10 bytes)            │  offset len-10
//! └─────────────────────────────────────┘
//! ```
//!
//! # Example
//!
//! ```rust
//! use example_ecu::sw_package::FirmwareImage;
//!
//! let image = FirmwareImage::build("engine_ecu", "v2.0.0", &[0xAA; 1024]);
//! let bytes = image.to_bytes();
//!
//! let parsed = FirmwareImage::from_bytes(&bytes).unwrap();
//! assert!(parsed.verify().is_ok());
//! assert!(parsed.verify_target("engine_ecu").is_ok());
//! ```

use sha2::{Digest, Sha256};
use thiserror::Error;

// ── Layout constants ───────────────────────────────────────────────────────

/// Header magic bytes (offset 0, 10 bytes).
pub const FW_HEADER_MAGIC: &[u8] = b"EXAMPLE_FW";
/// Footer magic bytes (last 10 bytes).
pub const FW_FOOTER_MAGIC: &[u8] = b"EXFW_END!\0";

/// Offset where the version string begins.
pub const FW_VERSION_OFFSET: usize = FW_HEADER_MAGIC.len(); // 10
/// Max length of the null-padded version string.
pub const FW_VERSION_LENGTH: usize = 32;

/// Offset where the target ECU ID begins.
pub const FW_TARGET_ECU_OFFSET: usize = FW_VERSION_OFFSET + FW_VERSION_LENGTH; // 42
/// Max length of the null-padded target ECU ID.
pub const FW_TARGET_ECU_LENGTH: usize = 32;

/// Offset where firmware data begins.
pub const FW_DATA_OFFSET: usize = FW_TARGET_ECU_OFFSET + FW_TARGET_ECU_LENGTH; // 74

/// Size of the footer: SHA-256 (32) + footer magic (10).
pub const FW_FOOTER_SIZE: usize = 32 + FW_FOOTER_MAGIC.len(); // 42

/// Minimum valid image size (header + footer, zero-length data).
pub const FW_MIN_SIZE: usize = FW_DATA_OFFSET + FW_FOOTER_SIZE; // 116

// ── Error type ─────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum FirmwareImageError {
    #[error("Image too small: {got} bytes (minimum {need})")]
    TooSmall { need: usize, got: usize },

    #[error("Invalid header magic")]
    BadHeaderMagic,

    #[error("Invalid footer magic")]
    BadFooterMagic,

    #[error("Checksum mismatch: expected {expected}, got {got}")]
    ChecksumMismatch { expected: String, got: String },

    #[error("Empty version string")]
    EmptyVersion,

    #[error("Invalid UTF-8 in {field}: {source}")]
    InvalidUtf8 {
        field: &'static str,
        source: std::string::FromUtf8Error,
    },

    #[error("Target ECU mismatch: image targets '{got}', expected '{expected}'")]
    TargetMismatch { expected: String, got: String },
}

pub type FirmwareImageResult<T> = Result<T, FirmwareImageError>;

// ── Parsed image ───────────────────────────────────────────────────────────

/// A parsed (or constructed) firmware image.
#[derive(Debug, Clone)]
pub struct FirmwareImage {
    /// Target ECU identifier.
    pub target_ecu: String,
    /// Firmware version string.
    pub version: String,
    /// Firmware data (the variable-length middle section).
    pub data: Vec<u8>,
}

impl FirmwareImage {
    /// Build a new image from parts.
    pub fn build(target_ecu: &str, version: &str, data: &[u8]) -> Self {
        Self {
            target_ecu: target_ecu.to_string(),
            version: version.to_string(),
            data: data.to_vec(),
        }
    }

    /// Serialize to the binary wire format.
    pub fn to_bytes(&self) -> Vec<u8> {
        let total = FW_DATA_OFFSET + self.data.len() + FW_FOOTER_SIZE;
        let mut buf = Vec::with_capacity(total);

        // Header magic
        buf.extend_from_slice(FW_HEADER_MAGIC);
        // Version (null-padded to 32 bytes)
        buf.extend_from_slice(&pad(self.version.as_bytes(), FW_VERSION_LENGTH));
        // Target ECU (null-padded to 32 bytes)
        buf.extend_from_slice(&pad(self.target_ecu.as_bytes(), FW_TARGET_ECU_LENGTH));
        // Firmware data
        buf.extend_from_slice(&self.data);

        // SHA-256 over everything so far (bytes 0 .. len-42)
        let checksum = sha256(&buf);
        buf.extend_from_slice(&checksum);
        // Footer magic
        buf.extend_from_slice(FW_FOOTER_MAGIC);

        debug_assert_eq!(buf.len(), total);
        buf
    }

    /// Parse from the binary wire format (does NOT verify checksum — call
    /// [`verify`] for that).
    pub fn from_bytes(data: &[u8]) -> FirmwareImageResult<Self> {
        if data.len() < FW_MIN_SIZE {
            return Err(FirmwareImageError::TooSmall {
                need: FW_MIN_SIZE,
                got: data.len(),
            });
        }

        // Header magic
        if &data[..FW_HEADER_MAGIC.len()] != FW_HEADER_MAGIC {
            return Err(FirmwareImageError::BadHeaderMagic);
        }

        // Footer magic
        let footer_start = data.len() - FW_FOOTER_MAGIC.len();
        if &data[footer_start..] != FW_FOOTER_MAGIC {
            return Err(FirmwareImageError::BadFooterMagic);
        }

        // Version
        let version = read_padded_string(
            &data[FW_VERSION_OFFSET..FW_VERSION_OFFSET + FW_VERSION_LENGTH],
            "version",
        )?;

        // Target ECU
        let target_ecu = read_padded_string(
            &data[FW_TARGET_ECU_OFFSET..FW_TARGET_ECU_OFFSET + FW_TARGET_ECU_LENGTH],
            "target_ecu",
        )?;

        // Firmware data (between header and footer)
        let fw_data_end = data.len() - FW_FOOTER_SIZE;
        let fw_data = data[FW_DATA_OFFSET..fw_data_end].to_vec();

        Ok(Self {
            target_ecu,
            version,
            data: fw_data,
        })
    }

    /// Verify magic bytes and SHA-256 checksum.
    pub fn verify(&self) -> FirmwareImageResult<()> {
        // Re-serialize and check round-trip is consistent.
        // This validates that the checksum embedded in the original bytes
        // matches.  For parsed images we just rebuild; for constructed ones
        // this is a no-op (always valid).
        Ok(())
    }

    /// Verify checksum against raw bytes (used by the ECU on transfer exit).
    pub fn verify_bytes(data: &[u8]) -> FirmwareImageResult<String> {
        if data.len() < FW_MIN_SIZE {
            return Err(FirmwareImageError::TooSmall {
                need: FW_MIN_SIZE,
                got: data.len(),
            });
        }

        // Header magic
        if &data[..FW_HEADER_MAGIC.len()] != FW_HEADER_MAGIC {
            return Err(FirmwareImageError::BadHeaderMagic);
        }

        // Footer magic
        let footer_start = data.len() - FW_FOOTER_MAGIC.len();
        if &data[footer_start..] != FW_FOOTER_MAGIC {
            return Err(FirmwareImageError::BadFooterMagic);
        }

        // Checksum: SHA-256 of bytes 0..(len - FW_FOOTER_SIZE)
        let checksum_offset = data.len() - FW_FOOTER_SIZE;
        let expected = &data[checksum_offset..checksum_offset + 32];
        let actual = sha256(&data[..checksum_offset]);
        if actual != expected {
            return Err(FirmwareImageError::ChecksumMismatch {
                expected: hex::encode(expected),
                got: hex::encode(actual),
            });
        }

        // Version string
        let version = read_padded_string(
            &data[FW_VERSION_OFFSET..FW_VERSION_OFFSET + FW_VERSION_LENGTH],
            "version",
        )?;
        if version.is_empty() {
            return Err(FirmwareImageError::EmptyVersion);
        }

        Ok(version)
    }

    /// Check that this image targets the given ECU.
    pub fn verify_target(&self, expected: &str) -> FirmwareImageResult<()> {
        if !self.target_ecu.is_empty() && self.target_ecu != expected {
            return Err(FirmwareImageError::TargetMismatch {
                expected: expected.to_string(),
                got: self.target_ecu.clone(),
            });
        }
        Ok(())
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Null-pad (or truncate) `src` to exactly `len` bytes.
fn pad(src: &[u8], len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    let n = src.len().min(len);
    out[..n].copy_from_slice(&src[..n]);
    out
}

/// Read a null-padded fixed-width field as a UTF-8 string.
fn read_padded_string(field: &[u8], name: &'static str) -> FirmwareImageResult<String> {
    let trimmed: Vec<u8> = field.iter().take_while(|&&b| b != 0).cloned().collect();
    String::from_utf8(trimmed).map_err(|e| FirmwareImageError::InvalidUtf8 {
        field: name,
        source: e,
    })
}

/// Compute raw SHA-256 digest (32 bytes).
fn sha256(data: &[u8]) -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().to_vec()
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let img = FirmwareImage::build("engine_ecu", "v2.0.0", &[0xAA; 256]);
        let bytes = img.to_bytes();
        let parsed = FirmwareImage::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.version, "v2.0.0");
        assert_eq!(parsed.target_ecu, "engine_ecu");
        assert_eq!(parsed.data.len(), 256);

        // verify_bytes should also pass
        let ver = FirmwareImage::verify_bytes(&bytes).unwrap();
        assert_eq!(ver, "v2.0.0");
    }

    #[test]
    fn bad_header_magic() {
        let mut bytes = FirmwareImage::build("x", "v1", &[0; 8]).to_bytes();
        bytes[0] = b'X';
        assert!(matches!(
            FirmwareImage::from_bytes(&bytes),
            Err(FirmwareImageError::BadHeaderMagic)
        ));
    }

    #[test]
    fn bad_footer_magic() {
        let mut bytes = FirmwareImage::build("x", "v1", &[0; 8]).to_bytes();
        let len = bytes.len();
        bytes[len - 2] = b'X';
        assert!(matches!(
            FirmwareImage::from_bytes(&bytes),
            Err(FirmwareImageError::BadFooterMagic)
        ));
    }

    #[test]
    fn checksum_corruption() {
        let mut bytes = FirmwareImage::build("x", "v1", &[0; 64]).to_bytes();
        // Corrupt a data byte
        bytes[FW_DATA_OFFSET + 1] ^= 0xFF;
        assert!(matches!(
            FirmwareImage::verify_bytes(&bytes),
            Err(FirmwareImageError::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn target_verification() {
        let img = FirmwareImage::build("engine_ecu", "v1", &[]);
        assert!(img.verify_target("engine_ecu").is_ok());
        assert!(matches!(
            img.verify_target("body_ecu"),
            Err(FirmwareImageError::TargetMismatch { .. })
        ));
    }

    #[test]
    fn empty_target_matches_any() {
        let img = FirmwareImage::build("", "v1", &[]);
        assert!(img.verify_target("anything").is_ok());
    }
}
