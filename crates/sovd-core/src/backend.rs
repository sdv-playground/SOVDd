//! DiagnosticBackend trait - the core abstraction for SOVD backends

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::error::BackendResult;
use crate::models::{
    Capabilities, ClearFaultsResult, DataPoint, DataValue, EntityInfo, Fault, FaultFilter,
    FaultsResult, IoControlAction, IoControlResult, LinkControlResult, LinkMode, LogEntry,
    LogFilter, OperationExecution, OperationInfo, OutputDetail, OutputInfo, ParameterInfo,
    SecurityMode, SessionMode,
};

// =============================================================================
// Package Management Types
// =============================================================================

/// Information about a stored software package
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageInfo {
    /// Unique package identifier
    pub id: String,
    /// Size of the package in bytes
    pub size: usize,
    /// Target ECU identifier (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_ecu: Option<String>,
    /// Software version from package header (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Package status
    pub status: PackageStatus,
    /// When the package was received
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
}

/// Status of a stored package
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageStatus {
    /// Package received, not yet verified
    Pending,
    /// Package verified successfully
    Verified,
    /// Package verification failed
    Invalid,
}

/// Result of package verification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyResult {
    /// Whether the package is valid
    pub valid: bool,
    /// Computed checksum (hex string)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,
    /// Checksum algorithm used (e.g., "crc32", "sha256")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub algorithm: Option<String>,
    /// Error message if verification failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// =============================================================================
// Flash Transfer Types
// =============================================================================

/// Status of a flash transfer operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlashStatus {
    /// Unique transfer identifier
    pub transfer_id: String,
    /// Package being flashed
    pub package_id: String,
    /// Current state of the transfer
    pub state: FlashState,
    /// Progress information (if transferring)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<FlashProgress>,
    /// Error message (if failed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// State of a flash transfer.
///
/// # Lifecycle
///
/// ```text
/// Queued → Preparing → Transferring → AwaitingExit
///                                         │
///                          finalize_flash()│
///                     ┌───────────────────┤
///                     │                    │
///             (no rollback)        (supports_rollback)
///                     │                    │
///                     ▼                    ▼
///                  Complete          AwaitingReset
///                                         │
///                           ecu_reset() or│auto-detect
///                                         ▼
///                                     Activated
///                                    /         \
///                          commit() /           \ rollback()
///                                  ▼             ▼
///                             Committed      RolledBack
/// ```
///
/// # Abort rules
///
/// - **Abortable** (via `abort_flash`): `Queued`, `Preparing`, `Transferring`, `AwaitingExit`
/// - **Not abortable**: `Complete`, `Failed`, `AwaitingReset`, `Activated`, `Committed`, `RolledBack`
/// - After `AwaitingReset`, call `ecu_reset()` to activate, then `rollback_flash()` to revert.
/// - Use `rollback_flash()` to revert firmware in the `Activated` state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlashState {
    /// Transfer is queued, waiting to start. **Abortable.**
    Queued,
    /// Preparing for transfer (session, security, erase). **Abortable.**
    Preparing,
    /// Actively transferring data blocks (UDS 0x36). **Abortable.**
    Transferring,
    /// Transfer complete, waiting for `finalize_flash()`. **Abortable.**
    AwaitingExit,
    /// Firmware written, awaiting ECU reset to activate. **Not abortable.**
    AwaitingReset,
    /// Transfer completed successfully (no rollback support). Terminal.
    Complete,
    /// Transfer failed or was aborted. Terminal.
    Failed,
    /// Firmware activated, pending commit or rollback. **Use `rollback_flash()` to revert.**
    Activated,
    /// Firmware committed (made permanent). Terminal, irreversible.
    Committed,
    /// Firmware rolled back to previous version. Terminal.
    RolledBack,
}

impl std::fmt::Display for FlashState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            FlashState::Queued => "queued",
            FlashState::Preparing => "preparing",
            FlashState::Transferring => "transferring",
            FlashState::AwaitingExit => "awaiting_exit",
            FlashState::AwaitingReset => "awaiting_reset",
            FlashState::Complete => "complete",
            FlashState::Failed => "failed",
            FlashState::Activated => "activated",
            FlashState::Committed => "committed",
            FlashState::RolledBack => "rolled_back",
        };
        f.write_str(s)
    }
}

