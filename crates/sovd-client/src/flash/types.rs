//! Flash client types for file management and transfer operations

use serde::{Deserialize, Serialize};

// =============================================================================
// File Management Types (Phase 1: Upload)
// =============================================================================

/// Response from starting a file upload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadResponse {
    /// Upload/file tracking ID (accepts both "upload_id" and "file_id" from server)
    #[serde(alias = "file_id")]
    pub upload_id: String,

    /// Size of uploaded data
    #[serde(default)]
    pub size: Option<usize>,

    /// URL to verify this file (SOVD server response)
    #[serde(default)]
    pub verify_url: Option<String>,

    /// HATEOAS link (SOVD server response)
    #[serde(default)]
    pub href: Option<String>,

    /// Initial state (container-style response)
    #[serde(default)]
    pub state: TransferState,
}

/// File/upload status response
/// Compatible with both container-style (state) and SOVD server (status) responses
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileStatus {
    /// Upload/file ID
    pub id: String,

    /// Current state (container-style uses "state", SOVD server uses "status")
    #[serde(alias = "status")]
    pub state: TransferState,

    /// File size in bytes (SOVD server response)
    #[serde(default)]
    pub size: Option<usize>,

    /// File ID (populated when upload completes)
    #[serde(default)]
    pub file_id: Option<String>,

    /// Progress information
    #[serde(default)]
    pub progress: Option<UploadProgress>,

    /// Error information (if state is "error" or "aborted")
    #[serde(default)]
    pub error: Option<TransferError>,

    /// HATEOAS link (SOVD server response)
    #[serde(default)]
    pub href: Option<String>,

    /// Verify URL (SOVD server response)
    #[serde(default)]
    pub verify_url: Option<String>,
}

/// Upload progress information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadProgress {
    /// Bytes received so far
    pub bytes_received: u64,

    /// Total bytes expected (from Content-Length)
    #[serde(default)]
    pub bytes_total: Option<u64>,

    /// Progress percentage (0-100)
    #[serde(default)]
    pub percent: Option<f64>,
}

/// File information (after upload complete)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileInfo {
    /// File ID
    pub id: String,

    /// Original filename
    #[serde(default)]
    pub filename: Option<String>,

    /// File size in bytes
    pub size: u64,

    /// MIME type
    #[serde(default)]
    pub mimetype: Option<String>,

    /// SHA256 checksum (hex)
    #[serde(default)]
    pub checksum: Option<String>,

    /// Upload timestamp
    #[serde(default)]
    pub uploaded_at: Option<String>,
}

/// File list response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileListResponse {
    /// List of files
    pub files: Vec<FileInfo>,

    /// Total count
    #[serde(default)]
    pub count: Option<usize>,
}

/// File verification request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyRequest {
    /// Expected checksum (optional, for validation)
    #[serde(default)]
    pub expected_checksum: Option<String>,

    /// Checksum algorithm (default: sha256)
    #[serde(default = "default_checksum_algo")]
    pub algorithm: String,
}

fn default_checksum_algo() -> String {
    "sha256".to_string()
}

/// File verification response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyResponse {
    /// Whether verification passed
    pub valid: bool,

    /// Computed checksum
    #[serde(default)]
    pub checksum: Option<String>,

    /// Algorithm used
    #[serde(default)]
    pub algorithm: Option<String>,

    /// Error message if invalid
    #[serde(default)]
    pub error: Option<String>,
}

// =============================================================================
// Flash Transfer Types (Phase 2: ECU Flash)
// =============================================================================

/// Request to start a flash transfer
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartFlashRequest {
    /// File ID to flash
    pub file_id: String,

    /// Target memory address (optional, may be in package header)
    #[serde(default)]
    pub memory_address: Option<u32>,

    /// Block size override (optional)
    #[serde(default)]
    pub block_size: Option<usize>,
}

/// Response from starting a flash transfer
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartFlashResponse {
    /// Transfer tracking ID
    pub transfer_id: String,

    /// URL to check transfer status (SOVD server response)
    #[serde(default)]
    pub status_url: Option<String>,

    /// URL to finalize the transfer (SOVD server response)
    #[serde(default)]
    pub finalize_url: Option<String>,

    /// Initial state (container-style response)
    #[serde(default)]
    pub state: TransferState,

    /// Expected number of blocks (container-style response)
    #[serde(default)]
    pub total_blocks: Option<u32>,
}

/// Response for listing flash transfers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferListResponse {
    /// List of transfers
    pub transfers: Vec<TransferListItem>,
}

/// Individual transfer in a list response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferListItem {
    /// Transfer ID
    pub transfer_id: String,

    /// Package ID
    #[serde(default)]
    pub package_id: Option<String>,

    /// Current state
    pub state: TransferState,

    /// Error information
    #[serde(default, deserialize_with = "deserialize_error_field")]
    pub error: Option<TransferError>,

    /// HATEOAS link
    #[serde(default)]
    pub href: Option<String>,
}

