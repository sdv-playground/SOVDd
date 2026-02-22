//! UDS Backend implementation
//!
//! This module provides the UdsBackend that implements DiagnosticBackend
//! for traditional ECUs accessible via UDS over CAN/ISO-TP.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use parking_lot::RwLock;
use sovd_core::{
    ActivationState, BackendError, BackendResult, Capabilities, ClearFaultsResult, DataPoint,
    DataValue, DiagnosticBackend, EntityInfo, Fault, FaultFilter, FaultSeverity, FaultsResult,
    FlashProgress, FlashState, FlashStatus, IoControlAction, IoControlResult, LinkControlResult,
    LinkMode, LogEntry, LogFilter, OperationExecution, OperationInfo, OperationStatus,
    OutputDetail, OutputInfo, PackageInfo, PackageStatus, ParameterInfo, SecurityMode,
    SecurityState, SessionMode, SoftwareInfo, VerifyResult,
};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::config::{FlashCommitConfig, UdsBackendConfig};
use crate::error::UdsBackendError;
use crate::output_conv;
use crate::session::SessionManager;
use crate::subscription::StreamManager;
use crate::transport::{create_transport, TransportAdapter};
use crate::uds::{
    dtc::{parse_dtc_by_status_mask_response, status_bit, Dtc},
    link_baud_rate, ServiceIds, UdsService,
};

// =============================================================================
// I/O Control State Tracking (tester-side bookkeeping)
// =============================================================================

/// Tracks the current control mode for an I/O output.
///
/// ISO 14229 does not define a way to query which outputs are under tester
/// control — the tester must track its own commands. This enum represents
/// the state after the last successful 0x2F command for a given IOID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoControlState {
    /// ECU application has control (after ReturnControlToECU)
    EcuControlled,
    /// Tester has overridden the output (ShortTermAdjustment active)
    TesterControlled,
    /// Output value is frozen at its current instantaneous value
    Frozen,
    /// Output was reset to OEM default (tester-initiated, but not an override)
    DefaultReset,
}

// =============================================================================
// Internal Package and Flash State
// =============================================================================

/// A stored software package
struct StoredPackage {
    id: String,
    data: Vec<u8>,
    status: PackageStatus,
    created_at: chrono::DateTime<Utc>,
}

/// State of an active flash transfer
struct FlashTransfer {
    id: String,
    package_id: String,
    state: FlashState,
    progress: FlashProgress,
    error: Option<String>,
    /// Handle to abort the transfer task
    abort_handle: Option<tokio::task::AbortHandle>,
}

/// UDS diagnostic backend
///
/// Implements the DiagnosticBackend trait for ECUs accessible via UDS over CAN/ISO-TP.
pub struct UdsBackend {
    /// Backend configuration
    config: UdsBackendConfig,
    /// Entity information
    entity_info: EntityInfo,
    /// Capabilities
    capabilities: Capabilities,
    /// Transport adapter for UDS communication (kept alive via Arc)
    #[allow(dead_code)]
    transport: Arc<dyn TransportAdapter>,
    /// UDS service layer
    uds: UdsService,
    /// Session manager for keepalive and security
    session_manager: Arc<SessionManager>,
    /// Stream manager for periodic data subscriptions
    stream_manager: Arc<StreamManager>,
    /// Stored software packages (for async flash flow)
    packages: Arc<RwLock<HashMap<String, StoredPackage>>>,
    /// Current flash transfer state
    flash_state: Arc<RwLock<Option<FlashTransfer>>>,
    /// Per-output I/O control state (tester-side bookkeeping).
    /// Key is the IOID (u16). Cleared on session change per ISO 14229.
    io_control_states: Arc<RwLock<HashMap<u16, IoControlState>>>,
    /// Firmware activation state for commit/rollback flow
    activation_state: Arc<RwLock<ActivationState>>,
    /// Flash commit/rollback configuration
    flash_commit_config: FlashCommitConfig,
}

impl UdsBackend {
    /// Create a new UDS backend from configuration
    pub async fn new(config: UdsBackendConfig) -> Result<Self, UdsBackendError> {
        let entity_info = EntityInfo {
            id: config.id.clone(),
            name: config.name.clone(),
            entity_type: "ecu".to_string(),
            description: config.description.clone(),
            href: format!("/vehicle/v1/components/{}", config.id),
            status: Some("connected".to_string()),
        };

        let capabilities = Capabilities::uds_ecu();

        // Create transport from configuration
        let transport = create_transport(&config.transport)
            .await
            .map_err(|e| UdsBackendError::Transport(e.to_string()))?;

        // Create service IDs with any OEM overrides
        let service_ids = ServiceIds::from_overrides(&config.service_overrides);

        // Create UDS service layer
        let uds = UdsService::with_service_ids(transport.clone(), service_ids);

        // Create session manager
        let session_manager = Arc::new(SessionManager::with_service_ids(
            transport.clone(),
            config.sessions.clone(),
            service_ids,
        ));

        // Create stream manager for periodic data
        let stream_manager = Arc::new(StreamManager::new(transport.clone(), config.clone()));

        let flash_commit_config = config.flash_commit.clone();
        let activation_state = ActivationState {
            supports_rollback: flash_commit_config.supports_rollback,
            state: FlashState::Complete,
            active_version: None,
            previous_version: None,
        };

        Ok(Self {
            config,
            entity_info,
            capabilities,
            transport,
            uds,
            session_manager,
            stream_manager,
            packages: Arc::new(RwLock::new(HashMap::new())),
            flash_state: Arc::new(RwLock::new(None)),
            io_control_states: Arc::new(RwLock::new(HashMap::new())),
            activation_state: Arc::new(RwLock::new(activation_state)),
            flash_commit_config,
        })
    }

    /// Parse a hex DID string to u16
    fn parse_did(did_str: &str) -> Option<u16> {
        let cleaned = did_str.trim_start_matches("0x").trim_start_matches("0X");
        u16::from_str_radix(cleaned, 16).ok()
    }

    /// Convert a UDS DTC to a SOVD Fault
    fn dtc_to_fault(&self, dtc: &Dtc) -> Fault {
        let severity = if dtc.status.warning_indicator_requested {
            FaultSeverity::Critical
        } else if dtc.status.confirmed_dtc {
            FaultSeverity::Error
        } else {
            FaultSeverity::Warning
        };

        Fault {
            id: dtc.to_id(),
            code: dtc.to_code_string(),
            message: format!("DTC {} - {}", dtc.to_code_string(), dtc.category().prefix()),
            severity,
            category: Some(dtc.category().to_string()),
            active: dtc.status.is_active(),
            occurrence_count: None,
            first_occurrence: None,
            last_occurrence: Some(Utc::now()),
            status: Some(serde_json::json!({
                "raw": format!("0x{:02X}", dtc.status.raw),
                "test_failed": dtc.status.test_failed,
                "confirmed_dtc": dtc.status.confirmed_dtc,
                "pending_dtc": dtc.status.pending_dtc,
                "warning_indicator": dtc.status.warning_indicator_requested,
            })),
            href: format!(
                "/vehicle/v1/components/{}/faults/{}",
                self.config.id,
                dtc.to_id()
            ),
        }
    }

    /// Parse routine ID from hex string
    fn parse_rid(rid_str: &str) -> Result<u16, UdsBackendError> {
        let cleaned = rid_str.trim_start_matches("0x").trim_start_matches("0X");
        u16::from_str_radix(cleaned, 16)
            .map_err(|_| UdsBackendError::Config(format!("Invalid RID: {}", rid_str)))
    }

