//! DiagnosticBackend trait - the core abstraction for SOVD backends

use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures_core::Stream;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::error::BackendResult;
use crate::models::{
    Capabilities, ClearFaultsResult, CommControlMode, DataPoint, DataValue, DtcSettingMode,
    EntityInfo, Fault, FaultFilter, FaultsResult, IoControlAction, IoControlResult,
    LinkControlResult, LinkMode, LogEntry, LogFilter, LogPage, OperationExecution, OperationInfo,
    OutputDetail, OutputInfo, ParameterInfo, SecurityMode, SessionMode,
};

/// Byte stream for streaming package upload (HTTP/1.1 chunked transfer).
///
/// Used by `receive_package_stream` to avoid buffering the full firmware
/// payload in memory. Each `Bytes` chunk is forwarded or processed inline.
pub type PackageStream =
    Pin<Box<dyn Stream<Item = Result<Bytes, Box<dyn std::error::Error + Send + Sync>>> + Send>>;

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

/// Runtime status of an entity — ISO 17978-3 §7.19.2, Table 281.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum EntityStatus {
    /// Entity is able to answer SOVD requests.
    #[default]
    Ready,
    /// Entity cannot (yet) answer SOVD requests (e.g. restarting / down).
    NotReady,
}

/// Response body of `GET /{entity}/status` — ISO 17978-3 §7.19.2, Table 280: the
/// standard `status` (+ control-resource links), plus a vendor-extension
/// passthrough (§5.4.5; e.g. `x-sumo-runtime { boot_count, uptime_s }`) the
/// backend supplies. SOVDd core stays spec-pure — it never authors `x-sumo-*`
/// fields, only flattens whatever the backend returns.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EntityStatusBody {
    /// Standard runtime status (M).
    pub status: EntityStatus,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub start: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub restart: Vec<String>,
    #[serde(
        rename = "force-restart",
        skip_serializing_if = "Vec::is_empty",
        default
    )]
    pub force_restart: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub shutdown: Vec<String>,
    #[serde(
        rename = "force-shutdown",
        skip_serializing_if = "Vec::is_empty",
        default
    )]
    pub force_shutdown: Vec<String>,
    /// Vendor extensions (e.g. `x-sumo-runtime`) supplied verbatim by the backend.
    #[serde(flatten)]
    pub extensions: serde_json::Map<String, serde_json::Value>,
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
/// Common transfer prefix:
///
/// ```text
/// Queued → Preparing → Transferring → AwaitingActivation ◀── invalidate() ──┐
///                                         │                                  │
///                          finalize_flash()│                                 │
///                                         │                                  │
///                              optional: validate()                          │
///                                         ▼                                  │
///                                     Validated ─────────────────────────────┘
///                                         │
///                                   activate()
/// ```
///
/// Branch on `supports_rollback`:
///
/// **Dual-bank** (`supports_rollback = true`): activation requires a reboot,
/// then the component runs its own post-reset health check before declaring
/// the new firmware ready for the commit/rollback decision.
///
/// ```text
///                                         │  activate()
///                                         ▼
///                                  AwaitingReboot
///                                         │  ecu_reset() (or auto-detect)
///                                         ▼
///                                    Verifying  (post-reset health check)
///                                         │  component-driven
///                                         ▼
///                                     Activated  (trial mode)
///                                    /         \
///                          commit() /           \ rollback()
///                                  ▼             ▼
///                              Committed      RolledBack
/// ```
///
/// **Single-bank** (`supports_rollback = false`): the activation event is
/// the artifact write itself — no reboot, no trial, no Verifying step. The
/// lifecycle still passes through `Activated` so the orchestrator and viewer
/// observe the "new artifact in effect" moment, then `Complete` after
/// `commit_flash()`.
///
/// ```text
///                                         │  activate()
///                                         ▼
///                                     Activated
///                                         │  commit_flash()
///                                         ▼
///                                     Complete
/// ```
///
/// `validate()` and `Validated` are opt-in. The classic flow
/// (`finalize_flash()` → `AwaitingReboot` or `Activated`) still works for
/// callers that don't need the explicit validation step. New orchestrators
/// can use `validate()` for re-runnable crypto checks (e.g. multi-cycle
/// fleet campaigns) and `invalidate()` to demote `Validated` back to
/// `AwaitingActivation` when re-validation is required after a power cycle.
///
/// # Abort rules
///
/// - **Abortable** (via `abort_flash`): `Queued`, `Preparing`, `Transferring`, `AwaitingActivation`, `Validated`
/// - **Not abortable**: `Complete`, `Failed`, `AwaitingReboot`, `Verifying`, `Activated`, `Committed`, `RolledBack`
/// - After `AwaitingReboot`, call `ecu_reset()` to activate, then `rollback_flash()` to revert.
/// - Use `rollback_flash()` to revert firmware in the `Activated` state.
/// - Use `invalidate()` to demote `Validated` back to `AwaitingActivation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlashState {
    /// Component has never been flashed via OTA — factory-fresh / pristine.
    /// Distinguishes the initial post-deploy state from `Complete` (which
    /// implies a transfer ran). Not a state reachable from the flash flow;
    /// `start_flash()` may move directly from `Initial` to `Queued`.
    Initial,
    /// Transfer is queued, waiting to start. **Abortable.**
    Queued,
    /// Preparing for transfer (session, security, erase). **Abortable.**
    Preparing,
    /// Actively transferring data blocks (UDS 0x36). **Abortable.**
    Transferring,
    /// Transfer complete, waiting for `finalize_flash()`. **Abortable.**
    AwaitingActivation,
    /// Firmware cryptographically validated, awaiting `activate()`.
    /// **Abortable** via `invalidate()` (back to `AwaitingActivation`) or
    /// `abort_flash()`. Reachable only via the opt-in `validate()` flow;
    /// classic `finalize_flash()` skips this state.
    Validated,
    /// Firmware written, awaiting ECU reset to activate. **Not abortable.**
    AwaitingReboot,
    /// Post-reset health check in progress. The new firmware has been
    /// reached but the component is still verifying it (e.g. waiting
    /// for the guest VM to boot and report healthy). The component
    /// itself transitions out of this state into `Activated` when the
    /// check passes — orchestrators just poll.
    /// **Not abortable** — call `rollback_flash()` once `Activated` if
    /// the trial firmware doesn't pan out.
    Verifying,
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
            FlashState::Initial => "initial",
            FlashState::Queued => "queued",
            FlashState::Preparing => "preparing",
            FlashState::Transferring => "transferring",
            FlashState::AwaitingActivation => "awaiting_activation",
            FlashState::Validated => "validated",
            FlashState::AwaitingReboot => "awaiting_reboot",
            FlashState::Verifying => "verifying",
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
            "initial" => Ok(FlashState::Initial),
            "queued" => Ok(FlashState::Queued),
            "preparing" => Ok(FlashState::Preparing),
            "transferring" => Ok(FlashState::Transferring),
            "awaiting_activation" => Ok(FlashState::AwaitingActivation),
            "validated" => Ok(FlashState::Validated),
            "awaiting_reboot" => Ok(FlashState::AwaitingReboot),
            "verifying" => Ok(FlashState::Verifying),
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
    /// Which kind of reset is needed to activate a newly-staged image.
    /// Orchestrator reads this to decide between per-component
    /// `PUT components/{id}/status/restart` (Local) and a coalesced
    /// `PUT {ecu-path}/status/restart` (RequiresEcuReset). `#[serde(default)]`
    /// keeps older SOVD payloads deserialising — they get `Local` which
    /// matches their actual behaviour before the field existed.
    /// Spec: ISO 17978-3 §7.19 + CDA §8.7.
    #[serde(default)]
    pub reset_kind: ResetKind,
}