/// Flash transfer status response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlashTransferStatus {
    /// Transfer ID (accepts both "id" and "transfer_id" from server)
    #[serde(alias = "transfer_id")]
    pub id: String,

    /// Current state
    pub state: TransferState,

    /// Progress information
    #[serde(default)]
    pub progress: Option<FlashProgress>,

    /// Error information (accepts both "error" as string or object)
    #[serde(default, deserialize_with = "deserialize_error_field")]
    pub error: Option<TransferError>,

    /// File/package being flashed (accepts both "file_id" and "package_id")
    #[serde(default, alias = "package_id")]
    pub file_id: Option<String>,

    /// HATEOAS link (SOVD server response)
    #[serde(default)]
    pub href: Option<String>,
}

/// Flash progress information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlashProgress {
    /// Blocks transferred so far
    pub blocks_transferred: u32,

    /// Total blocks to transfer
    pub blocks_total: u32,

    /// Bytes acknowledged by ECU
    #[serde(default)]
    pub bytes_acknowledged: Option<u64>,

    /// Current memory address being written
    #[serde(default)]
    pub current_address: Option<String>,

    /// Progress percentage (0-100)
    #[serde(default)]
    pub percent: Option<f64>,

    /// Next block sequence counter
    #[serde(default)]
    pub next_block_counter: Option<u8>,
}

/// Transfer exit response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferExitResponse {
    /// Whether exit was successful
    pub success: bool,

    /// Final state
    #[serde(default)]
    pub state: TransferState,

    /// Total bytes transferred
    #[serde(default)]
    pub total_bytes: Option<u64>,

    /// Message
    #[serde(default)]
    pub message: Option<String>,
}

/// Response from flash commit or rollback
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitRollbackResponse {
    /// Whether the operation succeeded
    pub success: bool,

    /// Status message
    #[serde(default)]
    pub message: Option<String>,
}

/// Activation state response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivationStateResponse {
    /// Whether this ECU supports rollback
    pub supports_rollback: bool,

    /// Current activation state
    pub state: String,

    /// Currently active firmware version
    #[serde(default)]
    pub active_version: Option<String>,

    /// Previous firmware version (available for rollback)
    #[serde(default)]
    pub previous_version: Option<String>,
}

/// ECU reset request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResetRequest {
    /// Reset type: "hard", "soft", "key_off_on"
    #[serde(default = "default_reset_type")]
    pub reset_type: String,
}

fn default_reset_type() -> String {
    "hard".to_string()
}

/// ECU reset response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResetResponse {
    /// Whether reset was initiated
    pub success: bool,

    /// Reset type performed
    pub reset_type: String,

    /// Message
    #[serde(default)]
    pub message: Option<String>,
}

// =============================================================================
// Common Types
// =============================================================================

/// Transfer state (used for both upload and flash)
/// Accepts states from both container-style and SOVD server responses
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TransferState {
    /// Waiting to start
    #[default]
    Queued,

    /// Preparing for transfer (session, security, erase)
    Preparing,

    /// Actively transferring data (SOVD server state)
    Transferring,

    /// In progress (container-style)
    Running,

    /// Completed successfully (container-style)
    Finished,

    /// Completed successfully (SOVD server state)
    Complete,

    /// Waiting for explicit exit (flash only)
    AwaitingExit,

    /// Firmware written, awaiting ECU reset to activate
    AwaitingReset,

    /// Aborted by user or error
    Aborted,

    /// Error occurred (container-style)
    Error,

    /// Failed (SOVD server state)
    Failed,

    /// Firmware activated, pending commit or rollback
    Activated,

    /// Firmware committed (made permanent)
    Committed,

    /// Firmware rolled back to previous version
    RolledBack,

    // PackageStatus values from SOVD server file API
    /// Package received, not yet verified (SOVD server file status)
    #[serde(alias = "Pending")]
    Pending,

    /// Package verified successfully (SOVD server file status)
    #[serde(alias = "Verified")]
    Verified,

    /// Package verification failed (SOVD server file status)
    #[serde(alias = "Invalid")]
    Invalid,
}