    /// Parse output ID from hex string
    fn parse_ioid(ioid_str: &str) -> Result<u16, UdsBackendError> {
        let cleaned = ioid_str.trim_start_matches("0x").trim_start_matches("0X");
        u16::from_str_radix(cleaned, 16)
            .map_err(|_| UdsBackendError::Config(format!("Invalid IOID: {}", ioid_str)))
    }

    /// Convert session ID to name
    fn session_id_to_name(&self, session_id: u8) -> String {
        let sessions = &self.config.sessions;
        if session_id == sessions.default_session {
            "default".to_string()
        } else if session_id == sessions.programming_session {
            "programming".to_string()
        } else if session_id == sessions.extended_session {
            "extended".to_string()
        } else if session_id == sessions.engineering_session {
            "engineering".to_string()
        } else {
            // Check custom sessions
            for (name, &id) in &sessions.custom_sessions {
                if id == session_id {
                    return name.clone();
                }
            }
            format!("0x{:02X}", session_id)
        }
    }

    /// Parse session name to UDS session ID
    fn parse_session_name(&self, s: &str) -> Result<u8, BackendError> {
        let sessions = &self.config.sessions;
        let s_lower = s.to_lowercase();

        tracing::debug!(
            "parse_session_name: input={}, default={:#x}, programming={:#x}, extended={:#x}",
            s,
            sessions.default_session,
            sessions.programming_session,
            sessions.extended_session
        );

        match s_lower.as_str() {
            "default" => Ok(sessions.default_session),
            "programming" => Ok(sessions.programming_session),
            "extended" => Ok(sessions.extended_session),
            "engineering" => Ok(sessions.engineering_session),
            _ => {
                // Check custom sessions (e.g., "telematics")
                if let Some(&id) = sessions.custom_sessions.get(&s_lower) {
                    return Ok(id);
                }

                // Try to parse as hex number (e.g., "0x60" or "0x40")
                let s = s.trim();
                if let Some(hex_str) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                    u8::from_str_radix(hex_str, 16).map_err(|_| {
                        BackendError::InvalidRequest(format!("Invalid session: {}", s))
                    })
                } else {
                    // Try as decimal
                    s.parse::<u8>().map_err(|_| {
                        BackendError::InvalidRequest(format!(
                            "Invalid session: {}. Use 'default', 'extended', 'programming', 'engineering', or hex value",
                            s
                        ))
                    })
                }
            }
        }
    }

    /// Parse security level from string like "level1" or "level3"
    fn parse_security_level(&self, s: &str) -> Result<u8, BackendError> {
        let s = s.trim().to_lowercase();
        let level_str = s.strip_prefix("level").unwrap_or(&s);

        level_str
            .parse::<u8>()
            .map_err(|_| BackendError::InvalidRequest(format!("Invalid security level: {}", s)))
    }

    /// Parse baud rate ID string to (UDS ID, actual rate)
    fn parse_baud_rate_id(s: &str) -> Result<(u8, u32), BackendError> {
        match s.to_lowercase().as_str() {
            "125k" | "125000" => Ok((link_baud_rate::CAN_125K, 125000)),
            "250k" | "250000" => Ok((link_baud_rate::CAN_250K, 250000)),
            "500k" | "500000" => Ok((link_baud_rate::CAN_500K, 500000)),
            "1m" | "1000k" | "1000000" => Ok((link_baud_rate::CAN_1M, 1000000)),
            _ => {
                // Try to parse as hex ID
                let s = s.trim_start_matches("0x").trim_start_matches("0X");
                let id = u8::from_str_radix(s, 16).map_err(|_| {
                    BackendError::InvalidRequest(format!("Invalid baud rate ID: {}", s))
                })?;

                let rate = match id {
                    link_baud_rate::CAN_125K => 125000,
                    link_baud_rate::CAN_250K => 250000,
                    link_baud_rate::CAN_500K => 500000,
                    link_baud_rate::CAN_1M => 1000000,
                    _ => {
                        return Err(BackendError::InvalidRequest(format!(
                            "Unknown baud rate ID: 0x{:02X}",
                            id
                        )))
                    }
                };

                Ok((id, rate))
            }
        }
    }
}

#[async_trait]
impl DiagnosticBackend for UdsBackend {
    fn entity_info(&self) -> &EntityInfo {
        &self.entity_info
    }

    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    async fn list_parameters(&self) -> BackendResult<Vec<ParameterInfo>> {
        // Parameters are managed dynamically via the ConversionStore in sovd-api.
        // This returns an empty list - use /admin/conversions to see registered DIDs.
        Ok(vec![])
    }

    async fn read_data(&self, did_strs: &[String]) -> BackendResult<Vec<DataValue>> {
        // Interpret param_ids as DIDs (hex strings like "F405" or "0xF405")
        let mut values = Vec::new();

        for did_str in did_strs {
            let did = Self::parse_did(did_str)
                .ok_or_else(|| BackendError::InvalidRequest(format!("Invalid DID: {}", did_str)))?;

            let raw_bytes = self.read_raw_did(did).await?;

            // Return raw hex - conversions are applied in the API layer
            let raw_hex = hex::encode(&raw_bytes);
            values.push(DataValue {
                id: did_str.to_uppercase(),
                name: did_str.to_uppercase(),
                value: serde_json::json!(&raw_hex),
                unit: None,
                timestamp: Utc::now(),
                raw: Some(raw_hex),
                did: Some(format!("{:04X}", did)),
                length: Some(raw_bytes.len()),
            });
        }

        Ok(values)
    }

    async fn write_data(&self, did_str: &str, value: &[u8]) -> BackendResult<()> {
        // Interpret param_id as DID (hex string like "F405" or "0xF405")
        let did = Self::parse_did(did_str)
            .ok_or_else(|| BackendError::InvalidRequest(format!("Invalid DID: {}", did_str)))?;

        self.write_raw_did(did, value).await
    }

    async fn read_raw_did(&self, did: u16) -> BackendResult<Vec<u8>> {
        debug!(did = format!("0x{:04X}", did), "Reading raw DID");

        // Call UDS ReadDataByIdentifier (0x22)
        let response = self
            .uds
            .read_data_by_id(&[did])
            .await
            .map_err(crate::error::convert_uds_error)?;

        // Parse response: 0x62 [DID_HI] [DID_LO] [DATA...]
        if response.len() < 3 {
            return Err(BackendError::Protocol("Response too short".to_string()));
        }

        // Return just the data bytes (skip service ID and DID echo)
        Ok(response[3..].to_vec())
    }

    async fn write_raw_did(&self, did: u16, data: &[u8]) -> BackendResult<()> {
        debug!(
            did = format!("0x{:04X}", did),
            len = data.len(),
            "Writing raw DID"
        );

        // Call UDS WriteDataByIdentifier (0x2E)
        self.uds
            .write_data_by_id(did, data)
            .await
            .map_err(crate::error::convert_uds_error)?;

        Ok(())
    }

    async fn subscribe_data(
        &self,
        param_ids: &[String],
        rate_hz: u32,
    ) -> BackendResult<broadcast::Receiver<DataPoint>> {
        // Use stream manager for UDS 0x2A periodic data
        self.stream_manager
            .subscribe(param_ids.to_vec(), rate_hz)
            .await
            .map_err(|e| BackendError::Protocol(e.to_string()))
    }

