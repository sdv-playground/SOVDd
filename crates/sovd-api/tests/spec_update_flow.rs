//! Phase A tests for the ISO 17978-3 §7.18 spec verbs on /updates.
//!
//! Covers PUT /prepare → /execute → GET /status round-trip for both
//! singleshot and banked backends, the auto-complete behaviour without
//! orchestrated mode (Phase A scope), and the deprecation header on
//! the retired /executions wire.
//!
//! tasks/spec-aligned-updates-wire.md UPDATE-WIRE-001 — Phase A.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::Value;
use sovd_api::{create_router, state::UpdatesConfig, AppState};
use sovd_client::testing::TestServer;
use sovd_core::{
    BackendError, BackendResult, Capabilities, DataValue, DiagnosticBackend, EntityInfo,
    FaultFilter, FaultsResult, FlashProgress, FlashState as CoreFlashState, FlashStatus,
    OperationExecution, OperationInfo, PackageStream, ParameterInfo,
};

// ---------------------------------------------------------------------------
// Mock backend with enough surface area to drive the /updates wire
// ---------------------------------------------------------------------------

struct MockShape {
    shape: &'static str,
}

struct MockBackend {
    info: EntityInfo,
    capabilities: Capabilities,
    shape: MockShape,
    /// monotonic counter for file_id allocation
    next_id: Mutex<u64>,
    /// transfer_id allocated by start_flash
    transfer_id: Mutex<Option<String>>,
    /// VmBackend-style flash transfer state
    flash_state: Mutex<CoreFlashState>,
    /// Toggle to make verify_part fail (for the failure-path test)
    fail_verify: Mutex<bool>,
    /// Optional gateway-style child sub-entity (drives the C-073
    /// gateway-child subscription-resource path).
    child: Option<Arc<MockBackend>>,
    /// Kept-alive broadcast sender so `subscribe_data` can hand out a live
    /// (idle) receiver — the C-070 SSE-header test never needs an event,
    /// only the response headers.
    data_tx: tokio::sync::broadcast::Sender<sovd_core::DataPoint>,
}

impl MockBackend {
    fn new(id: &str, shape: &'static str) -> Self {
        Self::with_child(id, shape, None)
    }

    fn with_child(id: &str, shape: &'static str, child: Option<Arc<MockBackend>>) -> Self {
        Self {
            info: EntityInfo {
                id: id.into(),
                name: format!("{id} mock"),
                entity_type: "ecu".into(),
                description: Some(format!("{shape} mock for spec-update tests")),
                href: format!("/vehicle/v1/components/{id}"),
                status: Some("online".into()),
            },
            capabilities: Capabilities {
                software_update: true,
                ..Default::default()
            },
            shape: MockShape { shape },
            next_id: Mutex::new(0),
            transfer_id: Mutex::new(None),
            flash_state: Mutex::new(CoreFlashState::Transferring),
            fail_verify: Mutex::new(false),
            child,
            data_tx: tokio::sync::broadcast::channel(16).0,
        }
    }
}

