//! ManagedEcuBackend - Sub-entity representing the physical ECU managed by the app entity
//!
//! All ECU-level concerns live here: proxied DIDs, faults, operations, outputs,
//! session/security modes, OTA package management, and flash transfer.
//! The app entity delegates to this backend for ECU operations.

use std::collections::HashMap;

use async_trait::async_trait;
use chrono::Utc;
use sovd_client::flash::TransferState;
use sovd_client::FlashClient;
use sovd_core::{
    ActivationState, BackendError, BackendResult, Capabilities, ClearFaultsResult, DataValue,
    DiagnosticBackend, EntityInfo, Fault, FaultFilter, FaultsResult, FlashState, FlashStatus,
    IoControlAction, IoControlResult, LogEntry, LogFilter, OperationExecution, OperationInfo,
    OutputDetail, OutputInfo, PackageInfo, PackageStatus, ParameterInfo, SecurityMode, SessionMode,
    SoftwareInfo, VerifyResult,
};
use sovd_proxy::SovdProxyBackend;
use sovd_uds::config::{OperationConfig, OutputConfig};
use tokio::sync::RwLock;

use crate::config::ParameterDef;

/// Stored package data for OTA interception
struct PackageData {
    data: Vec<u8>,
    info: PackageInfo,
}

/// Managed ECU backend — a sub-entity of the app entity.
///
/// Handles all ECU-level operations:
/// - Proxied diagnostic data (parameters, faults, operations, outputs)
/// - Session/security mode management (outer session lock + proxied security)
/// - OTA package interception and flash transfer via FlashClient
pub struct ManagedEcuBackend {
    proxy: SovdProxyBackend,
    flash_client: FlashClient,
    entity_info: EntityInfo,
    capabilities: Capabilities,
    packages: RwLock<HashMap<String, PackageData>>,
    /// Application-level (outer) session state, independent of the ECU's UDS session.
    /// Values: "default", "programming", "extended".
    local_session: RwLock<String>,
    /// Config-driven output definitions
    output_definitions: Vec<OutputConfig>,
    /// Config-driven parameter definitions
    parameter_definitions: Vec<ParameterDef>,
    /// Config-driven operation definitions
    operation_definitions: Vec<OperationConfig>,
    /// Supplier security secret for internal seed-key computation during flash.
    /// Parsed from hex config. When present, the app handles security access
    /// internally and rejects external unlock requests.
    security_secret: Option<Vec<u8>>,
}