    async fn define_data_identifier(
        &self,
        ddid: u16,
        sources: &[(u16, u8, u8)],
    ) -> BackendResult<()> {
        self.uds
            .define_data_identifier(ddid, sources)
            .await
            .map_err(crate::error::convert_uds_error)
    }

    async fn clear_data_identifier(&self, ddid: u16) -> BackendResult<()> {
        self.uds
            .clear_data_identifier(ddid)
            .await
            .map_err(crate::error::convert_uds_error)
    }

    async fn ecu_reset(&self, reset_type: u8) -> BackendResult<Option<u8>> {
        // ECU reset is special: the ECU may reboot before sending a response,
        // so timeout/transport errors are treated as success (ECU rebooted).
        let result = match self.uds.ecu_reset(reset_type).await {
            Ok(r) => r,
            Err(crate::uds::UdsError::Timeout) | Err(crate::uds::UdsError::Transport(_)) => {
                info!("ECU reset: no response (ECU likely rebooting)");
                None
            }
            Err(e) => return Err(crate::error::convert_uds_error(e)),
        };

        // ECU rebooted → back in default session with security locked
        self.session_manager.notify_ecu_reset().await;

        // If firmware is awaiting reset, transition to Activated now that the ECU has rebooted
        let needs_transition = {
            let activation = self.activation_state.read();
            activation.state == FlashState::AwaitingReset
        };

        if needs_transition {
            {
                let mut activation = self.activation_state.write();
                activation.state = FlashState::Activated;
            }
            {
                let mut flash_state = self.flash_state.write();
                if let Some(ref mut transfer) = *flash_state {
                    transfer.state = FlashState::Activated;
                }
            }
            info!("ECU reset detected AwaitingReset state, transitioned to Activated");
        }

        Ok(result)
    }

    async fn get_faults(&self, filter: Option<&FaultFilter>) -> BackendResult<FaultsResult> {
        // Build status mask based on filter
        let status_mask = match filter {
            Some(f) if f.active_only == Some(true) => status_bit::ACTIVE_MASK,
            _ => 0xFF, // All DTCs
        };

        // Call UDS ReadDTCInformation (0x19) sub-function 0x02
        let response = self
            .uds
            .read_dtc_by_status_mask(status_mask)
            .await
            .map_err(crate::error::convert_uds_error)?;

        // Parse DTC response - returns (status_availability_mask, dtcs)
        let (status_availability_mask, dtcs) =
            parse_dtc_by_status_mask_response(&response).map_err(|e| BackendError::Protocol(e))?;

        // Convert DTCs to Faults
        let mut faults: Vec<Fault> = dtcs.iter().map(|dtc| self.dtc_to_fault(dtc)).collect();

        // Apply additional filters
        if let Some(f) = filter {
            // Filter by active status (test_failed = true)
            if f.active_only == Some(true) {
                faults.retain(|fault| fault.active);
            }
            if let Some(ref severity) = f.severity {
                faults.retain(|fault| &fault.severity == severity);
            }
            if let Some(ref category) = f.category {
                faults.retain(|fault| fault.category.as_ref() == Some(category));
            }
        }

        Ok(FaultsResult {
            faults,
            status_availability_mask: Some(status_availability_mask),
        })
    }

    async fn get_fault_detail(&self, fault_id: &str) -> BackendResult<Fault> {
        // Validate fault ID format by parsing it
        let _dtc_bytes = Dtc::parse_id(fault_id).ok_or_else(|| {
            BackendError::EntityNotFound(format!("Invalid fault ID: {}", fault_id))
        })?;

        // Get all faults and find the one with matching ID
        let result = self.get_faults(None).await?;

        result
            .faults
            .into_iter()
            .find(|f| f.id == fault_id)
            .ok_or_else(|| BackendError::EntityNotFound(format!("Fault not found: {}", fault_id)))
    }

    async fn clear_faults(&self, group: Option<u32>) -> BackendResult<ClearFaultsResult> {
        let dtc_group = group.unwrap_or(0xFFFFFF); // Default to all DTCs

        // Call UDS ClearDiagnosticInformation (0x14)
        self.uds
            .clear_dtc(dtc_group)
            .await
            .map_err(crate::error::convert_uds_error)?;

        Ok(ClearFaultsResult {
            success: true,
            cleared_count: 0, // UDS doesn't return count
            message: format!("Cleared DTCs for group 0x{:06X}", dtc_group),
        })
    }

    async fn get_logs(&self, _filter: &LogFilter) -> BackendResult<Vec<LogEntry>> {
        // ECUs don't typically have logs - this is for HPC backends
        Ok(vec![])
    }

    async fn list_operations(&self) -> BackendResult<Vec<OperationInfo>> {
        Ok(self
            .config
            .operations
            .iter()
            .map(|op| OperationInfo {
                id: op.id.clone(),
                name: op.name.clone(),
                description: op.description.clone(),
                parameters: vec![],
                requires_security: op.security_level > 0,
                security_level: op.security_level,
                href: format!(
                    "/vehicle/v1/components/{}/operations/{}",
                    self.config.id, op.id
                ),
            })
            .collect())
    }

    async fn start_operation(
        &self,
        operation_id: &str,
        params: &[u8],
    ) -> BackendResult<OperationExecution> {
        let op = self
            .config
            .operations
            .iter()
            .find(|o| o.id == operation_id)
            .ok_or_else(|| BackendError::OperationNotFound(operation_id.to_string()))?;

        // Check security level
        if op.security_level > 0 {
            let security_state = self.session_manager.security_state();
            if !security_state.unlocked {
                return Err(BackendError::SecurityRequired(op.security_level));
            }
        }

        // Parse routine ID
        let rid = Self::parse_rid(&op.rid).map_err(|e| BackendError::Protocol(e.to_string()))?;

        // Extract sub-function from first byte (default to start)
        let sub_function = params.first().copied().unwrap_or(0x01);
        let routine_params = if params.len() > 1 { &params[1..] } else { &[] };

        // Call appropriate UDS RoutineControl sub-function
        let result = match sub_function {
            0x01 => {
                // Start Routine
                self.uds
                    .routine_control_start(rid, routine_params)
                    .await
                    .map_err(crate::error::convert_uds_error)?
            }
            0x02 => {
                // Stop Routine
                self.uds
                    .routine_control_stop(rid)
                    .await
                    .map_err(crate::error::convert_uds_error)?
            }
            0x03 => {
                // Request Routine Results
                self.uds
                    .routine_control_result(rid)
                    .await
                    .map_err(crate::error::convert_uds_error)?
            }
            _ => {
                return Err(BackendError::InvalidRequest(format!(
                    "Invalid sub-function: 0x{:02X}",
                    sub_function
                )));
            }
        };

        let execution_id = Uuid::new_v4().to_string();

        Ok(OperationExecution {
            execution_id,
            operation_id: op.id.clone(),
            status: OperationStatus::Completed,
            result: Some(serde_json::json!({
                "routine_result": hex::encode(&result),
            })),
            error: None,
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
        })
    }

    async fn get_operation_status(&self, execution_id: &str) -> BackendResult<OperationExecution> {
        // For UDS routines, we don't track execution state
        // In a full implementation, we'd store running operations
        Err(BackendError::EntityNotFound(format!(
            "Operation execution not found: {}",
            execution_id
        )))
    }

