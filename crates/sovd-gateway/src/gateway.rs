//! Gateway Backend - Aggregates multiple diagnostic backends
//!
//! The GatewayBackend provides a federated view of multiple backends,
//! allowing a central SOVD server to delegate requests to appropriate
//! backend implementations (UDS ECUs, HPC nodes, etc.).
//!
//! Per the SOVD spec (§6.5), per-ECU resources like files, flash, and
//! modes are accessed via sub-entity routes (`/apps/{ecu_id}/...`),
//! not through the gateway's own endpoints.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use sovd_core::routing;
use sovd_core::{
    BackendError, BackendResult, Capabilities, ClearFaultsResult, DataPoint, DataValue,
    DiagnosticBackend, EntityInfo, Fault, FaultFilter, FaultsResult, IoControlAction,
    IoControlResult, LogEntry, LogFilter, OperationExecution, OperationInfo, OutputDetail,
    OutputInfo, ParameterInfo, SoftwareInfo,
};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

/// Gateway backend that federates multiple diagnostic backends
///
/// This backend acts as a central hub that:
/// - Aggregates multiple sub-backends (ECUs, HPCs, etc.)
/// - Routes requests to the appropriate backend
/// - Provides a unified view of all entities
///
/// Per-ECU resources (files, flash, session/security modes) are accessed
/// via the sub-entity API routes, not through gateway-level methods.
pub struct GatewayBackend {
    /// Gateway entity information
    entity_info: EntityInfo,
    /// Gateway capabilities (aggregate of all backends)
    capabilities: Capabilities,
    /// Registered backends by ID
    backends: HashMap<String, Arc<dyn DiagnosticBackend>>,
}

impl GatewayBackend {
    /// Create a new gateway backend
    pub fn new(id: &str, name: &str, description: Option<String>) -> Self {
        let entity_info = EntityInfo {
            id: id.to_string(),
            name: name.to_string(),
            entity_type: "gateway".to_string(),
            description,
            href: format!("/vehicle/v1/components/{}", id),
            status: Some("operational".to_string()),
        };

        Self {
            entity_info,
            capabilities: Capabilities::gateway(),
            backends: HashMap::new(),
        }
    }

    /// Register a backend with this gateway
    pub fn register_backend(&mut self, backend: Arc<dyn DiagnosticBackend>) {
        let id = backend.entity_info().id.clone();
        info!(backend_id = %id, "Registering backend with gateway");
        self.backends.insert(id, backend);
        self.update_capabilities();
    }

    /// Unregister a backend from this gateway
    pub fn unregister_backend(&mut self, id: &str) -> Option<Arc<dyn DiagnosticBackend>> {
        let removed = self.backends.remove(id);
        if removed.is_some() {
            info!(backend_id = %id, "Unregistered backend from gateway");
            self.update_capabilities();
        }
        removed
    }

    /// Get a backend by ID
    pub fn get_backend(&self, id: &str) -> Option<&Arc<dyn DiagnosticBackend>> {
        self.backends.get(id)
    }

    /// List all registered backend IDs
    pub fn backend_ids(&self) -> Vec<String> {
        self.backends.keys().cloned().collect()
    }

    /// Update gateway capabilities.
    ///
    /// Per SOVD §6.4 / §6.5, a gateway reports only its own capabilities.
    /// Child capabilities are discoverable via their own detail endpoints.
    /// The gateway itself is a pure routing entity — it has no data, faults,
    /// or operations of its own.
    fn update_capabilities(&mut self) {
        self.capabilities = Capabilities::gateway();
    }

    /// Find which backend owns a parameter
    #[allow(dead_code)]
    fn find_backend_for_param(&self, param_id: &str) -> Option<&Arc<dyn DiagnosticBackend>> {
        if let Some((backend_id, _)) = routing::split_entity_prefix(param_id) {
            return self.backends.get(backend_id);
        }

        warn!(param_id = %param_id, "Parameter ID without backend prefix, searching all backends");
        None
    }

    /// Find which backend owns an operation
    #[allow(dead_code)]
    fn find_backend_for_operation(
        &self,
        operation_id: &str,
    ) -> Option<&Arc<dyn DiagnosticBackend>> {
        if let Some((backend_id, _)) = routing::split_entity_prefix(operation_id) {
            return self.backends.get(backend_id);
        }
        None
    }
}