impl std::str::FromStr for FlashState {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "queued" => Ok(FlashState::Queued),
            "preparing" => Ok(FlashState::Preparing),
            "transferring" => Ok(FlashState::Transferring),
            "awaiting_exit" => Ok(FlashState::AwaitingExit),
            "awaiting_reset" => Ok(FlashState::AwaitingReset),
            "complete" => Ok(FlashState::Complete),
            "failed" => Ok(FlashState::Failed),
            "activated" => Ok(FlashState::Activated),
            "committed" => Ok(FlashState::Committed),
            "rolled_back" => Ok(FlashState::RolledBack),
            _ => Err(format!("Unknown flash state: '{}'", s)),
        }
    }
}

/// Activation state for firmware commit/rollback
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivationState {
    /// Whether this ECU supports rollback
    pub supports_rollback: bool,
    /// Current flash state
    pub state: FlashState,
    /// Currently active firmware version
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_version: Option<String>,
    /// Previous firmware version (available for rollback)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_version: Option<String>,
}

/// Progress information for an active flash transfer
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlashProgress {
    /// Bytes transferred so far
    pub bytes_transferred: u64,
    /// Total bytes to transfer
    pub bytes_total: u64,
    /// Number of blocks transferred
    pub blocks_transferred: u32,
    /// Total number of blocks
    pub blocks_total: u32,
    /// Progress percentage (0.0 - 100.0)
    pub percent: f64,
}

/// The core trait that all diagnostic backends implement.
///
/// This abstraction allows the same SOVD REST API to be served by different backends:
/// - `UdsBackend` - For traditional ECUs via CAN/UDS
/// - `HpcBackend` - For HPC nodes with containers, logs, metrics
/// - `ContainerBackend` - For individual containers as sub-entities
///
/// Backends can leave default implementations for features they don't support.
#[async_trait]
pub trait DiagnosticBackend: Send + Sync {
    // =========================================================================
    // Entity Information
    // =========================================================================

    /// Get information about this entity
    fn entity_info(&self) -> &EntityInfo;

    /// Get capabilities of this entity
    fn capabilities(&self) -> &Capabilities;

    // =========================================================================
    // Data Access
    // =========================================================================

    /// List available data parameters
    async fn list_parameters(&self) -> BackendResult<Vec<ParameterInfo>>;

    /// Read one or more data parameters
    async fn read_data(&self, param_ids: &[String]) -> BackendResult<Vec<DataValue>>;

    /// Write a data parameter (if supported)
    async fn write_data(&self, param_id: &str, value: &[u8]) -> BackendResult<()> {
        let _ = (param_id, value);
        Err(crate::error::BackendError::NotSupported(
            "write_data".to_string(),
        ))
    }

    /// Read raw bytes from a DID (for dynamic/generic access)
    async fn read_raw_did(&self, did: u16) -> BackendResult<Vec<u8>> {
        let _ = did;
        Err(crate::error::BackendError::NotSupported(
            "read_raw_did".to_string(),
        ))
    }

    /// Write raw bytes to a DID (for dynamic/generic access)
    async fn write_raw_did(&self, did: u16, data: &[u8]) -> BackendResult<()> {
        let _ = (did, data);
        Err(crate::error::BackendError::NotSupported(
            "write_raw_did".to_string(),
        ))
    }

    /// Define a dynamic data identifier (DDID)
    /// Sources are tuples of (source_did, position, size)
    async fn define_data_identifier(
        &self,
        ddid: u16,
        sources: &[(u16, u8, u8)],
    ) -> BackendResult<()> {
        let _ = (ddid, sources);
        Err(crate::error::BackendError::NotSupported(
            "define_data_identifier".to_string(),
        ))
    }

    /// Clear a dynamic data identifier
    async fn clear_data_identifier(&self, ddid: u16) -> BackendResult<()> {
        let _ = ddid;
        Err(crate::error::BackendError::NotSupported(
            "clear_data_identifier".to_string(),
        ))
    }