    async fn stop_operation(&self, execution_id: &str) -> BackendResult<()> {
        // For UDS, we'd need to track which routine is running
        Err(BackendError::EntityNotFound(format!(
            "Operation execution not found: {}",
            execution_id
        )))
    }

    // =========================================================================
    // I/O Control (Outputs)
    // =========================================================================

    async fn list_outputs(&self) -> BackendResult<Vec<OutputInfo>> {
        Ok(self
            .config
            .outputs
            .iter()
            .map(|o| OutputInfo {
                id: o.id.clone(),
                name: o.name.clone(),
                output_id: o.ioid.clone(),
                requires_security: o.security_level > 0,
                security_level: o.security_level,
                href: format!("/vehicle/v1/components/{}/outputs/{}", self.config.id, o.id),
                data_type: None,
                unit: None,
            })
            .collect())
    }

    async fn get_output(&self, output_id: &str) -> BackendResult<OutputDetail> {
        let output = self
            .config
            .outputs
            .iter()
            .find(|o| o.id == output_id)
            .ok_or_else(|| BackendError::OutputNotFound(output_id.to_string()))?;

        // Read current value using UDS 0x22 ReadDataByIdentifier — a pure read
        // with no side-effects. Using 0x2F ReturnControlToECU here would release
        // any active tester overrides, which is wrong for a GET request.
        let ioid =
            Self::parse_ioid(&output.ioid).map_err(|e| BackendError::Protocol(e.to_string()))?;

        let current_value = match self.uds.read_data_by_id(&[ioid]).await {
            // 0x22 response: [0x62, DID_hi, DID_lo, data...] — data starts at byte 3
            Ok(response) if response.len() > 3 => hex::encode(&response[3..]),
            _ => output.default_value.clone(), // Fall back to config default
        };

        // Read tester-side control state for this output
        let (controlled_by_tester, frozen) = match self.io_control_states.read().get(&ioid) {
            Some(IoControlState::TesterControlled) => (true, false),
            Some(IoControlState::Frozen) => (true, true),
            Some(IoControlState::DefaultReset) => (false, false),
            Some(IoControlState::EcuControlled) | None => (false, false),
        };

        Ok(OutputDetail {
            id: output.id.clone(),
            name: output.name.clone(),
            output_id: output.ioid.clone(),
            current_value,
            default_value: if output.default_value.is_empty() {
                "00".to_string()
            } else {
                output.default_value.clone()
            },
            controlled_by_tester,
            frozen,
            requires_security: output.security_level > 0,
            security_level: output.security_level,
            value: None,
            default: None,
            data_type: None,
            unit: None,
            min: None,
            max: None,
            allowed: Vec::new(),
        })
    }

    async fn control_output(
        &self,
        output_id: &str,
        action: IoControlAction,
        value: Option<serde_json::Value>,
    ) -> BackendResult<IoControlResult> {
        let output = self
            .config
            .outputs
            .iter()
            .find(|o| o.id == output_id)
            .ok_or_else(|| BackendError::OutputNotFound(output_id.to_string()))?;

        // Check security level
        if output.security_level > 0 {
            let security_state = self.session_manager.security_state();
            if !security_state.unlocked {
                return Err(BackendError::SecurityRequired(output.security_level));
            }
        }

        let ioid =
            Self::parse_ioid(&output.ioid).map_err(|e| BackendError::Protocol(e.to_string()))?;

        let result = match action {
            IoControlAction::ReturnToEcu => self.uds.io_control_return_to_ecu(ioid).await,
            IoControlAction::ResetToDefault => self.uds.io_control_reset_to_default(ioid).await,
            IoControlAction::Freeze => self.uds.io_control_freeze(ioid).await,
            IoControlAction::ShortTermAdjust => {
                let json_val = value.ok_or_else(|| {
                    BackendError::InvalidRequest("Value required for short_term_adjust".to_string())
                })?;
                // Encode JSON value to raw bytes using this output's config
                let data = output_conv::encode_output_value(output, &json_val)
                    .map_err(|e| BackendError::InvalidRequest(format!("Invalid value: {}", e)))?;
                self.uds
                    .io_control_short_term_adjustment(ioid, &data, None)
                    .await
            }
        };

        let action_str = match action {
            IoControlAction::ReturnToEcu => "return_to_ecu",
            IoControlAction::ResetToDefault => "reset_to_default",
            IoControlAction::Freeze => "freeze",
            IoControlAction::ShortTermAdjust => "short_term_adjust",
        };

        // Determine control state based on action
        let io_state = match action {
            IoControlAction::ReturnToEcu => IoControlState::EcuControlled,
            IoControlAction::ResetToDefault => IoControlState::DefaultReset,
            IoControlAction::Freeze => IoControlState::Frozen,
            IoControlAction::ShortTermAdjust => IoControlState::TesterControlled,
        };

        let (controlled_by_tester, frozen) = match io_state {
            IoControlState::EcuControlled | IoControlState::DefaultReset => (false, false),
            IoControlState::TesterControlled => (true, false),
            IoControlState::Frozen => (true, true),
        };

        match result {
            Ok(response) => {
                // Store tester-side control state for this output
                self.io_control_states.write().insert(ioid, io_state);

                // UdsService::io_control_*() already strips the 4-byte UDS header
                // (0x6F, DID_hi, DID_lo, controlParam) and returns only the
                // controlStatusRecord — use it directly.
                let new_value = if !response.is_empty() {
                    Some(hex::encode(&response))
                } else {
                    None
                };
                Ok(IoControlResult {
                    output_id: output_id.to_string(),
                    action: action_str.to_string(),
                    success: true,
                    controlled_by_tester,
                    frozen,
                    new_value,
                    value: None,
                    error: None,
                })
            }
            Err(e) => Ok(IoControlResult {
                output_id: output_id.to_string(),
                action: action_str.to_string(),
                success: false,
                controlled_by_tester: false,
                frozen: false,
                new_value: None,
                value: None,
                error: Some(e.to_string()),
            }),
        }
    }

    async fn get_software_info(&self) -> BackendResult<SoftwareInfo> {
        // Read standard identification DIDs
        let mut details = serde_json::Map::new();

        // Try to read common identification DIDs
        let identification_dids: [(u16, &str); 4] = [
            (0xF190, "vin"),
            (0xF191, "ecu_hw_number"),
            (0xF193, "ecu_hw_version"),
            (0xF195, "supplier_sw_version"),
        ];

        for (did, name) in identification_dids {
            match self.uds.read_data_by_id(&[did]).await {
                Ok(response) if response.len() > 3 => {
                    let data = &response[3..];
                    let value = String::from_utf8_lossy(data).trim().to_string();
                    details.insert(name.to_string(), serde_json::json!(value));
                }
                _ => {
                    // DID not supported or error - skip it
                }
            }
        }

        let version = details
            .get("ecu_sw_version")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        Ok(SoftwareInfo {
            version,
            details: Some(serde_json::Value::Object(details)),
        })
    }

    async fn list_sub_entities(&self) -> BackendResult<Vec<EntityInfo>> {
        // ECUs don't have sub-entities
        Ok(vec![])
    }

    async fn get_sub_entity(&self, id: &str) -> BackendResult<Arc<dyn DiagnosticBackend>> {
        Err(BackendError::EntityNotFound(id.to_string()))
    }

    // =========================================================================
    // Mode Control (Session, Security, Link)
    // =========================================================================

    async fn get_session_mode(&self) -> BackendResult<SessionMode> {
        let session_id = self.session_manager.current_session_id();
        let session_name = self.session_id_to_name(session_id);

        Ok(SessionMode {
            mode: "session".to_string(),
            session: session_name,
            session_id,
        })
    }

