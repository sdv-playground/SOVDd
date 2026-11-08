//! ISO 17978-3:2026 conformance suite — the same clause-cited checks run
//! against both trees (`/v1` and `/vehicle/v1`).
//!
//! Every known gap is an `#[ignore = "Fxx: <clause> — <one line>"]` test that
//! fails today; `cargo test -p sovd-api --test conformance -- --ignored` lists
//! the backlog. See `README.md` in this directory.

#[path = "../common/mod.rs"]
mod common;

mod auth;
mod bulk_data;
mod discovery;
mod errors;
mod operations;
mod updates;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use sovd_api::{create_router, AppState, Tree};
use sovd_client::testing::TestServer;
use sovd_core::{DiagnosticBackend, VerdictScope};

use common::{BankedBackend, MockBackend};

/// The update-capable component (banked executor, bulk-data capable).
pub const OTA: &str = "ota";

/// `ecu_a` / `ecu_b` (plain mocks) and [`OTA`], without authorization.
pub async fn server() -> TestServer {
    let mut ota = BankedBackend::new(OTA, VerdictScope::Update);
    ota.capabilities.bulk_data = true;
    let mut backends: HashMap<String, Arc<dyn DiagnosticBackend>> = HashMap::new();
    for id in ["ecu_a", "ecu_b"] {
        backends.insert(id.to_string(), Arc::new(MockBackend::new(id)));
    }
    backends.insert(OTA.to_string(), Arc::new(ota));
    TestServer::start(create_router(AppState::new(backends)))
        .await
        .expect("test server")
}

/// Send `method path` with optional extra headers and body.
pub async fn send(
    server: &TestServer,
    method: reqwest::Method,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<Vec<u8>>,
) -> reqwest::Response {
    let mut req = reqwest::Client::new().request(method, format!("{}{path}", server.base_url()));
    for (name, value) in headers {
        req = req.header(*name, *value);
    }
    if let Some(body) = body {
        req = req.body(body);
    }
    req.send().await.expect("request")
}

/// A response header as a string, if present.
pub fn header(resp: &reqwest::Response, name: &str) -> Option<String> {
    resp.headers()
        .get(name)
        .map(|v| v.to_str().expect("ASCII header").to_string())
}

/// Assert `body` is a `GenericError` (§5.8.3 Table 16): `error_code` and
/// `message` strings, `parameters` an object when present.
pub fn assert_generic_error(body: &Value, ctx: &str) {
    assert!(body["error_code"].is_string(), "{ctx}: {body}");
    assert!(body["message"].is_string(), "{ctx}: {body}");
    if let Some(parameters) = body.get("parameters") {
        assert!(parameters.is_object(), "{ctx}: {body}");
    }
}

/// `{tree.component(OTA)}/updates/{id}`.
pub fn update_url(tree: Tree, id: &str) -> String {
    format!("{}/updates/{id}", tree.component(OTA))
}

/// Register update `id` on [`OTA`]; returns the response.
pub async fn register(server: &TestServer, tree: Tree, body: Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!(
            "{}{}/updates",
            server.base_url(),
            tree.component(OTA)
        ))
        .json(&body)
        .send()
        .await
        .expect("register")
}

/// GET `{update}/status`, which must be 200.
pub async fn status(server: &TestServer, update: &str) -> Value {
    let resp = send(
        server,
        reqwest::Method::GET,
        &format!("{update}/status"),
        &[],
        None,
    )
    .await;
    assert_eq!(resp.status().as_u16(), 200, "{update}/status");
    resp.json().await.expect("status body")
}

/// Poll `{update}/status` until `done` holds.
pub async fn poll_until(server: &TestServer, update: &str, done: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..200 {
        let body = status(server, update).await;
        if done(&body) {
            return body;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("{update} never reached the expected state");
}

pub fn terminal(body: &Value) -> bool {
    matches!(body["status"].as_str(), Some("completed") | Some("failed"))
}

/// Upload the manifest part and run `prepare` to completion.
pub async fn upload_and_prepare(server: &TestServer, update: &str) {
    let resp = send(
        server,
        reqwest::Method::PUT,
        &format!("{update}/bulk-data/manifest"),
        &[("content-type", "application/octet-stream")],
        Some(b"banked".to_vec()),
    )
    .await;
    assert_eq!(resp.status().as_u16(), 201, "{update}: manifest upload");
    let resp = send(
        server,
        reqwest::Method::PUT,
        &format!("{update}/prepare"),
        &[],
        None,
    )
    .await;
    assert_eq!(resp.status().as_u16(), 202, "{update}/prepare");
    let done = poll_until(server, update, terminal).await;
    assert_eq!(done["status"], "completed", "{update}: {done}");
}
