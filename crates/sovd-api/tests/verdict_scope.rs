//! Verdict scope — under `/v1`, a per-update verdict (and force-rollback) on a
//! component whose updates are settled at node level is refused with 409
//! `precondition-not-fulfilled`, before any state check, with a message naming
//! the node operations. `/vehicle/v1` keeps accepting them.
//!
//! Two mock components over one shared `AppState`: `node_member`
//! (`VerdictScope::Node`) and `solo` (the default, `VerdictScope::Update`).
//! Same in-process `TestServer` pattern as `spec_update_flow.rs`; the mock
//! lives in `common`.

mod common;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use common::BankedBackend;
use serde_json::Value;
use sovd_api::{create_router, AppState};
use sovd_client::testing::TestServer;
use sovd_core::{DiagnosticBackend, FlashState, VerdictScope};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

struct Fixture {
    server: TestServer,
    node_member: Arc<BankedBackend>,
}

async fn spawn() -> Fixture {
    let node_member = Arc::new(BankedBackend::new("node_member", VerdictScope::Node));
    let solo = Arc::new(BankedBackend::new("solo", VerdictScope::Update));
    let mut backends = HashMap::new();
    backends.insert(
        "node_member".to_string(),
        node_member.clone() as Arc<dyn DiagnosticBackend>,
    );
    backends.insert("solo".to_string(), solo as Arc<dyn DiagnosticBackend>);
    let server = TestServer::start(create_router(AppState::new(backends)))
        .await
        .expect("test server");
    Fixture {
        server,
        node_member,
    }
}

fn url(server: &TestServer, path: &str) -> String {
    format!("{}{}", server.base_url(), path)
}

async fn put(server: &TestServer, path: &str) -> reqwest::Response {
    reqwest::Client::new()
        .put(url(server, path))
        .send()
        .await
        .expect("put")
}

/// Register an update on `component` under `base`; returns its id.
async fn register(server: &TestServer, base: &str, component: &str, id: &str) -> String {
    let resp = reqwest::Client::new()
        .post(url(
            server,
            &format!("{base}/components/{component}/updates"),
        ))
        .json(&serde_json::json!({ "id": id }))
        .send()
        .await
        .expect("register");
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let body: Value = resp.json().await.unwrap();
    body["update_id"].as_str().unwrap().to_string()
}

async fn status(server: &TestServer, base: &str, component: &str, id: &str) -> Value {
    let resp = reqwest::Client::new()
        .get(url(
            server,
            &format!("{base}/components/{component}/updates/{id}/status"),
        ))
        .send()
        .await
        .expect("status");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    resp.json().await.unwrap()
}