    async fn set_session_mode(&self, session: &str) -> BackendResult<SessionMode> {
        let session_id = self.parse_session_name(session)?;

        self.session_manager
            .change_session(session_id)
            .await
            .map_err(|e| BackendError::Protocol(e.to_string()))?;

        // Per ISO 14229: all I/O overrides revert on session change.
        // Clear tester-side bookkeeping to stay in sync.
        let cleared = {
            let mut states = self.io_control_states.write();
            let count = states.len();
            states.clear();
            count
        };
        if cleared > 0 {
            info!(cleared, "Cleared I/O control states on session change");
        }

        let session_name = self.session_id_to_name(session_id);

        Ok(SessionMode {
            mode: "session".to_string(),
            session: session_name,
            session_id,
        })
    }

    async fn get_security_mode(&self) -> BackendResult<SecurityMode> {
        let security_state = self.session_manager.security_state();
        let available_levels = self.session_manager.available_security_levels();

        let (state, level, seed) = if security_state.unlocked {
            (SecurityState::Unlocked, Some(security_state.level), None)
        } else if security_state.pending_seed.is_some() {
            (
                SecurityState::SeedAvailable,
                Some(security_state.level),
                security_state.pending_seed.map(|s| hex::encode(&s)),
            )
        } else {
            (SecurityState::Locked, None, None)
        };

        Ok(SecurityMode {
            mode: "security".to_string(),
            state,
            level,
            available_levels: Some(available_levels),
            seed,
        })
    }

    async fn set_security_mode(
        &self,
        value: &str,
        key: Option<&[u8]>,
    ) -> BackendResult<SecurityMode> {
        let value_lower = value.to_lowercase();

        if value_lower.ends_with("_requestseed") {
            // Request seed flow
            let level_str = value_lower.trim_end_matches("_requestseed");
            let level = self.parse_security_level(level_str)?;

            let seed = self
                .session_manager
                .request_security_seed(level)
                .await
                .map_err(|e| BackendError::Protocol(e.to_string()))?;

            if seed.is_empty() {
                // Already unlocked (zero seed)
                Ok(SecurityMode {
                    mode: "security".to_string(),
                    state: SecurityState::Unlocked,
                    level: Some(level),
                    available_levels: Some(self.session_manager.available_security_levels()),
                    seed: None,
                })
            } else {
                Ok(SecurityMode {
                    mode: "security".to_string(),
                    state: SecurityState::SeedAvailable,
                    level: Some(level),
                    available_levels: None,
                    seed: Some(hex::encode(&seed)),
                })
            }
        } else {
            // Send key flow
            let level = self.parse_security_level(&value_lower)?;

            let key_bytes = key.ok_or_else(|| {
                BackendError::InvalidRequest(
                    "Missing 'key' field - required when sending key".to_string(),
                )
            })?;

            self.session_manager
                .send_security_key(level, key_bytes)
                .await
                .map_err(|e| BackendError::Protocol(e.to_string()))?;

            Ok(SecurityMode {
                mode: "security".to_string(),
                state: SecurityState::Unlocked,
                level: Some(level),
                available_levels: Some(self.session_manager.available_security_levels()),
                seed: None,
            })
        }
    }

    async fn get_link_mode(&self) -> BackendResult<LinkMode> {
        let link_state = self.session_manager.link_state();

        let state_desc = if link_state.pending_baud_rate.is_some() {
            "pending_transition"
        } else {
            "active"
        };

        Ok(LinkMode {
            current_baud_rate: link_state.current_baud_rate,
            pending_baud_rate: link_state.pending_baud_rate,
            link_state: state_desc.to_string(),
        })
    }

    async fn set_link_mode(
        &self,
        action: &str,
        baud_rate_id: Option<&str>,
        baud_rate: Option<u32>,
    ) -> BackendResult<LinkControlResult> {
        match action.to_lowercase().as_str() {
            "verify_fixed" => {
                let baud_rate_str = baud_rate_id.ok_or_else(|| {
                    BackendError::InvalidRequest(
                        "Missing 'baud_rate_id' for verify_fixed action".to_string(),
                    )
                })?;

                let (id, rate) = Self::parse_baud_rate_id(baud_rate_str)?;

                self.uds
                    .link_control_verify_fixed(id)
                    .await
                    .map_err(crate::error::convert_uds_error)?;

                self.session_manager.set_pending_baud_rate(Some(rate));

                Ok(LinkControlResult {
                    success: true,
                    action: "verify_fixed".to_string(),
                    baud_rate: Some(rate),
                    message: format!("Verified baud rate {} bps", rate),
                })
            }
            "verify_specific" => {
                let rate = baud_rate.ok_or_else(|| {
                    BackendError::InvalidRequest(
                        "Missing 'baud_rate' for verify_specific action".to_string(),
                    )
                })?;

                if rate < 10000 || rate > 1000000 {
                    return Err(BackendError::InvalidRequest(format!(
                        "Baud rate {} out of range (10000-1000000)",
                        rate
                    )));
                }

                self.uds
                    .link_control_verify_specific(rate)
                    .await
                    .map_err(crate::error::convert_uds_error)?;

                self.session_manager.set_pending_baud_rate(Some(rate));

                Ok(LinkControlResult {
                    success: true,
                    action: "verify_specific".to_string(),
                    baud_rate: Some(rate),
                    message: format!("Verified baud rate {} bps", rate),
                })
            }
            "transition" => {
                let pending = self.session_manager.link_state().pending_baud_rate;

                if pending.is_none() {
                    return Err(BackendError::InvalidRequest(
                        "No pending baud rate - call verify_fixed or verify_specific first"
                            .to_string(),
                    ));
                }

                self.uds
                    .link_control_transition()
                    .await
                    .map_err(crate::error::convert_uds_error)?;

                let rate = pending.unwrap();
                self.session_manager.set_current_baud_rate(rate);
                self.session_manager.set_pending_baud_rate(None);

                Ok(LinkControlResult {
                    success: true,
                    action: "transition".to_string(),
                    baud_rate: Some(rate),
                    message: format!("Transitioned to {} bps", rate),
                })
            }
            _ => Err(BackendError::InvalidRequest(format!(
                "Unknown action: {}. Use 'verify_fixed', 'verify_specific', or 'transition'",
                action
            ))),
        }
    }

    // =========================================================================
    // Package Management (async flash flow)
    // =========================================================================

    async fn receive_package(&self, data: &[u8]) -> BackendResult<String> {
        let package_id = Uuid::new_v4().to_string();

        let package = StoredPackage {
            id: package_id.clone(),
            data: data.to_vec(),
            status: PackageStatus::Pending,
            created_at: Utc::now(),
        };

        {
            let mut packages = self.packages.write();
            packages.insert(package_id.clone(), package);
        }

        info!(
            package_id = %package_id,
            size = data.len(),
            "Package received and stored"
        );

        Ok(package_id)
    }

    async fn list_packages(&self) -> BackendResult<Vec<PackageInfo>> {
        let packages = self.packages.read();
        Ok(packages
            .values()
            .map(|p| PackageInfo {
                id: p.id.clone(),
                size: p.data.len(),
                target_ecu: Some(self.config.id.clone()),
                version: None,
                status: p.status,
                created_at: Some(p.created_at),
            })
            .collect())
    }