impl TransferState {
    /// Check if the transfer is complete (finished or error)
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Finished
                | Self::Complete
                | Self::Aborted
                | Self::Error
                | Self::Failed
                | Self::Verified
                | Self::Invalid
                | Self::Committed
                | Self::RolledBack
        )
    }

    /// Check if the transfer is still in progress
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            Self::Queued | Self::Preparing | Self::Transferring | Self::Running | Self::Pending
        )
    }

    /// Check if the transfer succeeded
    pub fn is_success(&self) -> bool {
        matches!(
            self,
            Self::Finished
                | Self::Complete
                | Self::AwaitingExit
                | Self::AwaitingReset
                | Self::Verified
                | Self::Activated
                | Self::Committed
        )
    }

    /// Check if the transfer failed
    pub fn is_failed(&self) -> bool {
        matches!(
            self,
            Self::Error | Self::Failed | Self::Aborted | Self::Invalid
        )
    }
}

/// Custom deserializer for error field that accepts both string and object
fn deserialize_error_field<'de, D>(deserializer: D) -> Result<Option<TransferError>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, Visitor};

    struct ErrorFieldVisitor;

    impl<'de> Visitor<'de> for ErrorFieldVisitor {
        type Value = Option<TransferError>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("null, a string, or an error object")
        }

        fn visit_none<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(None)
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(None)
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(Some(TransferError {
                code: None,
                message: value.to_string(),
                nrc: None,
                details: None,
            }))
        }

        fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(Some(TransferError {
                code: None,
                message: value,
                nrc: None,
                details: None,
            }))
        }

        fn visit_map<M>(self, map: M) -> Result<Self::Value, M::Error>
        where
            M: de::MapAccess<'de>,
        {
            let error = TransferError::deserialize(de::value::MapAccessDeserializer::new(map))?;
            Ok(Some(error))
        }
    }

    deserializer.deserialize_any(ErrorFieldVisitor)
}

/// Transfer error information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferError {
    /// Error code
    #[serde(default)]
    pub code: Option<String>,

    /// Error message
    pub message: String,

    /// UDS NRC (Negative Response Code) if applicable
    #[serde(default)]
    pub nrc: Option<u8>,

    /// Additional details
    #[serde(default)]
    pub details: Option<serde_json::Value>,
}

// =============================================================================
// Discovery Types (Option 4: Self-describing API)
// =============================================================================

/// Client configuration served by discovery endpoint
/// GET /.well-known/flash-client
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryResponse {
    /// API version
    pub version: String,

    /// Server type identifier
    #[serde(default)]
    pub server_type: Option<String>,

    /// Endpoint definitions
    pub endpoints: DiscoveryEndpoints,

    /// Authentication requirements
    #[serde(default)]
    pub auth: Option<AuthConfig>,

    /// Capabilities
    #[serde(default)]
    pub capabilities: Option<Capabilities>,
}

/// Endpoint definitions from discovery
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryEndpoints {
    /// File management endpoints
    pub files: EndpointSet,

    /// Flash operation endpoints
    pub flash: EndpointSet,
}

/// Set of endpoints for a resource type
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndpointSet {
    /// List/base endpoint
    #[serde(default)]
    pub list: Option<EndpointDef>,

    /// Create/upload endpoint
    #[serde(default)]
    pub create: Option<EndpointDef>,

    /// Get single item endpoint
    #[serde(default)]
    pub get: Option<EndpointDef>,

    /// Delete endpoint
    #[serde(default)]
    pub delete: Option<EndpointDef>,

    /// Additional named endpoints
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, EndpointDef>,
}

/// Single endpoint definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndpointDef {
    /// HTTP method
    pub method: String,

    /// Path template (may include {id} placeholders)
    pub path: String,

    /// Description
    #[serde(default)]
    pub description: Option<String>,
}

/// Authentication configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    /// Auth type: "api_key", "bearer", "none"
    #[serde(rename = "type")]
    pub auth_type: String,

    /// Header name for API key auth
    #[serde(default)]
    pub header: Option<String>,
}

/// Server capabilities
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capabilities {
    /// Supports async upload with progress
    #[serde(default)]
    pub async_upload: bool,

    /// Supports file verification
    #[serde(default)]
    pub verification: bool,

    /// Supports transfer resume
    #[serde(default)]
    pub resume: bool,

    /// Maximum file size in bytes
    #[serde(default)]
    pub max_file_size: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transfer_state() {
        assert!(!TransferState::Queued.is_terminal());
        assert!(!TransferState::Running.is_terminal());
        assert!(TransferState::Finished.is_terminal());
        assert!(TransferState::Error.is_terminal());

        assert!(TransferState::Queued.is_active());
        assert!(TransferState::Running.is_active());
        assert!(!TransferState::Finished.is_active());

        assert!(TransferState::Finished.is_success());
        assert!(TransferState::AwaitingExit.is_success());
        assert!(!TransferState::Error.is_success());
    }

    #[test]
    fn test_state_serialization() {
        let state = TransferState::Running;
        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(json, "\"running\"");

        let parsed: TransferState = serde_json::from_str("\"finished\"").unwrap();
        assert_eq!(parsed, TransferState::Finished);
    }
}