impl ManagedEcuBackend {
    /// Create a new managed ECU backend.
    ///
    /// `proxy` handles all diagnostic operations to the upstream ECU.
    /// `upstream_url` is used together with the proxy's resolved routing
    /// info to create a FlashClient that mirrors the proxy's gateway
    /// auto-discovery.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: &str,
        name: &str,
        parent_id: &str,
        proxy: SovdProxyBackend,
        upstream_url: &str,
        output_definitions: Vec<OutputConfig>,
        parameter_definitions: Vec<ParameterDef>,
        operation_definitions: Vec<OperationConfig>,
        security_secret_hex: Option<&str>,
    ) -> Result<Self, String> {
        let entity_info = EntityInfo {
            id: id.to_string(),
            name: name.to_string(),
            entity_type: "ecu".to_string(),
            description: Some("Managed ECU sub-entity".to_string()),
            href: format!("/vehicle/v1/components/{}/apps/{}", parent_id, id),
            status: Some("running".to_string()),
        };

        let mut capabilities = Capabilities::uds_ecu();
        capabilities.software_update = true;
        if output_definitions.is_empty() {
            capabilities.io_control = false;
        }
        if operation_definitions.is_empty() {
            capabilities.operations = false;
        }

        // Use the proxy's resolved routing info so the FlashClient mirrors
        // the same gateway auto-discovery the proxy performed at startup.
        let (routing_component, sub_entity) = proxy.routing_info();
        let flash_client = if let Some(app_id) = sub_entity {
            FlashClient::for_sovd_sub_entity(upstream_url, routing_component, app_id)
        } else {
            FlashClient::for_sovd(upstream_url, routing_component)
        }
        .map_err(|e| format!("Failed to create flash client: {}", e))?;

        let security_secret = security_secret_hex
            .map(|s| {
                hex::decode(s).map_err(|e| format!("Invalid security secret hex '{}': {}", s, e))
            })
            .transpose()?;

        Ok(Self {
            proxy,
            flash_client,
            entity_info,
            capabilities,
            packages: RwLock::new(HashMap::new()),
            local_session: RwLock::new("default".to_string()),
            output_definitions,
            parameter_definitions,
            operation_definitions,
            security_secret,
        })
    }

    /// Find an output config by ID
    fn find_output_config(&self, output_id: &str) -> Option<&OutputConfig> {
        self.output_definitions.iter().find(|o| o.id == output_id)
    }

    /// Convert an OutputConfig to an OutputInfo
    fn config_to_output_info(config: &OutputConfig) -> OutputInfo {
        let data_type_str = config.data_type.as_ref().map(|dt| dt.to_string());
        OutputInfo {
            id: config.id.clone(),
            name: config.name.clone(),
            output_id: config.ioid.clone(),
            requires_security: config.security_level > 0,
            security_level: config.security_level,
            href: String::new(),
            data_type: data_type_str,
            unit: config.unit.clone(),
        }
    }

    /// Require that the app-level (outer) session is "programming".
    /// Flash operations are gated behind this check.
    async fn require_programming_session(&self) -> BackendResult<()> {
        let session = self.local_session.read().await;
        if session.as_str() != "programming" {
            return Err(BackendError::SessionRequired(
                "Programming session required for software update".to_string(),
            ));
        }
        Ok(())
    }

    /// Perform internal security access (seed-key) using the supplier secret.
    ///
    /// This keeps the secret within the app — the OEM gateway and external clients
    /// never see it. The seed-key algorithm is XOR with the secret, cycling over
    /// the secret bytes.
    async fn unlock_ecu_security(&self) -> BackendResult<()> {
        let secret = self.security_secret.as_ref().ok_or_else(|| {
            BackendError::Protocol("No security secret configured for internal unlock".into())
        })?;

        // Step 1: Request seed from the ECU via proxy
        let seed_result = self
            .proxy
            .set_security_mode("level1_requestseed", None)
            .await?;
        let seed_hex = seed_result.seed.ok_or_else(|| {
            BackendError::Protocol("ECU did not return a seed for security access".into())
        })?;
        let seed_bytes = hex::decode(&seed_hex)
            .map_err(|e| BackendError::Protocol(format!("Invalid seed hex from ECU: {}", e)))?;

        // Step 2: Compute key (XOR seed with secret, cycling over secret bytes)
        let key: Vec<u8> = seed_bytes
            .iter()
            .enumerate()
            .map(|(i, b)| b ^ secret[i % secret.len()])
            .collect();

        // Step 3: Send computed key to ECU via proxy
        self.proxy.set_security_mode("level1", Some(&key)).await?;

        tracing::info!("ECU security unlocked internally by app");
        Ok(())
    }
}

#[async_trait]
impl DiagnosticBackend for ManagedEcuBackend {
    // =========================================================================
    // Entity Information
    // =========================================================================

    fn entity_info(&self) -> &EntityInfo {
        &self.entity_info
    }

    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    // =========================================================================
    // Data Access — proxy with config-driven parameter list
    // =========================================================================

    async fn list_parameters(&self) -> BackendResult<Vec<ParameterInfo>> {
        if !self.parameter_definitions.is_empty() {
            // Whitelist mode: the supplier's config is authoritative. Only
            // parameters explicitly declared in [[managed_ecu.parameters]]
            // are exposed through the app's SOVD interface. Standard UDS
            // DIDs (e.g. 0xF190 VIN, 0xF180 Boot SW ID) are intentionally
            // omitted unless the supplier adds them to the config. This
            // lets the tier-1 curate exactly which data the OEM sees.
            //
            // When no parameters are configured, we fall back to the proxy
            // which returns whatever the upstream ECU advertises.
            Ok(self
                .parameter_definitions
                .iter()
                .map(|pd| ParameterInfo {
                    id: pd.id.clone(),
                    name: pd.name.clone(),
                    description: pd.description.clone(),
                    unit: pd.unit.clone(),
                    data_type: pd.data_type.clone(),
                    read_only: !pd.writable,
                    href: String::new(),
                    did: Some(pd.did.clone()),
                })
                .collect())
        } else {
            // No config — fall back to proxy (backwards compatible)
            self.proxy.list_parameters().await
        }
    }

