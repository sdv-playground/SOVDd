//! ManagedEcuBackend - Sub-entity representing the physical ECU managed by the app entity
//!
//! All ECU-level concerns live here: proxied DIDs, faults, operations, outputs,
//! session/security modes, OTA package management, and flash transfer.
//! The app entity delegates to this backend for ECU operations.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
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
    /// Stable update_id returned by `open_update` in `start_flash`.
    /// Held so post-reset `commit_flash` can re-`attach` the fresh
    /// FlashClient to the surviving server-side `/updates` entry, and
    /// so the status readers can describe the in-flight update.
    current_update_id: Arc<RwLock<Option<String>>>,
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
            current_update_id: Arc::new(RwLock::new(None)),
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

    async fn start_flash(&self) -> BackendResult<String> {
        self.require_programming_session().await?;

        // Find the verified package (no args — use the single verified package)
        let packages = self.packages.read().await;
        let (manifest_id, pkg) = packages
            .iter()
            .find(|(_, p)| p.info.status == PackageStatus::Verified)
            .ok_or_else(|| {
                BackendError::InvalidRequest(
                    "No verified package available for flashing".to_string(),
                )
            })?;

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
            manifest_id = %manifest_id,
            size = pkg.data.len(),
            "Uploading verified package to upstream ECU"
        );

        // /updates-native flow: open session → upload as a single
        // "manifest" part → return the update_id as the `transfer_id`
        // for the rest of the DiagnosticBackend contract.  The id is
        // also retained so post-reset `commit_flash` can re-attach.
        let opened =
            self.flash_client.open_update().await.map_err(|e| {
                BackendError::Transport(format!("Upstream open_update failed: {e}"))
            })?;
        *self.current_update_id.write().await = Some(opened.update_id.clone());
        self.flash_client
            .upload_part("manifest", &pkg.data)
            .await
            .map_err(|e| BackendError::Transport(format!("Upstream upload failed: {e}")))?;

        tracing::info!(update_id = %opened.update_id, "upstream upload complete");
        Ok(opened.update_id)
    }

    async fn get_flash_status(&self, _transfer_id: &str) -> BackendResult<FlashStatus> {
        self.current_flash_status()
            .await?
            .ok_or_else(|| BackendError::Transport("No /updates session open".to_string()))
    }

    async fn list_flash_transfers(&self) -> BackendResult<Vec<FlashStatus>> {
        // /updates doesn't expose a "list active updates" API through
        // the typed client.  Surface the current session if any; the
        // gateway / app-mgr managed-ecu proxy isn't expected to
        // enumerate multiple concurrent transfers.
        Ok(self.current_flash_status().await?.into_iter().collect())
    }

    async fn abort_flash(&self, _transfer_id: &str) -> BackendResult<()> {
        // Spec wire: DELETE /updates/{id} aborts the in-flight task
        // and releases the backend session.  Force-rollback covers
        // the case where the upstream's /updates entry has already
        // moved past awaiting-verdict (the more common stuck-trial
        // case in production).
        self.flash_client
            .force_rollback()
            .await
            .map_err(|e| BackendError::Transport(format!("Upstream force_rollback failed: {e}")))?;
        Ok(())
    }

    async fn finalize_flash(&self) -> BackendResult<()> {
        self.require_programming_session().await?;

        // Spec wire: prepare re-verifies uploaded parts + opens the
        // backend flash session; execute drives finalize_flash
        // (+ validate + activate for banked) and either auto-commits
        // (singleshot) or pauses at awaiting-verdict (banked +
        // orchestrated).  This shim chooses unorchestrated mode
        // because the supplier app is a leaf component — there's no
        // outer orchestrator to issue a verdict.
        tracing::info!("running PUT /prepare on upstream");
        let prepared = self
            .flash_client
            .prepare()
            .await
            .map_err(|e| BackendError::Transport(format!("Upstream prepare failed: {e}")))?;
        if prepared.status != "completed" {
            return Err(BackendError::Transport(format!(
                "Upstream prepare ended at {}/{}",
                prepared.phase, prepared.status
            )));
        }
        tracing::info!("running PUT /execute on upstream");
        let executed = self
            .flash_client
            .execute(false)
            .await
            .map_err(|e| BackendError::Transport(format!("Upstream execute failed: {e}")))?;
        if executed.status != "completed" {
            return Err(BackendError::Transport(format!(
                "Upstream execute ended at {}/{}",
                executed.phase, executed.status
            )));
        }
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

        // The upstream execute (unorchestrated above) already
        // committed; this is the outer-session commit equivalent.
        // If the upstream's in awaiting-verdict (orchestrated path),
        // spec_commit drives it; otherwise it's a no-op.  Idempotent
        // for our purposes; ignore "not in awaiting-verdict" 409s.
        //
        // The FlashClient may be a fresh handle after an ECU reset, so
        // re-attach to the update_id captured at start_flash before
        // posting the verdict.
        let update_id = self.current_update_id.read().await.clone().ok_or_else(|| {
            BackendError::Transport(
                "commit_flash called with no open /updates session (start_flash not run?)"
                    .to_string(),
            )
        })?;
        self.flash_client
            .attach(&update_id)
            .await
            .map_err(|e| BackendError::Transport(format!("Upstream attach failed: {e}")))?;
        match self.flash_client.spec_commit().await {
            Ok(_) => Ok(()),
            Err(sovd_client::flash::FlashError::Server { status: 409, .. }) => Ok(()),
            Err(e) => Err(BackendError::Transport(format!(
                "Upstream spec_commit failed: {e}"
            ))),
        }
    }

    async fn rollback_flash(&self) -> BackendResult<()> {
        tracing::info!("Setting ECU extended session for rollback (inner session)");
        self.proxy.set_session_mode("extended").await?;

        if self.security_secret.is_some() {
            self.unlock_ecu_security()
                .await
                .map_err(|e| BackendError::Protocol(format!("Security unlock failed: {}", e)))?;
        }

        // force_rollback unconditionally clears the upstream's trial
        // state — covers both the awaiting-verdict and
        // already-past-verdict cases without requiring an attached
        // session.
        self.flash_client
            .force_rollback()
            .await
            .map_err(|e| BackendError::Transport(format!("Upstream force_rollback failed: {e}")))?;
        Ok(())
    }

    async fn get_activation_state(&self) -> BackendResult<ActivationState> {
        // Synthesise an ActivationState from the Table 270 lifecycle
        // status (phase + status + substate).  awaiting-verdict maps to
        // Activated (firmware up, pending commit); execute/completed
        // maps to Committed (auto-commit drove it).
        let body = self.flash_client.spec_status().await.map_err(|e| {
            BackendError::Transport(format!("Upstream activation state failed: {e}"))
        })?;
        let state = map_phase_status(&body.phase, &body.status, body.substate.as_deref());
        Ok(ActivationState {
            supports_rollback: true,
            state,
            active_version: None,
            previous_version: None,
            reset_kind: sovd_core::ResetKind::default(),
        })
    }
}