#[async_trait]
impl DiagnosticBackend for GatewayBackend {
    fn entity_info(&self) -> &EntityInfo {
        &self.entity_info
    }

    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    async fn list_parameters(&self) -> BackendResult<Vec<ParameterInfo>> {
        // Gateway has no parameters of its own.
        // Child ECU parameters are accessed via sub-entity paths per SOVD §6.5.
        Ok(Vec::new())
    }

    async fn read_data(&self, param_ids: &[String]) -> BackendResult<Vec<DataValue>> {
        // Group params by backend
        let mut by_backend: HashMap<String, Vec<String>> = HashMap::new();

        for param_id in param_ids {
            if let Some((backend_id, local_id)) = routing::split_entity_prefix(param_id) {
                by_backend
                    .entry(backend_id.to_string())
                    .or_default()
                    .push(local_id.to_string());
            } else {
                return Err(BackendError::ParameterNotFound(format!(
                    "Parameter ID must be prefixed with backend ID: {}",
                    param_id
                )));
            }
        }

        let mut all_values = Vec::new();

        for (backend_id, local_ids) in by_backend {
            let backend = self.backends.get(&backend_id).ok_or_else(|| {
                BackendError::EntityNotFound(format!("Backend not found: {}", backend_id))
            })?;

            let values = backend.read_data(&local_ids).await?;

            for mut value in values {
                value.id = routing::prefixed_id(&value.id, Some(&backend_id));
                all_values.push(value);
            }
        }

        Ok(all_values)
    }

    async fn write_data(&self, param_id: &str, value: &[u8]) -> BackendResult<()> {
        let (backend_id, local_id) = routing::split_entity_prefix(param_id).ok_or_else(|| {
            BackendError::ParameterNotFound(format!(
                "Parameter ID must be prefixed with backend ID: {}",
                param_id
            ))
        })?;

        let backend = self.backends.get(backend_id).ok_or_else(|| {
            BackendError::EntityNotFound(format!("Backend not found: {}", backend_id))
        })?;

        backend.write_data(local_id, value).await
    }

    async fn subscribe_data(
        &self,
        param_ids: &[String],
        rate_hz: u32,
    ) -> BackendResult<broadcast::Receiver<DataPoint>> {
        // For now, don't support cross-backend subscriptions
        // Group by backend and require all params from same backend
        let mut backend_id: Option<String> = None;
        let mut local_ids = Vec::new();

        for param_id in param_ids {
            let (bid, lid) = routing::split_entity_prefix(param_id).ok_or_else(|| {
                BackendError::ParameterNotFound(format!(
                    "Parameter ID must be prefixed with backend ID: {}",
                    param_id
                ))
            })?;

            if let Some(ref existing) = backend_id {
                if existing != bid {
                    return Err(BackendError::InvalidRequest(
                        "Subscription across multiple backends not supported".to_string(),
                    ));
                }
            } else {
                backend_id = Some(bid.to_string());
            }
            local_ids.push(lid.to_string());
        }

        let bid = backend_id
            .ok_or_else(|| BackendError::InvalidRequest("No parameters specified".to_string()))?;

        let backend = self
            .backends
            .get(&bid)
            .ok_or_else(|| BackendError::EntityNotFound(format!("Backend not found: {}", bid)))?;

        backend.subscribe_data(&local_ids, rate_hz).await
    }

    async fn get_faults(&self, filter: Option<&FaultFilter>) -> BackendResult<FaultsResult> {
        let mut all_faults = Vec::new();

        for (backend_id, backend) in &self.backends {
            match backend.get_faults(filter).await {
                Ok(result) => {
                    for mut fault in result.faults {
                        fault.id = routing::prefixed_id(&fault.id, Some(backend_id));
                        fault.href = format!(
                            "/vehicle/v1/components/{}/faults/{}",
                            self.entity_info.id, fault.id
                        );
                        all_faults.push(fault);
                    }
                }
                Err(e) => {
                    warn!(backend_id = %backend_id, error = %e, "Failed to get faults from backend");
                }
            }
        }

        Ok(FaultsResult {
            faults: all_faults,
            status_availability_mask: None, // Gateway aggregates, doesn't have single mask
        })
    }

    async fn get_fault_detail(&self, fault_id: &str) -> BackendResult<Fault> {
        let (backend_id, local_id) = routing::split_entity_prefix(fault_id).ok_or_else(|| {
            BackendError::EntityNotFound(format!(
                "Fault ID must be prefixed with backend ID: {}",
                fault_id
            ))
        })?;

        let backend = self.backends.get(backend_id).ok_or_else(|| {
            BackendError::EntityNotFound(format!("Backend not found: {}", backend_id))
        })?;

        let mut fault = backend.get_fault_detail(local_id).await?;
        fault.id = routing::prefixed_id(&fault.id, Some(backend_id));
        fault.href = format!(
            "/vehicle/v1/components/{}/faults/{}",
            self.entity_info.id, fault.id
        );

        Ok(fault)
    }

