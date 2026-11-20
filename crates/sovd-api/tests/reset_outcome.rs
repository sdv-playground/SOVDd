//! ISO 17978-3 §7.19 `PUT {entity}/status/restart` — in-process router tests.
//!
//! The 202 body's `status` reflects what the backend reported through
//! `DiagnosticBackend::ecu_reset_outcome`:
//!   * a backend that only implements `ecu_reset` gets the trait default
//!     (`Completed`) → `completed`, as before;
//!   * a backend reporting `Initiated` → `running`, with its
//!     `power_down_time` forwarded;
//!   * a backend without reset support → 501.
//!
//! The app-level route (`.../apps/{app_id}/status/restart`) maps the same way.
//!
//! Same in-process `TestServer` pattern as `configurations.rs`.

use std::collections::HashMap;
use std::sync::Arc;

use sovd_client::testing::TestServer;
use sovd_core::{
    BackendError, BackendResult, Capabilities, DataValue, DiagnosticBackend, EcuResetOutcome,
    EntityInfo, FaultFilter, FaultsResult, OperationExecution, OperationInfo, ParameterInfo,
};

use sovd_api::{create_router, AppState};

// ---------------------------------------------------------------------------
// Mock backends
// ---------------------------------------------------------------------------

fn entity_info(id: &str) -> EntityInfo {
    EntityInfo {
        id: id.to_string(),
        name: format!("{id} ecu"),
        entity_type: "ecu".to_string(),
        description: None,
        href: format!("/vehicle/v1/components/{id}"),
        status: Some("online".to_string()),
    }
}

/// Implements only `ecu_reset`; with `supports_reset: false` it refuses with
/// `NotSupported`, as the trait default does.
struct LegacyBackend {
    info: EntityInfo,
    capabilities: Capabilities,
    supports_reset: bool,
}

#[async_trait::async_trait]
impl DiagnosticBackend for LegacyBackend {
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

    async fn ecu_reset(&self, _reset_type: u8) -> BackendResult<Option<u8>> {
        if self.supports_reset {
            Ok(Some(2))
        } else {
            Err(BackendError::NotSupported("ecu_reset".to_string()))
        }
    }
}

/// Opts in: reports that its reset was only triggered (e.g. a VM relaunch).
struct RelaunchBackend {
    info: EntityInfo,
    capabilities: Capabilities,
}

#[async_trait::async_trait]
impl DiagnosticBackend for RelaunchBackend {
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

    async fn get_sub_entity(&self, id: &str) -> BackendResult<Arc<dyn DiagnosticBackend>> {
        match id {
            "vm" => Ok(Arc::new(RelaunchBackend {
                info: entity_info("vm"),
                capabilities: Capabilities::default(),
            })),
            other => Err(BackendError::EntityNotFound(other.to_string())),
        }
    }

    async fn ecu_reset_outcome(&self, _reset_type: u8) -> BackendResult<EcuResetOutcome> {
        Ok(EcuResetOutcome::Initiated {
            power_down_time: Some(5),
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn server() -> TestServer {
    let mut backends: HashMap<String, Arc<dyn DiagnosticBackend>> = HashMap::new();
    for (id, supports_reset) in [("legacy", true), ("noreset", false)] {
        backends.insert(
            id.to_string(),
            Arc::new(LegacyBackend {
                info: entity_info(id),
                capabilities: Capabilities::default(),
                supports_reset,
            }),
        );
    }
    backends.insert(
        "relaunch".to_string(),
        Arc::new(RelaunchBackend {
            info: entity_info("relaunch"),
            capabilities: Capabilities::default(),
        }),
    );
    TestServer::start(create_router(AppState::new(backends)))
        .await
        .expect("test server")
}

/// `entity` is a component id, optionally followed by `/apps/{app_id}`.
async fn restart(server: &TestServer, entity: &str) -> reqwest::Response {
    reqwest::Client::new()
        .put(format!(
            "{}/vehicle/v1/components/{entity}/status/restart",
            server.base_url()
        ))
        .json(&serde_json::json!({ "reset_type": "hard" }))
        .send()
        .await
        .expect("put")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ecu_reset_only_backend_reports_completed() {
    let server = server().await;
    let resp = restart(&server, "legacy").await;

    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    let location = resp
        .headers()
        .get(reqwest::header::LOCATION)
        .expect("Location header")
        .to_str()
        .expect("ascii")
        .to_string();
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["status"], "completed");
    assert_eq!(body["reset_type"], "hard");
    assert_eq!(body["power_down_time"], 2);
    assert_eq!(body["href"], location);
}

#[tokio::test]
async fn initiated_outcome_reports_running() {
    let server = server().await;
    let resp = restart(&server, "relaunch").await;

    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    assert!(resp.headers().contains_key(reqwest::header::LOCATION));
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["status"], "running");
    assert_eq!(body["power_down_time"], 5);
}

#[tokio::test]
async fn app_route_initiated_outcome_reports_running() {
    let server = server().await;
    let resp = restart(&server, "relaunch/apps/vm").await;

    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["status"], "running");
    assert_eq!(body["power_down_time"], 5);
    assert!(body["href"]
        .as_str()
        .expect("href")
        .contains("/apps/vm/status/restart/"));
}

#[tokio::test]
async fn unsupported_reset_is_501() {
    let server = server().await;
    let resp = restart(&server, "noreset").await;
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_IMPLEMENTED);
}
