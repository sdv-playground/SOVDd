//! `PUT {entity}/status/restart` and `GET {entity}/status` — in-process
//! router tests, per tree.
//!
//! `/v1` (ISO 17978-3 §7.19.4): 202 with no body and
//! `Location: {entity}/status`; an entity whose backend does not control its
//! own restart (`supports_restart() == false`) gets 405
//! `precondition-not-fulfilled`, and its `GET status` carries no `restart`
//! link (Table 280 C2).
//!
//! Legacy (`/vehicle/v1`, frozen): 202 with the vendor legacy body; its
//! `status` reflects `DiagnosticBackend::ecu_reset_outcome` (`completed`, or
//! `running` for an `Initiated` reset).  A backend without reset support →
//! 501 with the non-fatal vendor code `not-supported`.
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

/// How a [`LegacyBackend`] answers a restart.
#[derive(Clone, Copy)]
enum Reset {
    /// `ecu_reset` succeeds with power-down time 2.
    Supported,
    /// `ecu_reset` refuses with `NotSupported`, as the trait default does.
    NotSupported,
    /// Restarted only with its node: `supports_restart() == false`, and
    /// `ecu_reset` keeps refusing.
    NodeOnly,
}

/// Never overrides `ecu_reset_outcome`, so the trait default applies.
struct LegacyBackend {
    info: EntityInfo,
    capabilities: Capabilities,
    reset: Reset,
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
        match self.reset {
            Reset::Supported => Ok(Some(2)),
            Reset::NotSupported | Reset::NodeOnly => {
                Err(BackendError::NotSupported("ecu_reset".to_string()))
            }
        }
    }

    fn supports_restart(&self) -> bool {
        !matches!(self.reset, Reset::NodeOnly)
    }
}

/// Opts in: reports that its reset was only triggered (e.g. a VM relaunch).
/// Has one app, `vm`, that behaves the same.
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

const V1: &str = "/v1";
const LEGACY: &str = "/vehicle/v1";