    async fn clear_faults(&self, group: Option<u32>) -> BackendResult<ClearFaultsResult> {
        let mut total_cleared = 0u32;
        let mut any_success = false;
        let mut messages = Vec::new();

        for (backend_id, backend) in &self.backends {
            match backend.clear_faults(group).await {
                Ok(result) => {
                    any_success |= result.success;
                    total_cleared += result.cleared_count;
                    messages.push(format!("{}: {}", backend_id, result.message));
                }
                Err(BackendError::NotSupported(_)) => {
                    debug!(backend_id = %backend_id, "Backend does not support clear_faults");
                }
                Err(e) => {
                    warn!(backend_id = %backend_id, error = %e, "Failed to clear faults on backend");
                    messages.push(format!("{}: error - {}", backend_id, e));
                }
            }
        }

        Ok(ClearFaultsResult {
            success: any_success,
            cleared_count: total_cleared,
            message: messages.join("; "),
        })
    }

    async fn get_logs(&self, filter: &LogFilter) -> BackendResult<Vec<LogEntry>> {
        let mut all_logs = Vec::new();

        for (backend_id, backend) in &self.backends {
            match backend.get_logs(filter).await {
                Ok(logs) => {
                    all_logs.extend(logs);
                }
                Err(BackendError::NotSupported(_)) => {
                    // Skip backends that don't support logs
                }
                Err(e) => {
                    warn!(backend_id = %backend_id, error = %e, "Failed to get logs from backend");
                }
            }
        }

        // Sort by timestamp (most recent first)
        all_logs.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

        Ok(all_logs)
    }

    async fn list_operations(&self) -> BackendResult<Vec<OperationInfo>> {
        let mut all_ops = Vec::new();

        for (backend_id, backend) in &self.backends {
            match backend.list_operations().await {
                Ok(ops) => {
                    for mut op in ops {
                        op.id = routing::prefixed_id(&op.id, Some(backend_id));
                        op.href = format!(
                            "/vehicle/v1/components/{}/operations/{}",
                            self.entity_info.id, op.id
                        );
                        all_ops.push(op);
                    }
                }
                Err(e) => {
                    warn!(backend_id = %backend_id, error = %e, "Failed to list operations from backend");
                }
            }
        }

        Ok(all_ops)
    }

    async fn start_operation(
        &self,
        operation_id: &str,
        params: &[u8],
    ) -> BackendResult<OperationExecution> {
        let (backend_id, local_id) =
            routing::split_entity_prefix(operation_id).ok_or_else(|| {
                BackendError::OperationNotFound(format!(
                    "Operation ID must be prefixed with backend ID: {}",
                    operation_id
                ))
            })?;

        let backend = self.backends.get(backend_id).ok_or_else(|| {
            BackendError::EntityNotFound(format!("Backend not found: {}", backend_id))
        })?;

        let mut execution = backend.start_operation(local_id, params).await?;
        execution.execution_id = routing::prefixed_id(&execution.execution_id, Some(backend_id));
        execution.operation_id = routing::prefixed_id(&execution.operation_id, Some(backend_id));

        Ok(execution)
    }

    async fn get_operation_status(&self, execution_id: &str) -> BackendResult<OperationExecution> {
        let (backend_id, local_id) =
            routing::split_entity_prefix(execution_id).ok_or_else(|| {
                BackendError::EntityNotFound(format!(
                    "Execution ID must be prefixed with backend ID: {}",
                    execution_id
                ))
            })?;

        let backend = self.backends.get(backend_id).ok_or_else(|| {
            BackendError::EntityNotFound(format!("Backend not found: {}", backend_id))
        })?;

        let mut execution = backend.get_operation_status(local_id).await?;
        execution.execution_id = routing::prefixed_id(&execution.execution_id, Some(backend_id));
        execution.operation_id = routing::prefixed_id(&execution.operation_id, Some(backend_id));

        Ok(execution)
    }