    async fn read_data(&self, param_ids: &[String]) -> BackendResult<Vec<DataValue>> {
        // Pure proxy delegation — no synthetic intercept at this level
        self.proxy.read_data(param_ids).await
    }

    async fn write_data(&self, param_id: &str, value: &[u8]) -> BackendResult<()> {
        self.proxy.write_data(param_id, value).await
    }

    async fn read_raw_did(&self, did: u16) -> BackendResult<Vec<u8>> {
        self.proxy.read_raw_did(did).await
    }

    async fn write_raw_did(&self, did: u16, data: &[u8]) -> BackendResult<()> {
        self.proxy.write_raw_did(did, data).await
    }

    async fn ecu_reset(&self, reset_type: u8) -> BackendResult<Option<u8>> {
        self.proxy.ecu_reset(reset_type).await
    }

    // =========================================================================
    // Faults — delegate to proxy
    // =========================================================================

    async fn get_faults(&self, filter: Option<&FaultFilter>) -> BackendResult<FaultsResult> {
        self.proxy.get_faults(filter).await
    }

    async fn get_fault_detail(&self, fault_id: &str) -> BackendResult<Fault> {
        self.proxy.get_fault_detail(fault_id).await
    }

    async fn clear_faults(&self, group: Option<u32>) -> BackendResult<ClearFaultsResult> {
        self.proxy.clear_faults(group).await
    }

    // =========================================================================
    // Operations — config-driven with proxy execution
    // =========================================================================

    async fn list_operations(&self) -> BackendResult<Vec<OperationInfo>> {
        if !self.operation_definitions.is_empty() {
            Ok(self
                .operation_definitions
                .iter()
                .map(|op| OperationInfo {
                    id: op.id.clone(),
                    name: op.name.clone(),
                    description: op.description.clone(),
                    parameters: Vec::new(),
                    requires_security: op.security_level > 0,
                    security_level: op.security_level,
                    href: String::new(),
                })
                .collect())
        } else {
            self.proxy.list_operations().await
        }
    }

    async fn start_operation(
        &self,
        operation_id: &str,
        params: &[u8],
    ) -> BackendResult<OperationExecution> {
        if !self.operation_definitions.is_empty()
            && !self
                .operation_definitions
                .iter()
                .any(|op| op.id == operation_id)
        {
            return Err(BackendError::OperationNotFound(operation_id.to_string()));
        }
        self.proxy.start_operation(operation_id, params).await
    }

    async fn get_operation_status(&self, execution_id: &str) -> BackendResult<OperationExecution> {
        self.proxy.get_operation_status(execution_id).await
    }

    async fn stop_operation(&self, execution_id: &str) -> BackendResult<()> {
        self.proxy.stop_operation(execution_id).await
    }

    // =========================================================================
    // Outputs — config-driven with proxy fallback
    // =========================================================================

    async fn list_outputs(&self) -> BackendResult<Vec<OutputInfo>> {
        if !self.output_definitions.is_empty() {
            Ok(self
                .output_definitions
                .iter()
                .map(Self::config_to_output_info)
                .collect())
        } else {
            self.proxy.list_outputs().await
        }
    }

    async fn get_output(&self, output_id: &str) -> BackendResult<OutputDetail> {
        if !self.output_definitions.is_empty() {
            let config = self
                .find_output_config(output_id)
                .ok_or_else(|| BackendError::OutputNotFound(output_id.to_string()))?;

            let (current_value, controlled_by_tester, frozen) =
                match self.proxy.get_output(output_id).await {
                    Ok(detail) => (
                        detail.current_value,
                        detail.controlled_by_tester,
                        detail.frozen,
                    ),
                    Err(_) => (config.default_value.clone(), false, false),
                };

            let data_type_str = config.data_type.as_ref().map(|dt| dt.to_string());

            Ok(OutputDetail {
                id: config.id.clone(),
                name: config.name.clone(),
                output_id: config.ioid.clone(),
                current_value,
                default_value: config.default_value.clone(),
                controlled_by_tester,
                frozen,
                requires_security: config.security_level > 0,
                security_level: config.security_level,
                value: None,
                default: None,
                data_type: data_type_str,
                unit: config.unit.clone(),
                min: config.min,
                max: config.max,
                allowed: config.allowed.clone(),
            })
        } else {
            self.proxy.get_output(output_id).await
        }
    }