    /// Request ECU reset (UDS 0x11)
    /// Returns optional power down time in seconds
    async fn ecu_reset(&self, reset_type: u8) -> BackendResult<Option<u8>> {
        let _ = reset_type;
        Err(crate::error::BackendError::NotSupported(
            "ecu_reset".to_string(),
        ))
    }

    /// Subscribe to data parameter updates
    async fn subscribe_data(
        &self,
        _param_ids: &[String],
        _rate_hz: u32,
    ) -> BackendResult<broadcast::Receiver<DataPoint>> {
        Err(crate::error::BackendError::NotSupported(
            "subscribe_data".to_string(),
        ))
    }

    // =========================================================================
    // Faults
    // =========================================================================

    /// Get faults/DTCs
    async fn get_faults(&self, filter: Option<&FaultFilter>) -> BackendResult<FaultsResult>;

    /// Get detailed information about a specific fault
    async fn get_fault_detail(&self, fault_id: &str) -> BackendResult<Fault> {
        let result = self.get_faults(None).await?;
        result
            .faults
            .into_iter()
            .find(|f| f.id == fault_id)
            .ok_or_else(|| crate::error::BackendError::EntityNotFound(fault_id.to_string()))
    }

    /// Clear faults (if supported)
    async fn clear_faults(&self, _group: Option<u32>) -> BackendResult<ClearFaultsResult> {
        Err(crate::error::BackendError::NotSupported(
            "clear_faults".to_string(),
        ))
    }

    // =========================================================================
    // Logs (primarily for HPC backends and message passing)
    // =========================================================================

    /// Get logs (default: empty, override for HPC)
    async fn get_logs(&self, _filter: &LogFilter) -> BackendResult<Vec<LogEntry>> {
        Ok(vec![])
    }

    /// Get a single log entry by ID
    async fn get_log(&self, log_id: &str) -> BackendResult<LogEntry> {
        let _ = log_id;
        Err(crate::error::BackendError::EntityNotFound(
            log_id.to_string(),
        ))
    }

    /// Get binary content of a log entry (for large dumps)
    /// Returns the raw bytes of the log content
    async fn get_log_content(&self, log_id: &str) -> BackendResult<Vec<u8>> {
        let _ = log_id;
        Err(crate::error::BackendError::NotSupported(
            "get_log_content".to_string(),
        ))
    }

    /// Delete/acknowledge a log entry
    /// Used to clean up after successful retrieval in message passing pattern
    async fn delete_log(&self, log_id: &str) -> BackendResult<()> {
        let _ = log_id;
        Err(crate::error::BackendError::NotSupported(
            "delete_log".to_string(),
        ))
    }

    /// Stream logs in real-time (default: not supported)
    async fn stream_logs(
        &self,
        _filter: &LogFilter,
    ) -> BackendResult<broadcast::Receiver<LogEntry>> {
        Err(crate::error::BackendError::NotSupported(
            "stream_logs".to_string(),
        ))
    }

    // =========================================================================
    // Operations
    // =========================================================================

    /// List available operations
    async fn list_operations(&self) -> BackendResult<Vec<OperationInfo>>;

    /// Start an operation
    async fn start_operation(
        &self,
        operation_id: &str,
        params: &[u8],
    ) -> BackendResult<OperationExecution>;

    /// Get status of a running operation
    async fn get_operation_status(&self, execution_id: &str) -> BackendResult<OperationExecution> {
        let _ = execution_id;
        Err(crate::error::BackendError::NotSupported(
            "get_operation_status".to_string(),
        ))
    }

    /// Stop a running operation
    async fn stop_operation(&self, execution_id: &str) -> BackendResult<()> {
        let _ = execution_id;
        Err(crate::error::BackendError::NotSupported(
            "stop_operation".to_string(),
        ))
    }

    // =========================================================================
    // I/O Control (Outputs)
    // =========================================================================

    /// List available I/O outputs
    async fn list_outputs(&self) -> BackendResult<Vec<OutputInfo>> {
        Ok(vec![])
    }

    /// Get detailed output information
    async fn get_output(&self, output_id: &str) -> BackendResult<OutputDetail> {
        let _ = output_id;
        Err(crate::error::BackendError::OutputNotFound(
            output_id.to_string(),
        ))
    }

