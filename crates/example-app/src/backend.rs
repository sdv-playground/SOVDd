//! ExampleAppBackend - An "app" entity with a managed ECU sub-entity
//!
//! The app entity exposes synthetic computed parameters (engine health score,
//! maintenance hours) and delegates all ECU-level concerns (data, faults,
//! operations, outputs, session/security, flash) to its managed ECU sub-entity.

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use chrono::Utc;
use sovd_core::{
    BackendError, BackendResult, Capabilities, DataValue, DiagnosticBackend, EntityInfo,
    FaultFilter, FaultsResult, OperationExecution, OperationInfo, ParameterInfo,
};
use tokio::sync::RwLock;

use crate::managed_ecu::ManagedEcuBackend;

/// A synthetic parameter computed locally
struct SyntheticParam {
    id: String,
    name: String,
    unit: String,
    data_type: String,
}

/// Example app backend — an "app" entity with sub-entities.
///
/// - Exposes synthetic computed parameters (engine_health_score, maintenance_hours)
/// - Delegates ECU-level operations to ManagedEcuBackend sub-entity
/// - Starts even when the upstream ECU is unreachable; returns 503 for ECU
///   requests until the background retry task connects successfully
pub struct ExampleAppBackend {
    entity_info: EntityInfo,
    capabilities: Capabilities,
    managed_ecu: Arc<RwLock<Option<Arc<ManagedEcuBackend>>>>,
    ecu_id: String,
    ecu_name: String,
    synthetic_params: Vec<SyntheticParam>,
    start_time: Instant,
}

impl ExampleAppBackend {
    /// Create a new supplier app backend wrapping a managed ECU sub-entity.
    ///
    /// `managed_ecu` may be `None` if the upstream is not yet reachable.
    /// The background retry task can populate it later via the shared
    /// `Arc<RwLock<Option<...>>>` returned by [`managed_ecu_slot`].
    pub fn new(
        id: &str,
        name: &str,
        ecu_id: &str,
        ecu_name: &str,
        managed_ecu: Option<Arc<ManagedEcuBackend>>,
    ) -> Self {
        let entity_info = EntityInfo {
            id: id.to_string(),
            name: name.to_string(),
            entity_type: "app".to_string(),
            description: Some("Example diagnostic app with managed ECU sub-entity".to_string()),
            href: format!("/vehicle/v1/components/{}", id),
            status: Some("running".to_string()),
        };

        let capabilities = Capabilities {
            read_data: true,
            sub_entities: true,
            ..Capabilities::default()
        };

        let synthetic_params = vec![
            SyntheticParam {
                id: "engine_health_score".to_string(),
                name: "Engine Health Score".to_string(),
                unit: "%".to_string(),
                data_type: "float64".to_string(),
            },
            SyntheticParam {
                id: "maintenance_hours".to_string(),
                name: "Maintenance Hours".to_string(),
                unit: "h".to_string(),
                data_type: "float64".to_string(),
            },
        ];

        Self {
            entity_info,
            capabilities,
            managed_ecu: Arc::new(RwLock::new(managed_ecu)),
            ecu_id: ecu_id.to_string(),
            ecu_name: ecu_name.to_string(),
            synthetic_params,
            start_time: Instant::now(),
        }
    }

    /// Returns the shared slot for the managed ECU backend.
    ///
    /// The background retry task uses this to populate the ECU backend
    /// once the upstream becomes reachable.
    pub fn managed_ecu_slot(&self) -> Arc<RwLock<Option<Arc<ManagedEcuBackend>>>> {
        self.managed_ecu.clone()
    }

    /// Compute engine health score from proxied RPM and coolant temp.
    /// Simple weighted formula: health = 100 - (rpm_penalty + temp_penalty)
    async fn compute_engine_health(&self) -> f64 {
        // Try to read RPM and coolant temp from the managed ECU's proxy.
        // If the managed ECU is not connected yet, use default values.
        let (rpm_val, temp_val) = {
            let guard = self.managed_ecu.read().await;
            if let Some(ref ecu) = *guard {
                let rpm = ecu
                    .read_data(&["engine_rpm".to_string()])
                    .await
                    .ok()
                    .and_then(|v| v.into_iter().next())
                    .and_then(|dv| dv.value.as_f64())
                    .unwrap_or(800.0);

                let temp = ecu
                    .read_data(&["coolant_temperature".to_string()])
                    .await
                    .ok()
                    .and_then(|v| v.into_iter().next())
                    .and_then(|dv| dv.value.as_f64())
                    .unwrap_or(90.0);

                (rpm, temp)
            } else {
                (800.0, 90.0)
            }
        };

        // RPM penalty: high RPM reduces health (above 4000 RPM)
        let rpm_penalty = if rpm_val > 4000.0 {
            ((rpm_val - 4000.0) / 100.0).min(30.0)
        } else {
            0.0
        };

        // Temp penalty: above 100°C reduces health
        let temp_penalty = if temp_val > 100.0 {
            ((temp_val - 100.0) * 2.0).min(40.0)
        } else {
            0.0
        };

        (100.0 - rpm_penalty - temp_penalty).clamp(0.0, 100.0)
    }