    async fn control_output(
        &self,
        output_id: &str,
        action: IoControlAction,
        value: Option<serde_json::Value>,
    ) -> BackendResult<IoControlResult> {
        if !self.output_definitions.is_empty() {
            let _config = self
                .find_output_config(output_id)
                .ok_or_else(|| BackendError::OutputNotFound(output_id.to_string()))?;
        }
        self.proxy.control_output(output_id, action, value).await
    }

    // =========================================================================
    // Logs — delegate to proxy
    // =========================================================================

    async fn get_logs(&self, filter: &LogFilter) -> BackendResult<Vec<LogEntry>> {
        self.proxy.get_logs(filter).await
    }

    async fn get_log(&self, log_id: &str) -> BackendResult<LogEntry> {
        self.proxy.get_log(log_id).await
    }

    async fn get_log_content(&self, log_id: &str) -> BackendResult<Vec<u8>> {
        self.proxy.get_log_content(log_id).await
    }

    async fn delete_log(&self, log_id: &str) -> BackendResult<()> {
        self.proxy.delete_log(log_id).await
    }

    // =========================================================================
    // Mode Control — outer session lock + proxied security
    // =========================================================================

    async fn get_session_mode(&self) -> BackendResult<SessionMode> {
        let session = self.local_session.read().await;
        Ok(SessionMode {
            mode: "session".to_string(),
            session: session.clone(),
            session_id: match session.as_str() {
                "programming" => 0x02,
                "extended" => 0x03,
                _ => 0x01,
            },
        })
    }

    async fn set_session_mode(&self, session: &str) -> BackendResult<SessionMode> {
        let session_lower = session.to_lowercase();
        match session_lower.as_str() {
            "default" | "programming" | "extended" => {}
            _ => {
                return Err(BackendError::InvalidRequest(format!(
                    "Invalid session: {}. Use 'default', 'programming', or 'extended'",
                    session
                )));
            }
        }

        {
            let mut local = self.local_session.write().await;
            *local = session_lower.clone();
        }

        tracing::info!(session = %session_lower, "ECU sub-entity (outer) session updated");

        // When returning to default, clean up the ECU's inner session too
        if session_lower == "default" {
            if let Err(e) = self.proxy.set_session_mode("default").await {
                tracing::warn!("Failed to reset ECU session: {}", e);
            }
        }

        Ok(SessionMode {
            mode: "session".to_string(),
            session: session_lower.clone(),
            session_id: match session_lower.as_str() {
                "programming" => 0x02,
                "extended" => 0x03,
                _ => 0x01,
            },
        })
    }

    async fn get_security_mode(&self) -> BackendResult<SecurityMode> {
        self.proxy.get_security_mode().await
    }

    async fn set_security_mode(
        &self,
        _value: &str,
        _key: Option<&[u8]>,
    ) -> BackendResult<SecurityMode> {
        Err(BackendError::NotSupported(
            "Security access is managed internally by the app during flash operations".into(),
        ))
    }

    // =========================================================================
    // Software Info
    // =========================================================================

    async fn get_software_info(&self) -> BackendResult<SoftwareInfo> {
        self.proxy.get_software_info().await
    }

    // =========================================================================
    // Package Management — local OTA interception
    // =========================================================================

    async fn receive_package(&self, data: &[u8]) -> BackendResult<String> {
        self.require_programming_session().await?;

        let package_id = uuid::Uuid::new_v4().to_string();

        if data.len() < 16 {
            tracing::warn!(size = data.len(), "Package too small, rejecting");
            return Err(BackendError::InvalidRequest(
                "Package too small (minimum 16 bytes)".to_string(),
            ));
        }

        let info = PackageInfo {
            id: package_id.clone(),
            size: data.len(),
            target_ecu: None,
            version: None,
            status: PackageStatus::Pending,
            created_at: Some(Utc::now()),
        };

        tracing::info!(
            package_id = %package_id,
            size = data.len(),
            "Package received and stored locally"
        );

        let mut packages = self.packages.write().await;
        packages.insert(
            package_id.clone(),
            PackageData {
                data: data.to_vec(),
                info,
            },
        );

        Ok(package_id)
    }

    async fn list_packages(&self) -> BackendResult<Vec<PackageInfo>> {
        let packages = self.packages.read().await;
        Ok(packages.values().map(|p| p.info.clone()).collect())
    }