async fn poll_until(
    server: &TestServer,
    base: &str,
    component: &str,
    id: &str,
    done: impl Fn(&Value) -> bool,
) -> Value {
    for _ in 0..200 {
        let body = status(server, base, component, id).await;
        if done(&body) {
            return body;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("update {id} on {component} never reached the expected state");
}

fn terminal(body: &Value) -> bool {
    matches!(body["status"].as_str(), Some("completed") | Some("failed"))
}

fn awaiting_verdict(body: &Value) -> bool {
    body["x-ota-substate"] == "awaiting-verdict"
}

/// Register, upload, prepare and orchestrated-execute an update on
/// `component` under `base`, until it awaits a verdict.
async fn drive_to_awaiting_verdict(
    server: &TestServer,
    base: &str,
    component: &str,
    id: &str,
) -> String {
    let id = register(server, base, component, id).await;
    let updates = format!("{base}/components/{component}/updates/{id}");
    let resp = reqwest::Client::new()
        .put(url(server, &format!("{updates}/bulk-data/manifest")))
        .header("content-type", "application/octet-stream")
        .body(b"banked".to_vec())
        .send()
        .await
        .expect("upload");
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let resp = put(server, &format!("{updates}/prepare")).await;
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    let prepared = poll_until(server, base, component, &id, terminal).await;
    assert_eq!(prepared["status"], "completed");
    let resp = put(
        server,
        &format!("{updates}/execute?x-ota-control=orchestrated"),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    poll_until(server, base, component, &id, awaiting_verdict).await;
    id
}

/// Assert a node-transaction refusal: 409 `precondition-not-fulfilled`,
/// naming the `/v1` node operations, never "trial mode".
async fn assert_node_refusal(resp: reqwest::Response) {
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error_code"], "precondition-not-fulfilled");
    let message = body["message"].as_str().unwrap();
    assert!(
        message.contains("belongs to a node transaction"),
        "{message}"
    );
    assert!(
        message.contains("POST /v1/operations/x-update-commit/executions"),
        "{message}"
    );
    assert!(
        message.contains("POST /v1/operations/x-update-rollback/executions"),
        "{message}"
    );
    assert!(!message.contains("trial mode"), "{message}");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn v1_refuses_per_update_verdicts_on_node_member() {
    let f = spawn().await;
    let id = drive_to_awaiting_verdict(&f.server, "/v1", "node_member", "u-a").await;
    let update = format!("/v1/components/node_member/updates/{id}");

    for verb in [
        "x-update-commit",
        "x-update-rollback",
        "x-ota-commit",
        "x-ota-rollback",
    ] {
        let resp = put(&f.server, &format!("{update}/{verb}")).await;
        let status = resp.status();
        let body: Value = resp.json().await.unwrap();
        assert_eq!(status, reqwest::StatusCode::CONFLICT, "{verb}: {body}");
        let message = body["message"].as_str().unwrap();
        assert!(
            message.starts_with(&format!(
                "update '{id}' on 'node_member' belongs to a node transaction"
            )),
            "{verb}: {message}"
        );
    }
    assert_node_refusal(put(&f.server, &format!("{update}/x-update-commit")).await).await;
    assert_node_refusal(put(&f.server, &format!("{update}/x-update-rollback")).await).await;

    let resp = put(&f.server, "/v1/components/node_member/x-ota-force-rollback").await;
    let body: Value = resp.json().await.unwrap();
    assert!(body["message"]
        .as_str()
        .unwrap()
        .starts_with("component 'node_member' belongs to a node transaction"));
    assert_node_refusal(put(&f.server, "/v1/components/node_member/x-ota-force-rollback").await)
        .await;

    // Nothing was settled: the update still awaits its verdict and the
    // backend never saw commit or rollback.
    let body = status(&f.server, "/v1", "node_member", &id).await;
    assert_eq!(body["status"], "inProgress");
    assert_eq!(body["x-ota-substate"], "awaiting-verdict");
    assert_eq!(*f.node_member.flash_state.lock(), FlashState::Activated);
}

#[tokio::test]
async fn legacy_accepts_commit_on_node_member() {
    let f = spawn().await;
    let base = "/vehicle/v1";
    let id = drive_to_awaiting_verdict(&f.server, base, "node_member", "u-b").await;
    let resp = put(
        &f.server,
        &format!("{base}/components/node_member/updates/{id}/x-update-commit"),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    let done = poll_until(&f.server, base, "node_member", &id, terminal).await;
    assert_eq!(done["status"], "completed");
    assert_eq!(*f.node_member.flash_state.lock(), FlashState::Committed);
}

#[tokio::test]
async fn legacy_accepts_rollback_and_force_rollback_on_node_member() {
    let f = spawn().await;
    let base = "/vehicle/v1";
    let id = drive_to_awaiting_verdict(&f.server, base, "node_member", "u-c").await;
    let resp = put(
        &f.server,
        &format!("{base}/components/node_member/updates/{id}/x-update-rollback"),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    let done = poll_until(&f.server, base, "node_member", &id, terminal).await;
    assert_eq!(done["status"], "failed");
    assert_eq!(done["error"]["error_code"], "x-ota-verdict-rollback");
    assert_eq!(*f.node_member.flash_state.lock(), FlashState::RolledBack);

    let resp = put(
        &f.server,
        &format!("{base}/components/node_member/x-ota-force-rollback"),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn v1_accepts_commit_on_solo() {
    let f = spawn().await;
    let id = drive_to_awaiting_verdict(&f.server, "/v1", "solo", "u-d").await;
    let resp = put(
        &f.server,
        &format!("/v1/components/solo/updates/{id}/x-update-commit"),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    let done = poll_until(&f.server, "/v1", "solo", &id, terminal).await;
    assert_eq!(done["status"], "completed");
}

#[tokio::test]
async fn v1_scope_refusal_precedes_state_check() {
    let f = spawn().await;
    let id = register(&f.server, "/v1", "node_member", "u-e").await;
    let update = format!("/v1/components/node_member/updates/{id}");
    assert_node_refusal(put(&f.server, &format!("{update}/x-update-commit")).await).await;
    assert_node_refusal(put(&f.server, &format!("{update}/x-update-rollback")).await).await;

    // The same request on an Update-scope component gets the state conflict.
    let solo_id = register(&f.server, "/v1", "solo", "u-f").await;
    let resp = put(
        &f.server,
        &format!("/v1/components/solo/updates/{solo_id}/x-update-commit"),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    let body: Value = resp.json().await.unwrap();
    assert!(!body["message"]
        .as_str()
        .unwrap()
        .contains("node transaction"));
}