    /// Control an output (UDS 0x2F)
    ///
    /// The `value` parameter carries the original JSON value from the API
    /// request.  Leaf backends (UDS) are responsible for encoding it to raw
    /// bytes using their output config.  Gateway and proxy backends forward
    /// it transparently to the server that owns the config.
    async fn control_output(
        &self,
        output_id: &str,
        action: IoControlAction,
        value: Option<serde_json::Value>,
    ) -> BackendResult<IoControlResult> {
        let _ = (output_id, action, value);
        Err(crate::error::BackendError::NotSupported(
            "control_output".to_string(),
        ))
    }

    // =========================================================================
    // Sub-entities (containers for HPC)
    // =========================================================================

    /// List sub-entities (default: empty)
    async fn list_sub_entities(&self) -> BackendResult<Vec<EntityInfo>> {
        Ok(vec![])
    }

    /// Get a sub-entity backend (default: not found)
    async fn get_sub_entity(&self, id: &str) -> BackendResult<Arc<dyn DiagnosticBackend>> {
        Err(crate::error::BackendError::EntityNotFound(id.to_string()))
    }

    // =========================================================================
    // Software Information
    // =========================================================================

    /// Get software/version information
    async fn get_software_info(&self) -> BackendResult<SoftwareInfo> {
        Err(crate::error::BackendError::NotSupported(
            "get_software_info".to_string(),
        ))
    }

    // =========================================================================
    // Package Management (for async flash flow)
    // =========================================================================

    /// Receive and store a software package
    /// Returns the package ID for subsequent operations
    async fn receive_package(&self, data: &[u8]) -> BackendResult<String> {
        let _ = data;
        Err(crate::error::BackendError::NotSupported(
            "receive_package".to_string(),
        ))
    }

    /// List all stored packages
    async fn list_packages(&self) -> BackendResult<Vec<PackageInfo>> {
        Err(crate::error::BackendError::NotSupported(
            "list_packages".to_string(),
        ))
    }

    /// Get information about a specific package
    async fn get_package(&self, package_id: &str) -> BackendResult<PackageInfo> {
        let _ = package_id;
        Err(crate::error::BackendError::NotSupported(
            "get_package".to_string(),
        ))
    }

    /// Verify a stored package (checksum, format validation)
    async fn verify_package(&self, package_id: &str) -> BackendResult<VerifyResult> {
        let _ = package_id;
        Err(crate::error::BackendError::NotSupported(
            "verify_package".to_string(),
        ))
    }

    /// Delete a stored package
    async fn delete_package(&self, package_id: &str) -> BackendResult<()> {
        let _ = package_id;
        Err(crate::error::BackendError::NotSupported(
            "delete_package".to_string(),
        ))
    }

    // =========================================================================
    // Async Flash Transfer
    // =========================================================================

    /// Start a flash transfer operation
    /// Returns the transfer ID for monitoring progress
    /// The backend handles session/security/erase/transfer internally
    async fn start_flash(&self, package_id: &str) -> BackendResult<String> {
        let _ = package_id;
        Err(crate::error::BackendError::NotSupported(
            "start_flash".to_string(),
        ))
    }

    /// Get status of a flash transfer
    async fn get_flash_status(&self, transfer_id: &str) -> BackendResult<FlashStatus> {
        let _ = transfer_id;
        Err(crate::error::BackendError::NotSupported(
            "get_flash_status".to_string(),
        ))
    }

    /// List all flash transfers (active and completed)
    async fn list_flash_transfers(&self) -> BackendResult<Vec<FlashStatus>> {
        Err(crate::error::BackendError::NotSupported(
            "list_flash_transfers".to_string(),
        ))
    }

    /// Abort an in-progress flash transfer.
    ///
    /// Only valid during active transfer phases: `Queued`, `Preparing`,
    /// `Transferring`, or `AwaitingExit`. Returns `InvalidRequest` for
    /// post-finalize states (`AwaitingReset`, `Activated`, `Committed`,
    /// `RolledBack`, `Complete`). Use `ecu_reset()` then `rollback_flash()`
    /// to revert firmware after finalization.
    ///
    /// Cleanup: aborts the async transfer task, sends UDS 0x37 to the ECU
    /// to clear its download state (errors ignored), sets state to `Failed`.
    async fn abort_flash(&self, transfer_id: &str) -> BackendResult<()> {
        let _ = transfer_id;
        Err(crate::error::BackendError::NotSupported(
            "abort_flash".to_string(),
        ))
    }