    async fn get_package(&self, package_id: &str) -> BackendResult<PackageInfo> {
        let packages = self.packages.read().await;
        packages
            .get(package_id)
            .map(|p| p.info.clone())
            .ok_or_else(|| BackendError::EntityNotFound(package_id.to_string()))
    }

    async fn verify_package(&self, package_id: &str) -> BackendResult<VerifyResult> {
        let mut packages = self.packages.write().await;
        let pkg = packages
            .get_mut(package_id)
            .ok_or_else(|| BackendError::EntityNotFound(package_id.to_string()))?;

        let valid = pkg.data.len() >= 16 && pkg.data[..4] != [0, 0, 0, 0];

        let checksum = {
            let sum: u32 = pkg.data.iter().map(|&b| b as u32).sum();
            format!("{:08x}", sum)
        };

        if valid {
            pkg.info.status = PackageStatus::Verified;
            tracing::info!(package_id = %package_id, "Package verified successfully");
        } else {
            pkg.info.status = PackageStatus::Invalid;
            tracing::warn!(package_id = %package_id, "Package verification failed");
        }

        Ok(VerifyResult {
            valid,
            checksum: Some(checksum),
            algorithm: Some("byte_sum".to_string()),
            error: if valid {
                None
            } else {
                Some("Invalid package header".to_string())
            },
        })
    }

    async fn delete_package(&self, package_id: &str) -> BackendResult<()> {
        let mut packages = self.packages.write().await;
        packages
            .remove(package_id)
            .ok_or_else(|| BackendError::EntityNotFound(package_id.to_string()))?;
        tracing::info!(package_id = %package_id, "Package deleted");
        Ok(())
    }

    // =========================================================================
    // Flash Transfer — proxy to upstream ECU via FlashClient
    // =========================================================================

    async fn start_flash(&self, package_id: &str) -> BackendResult<String> {
        self.require_programming_session().await?;

        let packages = self.packages.read().await;
        let pkg = packages
            .get(package_id)
            .ok_or_else(|| BackendError::EntityNotFound(package_id.to_string()))?;

        if pkg.info.status != PackageStatus::Verified {
            return Err(BackendError::InvalidRequest(
                "Package must be verified before flashing".to_string(),
            ));
        }

        // Inner session: set ECU to programming mode before uploading
        tracing::info!("Setting ECU programming session (inner session)");
        self.proxy.set_session_mode("programming").await?;

        // Unlock ECU security internally (supplier secret, not exposed to clients)
        if self.security_secret.is_some() {
            self.unlock_ecu_security()
                .await
                .map_err(|e| BackendError::Protocol(format!("Security unlock failed: {}", e)))?;
        }

        tracing::info!(
            package_id = %package_id,
            size = pkg.data.len(),
            "Uploading verified package to upstream ECU"
        );

        let upload_resp = self
            .flash_client
            .upload_file(&pkg.data)
            .await
            .map_err(|e| BackendError::Transport(format!("Upstream upload failed: {}", e)))?;

        let verify_resp = self
            .flash_client
            .verify_file(&upload_resp.upload_id)
            .await
            .map_err(|e| BackendError::Transport(format!("Upstream verify failed: {}", e)))?;

        if !verify_resp.valid {
            return Err(BackendError::InvalidRequest(format!(
                "Upstream package verification failed: {}",
                verify_resp.error.unwrap_or_default()
            )));
        }

        let flash_resp = self
            .flash_client
            .start_flash(&upload_resp.upload_id)
            .await
            .map_err(|e| BackendError::Transport(format!("Upstream flash start failed: {}", e)))?;

        tracing::info!(
            transfer_id = %flash_resp.transfer_id,
            "Flash started on upstream ECU"
        );

        Ok(flash_resp.transfer_id)
    }

    async fn get_flash_status(&self, transfer_id: &str) -> BackendResult<FlashStatus> {
        let status = self
            .flash_client
            .get_flash_status(transfer_id)
            .await
            .map_err(|e| BackendError::Transport(format!("Upstream flash status failed: {}", e)))?;

        Ok(FlashStatus {
            transfer_id: status.id,
            package_id: status.file_id.unwrap_or_default(),
            state: convert_transfer_state(status.state),
            progress: status.progress.map(|p| sovd_core::FlashProgress {
                bytes_transferred: p.bytes_acknowledged.unwrap_or(0),
                bytes_total: (p.blocks_total as u64) * 1024,
                blocks_transferred: p.blocks_transferred,
                blocks_total: p.blocks_total,
                percent: p.percent.unwrap_or(0.0),
            }),
            error: status.error.map(|e| e.message),
        })
    }