async fn server() -> TestServer {
    let mut backends: HashMap<String, Arc<dyn DiagnosticBackend>> = HashMap::new();
    for (id, reset) in [
        ("legacy", Reset::Supported),
        ("noreset", Reset::NotSupported),
        ("member", Reset::NodeOnly),
    ] {
        backends.insert(
            id.to_string(),
            Arc::new(LegacyBackend {
                info: entity_info(id),
                capabilities: Capabilities::default(),
                reset,
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
async fn restart(server: &TestServer, base: &str, entity: &str) -> reqwest::Response {
    reqwest::Client::new()
        .put(format!(
            "{}{base}/components/{entity}/status/restart",
            server.base_url()
        ))
        .json(&serde_json::json!({ "reset_type": "hard" }))
        .send()
        .await
        .expect("put")
}

async fn get(server: &TestServer, path: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!("{}{path}", server.base_url()))
        .send()
        .await
        .expect("get")
}

fn location(resp: &reqwest::Response) -> String {
    resp.headers()
        .get(reqwest::header::LOCATION)
        .expect("Location header")
        .to_str()
        .expect("ascii")
        .to_string()
}

// ---------------------------------------------------------------------------
// /v1 — ISO 17978-3 §7.19.4
// ---------------------------------------------------------------------------

#[tokio::test]
async fn v1_restart_is_202_without_body_pointing_at_status() {
    let server = server().await;
    // Whatever the backend's outcome, the `/v1` response is the same.
    for entity in ["legacy", "relaunch"] {
        let resp = restart(&server, V1, entity).await;
        assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED, "{entity}");
        assert_eq!(location(&resp), format!("/v1/components/{entity}/status"));
        assert!(resp.bytes().await.expect("body").is_empty(), "{entity}");
    }
}

#[tokio::test]
async fn v1_app_restart_points_at_the_app_status() {
    let server = server().await;
    let resp = restart(&server, V1, "relaunch/apps/vm").await;
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    let status_path = location(&resp);
    assert_eq!(status_path, "/v1/components/relaunch/apps/vm/status");
    assert!(resp.bytes().await.expect("body").is_empty());

    // The Location resolves to the app's runtime status.
    let resp = get(&server, &status_path).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["status"], "ready");
    assert_eq!(
        body["restart"][0],
        "/v1/components/relaunch/apps/vm/status/restart"
    );
}

#[tokio::test]
async fn v1_has_no_restart_execution_stub() {
    let server = server().await;
    let resp = get(&server, "/v1/components/legacy/status/restart/some-id").await;
    assert!(resp.status().is_client_error(), "got {}", resp.status());
    let resp = get(
        &server,
        "/v1/components/relaunch/apps/vm/status/restart/some-id",
    )
    .await;
    assert!(resp.status().is_client_error(), "got {}", resp.status());
}

#[tokio::test]
async fn v1_node_only_entity_is_405_precondition_not_fulfilled() {
    let server = server().await;
    let resp = restart(&server, V1, "member").await;
    assert_eq!(resp.status(), reqwest::StatusCode::METHOD_NOT_ALLOWED);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["error_code"], "precondition-not-fulfilled");
    assert_eq!(
        body["message"],
        "this entity is restarted by the node-level status/restart"
    );
}

// ---------------------------------------------------------------------------
// GET status — Table 280 C2 `restart` link, both trees
// ---------------------------------------------------------------------------

#[tokio::test]
async fn restart_link_only_when_the_entity_controls_its_restart() {
    let server = server().await;
    for base in [V1, LEGACY] {
        let body: serde_json::Value = get(&server, &format!("{base}/components/legacy/status"))
            .await
            .json()
            .await
            .expect("json");
        assert_eq!(
            body["restart"][0],
            format!("{base}/components/legacy/status/restart")
        );

        let resp = get(&server, &format!("{base}/components/member/status")).await;
        assert_eq!(resp.status(), reqwest::StatusCode::OK, "{base}");
        let body: serde_json::Value = resp.json().await.expect("json");
        assert_eq!(body["status"], "ready");
        assert!(body.get("restart").is_none(), "{base}: {body}");
    }
}

// ---------------------------------------------------------------------------
// Legacy tree — frozen vendor body
// ---------------------------------------------------------------------------

#[tokio::test]
async fn legacy_ecu_reset_only_backend_reports_completed() {
    let server = server().await;
    let resp = restart(&server, LEGACY, "legacy").await;

    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    let location = location(&resp);
    assert!(location.starts_with("/vehicle/v1/components/legacy/status/restart/"));
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["status"], "completed");
    assert_eq!(body["reset_type"], "hard");
    assert_eq!(body["power_down_time"], 2);
    assert_eq!(body["href"], location);

    // The execution stub stays on the legacy tree.
    let resp = get(&server, &location).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
}

#[tokio::test]
async fn legacy_initiated_outcome_reports_running() {
    let server = server().await;
    let resp = restart(&server, LEGACY, "relaunch").await;

    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["status"], "running");
    assert_eq!(body["power_down_time"], 5);
}

#[tokio::test]
async fn legacy_app_route_initiated_outcome_reports_running() {
    let server = server().await;
    let resp = restart(&server, LEGACY, "relaunch/apps/vm").await;

    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["status"], "running");
    assert_eq!(body["power_down_time"], 5);
    assert!(body["href"]
        .as_str()
        .expect("href")
        .starts_with("/vehicle/v1/components/relaunch/apps/vm/status/restart/"));
}

#[tokio::test]
async fn legacy_unsupported_reset_is_501_not_fatal() {
    let server = server().await;
    // Includes the node-only entity: the legacy tree keeps calling the backend.
    for entity in ["noreset", "member"] {
        let resp = restart(&server, LEGACY, entity).await;
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::NOT_IMPLEMENTED,
            "{entity}"
        );
        let body: serde_json::Value = resp.json().await.expect("json");
        assert_eq!(body["error_code"], "vendor-specific", "{entity}: {body}");
        assert_eq!(body["vendor_code"], "not-supported", "{entity}: {body}");
    }
}