#[async_trait::async_trait]
impl DiagnosticBackend for MockBackend {
    fn entity_info(&self) -> &EntityInfo {
        &self.info
    }
    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }
    fn update_shape(&self) -> &'static str {
        self.shape.shape
    }

    async fn list_parameters(&self) -> BackendResult<Vec<ParameterInfo>> {
        // One GET-able parameter so the C-073 subscription-resource
        // validation has something to resolve against (mirrors a real
        // backend exposing at least one data parameter).
        Ok(vec![ParameterInfo {
            id: "coolant_temp".into(),
            name: "Coolant temperature".into(),
            description: None,
            unit: Some("degC".into()),
            data_type: Some("uint8".into()),
            read_only: true,
            href: format!("/vehicle/v1/components/{}/data/coolant_temp", self.info.id),
            did: None,
        }])
    }
    async fn read_data(&self, ids: &[String]) -> BackendResult<Vec<DataValue>> {
        // Echo back any id we "know" so the C-073 read-probe fallback can
        // confirm GET-ability; unknown ids resolve to an empty vec.
        Ok(ids
            .iter()
            .filter(|id| id.as_str() == "coolant_temp")
            .map(|id| DataValue::new(id.clone(), "Coolant temperature", serde_json::json!(42)))
            .collect())
    }

    async fn list_sub_entities(&self) -> BackendResult<Vec<EntityInfo>> {
        // Expose a single gateway-style child so the C-073 gateway-child
        // path (`child/param`) can resolve via get_sub_entity.
        Ok(self.child.iter().map(|c| c.entity_info().clone()).collect())
    }

    async fn get_sub_entity(&self, id: &str) -> BackendResult<Arc<dyn DiagnosticBackend>> {
        match &self.child {
            Some(child) if child.info.id == id => Ok(child.clone() as Arc<dyn DiagnosticBackend>),
            _ => Err(BackendError::EntityNotFound(id.into())),
        }
    }

    async fn subscribe_data(
        &self,
        _param_ids: &[String],
        _rate_hz: u32,
    ) -> BackendResult<tokio::sync::broadcast::Receiver<sovd_core::DataPoint>> {
        // Live but idle receiver — enough for the C-070 SSE-header assertion.
        Ok(self.data_tx.subscribe())
    }
    async fn get_faults(&self, _: Option<&FaultFilter>) -> BackendResult<FaultsResult> {
        Ok(FaultsResult {
            faults: Vec::new(),
            status_availability_mask: None,
        })
    }
    async fn list_operations(&self) -> BackendResult<Vec<OperationInfo>> {
        Ok(Vec::new())
    }
    async fn start_operation(&self, op_id: &str, _: &[u8]) -> BackendResult<OperationExecution> {
        Err(BackendError::OperationNotFound(op_id.into()))
    }

    async fn receive_package_stream(
        &self,
        mut stream: PackageStream,
        _content_length: Option<u64>,
    ) -> BackendResult<String> {
        use futures::StreamExt;
        let mut buf = Vec::new();
        while let Some(chunk) = stream.next().await {
            let bytes = chunk.map_err(|e| BackendError::Internal(format!("stream: {e}")))?;
            buf.extend_from_slice(&bytes);
        }
        let mut counter = self.next_id.lock();
        *counter += 1;
        let id = format!("pkg-{}", *counter);
        // Pretend the manifest is at AwaitingActivation after streaming.
        *self.flash_state.lock() = CoreFlashState::AwaitingActivation;
        Ok(id)
    }

    async fn verify_part(&self, file_id: &str, expected_sha256: &str) -> BackendResult<()> {
        if *self.fail_verify.lock() {
            return Err(BackendError::InvalidRequest(format!(
                "verify_part forced failure for file_id {file_id} expected {expected_sha256}"
            )));
        }
        Ok(())
    }

    async fn start_flash(&self) -> BackendResult<String> {
        let mut counter = self.next_id.lock();
        *counter += 1;
        let tid = format!("xfer-{}", *counter);
        *self.transfer_id.lock() = Some(tid.clone());
        *self.flash_state.lock() = CoreFlashState::Transferring;
        Ok(tid)
    }

    async fn get_flash_status(&self, transfer_id: &str) -> BackendResult<FlashStatus> {
        Ok(FlashStatus {
            transfer_id: transfer_id.to_string(),
            package_id: "pkg".into(),
            state: *self.flash_state.lock(),
            progress: Some(FlashProgress {
                bytes_transferred: 100,
                bytes_total: 100,
                blocks_transferred: 1,
                blocks_total: 1,
                percent: 100.0,
            }),
            error: None,
        })
    }

    async fn finalize_flash(&self) -> BackendResult<()> {
        let mut state = self.flash_state.lock();
        *state = if self.shape.shape == "singleshot" {
            CoreFlashState::Activated
        } else {
            CoreFlashState::AwaitingReboot
        };
        Ok(())
    }

    async fn validate(&self) -> BackendResult<()> {
        let mut state = self.flash_state.lock();
        if matches!(
            *state,
            CoreFlashState::AwaitingActivation
                | CoreFlashState::Validated
                | CoreFlashState::AwaitingReboot
        ) {
            *state = CoreFlashState::Validated;
            Ok(())
        } else {
            Err(BackendError::InvalidRequest(format!(
                "validate from {:?}",
                *state
            )))
        }
    }

    async fn activate(&self) -> BackendResult<()> {
        let mut state = self.flash_state.lock();
        if *state == CoreFlashState::Validated {
            *state = if self.shape.shape == "singleshot" {
                CoreFlashState::Activated
            } else {
                CoreFlashState::AwaitingReboot
            };
            Ok(())
        } else {
            Err(BackendError::InvalidRequest(format!(
                "activate from {:?}",
                *state
            )))
        }
    }

    async fn commit_flash(&self) -> BackendResult<()> {
        *self.flash_state.lock() = CoreFlashState::Committed;
        Ok(())
    }

    async fn rollback_flash(&self) -> BackendResult<()> {
        *self.flash_state.lock() = CoreFlashState::RolledBack;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn spawn_with(shape: &'static str) -> (TestServer, Arc<MockBackend>) {
    spawn_with_watchdog(shape, Duration::from_secs(600)).await
}

async fn spawn_with_watchdog(
    shape: &'static str,
    watchdog: Duration,
) -> (TestServer, Arc<MockBackend>) {
    let backend = Arc::new(MockBackend::new("dev1", shape));
    let mut backends = HashMap::new();
    backends.insert(
        "dev1".to_string(),
        backend.clone() as Arc<dyn DiagnosticBackend>,
    );
    let state = AppState::new(backends).with_updates_config(UpdatesConfig {
        orchestrated_watchdog: watchdog,
    });
    let router = create_router(state);
    let server = TestServer::start(router).await.expect("test server");
    (server, backend)
}

/// Spawn a server whose `dev1` backend has a gateway-style child
/// sub-entity `childecu` — used by the C-073 gateway-child test.
async fn spawn_with_child() -> TestServer {
    let child = Arc::new(MockBackend::new("childecu", "singleshot"));
    let backend = Arc::new(MockBackend::with_child("dev1", "singleshot", Some(child)));
    let mut backends = HashMap::new();
    backends.insert("dev1".to_string(), backend as Arc<dyn DiagnosticBackend>);
    let state = AppState::new(backends);
    let router = create_router(state);
    TestServer::start(router).await.expect("test server")
}

fn http() -> reqwest::Client {
    reqwest::Client::new()
}

async fn open_update(server: &TestServer) -> String {
    let url = format!("{}/vehicle/v1/components/dev1/updates", server.base_url());
    let resp = http()
        .post(url)
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("open_update");
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let body: Value = resp.json().await.unwrap();
    body["update_id"].as_str().unwrap().to_string()
}

async fn upload_part(server: &TestServer, update_id: &str, part_id: &str, data: &[u8]) {
    // Percent-encode part_id since SUIT URIs contain '#' which is the
    // URL fragment delimiter; matches the sovd-client behaviour.
    let encoded = part_id.replace('#', "%23");
    let url = format!(
        "{}/vehicle/v1/components/dev1/updates/{}/bulk-data/{}",
        server.base_url(),
        update_id,
        encoded
    );
    let resp = http()
        .put(url)
        .header("content-type", "application/octet-stream")
        .body(data.to_vec())
        .send()
        .await
        .expect("upload");
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
}

async fn put(server: &TestServer, path: &str) -> reqwest::Response {
    let url = format!("{}{}", server.base_url(), path);
    http().put(url).send().await.expect("put")
}

async fn get_status(server: &TestServer, update_id: &str) -> Value {
    let url = format!(
        "{}/vehicle/v1/components/dev1/updates/{}/status",
        server.base_url(),
        update_id
    );
    let resp = http().get(url).send().await.expect("status");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    resp.json().await.unwrap()
}

async fn poll_terminal(server: &TestServer, update_id: &str) -> Value {
    for _ in 0..200 {
        let body = get_status(server, update_id).await;
        match body["status"].as_str() {
            Some("completed") | Some("failed") => return body,
            _ => tokio::time::sleep(Duration::from_millis(25)).await,
        }
    }
    panic!("status never reached terminal");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn singleshot_prepare_execute_round_trip() {
    let (server, _backend) = spawn_with("singleshot").await;
    let id = open_update(&server).await;
    upload_part(&server, &id, "manifest", b"hsm-manifest-bytes").await;

    let resp = put(
        &server,
        &format!("/vehicle/v1/components/dev1/updates/{}/prepare", id),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert!(location.ends_with("/status"));

    let prepared = poll_terminal(&server, &id).await;
    assert_eq!(prepared["phase"], "prepare");
    assert_eq!(prepared["status"], "completed");

    let resp = put(
        &server,
        &format!("/vehicle/v1/components/dev1/updates/{}/execute", id),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);

    let executed = poll_terminal(&server, &id).await;
    assert_eq!(executed["phase"], "execute");
    assert_eq!(executed["status"], "completed");
    assert!(executed.get("error").is_none());
}

#[tokio::test]
async fn banked_prepare_execute_round_trip() {
    let (server, _backend) = spawn_with("banked").await;
    let id = open_update(&server).await;
    upload_part(&server, &id, "manifest", b"banked-manifest").await;
    upload_part(&server, &id, "#kernel", b"\xCAfake-kernel").await;

    let resp = put(
        &server,
        &format!("/vehicle/v1/components/dev1/updates/{}/prepare", id),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    let prepared = poll_terminal(&server, &id).await;
    assert_eq!(prepared["phase"], "prepare");
    assert_eq!(prepared["status"], "completed");

    let resp = put(
        &server,
        &format!("/vehicle/v1/components/dev1/updates/{}/execute", id),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    let executed = poll_terminal(&server, &id).await;
    assert_eq!(executed["phase"], "execute");
    assert_eq!(executed["status"], "completed");
}

#[tokio::test]
async fn prepare_failure_surfaces_in_status() {
    let (server, backend) = spawn_with("singleshot").await;
    *backend.fail_verify.lock() = true;
    let id = open_update(&server).await;
    upload_part(&server, &id, "manifest", b"will-fail").await;

    let resp = put(
        &server,
        &format!("/vehicle/v1/components/dev1/updates/{}/prepare", id),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    let failed = poll_terminal(&server, &id).await;
    assert_eq!(failed["phase"], "prepare");
    assert_eq!(failed["status"], "failed");
    let err = &failed["error"];
    assert_eq!(err["error_code"], "update-preparation-failed");
    assert!(err["message"].as_str().unwrap().contains("forced failure"));
}

#[tokio::test]
async fn execute_requires_prepare_completed() {
    let (server, _backend) = spawn_with("singleshot").await;
    let id = open_update(&server).await;
    upload_part(&server, &id, "manifest", b"x").await;
    let resp = put(
        &server,
        &format!("/vehicle/v1/components/dev1/updates/{}/execute", id),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
}

#[tokio::test]
async fn status_body_shape_matches_table_270() {
    let (server, _backend) = spawn_with("singleshot").await;
    let id = open_update(&server).await;
    let body = get_status(&server, &id).await;
    assert!(body.get("phase").is_some(), "Table 270 phase field");
    assert!(body.get("status").is_some(), "Table 270 status field");
    // Default state: prepare phase, pending status.
    assert_eq!(body["phase"], "prepare");
    assert_eq!(body["status"], "pending");
    // error only when status=failed
    assert!(body.get("error").is_none());
}

// ---------------------------------------------------------------------------
// Phase B — orchestrated extension
// ---------------------------------------------------------------------------

async fn prepare_and_orchestrated_execute(server: &TestServer, id: &str) {
    let resp = put(
        server,
        &format!("/vehicle/v1/components/dev1/updates/{}/prepare", id),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    let prepared = poll_terminal(server, id).await;
    assert_eq!(prepared["status"], "completed");
    let resp = put(
        server,
        &format!(
            "/vehicle/v1/components/dev1/updates/{}/execute?x-sumo-control=orchestrated",
            id
        ),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
}

async fn wait_for_substate(server: &TestServer, id: &str, want: &str) -> Value {
    for _ in 0..200 {
        let body = get_status(server, id).await;
        if body
            .get("x-sumo-substate")
            .and_then(Value::as_str)
            .map(|s| s == want)
            .unwrap_or(false)
        {
            return body;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("substate never reached {want}");
}

#[tokio::test]
async fn orchestrated_banked_commit_round_trip() {
    let (server, backend) = spawn_with("banked").await;
    let id = open_update(&server).await;
    upload_part(&server, &id, "manifest", b"banked").await;
    upload_part(&server, &id, "#kernel", b"\xCAfake").await;
    prepare_and_orchestrated_execute(&server, &id).await;

    let body = wait_for_substate(&server, &id, "awaiting-verdict").await;
    assert_eq!(body["phase"], "execute");
    assert_eq!(body["status"], "inProgress");

    let resp = put(
        &server,
        &format!("/vehicle/v1/components/dev1/updates/{}/x-sumo-commit", id),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);

    let final_body = poll_terminal(&server, &id).await;
    assert_eq!(final_body["phase"], "execute");
    assert_eq!(final_body["status"], "completed");
    assert!(final_body.get("error").is_none());
    assert_eq!(*backend.flash_state.lock(), CoreFlashState::Committed);
}

#[tokio::test]
async fn orchestrated_banked_rollback_round_trip() {
    let (server, backend) = spawn_with("banked").await;
    let id = open_update(&server).await;
    upload_part(&server, &id, "manifest", b"banked").await;
    upload_part(&server, &id, "#kernel", b"\xCAfake").await;
    prepare_and_orchestrated_execute(&server, &id).await;
    wait_for_substate(&server, &id, "awaiting-verdict").await;

    let resp = put(
        &server,
        &format!("/vehicle/v1/components/dev1/updates/{}/x-sumo-rollback", id),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);

    let final_body = poll_terminal(&server, &id).await;
    assert_eq!(final_body["status"], "failed");
    assert_eq!(
        final_body["error"]["error_code"], "x-sumo-verdict-rollback",
        "rollback should attribute the failure to the orchestrator's verdict"
    );
    assert_eq!(*backend.flash_state.lock(), CoreFlashState::RolledBack);
}

#[tokio::test]
async fn orchestrated_banked_watchdog_auto_rollback() {
    // Short watchdog so the test doesn't wait the default 10 minutes.
    let (server, backend) = spawn_with_watchdog("banked", Duration::from_millis(250)).await;
    let id = open_update(&server).await;
    upload_part(&server, &id, "manifest", b"banked").await;
    upload_part(&server, &id, "#kernel", b"\xCAfake").await;
    prepare_and_orchestrated_execute(&server, &id).await;

    // Don't post a verdict — watchdog should fire and roll back.
    let final_body = poll_terminal(&server, &id).await;
    assert_eq!(final_body["status"], "failed");
    assert_eq!(final_body["error"]["error_code"], "x-sumo-verdict-rollback");
    assert_eq!(*backend.flash_state.lock(), CoreFlashState::RolledBack);
}

#[tokio::test]
async fn x_sumo_commit_rejected_when_not_awaiting_verdict() {
    let (server, _backend) = spawn_with("banked").await;
    let id = open_update(&server).await;
    upload_part(&server, &id, "manifest", b"x").await;
    // No prepare/execute yet → entry is at prepare/pending.
    let resp = put(
        &server,
        &format!("/vehicle/v1/components/dev1/updates/{}/x-sumo-commit", id),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
}

#[tokio::test]
async fn orchestrated_on_singleshot_falls_through_to_standard() {
    // Singleshot has no trial phase; the query parameter is silently
    // ignored and execute auto-completes.
    let (server, _backend) = spawn_with("singleshot").await;
    let id = open_update(&server).await;
    upload_part(&server, &id, "manifest", b"hsm").await;
    let resp = put(
        &server,
        &format!("/vehicle/v1/components/dev1/updates/{}/prepare", id),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    poll_terminal(&server, &id).await;
    let resp = put(
        &server,
        &format!(
            "/vehicle/v1/components/dev1/updates/{}/execute?x-sumo-control=orchestrated",
            id
        ),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    let final_body = poll_terminal(&server, &id).await;
    assert_eq!(final_body["status"], "completed");
    assert!(final_body.get("x-sumo-substate").is_none());
}

#[tokio::test]
async fn discovery_endpoint_lists_x_sumo_extensions() {
    let (server, _backend) = spawn_with("singleshot").await;
    let url = format!("{}/.well-known/sovd-extensions", server.base_url());
    let resp = http().get(url).send().await.expect("discovery");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    let exts = &body["extensions"];
    assert!(exts.get("x-sumo-control").is_some());
    assert!(exts.get("x-sumo-bulk-data").is_some());
    let verbs = &exts["x-sumo-control"]["verbs"];
    assert!(
        verbs
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str().unwrap_or("").contains("x-sumo-commit")),
        "x-sumo-commit verb should be advertised"
    );
}

// ---------------------------------------------------------------------------
// Phase C — FlashClient driving the spec wire end-to-end
// ---------------------------------------------------------------------------

fn flash_client_for(server: &TestServer) -> sovd_client::FlashClient {
    let cfg = sovd_client::flash::FlashConfig::builder(server.base_url())
        .component_id("dev1")
        // Tight polling so the test isn't dominated by sleeps.
        .flash_poll_ms(25)
        .build();
    sovd_client::FlashClient::new(cfg).expect("flash client")
}

#[tokio::test]
async fn flash_client_drives_singleshot_via_prepare_execute() {
    let (server, _backend) = spawn_with("singleshot").await;
    let client = flash_client_for(&server);
    client.open_update().await.expect("open_update");
    client
        .upload_part("manifest", b"hsm-bytes")
        .await
        .expect("upload_part");
    let prepared = client.prepare().await.expect("prepare");
    assert_eq!(prepared.status, "completed");
    let executed = client.execute(false).await.expect("execute");
    assert_eq!(executed.phase, "execute");
    assert_eq!(executed.status, "completed");
}

#[tokio::test]
async fn flash_client_drives_banked_orchestrated_then_spec_commit() {
    let (server, backend) = spawn_with("banked").await;
    let client = flash_client_for(&server);
    client.open_update().await.expect("open_update");
    client
        .upload_part("manifest", b"banked")
        .await
        .expect("manifest");
    client
        .upload_part("#kernel", b"\xCAkern")
        .await
        .expect("payload");

    let prepared = client.prepare().await.expect("prepare");
    assert_eq!(prepared.status, "completed");

    let paused = client.execute(true).await.expect("execute(orchestrated)");
    assert_eq!(paused.phase, "execute");
    assert_eq!(paused.status, "inProgress");
    assert_eq!(paused.substate.as_deref(), Some("awaiting-verdict"));

    let committed = client.spec_commit().await.expect("spec_commit");
    assert_eq!(committed.status, "completed");
    assert_eq!(*backend.flash_state.lock(), CoreFlashState::Committed);
    assert!(
        client.current_update_id().await.is_none(),
        "spec_commit should clear the local update_id"
    );
}

#[tokio::test]
async fn flash_client_drives_banked_orchestrated_then_spec_rollback() {
    let (server, backend) = spawn_with("banked").await;
    let client = flash_client_for(&server);
    client.open_update().await.expect("open_update");
    client
        .upload_part("manifest", b"banked")
        .await
        .expect("manifest");
    client
        .upload_part("#kernel", b"\xCAkern")
        .await
        .expect("payload");
    client.prepare().await.expect("prepare");
    client.execute(true).await.expect("execute(orchestrated)");

    let rolled_back = client.spec_rollback().await.expect("spec_rollback");
    assert_eq!(rolled_back.status, "failed");
    assert_eq!(
        rolled_back.error.as_ref().unwrap().error_code,
        "x-sumo-verdict-rollback"
    );
    assert_eq!(*backend.flash_state.lock(), CoreFlashState::RolledBack);
}

#[tokio::test]
async fn flash_client_automated_runs_prepare_then_execute() {
    let (server, _backend) = spawn_with("singleshot").await;
    let client = flash_client_for(&server);
    client.open_update().await.expect("open_update");
    client.upload_part("manifest", b"x").await.expect("upload");

    let final_status = client.automated().await.expect("automated");
    assert_eq!(final_status.phase, "execute");
    assert_eq!(final_status.status, "completed");
}

#[tokio::test]
async fn flash_client_propagates_prepare_failure() {
    let (server, backend) = spawn_with("singleshot").await;
    let client = flash_client_for(&server);
    *backend.fail_verify.lock() = true;
    client.open_update().await.expect("open_update");
    client
        .upload_part("manifest", b"will-fail")
        .await
        .expect("upload");
    let prepared = client.prepare().await.expect("prepare polls to terminal");
    assert_eq!(prepared.status, "failed");
    assert_eq!(
        prepared.error.as_ref().unwrap().error_code,
        "update-preparation-failed"
    );
}

#[tokio::test]
async fn flash_client_spec_status_carries_table_270_shape() {
    let (server, _backend) = spawn_with("singleshot").await;
    let client = flash_client_for(&server);
    client.open_update().await.expect("open_update");
    let body = client.spec_status().await.expect("spec_status");
    assert_eq!(body.phase, "prepare");
    assert_eq!(body.status, "pending");
    assert!(body.error.is_none());
    assert!(body.substate.is_none());
}

#[tokio::test]
async fn executions_wire_is_gone() {
    // The F.D8b vendor /executions{action} wire was retired in
    // Phase E along with all FlashClient deprecated methods.  POST
    // to that path now 404s; callers must use the spec verbs
    // (PUT /prepare, /execute, /x-sumo-commit, /x-sumo-rollback,
    // /x-sumo-force-rollback).
    let (server, _backend) = spawn_with("singleshot").await;
    let id = open_update(&server).await;
    upload_part(&server, &id, "manifest", b"m").await;
    let url = format!(
        "{}/vehicle/v1/components/dev1/updates/{}/executions",
        server.base_url(),
        id
    );
    let resp = http()
        .post(url)
        .json(&serde_json::json!({"action": "verify"}))
        .send()
        .await
        .expect("post executions");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::NOT_FOUND,
        "POST /executions should 404 after Phase E retirement (axum strips the route)"
    );
}

// ---------------------------------------------------------------------------
// C-063 — scoped online capability description ({path}/docs)
// ISO 17978-3 §6.3.3/7.5
// ---------------------------------------------------------------------------

/// HTTP GET helper for the docs tests.
async fn get_json(server: &TestServer, path: &str) -> (reqwest::StatusCode, Value) {
    let url = format!("{}{}", server.base_url(), path);
    let resp = http().get(url).send().await.expect("get");
    let status = resp.status();
    let body = resp.json().await.expect("json body");
    (status, body)
}

/// C-063: `GET {entity}/docs` returns 200 with an OpenAPI 3.1.0 doc whose
/// `paths` are scoped to that entity path, and is a strict subset of the
/// global `/vehicle/v1/docs`.
#[tokio::test]
async fn scoped_docs_are_path_scoped_subset_of_global() {
    let (server, _backend) = spawn_with("singleshot").await;

    // Global doc — still 200 with every path (unchanged behaviour).
    let (g_status, global) = get_json(&server, "/vehicle/v1/docs").await;
    assert_eq!(g_status, reqwest::StatusCode::OK);
    assert_eq!(global["openapi"], "3.1.0");
    let global_paths = global["paths"].as_object().expect("global paths object");
    assert!(!global_paths.is_empty(), "global doc must list paths");

    // Scoped doc — note vtx_ecm is NOT a registered backend; scoping is
    // purely by path-template match, existence is not validated (C-063).
    let scope = "/vehicle/v1/components/vtx_ecm";
    let (s_status, scoped) = get_json(&server, &format!("{scope}/docs")).await;
    assert_eq!(s_status, reqwest::StatusCode::OK);
    assert_eq!(scoped["openapi"], "3.1.0");
    assert!(
        scoped["info"].get("x-sovd-version").is_some(),
        "info.x-sovd-version must be present"
    );

    let scoped_paths = scoped["paths"].as_object().expect("scoped paths object");
    assert!(
        !scoped_paths.is_empty(),
        "scoped doc must have a non-empty paths object"
    );

    // Every emitted path is under the entity prefix …
    for key in scoped_paths.keys() {
        assert!(
            key.starts_with(scope),
            "scoped path {key:?} must start with {scope:?}"
        );
    }
    // … and it's a strict subset of the global path set (fewer entries,
    // and the global doc carries server-level paths like /health that a
    // component-scoped doc must not).
    assert!(
        scoped_paths.len() < global_paths.len(),
        "scoped paths ({}) should be fewer than global ({})",
        scoped_paths.len(),
        global_paths.len()
    );
    assert!(
        !scoped_paths.contains_key("/health"),
        "scoped component doc must not include server-level /health"
    );
    // A representative concrete substitution: the data sub-resource
    // template keeps its tail placeholder but pins the component id.
    assert!(
        scoped_paths.contains_key("/vehicle/v1/components/vtx_ecm/data/{param_id}"),
        "expected concrete-id + tail-placeholder path; got keys: {:?}",
        scoped_paths.keys().collect::<Vec<_>>()
    );
}

/// Unit-level check of the scoping builder independent of HTTP, asserting
/// the prefix invariant and that scoping strictly narrows the path set.
#[test]
fn build_capability_doc_scopes_to_component_prefix() {
    use sovd_api::handlers::meta::build_capability_doc;

    let global = build_capability_doc(None);
    let global_paths = global["paths"].as_object().unwrap();

    let scope = "/vehicle/v1/components/vtx_ecm";
    let scoped = build_capability_doc(Some(scope));
    assert_eq!(scoped["openapi"], "3.1.0");
    let scoped_paths = scoped["paths"].as_object().unwrap();

    assert!(!scoped_paths.is_empty());
    assert!(scoped_paths.len() < global_paths.len());
    for key in scoped_paths.keys() {
        assert!(key.starts_with(scope), "{key:?} not under {scope:?}");
    }
    // Templated tail is preserved; matched prefix is concrete.
    assert!(scoped_paths.contains_key("/vehicle/v1/components/vtx_ecm/faults/{fault_id}"));

    // An entity path no template matches yields a valid-but-empty paths
    // object, never a panic / missing envelope.
    let empty = build_capability_doc(Some("/no/such/entity/at/all"));
    assert_eq!(empty["openapi"], "3.1.0");
    assert!(empty["paths"].as_object().unwrap().is_empty());
}

/// C-060 (ISO 17978-3 §6.2.1 / Table 21): the capability description
/// declares `security` plus a bearer `securityScheme` — for both the
/// global and scoped docs. Token enforcement is the deferred auth slice;
/// the doc declares the intended mechanism per §5.4.4.
#[test]
fn build_capability_doc_declares_security() {
    use sovd_api::handlers::meta::build_capability_doc;

    for doc in [
        build_capability_doc(None),
        build_capability_doc(Some("/vehicle/v1/components/vtx_ecm")),
    ] {
        let security = doc["security"]
            .as_array()
            .expect("capability description must carry a `security` array");
        assert!(!security.is_empty(), "security must declare a requirement");
        assert!(
            security[0].get("bearerAuth").is_some(),
            "security references the bearerAuth scheme"
        );
        let scheme = &doc["components"]["securitySchemes"]["bearerAuth"];
        assert_eq!(scheme["type"], "http", "bearerAuth is an http scheme");
        assert_eq!(scheme["scheme"], "bearer");
        assert!(scheme["bearerFormat"].is_string());
    }
}

// ---------------------------------------------------------------------------
// C-070 — SSE Content-Type: text/event-stream  (ISO 17978-3 §5.2.2)
// ---------------------------------------------------------------------------

/// Create a cyclic subscription on `dev1` for `resource`, returning the
/// raw response so the caller can assert the status / read the body.
async fn create_subscription(server: &TestServer, resource: &str) -> reqwest::Response {
    let url = format!(
        "{}/vehicle/v1/components/dev1/cyclic-subscriptions",
        server.base_url()
    );
    http()
        .post(url)
        .json(&serde_json::json!({ "resource": resource, "interval": "slow" }))
        .send()
        .await
        .expect("create subscription")
}

/// C-070: the inline `streams` SSE responder sets
/// `Content-Type: text/event-stream`.
#[tokio::test]
async fn sse_inline_stream_carries_event_stream_ct() {
    let (server, _backend) = spawn_with("singleshot").await;
    let url = format!(
        "{}/vehicle/v1/components/dev1/streams?parameters=coolant_temp&rate_hz=2",
        server.base_url()
    );
    let resp = http().get(url).send().await.expect("open inline SSE");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(
        ct.starts_with("text/event-stream"),
        "inline SSE content-type must start with text/event-stream, got {ct:?}"
    );
}

/// C-070: the per-subscription `streams/{id}` SSE responder sets
/// `Content-Type: text/event-stream`.
#[tokio::test]
async fn sse_subscription_stream_carries_event_stream_ct() {
    let (server, _backend) = spawn_with("singleshot").await;

    // Create a subscription on a GET-able direct resource, then attach.
    let created = create_subscription(&server, "coolant_temp").await;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let body: Value = created.json().await.unwrap();
    let sub_id = body["subscription_id"].as_str().unwrap();

    let url = format!(
        "{}/vehicle/v1/components/dev1/streams/{}",
        server.base_url(),
        sub_id
    );
    let resp = http().get(url).send().await.expect("open subscription SSE");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(
        ct.starts_with("text/event-stream"),
        "subscription SSE content-type must start with text/event-stream, got {ct:?}"
    );
}

// ---------------------------------------------------------------------------
// C-073 — subscription resource must be same-entity + GET-able  (§7.10.3)
// ---------------------------------------------------------------------------

/// C-073: a valid direct resource (a GET-able parameter on the addressed
/// entity) is accepted → 201.
#[tokio::test]
async fn cyclic_subscription_valid_direct_resource_201() {
    let (server, _backend) = spawn_with("singleshot").await;
    let resp = create_subscription(&server, "coolant_temp").await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    // The spec uri-reference form `data/<param>` is tolerated too.
    let resp = create_subscription(&server, "data/coolant_temp").await;
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
}

/// C-073: a resource that resolves to nothing on the entity is rejected
/// with 4xx (400 incomplete-request) — not silently accepted.
#[tokio::test]
async fn cyclic_subscription_bogus_resource_4xx() {
    let (server, _backend) = spawn_with("singleshot").await;
    let resp = create_subscription(&server, "no_such_parameter").await;
    assert!(
        resp.status().is_client_error(),
        "bogus subscription resource must be a 4xx, got {}",
        resp.status()
    );
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error_code"], "incomplete-request");
}

/// C-073 + regression for `aad70a4`: a gateway-child resource
/// (`child/param`) still resolves via the sub-entity and is accepted → 201.
#[tokio::test]
async fn cyclic_subscription_gateway_child_resource_201() {
    let server = spawn_with_child().await;
    let resp = create_subscription(&server, "childecu/coolant_temp").await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CREATED,
        "gateway-child subscription resource must still succeed (aad70a4)"
    );

    // A child that doesn't exist is rejected.
    let resp = create_subscription(&server, "ghostchild/coolant_temp").await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // A real child but a bogus param on it is rejected.
    let resp = create_subscription(&server, "childecu/no_such_parameter").await;
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
}

// ---------------------------------------------------------------------------
// C-005 — version-info lists ALL supported versions  (§7.4.2)
// ---------------------------------------------------------------------------

/// C-005: `/version-info` derives its `versions` array from the shared
/// `API_VERSIONS` source of truth (v1 / /vehicle/v1 / x-sovd-version 1.1).
#[tokio::test]
async fn version_info_lists_all_supported_versions() {
    let (server, _backend) = spawn_with("singleshot").await;
    let url = format!("{}/version-info", server.base_url());
    let resp = http().get(url).send().await.expect("version-info");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.unwrap();

    let versions = body["versions"].as_array().expect("versions array");
    assert_eq!(versions.len(), 1, "exactly the one mounted edition");
    let v1 = &versions[0];
    assert_eq!(v1["version_identifier"], "v1");
    assert_eq!(v1["base_path"], "/vehicle/v1");
    assert_eq!(v1["x-sovd-version"], "1.1");
}

/// C-005: the HTTP body is exactly what the shared `API_VERSIONS` builder
/// produces — proving it's derived, not an independent literal.
#[tokio::test]
async fn version_info_is_sourced_from_shared_definition() {
    use sovd_api::handlers::meta::{build_version_info, API_VERSIONS};

    // The builder reflects the const slice 1:1.
    let built = build_version_info();
    assert_eq!(built.versions.len(), API_VERSIONS.len());
    for (entry, (id, base, sovd)) in built.versions.iter().zip(API_VERSIONS) {
        assert_eq!(entry.version_identifier, *id);
        assert_eq!(entry.base_path, *base);
        assert_eq!(entry.x_sovd_version, *sovd);
    }

    // …and the served body equals the builder output.
    let (server, _backend) = spawn_with("singleshot").await;
    let url = format!("{}/version-info", server.base_url());
    let served: Value = http().get(url).send().await.unwrap().json().await.unwrap();
    let expected = serde_json::to_value(&built).unwrap();
    assert_eq!(served, expected);
}

// ---------------------------------------------------------------------------
// (C-110 — /updates ?origin query + reserved 'autonomous' package id are
// covered by the §7.18 catalog reshape tests, not here.)
// ---------------------------------------------------------------------------