    async fn get_package(&self, package_id: &str) -> BackendResult<PackageInfo> {
        let packages = self.packages.read();
        let package = packages.get(package_id).ok_or_else(|| {
            BackendError::EntityNotFound(format!("Package not found: {}", package_id))
        })?;

        Ok(PackageInfo {
            id: package.id.clone(),
            size: package.data.len(),
            target_ecu: Some(self.config.id.clone()),
            version: None,
            status: package.status,
            created_at: Some(package.created_at),
        })
    }

    async fn verify_package(&self, package_id: &str) -> BackendResult<VerifyResult> {
        let mut packages = self.packages.write();
        let package = packages.get_mut(package_id).ok_or_else(|| {
            BackendError::EntityNotFound(format!("Package not found: {}", package_id))
        })?;

        // Compute CRC-32 checksum
        let crc_alg = crc::Crc::<u32>::new(&crc::CRC_32_ISO_HDLC);
        let crc = crc_alg.checksum(&package.data);
        let checksum = format!("{:08X}", crc);

        // Basic validation: ensure non-empty
        let valid = !package.data.is_empty();

        package.status = if valid {
            PackageStatus::Verified
        } else {
            PackageStatus::Invalid
        };

        info!(
            package_id = %package_id,
            checksum = %checksum,
            valid,
            "Package verified"
        );

        Ok(VerifyResult {
            valid,
            checksum: Some(checksum),
            algorithm: Some("crc32".to_string()),
            error: if valid {
                None
            } else {
                Some("Package is empty".to_string())
            },
        })
    }

    async fn delete_package(&self, package_id: &str) -> BackendResult<()> {
        let mut packages = self.packages.write();
        packages.remove(package_id).ok_or_else(|| {
            BackendError::EntityNotFound(format!("Package not found: {}", package_id))
        })?;

        info!(package_id = %package_id, "Package deleted");
        Ok(())
    }

    // =========================================================================
    // Async Flash Transfer
    // =========================================================================

    async fn start_flash(&self, package_id: &str) -> BackendResult<String> {
        // Check if there's already an active transfer.
        // Allow restart from terminal states and post-transfer states
        // (AwaitingReset/Activated — user may have power-cycled the ECU).
        {
            let flash_state = self.flash_state.read();
            if let Some(ref transfer) = *flash_state {
                if matches!(
                    transfer.state,
                    FlashState::Queued
                        | FlashState::Preparing
                        | FlashState::Transferring
                        | FlashState::AwaitingExit
                ) {
                    return Err(BackendError::InvalidRequest(format!(
                        "Flash transfer already in progress: {}",
                        transfer.id
                    )));
                }
            }
        }

        // Get the package data
        let package_data = {
            let packages = self.packages.read();
            let package = packages.get(package_id).ok_or_else(|| {
                BackendError::EntityNotFound(format!("Package not found: {}", package_id))
            })?;

            if package.status != PackageStatus::Verified {
                return Err(BackendError::InvalidRequest(
                    "Package must be verified before flashing".to_string(),
                ));
            }

            package.data.clone()
        };

        // Capture current SW version before flashing (for rollback support)
        if self.flash_commit_config.supports_rollback {
            // Read DID 0xF189 (ECU Software Version)
            match self.uds.read_data_by_id(&[0xF189]).await {
                Ok(response) if response.len() > 3 => {
                    let version_bytes = &response[3..];
                    let version = String::from_utf8_lossy(version_bytes).trim().to_string();
                    let mut activation = self.activation_state.write();
                    activation.previous_version = Some(version.clone());
                    info!(version = %version, "Captured current SW version for rollback");
                }
                _ => {
                    warn!("Could not read current SW version (DID 0xF189) for rollback");
                }
            }
        }

        // Switch to programming session before flash
        let current = self.session_manager.current_state();
        if !matches!(current, crate::session::SessionState::Programming) {
            self.session_manager
                .change_session(self.config.sessions.programming_session)
                .await
                .map_err(|e| {
                    BackendError::Protocol(format!("Failed to enter programming session: {}", e))
                })?;
        }

        // Security access (UDS 0x27) — unlock ECU if a secret is configured
        if let Some(ref security) = self.config.sessions.security {
            if let Some(ref secret_hex) = security.secret {
                let secret_bytes = hex::decode(secret_hex).map_err(|e| {
                    BackendError::Protocol(format!("Invalid security secret hex: {}", e))
                })?;
                let level = security.level;

                let seed = self
                    .session_manager
                    .request_security_seed(level)
                    .await
                    .map_err(|e| {
                        BackendError::Protocol(format!("Security seed request failed: {}", e))
                    })?;

                if !seed.is_empty() {
                    // Compute key: XOR seed with secret (cycling over secret bytes)
                    let key: Vec<u8> = seed
                        .iter()
                        .enumerate()
                        .map(|(i, b)| b ^ secret_bytes[i % secret_bytes.len()])
                        .collect();

                    self.session_manager
                        .send_security_key(level, &key)
                        .await
                        .map_err(|e| {
                            BackendError::Protocol(format!("Security access failed: {}", e))
                        })?;

                    info!("Security access unlocked for flash");
                }
            }
        }

        let transfer_id = Uuid::new_v4().to_string();
        let package_id = package_id.to_string();
        let data_len = package_data.len() as u64;

        // Create initial transfer state
        let transfer = FlashTransfer {
            id: transfer_id.clone(),
            package_id: package_id.clone(),
            state: FlashState::Queued,
            progress: FlashProgress {
                bytes_transferred: 0,
                bytes_total: data_len,
                blocks_transferred: 0,
                blocks_total: 0,
                percent: 0.0,
            },
            error: None,
            abort_handle: None,
        };

        {
            let mut flash_state = self.flash_state.write();
            *flash_state = Some(transfer);
        }

        // Spawn the flash task
        let uds = self.uds.clone();
        let flash_state = self.flash_state.clone();
        let transfer_id_clone = transfer_id.clone();
        let sessions = self.config.sessions.clone();

        let task = tokio::spawn(async move {
            Self::run_flash_transfer(uds, flash_state, sessions, transfer_id_clone, package_data)
                .await
        });

        // Store the abort handle
        {
            let mut flash_state = self.flash_state.write();
            if let Some(ref mut transfer) = *flash_state {
                transfer.abort_handle = Some(task.abort_handle());
            }
        }

        info!(
            transfer_id = %transfer_id,
            package_id = %package_id,
            size = data_len,
            "Flash transfer started"
        );

        Ok(transfer_id)
    }

    async fn get_flash_status(&self, transfer_id: &str) -> BackendResult<FlashStatus> {
        let flash_state = self.flash_state.read();
        let transfer = flash_state.as_ref().ok_or_else(|| {
            BackendError::EntityNotFound("No flash transfer in progress".to_string())
        })?;

        if transfer.id != transfer_id {
            return Err(BackendError::EntityNotFound(format!(
                "Flash transfer not found: {}",
                transfer_id
            )));
        }

        Ok(FlashStatus {
            transfer_id: transfer.id.clone(),
            package_id: transfer.package_id.clone(),
            state: transfer.state,
            progress: Some(transfer.progress.clone()),
            error: transfer.error.clone(),
        })
    }

    async fn list_flash_transfers(&self) -> BackendResult<Vec<FlashStatus>> {
        let flash_state = self.flash_state.read();
        match &*flash_state {
            Some(transfer) => Ok(vec![FlashStatus {
                transfer_id: transfer.id.clone(),
                package_id: transfer.package_id.clone(),
                state: transfer.state,
                progress: Some(transfer.progress.clone()),
                error: transfer.error.clone(),
            }]),
            None => Ok(vec![]),
        }
    }

