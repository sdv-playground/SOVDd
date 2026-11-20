//! The proxy forwards the upstream's restart outcome instead of fabricating
//! completion: an upstream `running` becomes `EcuResetOutcome::Initiated`.

use std::collections::HashMap;
use std::sync::Arc;

use sovd_api::{create_router, AppState};
use sovd_client::testing::TestServer;
use sovd_core::{
    BackendError, BackendResult, Capabilities, DataValue, DiagnosticBackend, EcuResetOutcome,
    EntityInfo, FaultFilter, FaultsResult, OperationExecution, OperationInfo, ParameterInfo,
};
use sovd_proxy::SovdProxyBackend;

/// Upstream entity: its reset completes, or (with `initiated`) is only triggered.
struct Upstream {
    info: EntityInfo,
    capabilities: Capabilities,
    initiated: bool,
}

#[async_trait::async_trait]
impl DiagnosticBackend for Upstream {
    fn entity_info(&self) -> &EntityInfo {
        &self.info
    }
    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }
    async fn list_parameters(&self) -> BackendResult<Vec<ParameterInfo>> {
        Ok(vec![])
    }
    async fn read_data(&self, _ids: &[String]) -> BackendResult<Vec<DataValue>> {
        Ok(vec![])
    }
    async fn get_faults(&self, _filter: Option<&FaultFilter>) -> BackendResult<FaultsResult> {
        Ok(FaultsResult {
            faults: vec![],
            status_availability_mask: None,
        })
    }
    async fn list_operations(&self) -> BackendResult<Vec<OperationInfo>> {
        Ok(vec![])
    }
    async fn start_operation(&self, op: &str, _params: &[u8]) -> BackendResult<OperationExecution> {
        Err(BackendError::OperationNotFound(op.to_string()))
    }

    async fn ecu_reset_outcome(&self, _reset_type: u8) -> BackendResult<EcuResetOutcome> {
        Ok(if self.initiated {
            EcuResetOutcome::Initiated {
                power_down_time: Some(5),
            }
        } else {
            EcuResetOutcome::Completed {
                power_down_time: Some(2),
            }
        })
    }
}

#[tokio::test]
async fn proxy_forwards_the_upstream_outcome() {
    let mut backends: HashMap<String, Arc<dyn DiagnosticBackend>> = HashMap::new();
    for (id, initiated) in [("vm", true), ("ecu", false)] {
        backends.insert(
            id.to_string(),
            Arc::new(Upstream {
                info: EntityInfo {
                    id: id.to_string(),
                    name: id.to_string(),
                    entity_type: "ecu".to_string(),
                    description: None,
                    href: format!("/vehicle/v1/components/{id}"),
                    status: Some("online".to_string()),
                },
                capabilities: Capabilities::default(),
                initiated,
            }),
        );
    }
    let server = TestServer::start(create_router(AppState::new(backends)))
        .await
        .expect("test server");

    let vm = SovdProxyBackend::new("vm", &server.base_url(), "vm")
        .await
        .expect("proxy");
    assert_eq!(
        vm.ecu_reset_outcome(0x01).await.expect("reset"),
        EcuResetOutcome::Initiated {
            power_down_time: Some(5)
        }
    );

    let ecu = SovdProxyBackend::new("ecu", &server.base_url(), "ecu")
        .await
        .expect("proxy");
    assert_eq!(
        ecu.ecu_reset_outcome(0x01).await.expect("reset"),
        EcuResetOutcome::Completed {
            power_down_time: Some(2)
        }
    );
    assert_eq!(ecu.ecu_reset(0x01).await.expect("reset"), Some(2));
}