impl ManagedEcuBackend {
    /// Build a [`FlashStatus`] for the currently-held update from the
    /// Table 270 lifecycle status, or `None` if no session is open.
    async fn current_flash_status(&self) -> BackendResult<Option<FlashStatus>> {
        let Some(update_id) = self.current_update_id.read().await.clone() else {
            return Ok(None);
        };
        match self.flash_client.spec_status().await {
            Ok(body) => Ok(Some(FlashStatus {
                transfer_id: update_id.clone(),
                package_id: update_id,
                state: map_phase_status(&body.phase, &body.status, body.substate.as_deref()),
                progress: None,
                error: None,
            })),
            Err(sovd_client::flash::FlashError::NoSession) => Ok(None),
            Err(e) => Err(BackendError::Transport(format!(
                "Upstream status failed: {e}"
            ))),
        }
    }
}

/// Map an ISO 17978-3 Table 270 lifecycle `(phase, status, substate)`
/// to a sovd-core [`FlashState`].  Preserves the intent of the retired
/// `map_update_state` string mapping:
///   prepare/*                        → Preparing   (was registered/uploading/verified)
///   execute/inProgress/awaiting-verdict → Activated (was finalized — firmware up, pending commit)
///   execute/completed                → Committed   (auto-commit)
///   */failed                         → Failed
fn map_phase_status(phase: &str, status: &str, substate: Option<&str>) -> FlashState {
    match (phase, status, substate) {
        (_, "failed", _) => FlashState::Failed,
        ("prepare", _, _) => FlashState::Preparing,
        ("execute", "inProgress", Some("awaiting-verdict")) => FlashState::Activated,
        ("execute", "completed", _) => FlashState::Committed,
        // execute in flight without the awaiting-verdict pause (singleshot
        // mid-run) — still preparing the activation from the caller's view.
        ("execute", _, _) => FlashState::Preparing,
        _ => FlashState::Failed,
    }
}