    async fn abort_flash(&self, transfer_id: &str) -> BackendResult<()> {
        // First, validate state and abort the task
        {
            let mut flash_state = self.flash_state.write();
            let transfer = flash_state.as_mut().ok_or_else(|| {
                BackendError::EntityNotFound("No flash transfer in progress".to_string())
            })?;

            if transfer.id != transfer_id {
                return Err(BackendError::EntityNotFound(format!(
                    "Flash transfer not found: {}",
                    transfer_id
                )));
            }

            // Only allow abort during active transfer phases.
            // Post-finalize states (AwaitingReset, Activated, Committed, RolledBack, Complete)
            // cannot be aborted — use ecu_reset() then rollback_flash() for post-finalize states.
            if !matches!(
                transfer.state,
                FlashState::Queued
                    | FlashState::Preparing
                    | FlashState::Transferring
                    | FlashState::AwaitingExit
            ) {
                return Err(BackendError::InvalidRequest(format!(
                    "Cannot abort transfer in state {:?}. {}",
                    transfer.state,
                    match transfer.state {
                        FlashState::AwaitingReset => "Firmware is written and awaiting ECU reset. Call ecu_reset() to activate, then rollback_flash() to revert.",
                        FlashState::Activated => "Use rollback_flash() to revert activated firmware.",
                        _ => "Transfer is already in a terminal state.",
                    }
                )));
            }

            // Abort the tokio task if still running
            if let Some(ref handle) = transfer.abort_handle {
                handle.abort();
            }

            transfer.state = FlashState::Failed;
            transfer.error = Some("Transfer aborted by user".to_string());
        }

        // Brief delay to let the aborted task's in-flight CAN messages drain.
        // Without this, our cleanup 0x37 can collide with the killed task's
        // pending UDS response on the bus.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Send RequestTransferExit to ECU to clear its download state.
        // Ignore errors — ECU might not be in a state that accepts this
        // (e.g. abort during Queued before any UDS traffic).
        if let Err(e) = self.uds.request_transfer_exit(&[]).await {
            warn!(transfer_id = %transfer_id, error = %e, "Failed to send transfer exit to ECU during abort");
        }

        // Return to default session so the ECU fully resets its state.
        // Per ISO 14229, session change to default clears active transfers.
        // Without this, a subsequent start_flash re-enters programming session
        // as a no-op and the ECU rejects RequestDownload (NRC 0x22) because
        // it still has residual state from the aborted transfer.
        if let Err(e) = self.session_manager.ensure_default_session().await {
            warn!(transfer_id = %transfer_id, error = %e, "Failed to return to default session after abort");
        }

        warn!(transfer_id = %transfer_id, "Flash transfer aborted");
        Ok(())
    }

    async fn finalize_flash(&self) -> BackendResult<()> {
        // Check transfer state
        {
            let flash_state = self.flash_state.read();
            let transfer = flash_state.as_ref().ok_or_else(|| {
                BackendError::EntityNotFound("No flash transfer in progress".to_string())
            })?;

            if transfer.state != FlashState::AwaitingExit {
                return Err(BackendError::InvalidRequest(format!(
                    "Cannot finalize transfer in state: {:?}",
                    transfer.state
                )));
            }
        }

        // Send UDS RequestTransferExit (0x37)
        self.uds
            .request_transfer_exit(&[])
            .await
            .map(|_| ())
            .map_err(crate::error::convert_uds_error)?;

        // Update state: AwaitingReset if rollback supported (ECU must reboot), otherwise Complete
        let new_state = if self.flash_commit_config.supports_rollback {
            FlashState::AwaitingReset
        } else {
            FlashState::Complete
        };

        {
            let mut flash_state = self.flash_state.write();
            if let Some(ref mut transfer) = *flash_state {
                transfer.state = new_state;
            }
        }

        // Update activation state
        if self.flash_commit_config.supports_rollback {
            let mut activation = self.activation_state.write();
            activation.state = FlashState::AwaitingReset;
            info!("Flash transfer finalized, awaiting ECU reset to activate firmware");
        } else {
            info!("Flash transfer finalized");
        }

        Ok(())
    }

    async fn commit_flash(&self) -> BackendResult<()> {
        if !self.flash_commit_config.supports_rollback {
            return Err(BackendError::NotSupported(
                "commit_flash: rollback not supported for this ECU".to_string(),
            ));
        }

        // Auto-detect reset if still in AwaitingReset (handles external power cycles)
        self.check_activation_transition().await;

        // Validate activation state
        {
            let activation = self.activation_state.read();
            if activation.state != FlashState::Activated {
                return Err(BackendError::InvalidRequest(format!(
                    "Cannot commit: firmware is not in activated state (current: {:?})",
                    activation.state
                )));
            }
        }

        // NOTE: We do NOT force a session change here. The caller is responsible
        // for setting the correct session (e.g., programming) and unlocking security
        // before calling commit. Forcing a session change would reset the ECU's
        // security access state per ISO 14229, causing the commit routine to fail
        // with NRC 0x33 (securityAccessDenied).

        // Call commit routine via UDS RoutineControl
        let commit_rid_str = self
            .flash_commit_config
            .commit_routine
            .as_ref()
            .ok_or_else(|| {
                BackendError::InvalidRequest("No commit routine configured".to_string())
            })?;
        let commit_rid =
            Self::parse_rid(commit_rid_str).map_err(|e| BackendError::Protocol(e.to_string()))?;

        self.uds
            .routine_control_start(commit_rid, &[])
            .await
            .map_err(crate::error::convert_uds_error)?;

        // Transition to Committed
        {
            let mut activation = self.activation_state.write();
            activation.state = FlashState::Committed;
        }
        {
            let mut flash_state = self.flash_state.write();
            if let Some(ref mut transfer) = *flash_state {
                transfer.state = FlashState::Committed;
            }
        }

        info!("Firmware committed successfully");
        Ok(())
    }

    async fn rollback_flash(&self) -> BackendResult<()> {
        if !self.flash_commit_config.supports_rollback {
            return Err(BackendError::NotSupported(
                "rollback_flash: rollback not supported for this ECU".to_string(),
            ));
        }

        // Auto-detect reset if still in AwaitingReset (handles external power cycles)
        self.check_activation_transition().await;

        // Validate activation state
        {
            let activation = self.activation_state.read();
            if activation.state != FlashState::Activated {
                return Err(BackendError::InvalidRequest(format!(
                    "Cannot rollback: firmware is not in activated state (current: {:?})",
                    activation.state
                )));
            }
        }

        // NOTE: We do NOT force a session change here. The caller is responsible
        // for setting the correct session and unlocking security before calling
        // rollback. Forcing a session change would reset the ECU's security
        // access state per ISO 14229.

        // Call rollback routine via UDS RoutineControl
        let rollback_rid_str = self
            .flash_commit_config
            .rollback_routine
            .as_ref()
            .ok_or_else(|| {
                BackendError::InvalidRequest("No rollback routine configured".to_string())
            })?;
        let rollback_rid =
            Self::parse_rid(rollback_rid_str).map_err(|e| BackendError::Protocol(e.to_string()))?;

        self.uds
            .routine_control_start(rollback_rid, &[])
            .await
            .map_err(crate::error::convert_uds_error)?;

        // Transition to RolledBack
        {
            let mut activation = self.activation_state.write();
            activation.state = FlashState::RolledBack;
        }
        {
            let mut flash_state = self.flash_state.write();
            if let Some(ref mut transfer) = *flash_state {
                transfer.state = FlashState::RolledBack;
            }
        }

        info!("Firmware rolled back successfully");
        Ok(())
    }