    /// Compute maintenance hours (simulated monotonic counter based on uptime)
    fn compute_maintenance_hours(&self) -> f64 {
        self.start_time.elapsed().as_secs_f64() / 3600.0
    }

    /// Check if a parameter ID is a synthetic parameter
    fn is_synthetic(&self, param_id: &str) -> bool {
        self.synthetic_params.iter().any(|p| p.id == param_id)
    }

    /// Read a synthetic parameter value
    async fn read_synthetic(&self, param_id: &str) -> BackendResult<DataValue> {
        let value = match param_id {
            "engine_health_score" => {
                let score = self.compute_engine_health().await;
                serde_json::json!(score)
            }
            "maintenance_hours" => {
                let hours = self.compute_maintenance_hours();
                serde_json::json!(hours)
            }
            _ => {
                return Err(BackendError::ParameterNotFound(param_id.to_string()));
            }
        };

        let unit = self
            .synthetic_params
            .iter()
            .find(|p| p.id == param_id)
            .map(|p| p.unit.clone());

        Ok(DataValue {
            id: param_id.to_string(),
            name: param_id.to_string(),
            value,
            unit,
            timestamp: Utc::now(),
            raw: None,
            did: None,
            length: None,
        })
    }
}

#[async_trait]
impl DiagnosticBackend for ExampleAppBackend {
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
    // Sub-Entities — managed ECU
    // =========================================================================

    async fn list_sub_entities(&self) -> BackendResult<Vec<EntityInfo>> {
        let guard = self.managed_ecu.read().await;
        match *guard {
            Some(ref ecu) => Ok(vec![ecu.entity_info().clone()]),
            None => {
                // Upstream not connected yet — return a placeholder so clients
                // can discover the sub-entity exists but is not yet reachable.
                Ok(vec![EntityInfo {
                    id: self.ecu_id.clone(),
                    name: self.ecu_name.clone(),
                    entity_type: "ecu".to_string(),
                    description: Some(
                        "Managed ECU sub-entity (upstream not connected)".to_string(),
                    ),
                    href: format!(
                        "/vehicle/v1/components/{}/apps/{}",
                        self.entity_info.id, self.ecu_id
                    ),
                    status: Some("not_available".to_string()),
                }])
            }
        }
    }

    async fn get_sub_entity(&self, id: &str) -> BackendResult<Arc<dyn DiagnosticBackend>> {
        let guard = self.managed_ecu.read().await;
        match *guard {
            Some(ref ecu) if id == ecu.entity_info().id => {
                Ok(ecu.clone() as Arc<dyn DiagnosticBackend>)
            }
            Some(_) => Err(BackendError::EntityNotFound(id.to_string())),
            None if id == self.ecu_id => Err(BackendError::Transport(
                "Upstream ECU not connected yet — retrying in background".to_string(),
            )),
            None => Err(BackendError::EntityNotFound(id.to_string())),
        }
    }

    // =========================================================================
    // Data Access — synthetic parameters only
    // =========================================================================

    async fn list_parameters(&self) -> BackendResult<Vec<ParameterInfo>> {
        Ok(self
            .synthetic_params
            .iter()
            .map(|sp| ParameterInfo {
                id: sp.id.clone(),
                name: sp.name.clone(),
                description: None,
                unit: Some(sp.unit.clone()),
                data_type: Some(sp.data_type.clone()),
                read_only: true,
                href: String::new(),
                did: None,
            })
            .collect())
    }

    async fn read_data(&self, param_ids: &[String]) -> BackendResult<Vec<DataValue>> {
        let mut results = Vec::new();
        for id in param_ids {
            if self.is_synthetic(id) {
                results.push(self.read_synthetic(id).await?);
            } else {
                return Err(BackendError::ParameterNotFound(id.to_string()));
            }
        }
        Ok(results)
    }

    // =========================================================================
    // Faults / Operations — not supported at app level
    // =========================================================================

    async fn get_faults(&self, _filter: Option<&FaultFilter>) -> BackendResult<FaultsResult> {
        Ok(FaultsResult {
            faults: vec![],
            status_availability_mask: None,
        })
    }

    async fn list_operations(&self) -> BackendResult<Vec<OperationInfo>> {
        Ok(vec![])
    }

    async fn start_operation(
        &self,
        operation_id: &str,
        _params: &[u8],
    ) -> BackendResult<OperationExecution> {
        Err(BackendError::OperationNotFound(operation_id.to_string()))
    }
}