    async fn stop_operation(&self, execution_id: &str) -> BackendResult<()> {
        let (backend_id, local_id) =
            routing::split_entity_prefix(execution_id).ok_or_else(|| {
                BackendError::EntityNotFound(format!(
                    "Execution ID must be prefixed with backend ID: {}",
                    execution_id
                ))
            })?;

        let backend = self.backends.get(backend_id).ok_or_else(|| {
            BackendError::EntityNotFound(format!("Backend not found: {}", backend_id))
        })?;

        backend.stop_operation(local_id).await
    }

    async fn list_outputs(&self) -> BackendResult<Vec<OutputInfo>> {
        let mut all_outputs = Vec::new();

        for (backend_id, backend) in &self.backends {
            match backend.list_outputs().await {
                Ok(outputs) => {
                    for mut output in outputs {
                        output.id = routing::prefixed_id(&output.id, Some(backend_id));
                        output.href = format!(
                            "/vehicle/v1/components/{}/outputs/{}",
                            self.entity_info.id, output.id
                        );
                        all_outputs.push(output);
                    }
                }
                Err(e) => {
                    warn!(backend_id = %backend_id, error = %e, "Failed to list outputs from backend");
                }
            }
        }

        Ok(all_outputs)
    }

    async fn get_output(&self, output_id: &str) -> BackendResult<OutputDetail> {
        let (backend_id, local_id) = routing::split_entity_prefix(output_id).ok_or_else(|| {
            BackendError::OutputNotFound(format!(
                "Output ID must be prefixed with backend ID: {}",
                output_id
            ))
        })?;

        let backend = self.backends.get(backend_id).ok_or_else(|| {
            BackendError::EntityNotFound(format!("Backend not found: {}", backend_id))
        })?;

        backend.get_output(local_id).await
    }

    async fn control_output(
        &self,
        output_id: &str,
        action: IoControlAction,
        value: Option<serde_json::Value>,
    ) -> BackendResult<IoControlResult> {
        let (backend_id, local_id) = routing::split_entity_prefix(output_id).ok_or_else(|| {
            BackendError::OutputNotFound(format!(
                "Output ID must be prefixed with backend ID: {}",
                output_id
            ))
        })?;

        let backend = self.backends.get(backend_id).ok_or_else(|| {
            BackendError::EntityNotFound(format!("Backend not found: {}", backend_id))
        })?;

        backend.control_output(local_id, action, value).await
    }

    async fn list_sub_entities(&self) -> BackendResult<Vec<EntityInfo>> {
        let mut entities: Vec<EntityInfo> = self
            .backends
            .values()
            .map(|b| {
                let mut info = b.entity_info().clone();
                // Update href to be relative to gateway
                info.href = format!("/vehicle/v1/components/{}/{}", self.entity_info.id, info.id);
                info
            })
            .collect();

        // Sort by ID for consistent ordering
        entities.sort_by(|a, b| a.id.cmp(&b.id));

        Ok(entities)
    }

    async fn get_sub_entity(&self, id: &str) -> BackendResult<Arc<dyn DiagnosticBackend>> {
        self.backends
            .get(id)
            .cloned()
            .ok_or_else(|| BackendError::EntityNotFound(id.to_string()))
    }

    async fn get_software_info(&self) -> BackendResult<SoftwareInfo> {
        let mut details = serde_json::Map::new();
        details.insert(
            "gateway_version".to_string(),
            serde_json::json!(env!("CARGO_PKG_VERSION")),
        );

        // Collect software info from all backends
        let mut backend_versions = serde_json::Map::new();
        for (id, backend) in &self.backends {
            match backend.get_software_info().await {
                Ok(info) => {
                    backend_versions.insert(id.clone(), serde_json::json!(info.version));
                }
                Err(_) => {
                    backend_versions.insert(id.clone(), serde_json::json!("unknown"));
                }
            }
        }
        details.insert(
            "backends".to_string(),
            serde_json::Value::Object(backend_versions),
        );

        Ok(SoftwareInfo {
            version: env!("CARGO_PKG_VERSION").to_string(),
            details: Some(serde_json::Value::Object(details)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gateway_creation() {
        let gateway =
            GatewayBackend::new("main", "Main Gateway", Some("Central gateway".to_string()));
        assert_eq!(gateway.entity_info().id, "main");
        assert_eq!(gateway.entity_info().entity_type, "gateway");
        assert!(gateway.capabilities().sub_entities);
    }

    #[test]
    fn test_empty_gateway_capabilities() {
        let gateway = GatewayBackend::new("main", "Main Gateway", None);
        let caps = gateway.capabilities();
        assert!(!caps.read_data);
        assert!(!caps.faults);
        assert!(caps.sub_entities); // Gateway always has sub_entities
    }
}