/// Reset class declared per-component to let the orchestrator coalesce.
///
/// - `None`: activation needs no reset (HSM keystore swap, container
///   hot-reload).
/// - `Local`: activation cycles the component itself (qvm restart,
///   container restart, daemon SIGHUP). Orchestrator PUTs the component's
///   own `status/restart`. **Default** — most components fall here.
/// - `RequiresEcuReset`: activation requires rebooting the parent ECU
///   because the newly-staged image only runs after a host boot (M7
///   firmware via m7loader, host-OS IFS via Dev/Partition activators).
///   Orchestrator coalesces all `RequiresEcuReset` components into one
///   `PUT {ecu-path}/status/restart`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResetKind {
    None,
    #[default]
    Local,
    RequiresEcuReset,
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
/// - `UdsBackend` (sovd-uds) - traditional ECUs via CAN/ISO-TP or DoIP
/// - `GatewayBackend` (sovd-gateway) - federates child backends behind one entity
/// - `SovdProxyBackend` (sovd-proxy) - forwards to a remote SOVD server over HTTP
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

    /// Read the entity's runtime status — ISO 17978-3 §7.19.2 (`GET .../status`).
    /// Backends that can report readiness (and optionally vendor `x-sumo-*`
    /// runtime fields like a monotonic boot/restart counter) override this; the
    /// default assumes a reachable entity is `Ready` with no vendor extras.
    async fn read_entity_status(&self) -> BackendResult<EntityStatusBody> {
        Ok(EntityStatusBody {
            status: EntityStatus::Ready,
            ..Default::default()
        })
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

    /// Get one page of logs plus pagination cursors (SOVD §7.21 + our cursor
    /// extension — see `tasks/log-retrieval-design.md`).
    ///
    /// Default: delegate to [`get_logs`](Self::get_logs) and return the whole
    /// result as a single terminal page (`next_cursor: None`). A backend with a
    /// monotonic ordering key (journald `__CURSOR`, or a host `(boot,gen,offset)`)
    /// overrides this to page reboot-safely: honour `filter.after`, set
    /// `next_cursor` until the head is reached, and report `oldest_cursor` so a
    /// caller can detect history that rotated away. Because the default never
    /// sets `next_cursor`, a client's "loop until next_cursor is None" terminates
    /// immediately against a non-paging backend — no behaviour change for them.
    async fn get_logs_paged(&self, filter: &LogFilter) -> BackendResult<LogPage> {
        Ok(LogPage {
            items: self.get_logs(filter).await?,
            next_cursor: None,
            oldest_cursor: None,
        })
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

    /// Describe an update package for the ISO 17978-3 §7.18.3 `/updates`
    /// catalog (Table 261 detail body).
    ///
    /// Invoked lazily on `GET /updates/{id}` — by which point all bulk-data
    /// parts (including the `"manifest"` part) have been uploaded, so a
    /// package-format-aware backend (e.g. SUIT/vm-mgr) can re-read the staged
    /// manifest via the part `file_id`s and fill in `update_name`,
    /// `affected_components`, `size`, etc.
    ///
    /// The default impl is format-agnostic: it builds the descriptor from
    /// what the client declared in the register body plus derivable values
    /// (`size` summed from uploaded parts). Override to enrich from a parsed
    /// manifest, starting from [`default_descriptor_from_context`] as the base.
    async fn describe_update_package(
        &self,
        ctx: &UpdatePackageContext<'_>,
    ) -> BackendResult<UpdatePackageDescriptor> {
        Ok(default_descriptor_from_context(ctx))
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

    /// Receive a software package from a streaming upload.
    ///
    /// Per ASAM SOVD, large uploads use HTTP/1.1 chunked transfer encoding.
    /// The stream delivers the payload in chunks without buffering the full
    /// body in memory. Backends that support streaming should override this;
    /// the default collects the stream to bytes and delegates to `receive_package`.
    async fn receive_package_stream(
        &self,
        mut stream: PackageStream,
        _content_length: Option<u64>,
    ) -> BackendResult<String> {
        // Default: collect stream to bytes, delegate to buffered receive_package
        let mut data = Vec::new();
        loop {
            let chunk = std::future::poll_fn(|cx| stream.as_mut().poll_next(cx)).await;
            match chunk {
                Some(Ok(bytes)) => data.extend_from_slice(&bytes),
                Some(Err(e)) => {
                    return Err(crate::error::BackendError::Internal(format!(
                        "stream read error: {e}"
                    )));
                }
                None => break,
            }
        }
        self.receive_package(&data).await
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

    /// Re-verify a single uploaded part by its file_id (returned from
    /// `receive_package_stream`) against the SHA-256 the wire recorded
    /// at upload time.
    ///
    /// Used by `/updates /executions{verify}` to confirm that every
    /// uploaded part (manifest plus any detached payloads) still
    /// matches what was streamed in — re-read from disk for payloads,
    /// re-hashed from in-memory for the manifest.  Catches on-disk
    /// corruption between upload and finalize.
    ///
    /// Returns Ok if the recomputed hash matches.  Default impl is
    /// `NotSupported`; backends that route detached payloads through
    /// streaming should override.
    async fn verify_part(&self, file_id: &str, expected_sha256: &str) -> BackendResult<()> {
        let _ = (file_id, expected_sha256);
        Err(crate::error::BackendError::NotSupported(
            "verify_part".to_string(),
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

    /// Start a flash transfer session.
    ///
    /// Initiates a session. After this, file uploads are processed in order:
    /// 1. First upload = SUIT manifest (parsed, validated)
    /// 2. Subsequent uploads = payloads in component order (streamed to bank)
    ///
    /// Returns the transfer ID for monitoring progress.
    async fn start_flash(&self) -> BackendResult<String> {
        Err(crate::error::BackendError::NotSupported(
            "start_flash".to_string(),
        ))
    }

    /// Describe the update shape so the `/updates` execute path can
    /// order Banked-vs-Singleshot finalize/commit correctly per ISO 17978-3
    /// §7.13 (and `tasks/sw-update-architecture.md` §5).
    ///
    /// Return one of:
    ///
    /// - `"banked"` — A/B + trial + rollback (host-os, VMs, RT core, …).
    ///   `finalize` flips a boot pointer; `commit` raises the security
    ///   floor after the trial boot confirms health; `rollback` reverts.
    /// - `"singleshot"` — write-through, no rollback (HSM keystore,
    ///   container app, config-only).  `finalize` writes live; `commit`
    ///   is bookkeeping; there is no `rollback`.
    /// - `"unknown"` (default) — backend doesn't advertise; the execute
    ///   path treats it as Banked (non-singleshot).
    ///
    /// Implementations override this when their lifecycle is one of the
    /// two named shapes.  Returning the string keeps the trait wire-only
    /// (no need for a typed enum across the crate boundary).
    fn update_shape(&self) -> &'static str {
        "unknown"
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
    /// `Transferring`, `AwaitingActivation`, or `Validated`. Returns
    /// `InvalidRequest` for post-finalize states (`AwaitingReboot`,
    /// `Activated`, `Committed`, `RolledBack`, `Complete`). Use `ecu_reset()`
    /// then `rollback_flash()` to revert firmware after finalization.
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
    /// Only valid when transfer state is `AwaitingActivation`. This is a
    /// point-of-no-return on the ECU side — once 0x37 is sent and
    /// acknowledged, the firmware is written to the ECU's flash.
    ///
    /// If `supports_rollback` is enabled, state transitions to `AwaitingReboot`
    /// (ECU must reboot before commit/rollback). Otherwise, transitions to `Complete`.
    async fn finalize_flash(&self) -> BackendResult<()> {
        Err(crate::error::BackendError::NotSupported(
            "finalize_flash".to_string(),
        ))
    }

    /// Validate a staged firmware artifact (crypto + signature checks).
    ///
    /// Idempotent and re-runnable — useful for multi-cycle fleet campaigns
    /// where an inactive bank may need re-validation across power cycles
    /// before activation. Implementations should re-read the inactive bank
    /// (or the staged artifact) and re-verify the SUIT signature, image
    /// digest, and any platform-specific seal.
    ///
    /// Only valid in `AwaitingActivation`. Transitions to `Validated` on
    /// success. On failure, the implementation may transition to `Failed`
    /// or remain in `AwaitingActivation` (caller should re-finalize and
    /// retry, or abort).
    async fn validate(&self) -> BackendResult<()> {
        Err(crate::error::BackendError::NotSupported(
            "validate".to_string(),
        ))
    }

    /// Demote a previously-validated artifact back to `AwaitingActivation`.
    ///
    /// Required when hardware sealing isn't available and a power cycle
    /// could have tampered with the staged bank — the orchestrator forces
    /// re-validation after Clamp-15 cycles by calling `invalidate()` then
    /// `validate()` before activation can resume.
    ///
    /// Only valid in `Validated`. Transitions to `AwaitingActivation`.
    async fn invalidate(&self) -> BackendResult<()> {
        Err(crate::error::BackendError::NotSupported(
            "invalidate".to_string(),
        ))
    }

    /// Activate a validated firmware artifact.
    ///
    /// For dual-bank components (`supports_rollback = true`), schedules
    /// the bank pointer swap and transitions to `AwaitingReboot` — the
    /// caller must then issue `ecu_reset()` to complete activation.
    ///
    /// For single-bank components (`supports_rollback = false`), the
    /// activation event is the artifact write itself — transitions
    /// directly to `Activated`. The caller should then `commit_flash()`
    /// to reach `Complete`.
    ///
    /// Only valid in `Validated`.
    async fn activate(&self) -> BackendResult<()> {
        Err(crate::error::BackendError::NotSupported(
            "activate".to_string(),
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

    /// Get current communication-control mode (UDS CommunicationControl 0x28).
    ///
    /// 0x28 is write-only on the wire — there is no UDS read for the active
    /// communication state — so the backend returns the value it last set
    /// (initial = `enable-rx-tx`). `supported` lists the ECU-specific
    /// subfunction enum per ISO 17978-3 §8.3.4 / Table 343.
    async fn get_communication_control(&self) -> BackendResult<CommControlMode> {
        Err(crate::error::BackendError::NotSupported(
            "get_communication_control".to_string(),
        ))
    }

    /// Set communication-control mode (UDS CommunicationControl 0x28).
    ///
    /// `value` is one of the kebab-case enum members the backend advertises
    /// in [`CommControlMode::supported`] (e.g. `disable-rx-tx`). An unknown
    /// value maps to [`crate::error::BackendError::InvalidRequest`] (→ 400).
    async fn set_communication_control(&self, value: &str) -> BackendResult<CommControlMode> {
        let _ = value;
        Err(crate::error::BackendError::NotSupported(
            "set_communication_control".to_string(),
        ))
    }

    /// Get current DTC-setting mode (UDS ControlDTCSetting 0x85).
    ///
    /// Write-only on the wire (like 0x28) — returns the last-set value
    /// (initial = `on`). Per ISO 17978-3 §8.3.5 the enum is `on`/`off`.
    async fn get_dtc_setting(&self) -> BackendResult<DtcSettingMode> {
        Err(crate::error::BackendError::NotSupported(
            "get_dtc_setting".to_string(),
        ))
    }

    /// Set DTC-setting mode (UDS ControlDTCSetting 0x85).
    ///
    /// `value` is `on` (0x01) or `off` (0x02). Anything else maps to
    /// [`crate::error::BackendError::InvalidRequest`] (→ 400).
    async fn set_dtc_setting(&self, value: &str) -> BackendResult<DtcSettingMode> {
        let _ = value;
        Err(crate::error::BackendError::NotSupported(
            "set_dtc_setting".to_string(),
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

/// ISO 17978-3 §7.18.3 Table 261 — detail body for one update package
/// (`GET /updates/{update-package-id}`).
///
/// Mandatory fields: `id`, `update_name`, `size`. Everything else is optional
/// and omitted from the wire when `None`/empty.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UpdatePackageDescriptor {
    /// Identifier for the update package (Table 261 `id`).
    pub id: String,
    /// Human-readable name of the update.
    pub update_name: String,
    /// Download size in KILOBYTES (Table 261 — "defined in kilo bytes").
    pub size: u64,
    /// Whether the package can be installed automatedly. Table 261 defaults
    /// this to `false` when absent; omitted here when not known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub automated: Option<bool>,
    /// Origins (Table 254) for which the package is applicable. Empty = any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub origin: Vec<String>,
    /// Release notes (free text or a URI).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// Estimated time (seconds) the vehicle is unavailable during install.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<u64>,
    /// Preconditions to install the update (e.g. "parking").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preconditions: Option<String>,
    /// Components/apps added by the update (entity-path uri-references).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added_components: Vec<String>,
    /// Components/apps removed by the update.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed_components: Vec<String>,
    /// Components/apps whose version changed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub updated_components: Vec<String>,
    /// Components/apps with changed user-perceived behaviour.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub affected_components: Vec<String>,
}

/// One uploaded bulk-data part, as seen by [`DiagnosticBackend::describe_update_package`].
#[derive(Debug, Clone, Copy)]
pub struct UpdatePartRef<'a> {
    /// The part identifier (e.g. `"manifest"` or a payload uri).
    pub part_id: &'a str,
    /// Uploaded size in bytes.
    pub size: u64,
    /// SHA-256 recorded at upload.
    pub sha256: &'a str,
    /// Backend handle from `receive_package_stream` (re-read key).
    pub file_id: &'a str,
}

/// Read-model the API layer hands to [`DiagnosticBackend::describe_update_package`].
///
/// Carries what the SOVD `/updates` layer knows about a registered update —
/// the stable package id, the addressed component, the client's register body
/// (verbatim), and the uploaded parts — so a backend can build the Table 261
/// descriptor without sovd-core depending on the API layer's update store.
#[derive(Debug, Clone, Copy)]
pub struct UpdatePackageContext<'a> {
    /// Stable package id (URL key / Table 261 `id`).
    pub id: &'a str,
    /// The component the update targets (entity-path leaf).
    pub component_id: &'a str,
    /// What the client declared in the `POST /updates` body, verbatim.
    pub register_body: Option<&'a serde_json::Value>,
    /// Bulk-data parts uploaded so far.
    pub parts: &'a [UpdatePartRef<'a>],
}

/// Default [`UpdatePackageDescriptor`] for a context: format-agnostic, built
/// from client-declared fields in the register body plus derivable values
/// (`size` summed from uploaded parts, bytes → KiB rounded up). SUIT-aware
/// backends call this as a base and override the fields they can enrich.
pub fn default_descriptor_from_context(ctx: &UpdatePackageContext<'_>) -> UpdatePackageDescriptor {
    let body = ctx.register_body;
    let body_str = |k: &str| {
        body.and_then(|b| b.get(k))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };
    let body_u64 = |k: &str| body.and_then(|b| b.get(k)).and_then(|v| v.as_u64());
    let total_bytes: u64 = ctx.parts.iter().map(|p| p.size).sum();
    // Table 261 `size` is in kilobytes; round up so a sub-1KiB package isn't 0.
    let size_kib = total_bytes.div_ceil(1024);
    UpdatePackageDescriptor {
        id: ctx.id.to_string(),
        update_name: body_str("update_name").unwrap_or_else(|| ctx.id.to_string()),
        size: body_u64("size").unwrap_or(size_kib),
        automated: body
            .and_then(|b| b.get("automated"))
            .and_then(|v| v.as_bool()),
        // SOVDd's `/updates` tracks workshop-pushed staging sessions → proximity.
        origin: vec!["proximity".to_string()],
        notes: body_str("notes"),
        duration: body_u64("duration"),
        preconditions: body_str("preconditions"),
        added_components: Vec::new(),
        removed_components: Vec::new(),
        updated_components: Vec::new(),
        // Default: the addressed component is the one affected.
        affected_components: vec![format!("/vehicle/v1/components/{}", ctx.component_id)],
    }
}
