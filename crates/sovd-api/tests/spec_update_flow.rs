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
use sovd_api::{create_router, AppState};
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
    /// part_id → (file_id, sha256)
    parts: Mutex<Vec<(String, String, String)>>,
    /// monotonic counter for file_id allocation
    next_id: Mutex<u64>,
    /// transfer_id allocated by start_flash
    transfer_id: Mutex<Option<String>>,
    /// VmBackend-style flash transfer state
    flash_state: Mutex<CoreFlashState>,
    /// Toggle to make verify_part fail (for the failure-path test)
    fail_verify: Mutex<bool>,
}

impl MockBackend {
    fn new(id: &str, shape: &'static str) -> Self {
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
            parts: Mutex::new(Vec::new()),
            next_id: Mutex::new(0),
            transfer_id: Mutex::new(None),
            flash_state: Mutex::new(CoreFlashState::Transferring),
            fail_verify: Mutex::new(false),
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
        Ok(Vec::new())
    }
    async fn read_data(&self, _ids: &[String]) -> BackendResult<Vec<DataValue>> {
        Ok(Vec::new())
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
    let backend = Arc::new(MockBackend::new("dev1", shape));
    let mut backends = HashMap::new();
    backends.insert(
        "dev1".to_string(),
        backend.clone() as Arc<dyn DiagnosticBackend>,
    );
    let state = AppState::new(backends);
    let router = create_router(state);
    let server = TestServer::start(router).await.expect("test server");
    (server, backend)
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

#[tokio::test]
async fn executions_wire_carries_deprecation_header() {
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
    let deprecation = resp.headers().get("deprecation");
    assert!(
        deprecation.is_some(),
        "Deprecation header missing on /executions response"
    );
    assert_eq!(deprecation.unwrap(), "true");
}