    /// Finalize a flash transfer (UDS 0x37 RequestTransferExit).
    ///
    /// Only valid when transfer state is `AwaitingExit`. This is a
    /// point-of-no-return on the ECU side — once 0x37 is sent and
    /// acknowledged, the firmware is written to the ECU's flash.
    ///
    /// If `supports_rollback` is enabled, state transitions to `AwaitingReset`
    /// (ECU must reboot before commit/rollback). Otherwise, transitions to `Complete`.
    async fn finalize_flash(&self) -> BackendResult<()> {
        Err(crate::error::BackendError::NotSupported(
            "finalize_flash".to_string(),
        ))
    }

    /// Commit activated firmware, making it permanent.
    ///
    /// Only valid when activation state is `Activated`. Sends the
    /// configured commit routine via UDS RoutineControl (0x31).
    /// After commit, the firmware cannot be rolled back.
    async fn commit_flash(&self) -> BackendResult<()> {
        Err(crate::error::BackendError::NotSupported(
            "commit_flash".to_string(),
        ))
    }

    /// Rollback activated firmware to the previous version.
    ///
    /// Only valid when activation state is `Activated`. Sends the
    /// configured rollback routine via UDS RoutineControl (0x31).
    /// This is the correct way to abort a firmware update after
    /// finalization — `abort_flash()` is not valid in this state.
    async fn rollback_flash(&self) -> BackendResult<()> {
        Err(crate::error::BackendError::NotSupported(
            "rollback_flash".to_string(),
        ))
    }

    /// Get firmware activation state for commit/rollback flow.
    ///
    /// Returns the current activation state including whether rollback
    /// is supported, the current flash state, and the active/previous
    /// firmware versions. Only available when `supports_rollback = true`.
    async fn get_activation_state(&self) -> BackendResult<ActivationState> {
        Err(crate::error::BackendError::NotSupported(
            "get_activation_state".to_string(),
        ))
    }

    // =========================================================================
    // Mode Control (Session, Security, Link)
    // =========================================================================

    /// Get current session mode
    async fn get_session_mode(&self) -> BackendResult<SessionMode> {
        Err(crate::error::BackendError::NotSupported(
            "get_session_mode".to_string(),
        ))
    }

    /// Change diagnostic session
    async fn set_session_mode(&self, session: &str) -> BackendResult<SessionMode> {
        let _ = session;
        Err(crate::error::BackendError::NotSupported(
            "set_session_mode".to_string(),
        ))
    }

    /// Get current security mode
    async fn get_security_mode(&self) -> BackendResult<SecurityMode> {
        Err(crate::error::BackendError::NotSupported(
            "get_security_mode".to_string(),
        ))
    }

    /// Request security seed or send key
    /// - value like "level1_requestseed" requests a seed
    /// - value like "level1" with key sends the key
    async fn set_security_mode(
        &self,
        value: &str,
        key: Option<&[u8]>,
    ) -> BackendResult<SecurityMode> {
        let _ = (value, key);
        Err(crate::error::BackendError::NotSupported(
            "set_security_mode".to_string(),
        ))
    }

    /// Get current link status
    async fn get_link_mode(&self) -> BackendResult<LinkMode> {
        Err(crate::error::BackendError::NotSupported(
            "get_link_mode".to_string(),
        ))
    }

    /// Control link baud rate
    /// - action: "verify_fixed", "verify_specific", or "transition"
    async fn set_link_mode(
        &self,
        action: &str,
        baud_rate_id: Option<&str>,
        baud_rate: Option<u32>,
    ) -> BackendResult<LinkControlResult> {
        let _ = (action, baud_rate_id, baud_rate);
        Err(crate::error::BackendError::NotSupported(
            "set_link_mode".to_string(),
        ))
    }
}

/// Software/version information
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SoftwareInfo {
    /// Software version string
    pub version: String,
    /// Additional version details
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}