    async fn list_flash_transfers(&self) -> BackendResult<Vec<FlashStatus>> {
        let resp = self.flash_client.list_transfers().await.map_err(|e| {
            BackendError::Transport(format!("Upstream list transfers failed: {}", e))
        })?;

        Ok(resp
            .transfers
            .into_iter()
            .map(|t| FlashStatus {
                transfer_id: t.transfer_id,
                package_id: t.package_id.unwrap_or_default(),
                state: convert_transfer_state(t.state),
                progress: None,
                error: t.error.map(|e| e.message),
            })
            .collect())
    }

    async fn abort_flash(&self, transfer_id: &str) -> BackendResult<()> {
        self.flash_client
            .abort_flash(transfer_id)
            .await
            .map_err(|e| BackendError::Transport(format!("Upstream abort failed: {}", e)))
    }

    async fn finalize_flash(&self) -> BackendResult<()> {
        self.require_programming_session().await?;

        tracing::info!("Sending transfer exit to upstream ECU");
        self.flash_client.transfer_exit().await.map_err(|e| {
            BackendError::Transport(format!("Upstream transfer exit failed: {}", e))
        })?;

        Ok(())
    }

    async fn commit_flash(&self) -> BackendResult<()> {
        // After ECU reset, the inner session reverts to default and security
        // re-locks. The commit routine requires extended session + security,
        // so we must set those up — same pattern as start_flash() for
        // programming session.
        tracing::info!("Setting ECU extended session for commit (inner session)");
        self.proxy.set_session_mode("extended").await?;

        if self.security_secret.is_some() {
            self.unlock_ecu_security()
                .await
                .map_err(|e| BackendError::Protocol(format!("Security unlock failed: {}", e)))?;
        }

        self.flash_client
            .commit_flash()
            .await
            .map_err(|e| BackendError::Transport(format!("Upstream commit failed: {}", e)))?;
        Ok(())
    }

    async fn rollback_flash(&self) -> BackendResult<()> {
        // After ECU reset, the inner session reverts to default and security
        // re-locks. The rollback routine requires extended session + security,
        // so we must set those up — same pattern as start_flash() for
        // programming session.
        tracing::info!("Setting ECU extended session for rollback (inner session)");
        self.proxy.set_session_mode("extended").await?;

        if self.security_secret.is_some() {
            self.unlock_ecu_security()
                .await
                .map_err(|e| BackendError::Protocol(format!("Security unlock failed: {}", e)))?;
        }

        self.flash_client
            .rollback_flash()
            .await
            .map_err(|e| BackendError::Transport(format!("Upstream rollback failed: {}", e)))?;
        Ok(())
    }

    async fn get_activation_state(&self) -> BackendResult<ActivationState> {
        let resp = self
            .flash_client
            .get_activation_state()
            .await
            .map_err(|e| {
                BackendError::Transport(format!("Upstream activation state failed: {}", e))
            })?;

        let state = resp.state.parse::<FlashState>().unwrap_or_else(|_| {
            tracing::error!(
                raw_state = %resp.state,
                "Unknown flash state from upstream — defaulting to Failed"
            );
            FlashState::Failed
        });

        Ok(ActivationState {
            supports_rollback: resp.supports_rollback,
            state,
            active_version: resp.active_version,
            previous_version: resp.previous_version,
        })
    }
}

/// Convert a sovd-client TransferState into a sovd-core FlashState
fn convert_transfer_state(s: TransferState) -> FlashState {
    match s {
        TransferState::Queued | TransferState::Pending => FlashState::Queued,
        TransferState::Preparing => FlashState::Preparing,
        TransferState::Transferring | TransferState::Running => FlashState::Transferring,
        TransferState::AwaitingExit => FlashState::AwaitingExit,
        TransferState::AwaitingReset => FlashState::AwaitingReset,
        TransferState::Complete | TransferState::Finished | TransferState::Verified => {
            FlashState::Complete
        }
        TransferState::Failed
        | TransferState::Error
        | TransferState::Aborted
        | TransferState::Invalid => FlashState::Failed,
        TransferState::Activated => FlashState::Activated,
        TransferState::Committed => FlashState::Committed,
        TransferState::RolledBack => FlashState::RolledBack,
    }
}