    async fn get_activation_state(&self) -> BackendResult<ActivationState> {
        if !self.flash_commit_config.supports_rollback {
            return Err(BackendError::NotSupported(
                "get_activation_state: rollback not supported for this ECU".to_string(),
            ));
        }

        let active_version = self.check_activation_transition().await;

        let activation = self.activation_state.read();
        Ok(ActivationState {
            supports_rollback: activation.supports_rollback,
            state: activation.state,
            active_version,
            previous_version: activation.previous_version.clone(),
        })
    }
}

impl UdsBackend {
    /// Read the ECU's current SW version and, if in AwaitingReset state,
    /// auto-detect whether the ECU has rebooted with new firmware.
    ///
    /// This handles both SOVD-triggered resets (ecu_reset already transitions)
    /// and external resets (power cycle, hardware reset) by comparing the
    /// active version against the saved previous version.
    async fn check_activation_transition(&self) -> Option<String> {
        // Read current version from ECU (DID 0xF189)
        let active_version = match self.uds.read_data_by_id(&[0xF189]).await {
            Ok(response) if response.len() > 3 => {
                let version_bytes = &response[3..];
                Some(String::from_utf8_lossy(version_bytes).trim().to_string())
            }
            _ => None,
        };

        // If in AwaitingReset and version differs from previous, the ECU has
        // rebooted with new firmware — transition to Activated
        let needs_transition = {
            let activation = self.activation_state.read();
            if activation.state == FlashState::AwaitingReset {
                if let (Some(ref active), Some(ref previous)) =
                    (&active_version, &activation.previous_version)
                {
                    active != previous
                } else {
                    false
                }
            } else {
                false
            }
        };

        if needs_transition {
            {
                let mut activation = self.activation_state.write();
                activation.state = FlashState::Activated;
            }
            {
                let mut flash_state = self.flash_state.write();
                if let Some(ref mut transfer) = *flash_state {
                    transfer.state = FlashState::Activated;
                }
            }
            // ECU rebooted → back in default session with security locked
            self.session_manager.notify_ecu_reset().await;
            info!("Auto-detected ECU reset via version change, transitioned to Activated");
        }

        active_version
    }
}

impl UdsBackend {
    /// Internal method to run the flash transfer process
    async fn run_flash_transfer(
        uds: UdsService,
        flash_state: Arc<RwLock<Option<FlashTransfer>>>,
        sessions: crate::config::SessionConfig,
        transfer_id: String,
        data: Vec<u8>,
    ) {
        // Helper to update state
        let update_state = |state: FlashState| {
            let mut fs = flash_state.write();
            if let Some(ref mut transfer) = *fs {
                if transfer.id == transfer_id {
                    transfer.state = state;
                }
            }
        };

        let update_error = |error: String| {
            let mut fs = flash_state.write();
            if let Some(ref mut transfer) = *fs {
                if transfer.id == transfer_id {
                    transfer.state = FlashState::Failed;
                    transfer.error = Some(error);
                }
            }
        };

        let update_progress = |bytes: u64, blocks: u32, total_blocks: u32| {
            let mut fs = flash_state.write();
            if let Some(ref mut transfer) = *fs {
                if transfer.id == transfer_id {
                    transfer.progress.bytes_transferred = bytes;
                    transfer.progress.blocks_transferred = blocks;
                    transfer.progress.blocks_total = total_blocks;
                    transfer.progress.percent = if transfer.progress.bytes_total > 0 {
                        (bytes as f64 / transfer.progress.bytes_total as f64) * 100.0
                    } else {
                        100.0
                    };
                }
            }
        };

        // Session and security are already set up by start_flash before spawning.
        // Step 1: Preparing - request download
        update_state(FlashState::Preparing);

        // Step 2: Request Download (UDS 0x34)
        let memory_address: &[u8] = &[0x00, 0x00, 0x00, 0x00];
        let memory_size = (data.len() as u32).to_be_bytes();

        let max_block_size = match uds
            .request_download(0x00, 0x44, memory_address, &memory_size)
            .await
        {
            Ok(size) => size,
            Err(e) => {
                update_error(format!("RequestDownload failed: {}", e));
                return;
            }
        };

        // Calculate block count
        let block_size = (max_block_size as usize).saturating_sub(2); // Account for block counter
        if block_size == 0 {
            update_error("Invalid block size from ECU".to_string());
            return;
        }

        let total_blocks = ((data.len() + block_size - 1) / block_size) as u32;

        // Step 3: Transfer Data (UDS 0x36)
        update_state(FlashState::Transferring);

        let block_counter_start = sessions.transfer_data_block_counter_start;
        let mut block_counter: u8 = block_counter_start;
        let mut bytes_sent: u64 = 0;

        for chunk in data.chunks(block_size) {
            match uds.transfer_data(block_counter, chunk).await {
                Ok(_) => {
                    bytes_sent += chunk.len() as u64;
                    update_progress(bytes_sent, block_counter as u32, total_blocks);
                    block_counter = block_counter.wrapping_add(1);
                    // Wrap to configured value (some ECUs skip 0)
                    if block_counter == 0 && sessions.transfer_data_block_counter_wrap > 0 {
                        block_counter = sessions.transfer_data_block_counter_wrap;
                    }
                }
                Err(e) => {
                    update_error(format!(
                        "TransferData failed at block {}: {}",
                        block_counter, e
                    ));
                    return;
                }
            }
        }

        // Step 4: Ready for RequestTransferExit
        update_state(FlashState::AwaitingExit);
        info!(
            transfer_id = %transfer_id,
            bytes_sent,
            blocks = total_blocks,
            "Flash transfer complete, awaiting finalize"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{MockConfig, TransportConfig};

    fn test_config() -> UdsBackendConfig {
        UdsBackendConfig {
            id: "example_ecu".to_string(),
            name: "Test ECU".to_string(),
            description: Some("Test ECU for unit tests".to_string()),
            transport: TransportConfig::Mock(MockConfig { latency_ms: 0 }),
            operations: vec![],
            outputs: vec![],
            service_overrides: Default::default(),
            sessions: Default::default(),
            flash_commit: Default::default(),
        }
    }

    #[tokio::test]
    async fn test_list_parameters_empty() {
        // Parameters are now managed dynamically via ConversionStore
        let backend = UdsBackend::new(test_config()).await.unwrap();
        let params = backend.list_parameters().await.unwrap();
        assert!(params.is_empty());
    }

    #[tokio::test]
    async fn test_entity_info() {
        let backend = UdsBackend::new(test_config()).await.unwrap();
        let info = backend.entity_info();

        assert_eq!(info.id, "example_ecu");
        assert_eq!(info.entity_type, "ecu");
    }

    #[tokio::test]
    async fn test_capabilities() {
        let backend = UdsBackend::new(test_config()).await.unwrap();
        let caps = backend.capabilities();

        assert!(caps.read_data);
        assert!(caps.faults);
        assert!(!caps.logs); // ECUs don't have logs
        assert!(!caps.sub_entities); // ECUs don't have sub-entities
    }
}
