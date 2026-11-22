//! Node topology — a server configured with a node serves its backends on `/v1`
//! as the node's subcomponents (`/v1/components/{node}/subcomponents/{id}/...`)
//! only.
//!
//! Covers: without a node nothing changes; `/v1/components` lists the node
//! alone; the node document; the subcomponents listing; a member's entity API
//! under the node form, with node-form hrefs and `Location`s (a whole update
//! flow walked); node-level resources under the node served by their root
//! forms' handlers, embedder routes (node-level data items and the whole status
//! family included); the node-level data and operations collections list the
//! embedder's contributed items, unless an embedder route serves them; the flat
//! member paths and the root forms of node-level resources answer 404, the
//! root documents a client starts from do not; other node paths 404; the
//! node's identification; the legacy tree is unchanged with a node; the `/v1`
//! capability doc lists the node forms only; the component scope applies
//! through the node form; refused configurations.

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use axum::http::{StatusCode, Uri};
use axum::response::IntoResponse;
use axum::Router;
use common::*;
use serde_json::Value;
use sovd_api::{create_router, with_node_topology, AppState, NodeTopology};
use sovd_client::testing::TestServer;
use sovd_core::{DiagnosticBackend, VerdictScope};

/// A state over mock backends with the ids `ids`.
fn members(ids: &[&str]) -> AppState {
    let backends: HashMap<String, Arc<dyn DiagnosticBackend>> = ids
        .iter()
        .map(|id| {
            (
                id.to_string(),
                Arc::new(MockBackend::new(id)) as Arc<dyn DiagnosticBackend>,
            )
        })
        .collect();
    AppState::new(backends)
}

/// A state over banked update executors `vm1` and `vm2`.
fn banked() -> AppState {
    let backends: HashMap<String, Arc<dyn DiagnosticBackend>> = ["vm1", "vm2"]
        .iter()
        .map(|id| {
            (
                id.to_string(),
                Arc::new(BankedBackend::new(id, VerdictScope::Update))
                    as Arc<dyn DiagnosticBackend>,
            )
        })
        .collect();
    AppState::new(backends)
}

/// An embedder's own root-form node-level routes, merged into the router: each
/// answers the path it was routed as, the restart with a root-form `Location`;
/// the data item `x-update-state` answers a fixed value.
fn embedder_routes() -> Router {
    async fn served(uri: Uri) -> impl IntoResponse {
        axum::Json(serde_json::json!({ "served": uri.path() }))
    }
    async fn restart(uri: Uri) -> impl IntoResponse {
        (
            StatusCode::ACCEPTED,
            [(axum::http::header::LOCATION, "/v1/status")],
            axum::Json(serde_json::json!({ "served": uri.path() })),
        )
    }
    async fn update_state() -> impl IntoResponse {
        axum::Json(serde_json::json!({ "id": "x-update-state", "data": { "state": "idle" } }))
    }
    Router::new()
        .route("/v1/status", axum::routing::get(served))
        .route("/v1/status/x-boot-id", axum::routing::get(served))
        .route("/v1/data/x-update-state", axum::routing::get(update_state))
        .route("/v1/status/restart", axum::routing::put(restart))
        .route(
            "/v1/operations/x-update-commit/executions",
            axum::routing::post(served),
        )
}

/// `state` as node `cvc`, its router (with the embedder's routes merged in)
/// wrapped in the node topology.
async fn node_server(state: AppState) -> TestServer {
    node_server_with(state, Router::new()).await
}

/// [`node_server`] with the embedder's `extra` routes merged in too.
async fn node_server_with(state: AppState, extra: Router) -> TestServer {
    let state = state
        .with_node(NodeTopology::new("cvc", "Central vehicle computer"))
        .expect("valid node");
    let router = create_router(state.clone())
        .merge(embedder_routes())
        .merge(extra);
    TestServer::start(with_node_topology(router, &state))
        .await
        .expect("test server")
}

/// `state` without a node, routed as [`node_server`] routes it.
async fn flat_server(state: AppState) -> TestServer {
    let router = create_router(state.clone()).merge(embedder_routes());
    TestServer::start(with_node_topology(router, &state))
        .await
        .expect("test server")
}

async fn send_json(
    server: &TestServer,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> reqwest::Response {
    request(server, method, path, body).await
}

fn header<'a>(resp: &'a reqwest::Response, name: &str) -> Option<&'a str> {
    resp.headers()
        .get(name)
        .map(|v| v.to_str().expect("ASCII header"))
}

fn ids(body: &Value) -> Vec<&str> {
    body["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|i| i["id"].as_str().expect("id"))
        .collect()
}

#[tokio::test]
async fn a_server_without_a_node_is_unchanged() {
    let plain = open_server().await;
    let wrapped = flat_server(state()).await;
    for path in [
        "/v1",
        "/v1/components/ecu_a",
        "/v1/components/ecu_a/data",
        "/v1/components/ecu_a/faults",
        "/v1/components/ecu_a/subcomponents",
        "/v1/components/cvc/subcomponents/ecu_a",
        "/v1/docs",
        "/vehicle/v1/components/ecu_a",
    ] {
        assert_eq!(
            get(&wrapped, path, None).await,
            get(&plain, path, None).await,
            "{path}"
        );
    }
    let (status, body) = get(&wrapped, "/v1/components", None).await;
    assert_eq!(status, 200);
    let body = json(&body);
    let mut listed = ids(&body);
    listed.sort();
    assert_eq!(listed, ["ecu_a", "ecu_b"]);
    // The embedder's root-form node-level routes are served as they are.
    for path in ["/v1/status", "/v1/status/x-boot-id"] {
        let (status, body) = get(&wrapped, path, None).await;
        assert_eq!(status, 200, "{path}");
        assert_eq!(json(&body)["served"], path);
    }
    // No node-level collections without a node.
    for path in ["/v1/data", "/v1/operations"] {
        let (status, body) = get(&wrapped, path, None).await;
        assert_eq!(status, 404, "{path}");
        assert_eq!(json(&body)["message"], format!("No resource at {path}"));
    }
}

#[tokio::test]
async fn components_lists_the_node_alone() {
    let server = node_server(members(&["vm1", "vm2"])).await;
    let (status, body) = get(&server, "/v1/components", None).await;
    assert_eq!(status, 200);
    assert_eq!(
        json(&body),
        serde_json::json!({ "items": [{
            "id": "cvc",
            "name": "Central vehicle computer",
            "type": "node",
            "href": "/v1/components/cvc",
        }] })
    );
}

#[tokio::test]
async fn the_node_document_links_node_level_resources_and_subcomponents() {
    let server = node_server(members(&["vm1", "vm2"])).await;
    let (status, body) = get(&server, "/v1/components/cvc", None).await;
    assert_eq!(status, 200);
    assert_eq!(
        json(&body),
        serde_json::json!({
            "id": "cvc",
            "name": "Central vehicle computer",
            "type": "node",
            "status": "/v1/components/cvc/status",
            "operations": "/v1/components/cvc/operations",
            "data": "/v1/components/cvc/data",
            "subcomponents": "/v1/components/cvc/subcomponents",
        })
    );
    // With an update target, the node links its update collection, as the root
    // document does; it aliases no member's resources.
    let server = node_server(banked()).await;
    let (status, body) = get(&server, "/v1/components/cvc", None).await;
    assert_eq!(status, 200);
    let body = json(&body);
    assert_eq!(body["updates"], "/v1/components/cvc/updates");
    assert!(body.get("faults").is_none(), "{body}");
    assert!(body.get("capabilities").is_none(), "{body}");
}

#[tokio::test]
async fn the_node_document_carries_the_identification_when_set() {
    // Not set: absent, not null (the exact document above).
    let server = node_server(members(&["vm1"])).await;
    let (_, body) = get(&server, "/v1/components/cvc", None).await;
    assert!(json(&body).get("x-identification").is_none());

    let node = NodeTopology::new("cvc", "CVC").with_identification("cvc-host-f88a13a329a72db1");
    assert_eq!(node.identification(), Some("cvc-host-f88a13a329a72db1"));
    let state = members(&["vm1"]).with_node(node).expect("valid node");
    let server = TestServer::start(with_node_topology(create_router(state.clone()), &state))
        .await
        .expect("test server");
    let (status, body) = get(&server, "/v1/components/cvc", None).await;
    assert_eq!(status, 200);
    let body = json(&body);
    assert_eq!(body["x-identification"], "cvc-host-f88a13a329a72db1");
    // The node's id stays its logical id; the listing does not carry it.
    assert_eq!(body["id"], "cvc");
    let (_, list) = get(&server, "/v1/components", None).await;
    assert!(!String::from_utf8(list)
        .unwrap()
        .contains("x-identification"));
}

#[tokio::test]
async fn subcomponents_lists_the_members_with_nested_hrefs() {
    let server = node_server(members(&["vm1", "vm2"])).await;
    let (status, body) = get(&server, "/v1/components/cvc/subcomponents", None).await;
    assert_eq!(status, 200);
    let mut items: Vec<Value> = json(&body)["items"].as_array().unwrap().clone();
    items.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    assert_eq!(
        items,
        [
            serde_json::json!({
                "id": "vm1", "name": "vm1 ecu", "type": "ecu",
                "href": "/v1/components/cvc/subcomponents/vm1",
            }),
            serde_json::json!({
                "id": "vm2", "name": "vm2 ecu", "type": "ecu",
                "href": "/v1/components/cvc/subcomponents/vm2",
            }),
        ]
    );
}

#[tokio::test]
async fn a_member_serves_under_the_node_what_it_serves_flat_without_one() {
    let server = node_server(members(&["vm1", "vm2"])).await;
    let reference = flat_server(members(&["vm1", "vm2"])).await;
    for rest in ["", "/data", "/faults", "/operations", "/docs"] {
        let (status, flat) = get(&reference, &format!("/v1/components/vm1{rest}"), None).await;
        assert_eq!(status, 200, "{rest}");
        let (status, nested) = get(
            &server,
            &format!("/v1/components/cvc/subcomponents/vm1{rest}"),
            None,
        )
        .await;
        assert_eq!(status, 200, "{rest}");
        // The same resources, at the node-form URLs.
        let expected = String::from_utf8(flat)
            .unwrap()
            .replace("/v1/components/vm1", "/v1/components/cvc/subcomponents/vm1");
        assert_eq!(
            String::from_utf8(nested.clone()).unwrap(),
            expected,
            "{rest}"
        );
        let mut found = Vec::new();
        urls(&json(&nested), &mut found);
        // (`/v1` itself is the docs' server URL.)
        for url in found.iter().filter(|url| url.starts_with("/v1/")) {
            assert!(
                url.starts_with("/v1/components/cvc/subcomponents/vm1"),
                "{rest}: {url} is not a node-form URL"
            );
        }
    }
}

/// The URLs of a response: its `Location` and `Content-Location`, and those of
/// its body when it is JSON.
fn response_urls(headers: &reqwest::header::HeaderMap, body: &[u8]) -> Vec<String> {
    let mut found: Vec<String> = ["location", "content-location"]
        .iter()
        .filter_map(|name| headers.get(*name))
        .map(|v| v.to_str().expect("ASCII header").to_owned())
        .collect();
    if let Ok(body) = serde_json::from_slice::<Value>(body) {
        urls(&body, &mut found);
    }
    found
}

/// Sends `method path` (with a JSON body or `octets` when given), asserting
/// `status` and that every URL of the response is a node-form one; returns
/// the response's `Location` (if any) and JSON body (`Null` if not JSON).
async fn node_form_step(
    server: &TestServer,
    method: reqwest::Method,
    path: &str,
    json_body: Option<Value>,
    octets: Option<&'static [u8]>,
    status: u16,
) -> (Option<String>, Value) {
    let mut req =
        reqwest::Client::new().request(method.clone(), format!("{}{path}", server.base_url()));
    if let Some(body) = json_body {
        req = req.json(&body);
    }
    if let Some(octets) = octets {
        req = req
            .header("content-type", "application/octet-stream")
            .body(octets);
    }
    let resp = req.send().await.expect("request");
    let headers = resp.headers().clone();
    let got = resp.status().as_u16();
    let body = resp.bytes().await.unwrap().to_vec();
    assert_eq!(
        got,
        status,
        "{method} {path}: {}",
        String::from_utf8_lossy(&body)
    );
    assert!(headers.get("deprecation").is_none(), "{method} {path}");
    assert!(headers.get("link").is_none(), "{method} {path}");
    for url in response_urls(&headers, &body) {
        assert!(
            url.starts_with("/v1/components/cvc/"),
            "{method} {path}: {url} is not a node-form URL"
        );
    }
    let location = headers
        .get("location")
        .map(|l| l.to_str().unwrap().to_owned());
    (
        location,
        serde_json::from_slice(&body).unwrap_or(Value::Null),
    )
}

/// Polls `GET {update}/status` (node form) until `done`.
async fn poll_node_form(server: &TestServer, update: &str, done: impl Fn(&Value) -> bool) {
    for _ in 0..200 {
        let (_, body) = node_form_step(
            server,
            reqwest::Method::GET,
            &format!("{update}/status"),
            None,
            None,
            200,
        )
        .await;
        if done(&body) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("{update} never reached the expected state");
}

#[tokio::test]
async fn a_member_update_flow_renders_node_form_urls_only() {
    let server = node_server(banked()).await;
    let member = "/v1/components/cvc/subcomponents/vm1";
    let update = format!("{member}/updates/u1");
    let get_ = reqwest::Method::GET;
    let put = reqwest::Method::PUT;

    let (location, _) = node_form_step(
        &server,
        reqwest::Method::POST,
        &format!("{member}/updates"),
        Some(serde_json::json!({ "id": "u1" })),
        None,
        201,
    )
    .await;
    assert_eq!(location.as_deref(), Some(update.as_str()));
    node_form_step(&server, get_.clone(), &update, None, None, 200).await;
    node_form_step(
        &server,
        get_.clone(),
        &format!("{member}/updates"),
        None,
        None,
        200,
    )
    .await;

    let part = format!("{update}/bulk-data/manifest");
    let (_, stored) = node_form_step(&server, put.clone(), &part, None, Some(b"banked"), 201).await;
    let mut found = Vec::new();
    urls(&stored, &mut found);
    assert!(found.contains(&part), "{stored}");
    // Read back, its bytes as uploaded.
    node_form_step(&server, get_.clone(), &part, None, None, 200).await;
    assert_eq!(get(&server, &part, None).await, (200, b"banked".to_vec()));
    node_form_step(
        &server,
        get_.clone(),
        &format!("{update}/bulk-data"),
        None,
        None,
        200,
    )
    .await;

    let (location, _) = node_form_step(
        &server,
        put.clone(),
        &format!("{update}/prepare"),
        None,
        None,
        202,
    )
    .await;
    assert_eq!(location, Some(format!("{update}/status")));
    poll_node_form(&server, &update, |s| s["status"] == "completed").await;

    let (location, _) = node_form_step(
        &server,
        put.clone(),
        &format!("{update}/execute?x-ota-control=orchestrated"),
        None,
        None,
        202,
    )
    .await;
    assert_eq!(location, Some(format!("{update}/status")));
    poll_node_form(&server, &update, |s| {
        s["x-ota-substate"] == "awaiting-verdict"
    })
    .await;

    let (location, _) = node_form_step(
        &server,
        put.clone(),
        &format!("{update}/x-update-commit"),
        None,
        None,
        202,
    )
    .await;
    assert_eq!(location, Some(format!("{update}/status")));
    node_form_step(
        &server,
        get_.clone(),
        &format!("{update}/status"),
        None,
        None,
        200,
    )
    .await;
    node_form_step(&server, get_, &update, None, None, 200).await;
}

#[tokio::test]
async fn a_node_transaction_refusal_names_the_node_operations() {
    let backends: HashMap<String, Arc<dyn DiagnosticBackend>> = HashMap::from([(
        "vm1".to_string(),
        Arc::new(BankedBackend::new("vm1", VerdictScope::Node)) as Arc<dyn DiagnosticBackend>,
    )]);
    let server = node_server(AppState::new(backends)).await;
    let member = "/v1/components/cvc/subcomponents/vm1";
    let resp = send_json(
        &server,
        reqwest::Method::POST,
        &format!("{member}/updates"),
        Some(serde_json::json!({ "id": "u1" })),
    )
    .await;
    assert_eq!(resp.status().as_u16(), 201);
    let (status, body) = send(
        &server,
        reqwest::Method::PUT,
        &format!("{member}/updates/u1/x-update-commit"),
        None,
    )
    .await;
    assert_eq!(status, 409);
    let message = json(&body)["message"].as_str().unwrap().to_owned();
    assert!(
        message.contains("POST /v1/components/cvc/operations/x-update-commit/executions"),
        "{message}"
    );
    assert!(!message.contains("POST /v1/operations"), "{message}");
}

#[tokio::test]
async fn node_level_updates_render_node_form_urls_only() {
    let server = node_server(banked()).await;
    let (location, _) = node_form_step(
        &server,
        reqwest::Method::POST,
        "/v1/components/cvc/updates",
        Some(serde_json::json!({ "id": "u1", "target": "vm2" })),
        None,
        201,
    )
    .await;
    assert_eq!(location.as_deref(), Some("/v1/components/cvc/updates/u1"));
    for path in [
        "/v1/components/cvc/updates",
        "/v1/components/cvc/updates/u1",
    ] {
        node_form_step(&server, reqwest::Method::GET, path, None, None, 200).await;
    }
    // The update registered on the node collection is its member's too.
    node_form_step(
        &server,
        reqwest::Method::GET,
        "/v1/components/cvc/subcomponents/vm2/updates/u1",
        None,
        None,
        200,
    )
    .await;
}

/// The 404 a flat or root form answers on `/v1` of node `cvc`.
fn not_served(path: &str, node_form: &str) -> Value {
    serde_json::json!({
        "error_code": "incomplete-request",
        "message": format!(
            "No resource at {path}: on /v1 this server is node 'cvc', whose path for it is {node_form}"
        ),
        "parameters": { "http_code": ["404"] },
    })
}

#[tokio::test]
async fn flat_member_paths_answer_404() {
    let server = node_server(banked()).await;
    for (method, rest) in [
        (reqwest::Method::GET, ""),
        (reqwest::Method::GET, "/data"),
        (reqwest::Method::GET, "/faults"),
        (reqwest::Method::GET, "/docs"),
        (reqwest::Method::GET, "/updates"),
        (reqwest::Method::POST, "/updates"),
        (reqwest::Method::GET, "/updates/u1"),
        (reqwest::Method::PUT, "/updates/u1/bulk-data/p1"),
        (reqwest::Method::GET, "/updates/u1/bulk-data/p1"),
        (reqwest::Method::GET, "/bulk-data"),
        (reqwest::Method::PUT, "/updates/u1/execute"),
    ] {
        // A member's id and any other id alike.
        for id in ["vm1", "nope"] {
            let path = format!("/v1/components/{id}{rest}");
            let resp = send_json(&server, method.clone(), &path, None).await;
            assert_eq!(resp.status().as_u16(), 404, "{method} {path}");
            assert!(header(&resp, "deprecation").is_none(), "{path}");
            assert!(header(&resp, "link").is_none(), "{path}");
            assert_eq!(
                resp.json::<Value>().await.unwrap(),
                not_served(
                    &path,
                    &format!("/v1/components/cvc/subcomponents/{id}{rest}")
                ),
                "{method} {path}"
            );
        }
    }
    // Nothing was registered through them.
    let (_, body) = get(
        &server,
        "/v1/components/cvc/subcomponents/vm1/updates",
        None,
    )
    .await;
    assert_eq!(json(&body)["items"], serde_json::json!([]));
}

#[tokio::test]
async fn root_forms_of_node_level_resources_answer_404_but_route_the_node_forms() {
    let server = node_server(banked()).await;
    for (method, rest) in [
        (reqwest::Method::GET, "/status"),
        (reqwest::Method::PUT, "/status/restart"),
        (reqwest::Method::GET, "/status/x-boot-id"),
        (reqwest::Method::GET, "/updates"),
        (reqwest::Method::POST, "/updates"),
        (reqwest::Method::GET, "/updates/u1/status"),
        (reqwest::Method::PUT, "/updates/u1/bulk-data/p1"),
        (
            reqwest::Method::POST,
            "/operations/x-update-commit/executions",
        ),
        (reqwest::Method::GET, "/data"),
        (reqwest::Method::GET, "/data/x-update-state"),
        (reqwest::Method::GET, "/operations"),
    ] {
        let path = format!("/v1{rest}");
        let resp = send_json(&server, method.clone(), &path, None).await;
        assert_eq!(resp.status().as_u16(), 404, "{method} {path}");
        assert_eq!(
            resp.json::<Value>().await.unwrap(),
            not_served(&path, &format!("/v1/components/cvc{rest}")),
            "{method} {path}"
        );
    }

    // The node forms reach the embedder's root-form routes all the same.
    for rest in ["/status", "/status/x-boot-id"] {
        let (status, body) = get(&server, &format!("/v1/components/cvc{rest}"), None).await;
        assert_eq!(status, 200, "{rest}");
        assert_eq!(json(&body)["served"], format!("/v1{rest}"));
    }
    let resp = send_json(
        &server,
        reqwest::Method::PUT,
        "/v1/components/cvc/status/restart",
        None,
    )
    .await;
    // The embedder's root-form `Location`, in the node form.
    assert_eq!(location(&resp, 202), "/v1/components/cvc/status");
    assert_eq!(
        resp.json::<Value>().await.unwrap()["served"],
        "/v1/status/restart"
    );
    let resp = send_json(
        &server,
        reqwest::Method::POST,
        "/v1/components/cvc/operations/x-update-commit/executions",
        None,
    )
    .await;
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(
        resp.json::<Value>().await.unwrap()["served"],
        "/v1/operations/x-update-commit/executions"
    );
    // And SOVDd's own root-form routes.
    let (status, _) = get(&server, "/v1/components/cvc/updates", None).await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn the_documents_a_client_starts_from_stay_served() {
    let server = node_server(banked()).await;
    let reference = flat_server(banked()).await;
    for path in ["/v1", "/v1/"] {
        let (status, body) = get(&server, path, None).await;
        assert_eq!(status, 200, "{path}");
        let body = json(&body);
        assert_eq!(body["components"], "/v1/components", "{path}");
        // The node-level update collection, in the node form.
        assert_eq!(body["updates"], "/v1/components/cvc/updates", "{path}");
        assert_eq!(
            body["x-update"]["update-targets"],
            serde_json::json!(["vm1", "vm2"])
        );
    }
    for path in ["/v1/components", "/v1/docs"] {
        let (status, _) = get(&server, path, None).await;
        assert_eq!(status, 200, "{path}");
    }
    // The server-root documents are those of a server without a node.
    for path in ["/version-info", "/health", "/.well-known/sovd-extensions"] {
        let served = get(&server, path, None).await;
        assert_eq!(served.0, 200, "{path}");
        assert_eq!(served, get(&reference, path, None).await, "{path}");
    }
}

#[tokio::test]
async fn node_level_data_is_the_root_data() {
    let server = node_server(members(&["vm1"])).await;
    let node = get(&server, "/v1/components/cvc/data/x-update-state", None).await;
    assert_eq!(node.0, 200);
    assert_eq!(json(&node.1)["data"]["state"], "idle");
    // The query is kept.
    assert_eq!(
        get(
            &server,
            "/v1/components/cvc/data/x-update-state?include-schema=true",
            None
        )
        .await,
        node
    );
    // An item the root form does not serve: the router's 404, naming the
    // node-form path.
    let (status, body) = get(&server, "/v1/components/cvc/data/nope", None).await;
    assert_eq!(status, 404);
    assert_eq!(
        json(&body)["message"],
        "No resource at /v1/components/cvc/data/nope"
    );
    // The legacy tree has no node-level data.
    let (status, _) = get(
        &server,
        "/vehicle/v1/components/cvc/data/x-update-state",
        None,
    )
    .await;
    assert_eq!(status, 404);
}

/// Node-level items as an embedder contributes them to the capability
/// description (either base), and paths that are none.
fn contributed_items() -> sovd_api::CapabilityExtensions {
    sovd_api::CapabilityExtensions::from_openapi(&serde_json::json!({ "paths": {
        "/vehicle/v1/data/x-update-state": { "get": { "summary": "Update state" } },
        "/v1/data/x-plain": { "get": {} },
        "/vehicle/v1/operations/x-update-commit/executions": {
            "post": { "summary": "Commit the node's update transaction" },
        },
        "/vehicle/v1/operations/x-ota-pull-update/executions": {
            "post": { "summary": "Pull an update" },
        },
        "/vehicle/v1/operations/x-ota-pull-update/executions/{execution_id}": {
            "get": { "summary": "Read a pull" },
        },
        "/v1/operations/x-reboot": { "get": { "summary": "Reboot the node" } },
        // No item: a write-only data path, a template, a member's path.
        "/v1/data/x-write-only": { "put": {} },
        "/v1/data/{data_id}": { "get": {} },
        "/vehicle/v1/components/{component_id}/data/x-member": { "get": {} },
    }}))
}

#[tokio::test]
async fn node_level_collections_list_the_contributed_items() {
    async fn served(uri: Uri) -> impl IntoResponse {
        axum::Json(serde_json::json!({ "served": uri.path() }))
    }
    // The embedder's item routes the hrefs reach.
    let items = Router::new()
        .route("/v1/data/x-plain", axum::routing::get(served))
        .route("/v1/operations/{op}", axum::routing::get(served));
    let state = members(&["vm1"]).with_capability_extensions(contributed_items());
    let server = node_server_with(state, items).await;

    let (status, body) = get(&server, "/v1/components/cvc/data", None).await;
    assert_eq!(status, 200);
    let data = json(&body);
    assert_eq!(
        data,
        serde_json::json!({ "items": [
            { "id": "x-plain", "name": "x-plain", "href": "/v1/components/cvc/data/x-plain" },
            {
                "id": "x-update-state",
                "name": "Update state",
                "href": "/v1/components/cvc/data/x-update-state",
            },
        ] })
    );
    let (status, body) = get(&server, "/v1/components/cvc/operations", None).await;
    assert_eq!(status, 200);
    let operations = json(&body);
    assert_eq!(
        operations,
        serde_json::json!({ "items": [
            {
                "id": "x-ota-pull-update",
                "name": "Pull an update",
                "href": "/v1/components/cvc/operations/x-ota-pull-update",
            },
            {
                "id": "x-reboot",
                "name": "Reboot the node",
                "href": "/v1/components/cvc/operations/x-reboot",
            },
            {
                "id": "x-update-commit",
                "name": "Commit the node's update transaction",
                "href": "/v1/components/cvc/operations/x-update-commit",
            },
        ] })
    );
    // Every href resolves.
    for item in data["items"]
        .as_array()
        .unwrap()
        .iter()
        .chain(operations["items"].as_array().unwrap())
    {
        let href = item["href"].as_str().unwrap();
        let (status, _) = get(&server, href, None).await;
        assert_eq!(status, 200, "{href}");
    }
    // The root forms stay unserved.
    for path in ["/v1/data", "/v1/operations"] {
        let (status, body) = get(&server, path, None).await;
        assert_eq!(status, 404, "{path}");
        assert_eq!(
            json(&body),
            not_served(path, &format!("/v1/components/cvc{}", &path[3..]))
        );
    }
}

#[tokio::test]
async fn node_level_collections_without_contributed_items_are_empty() {
    let server = node_server(members(&["vm1"])).await;
    for path in ["/v1/components/cvc/data", "/v1/components/cvc/operations"] {
        let (status, body) = get(&server, path, None).await;
        assert_eq!(status, 200, "{path}");
        assert_eq!(json(&body), serde_json::json!({ "items": [] }), "{path}");
    }
}

#[tokio::test]
async fn an_embedder_route_serving_a_node_level_collection_wins() {
    async fn served(uri: Uri) -> impl IntoResponse {
        axum::Json(serde_json::json!({ "served": uri.path() }))
    }
    let state = members(&["vm1"]).with_capability_extensions(contributed_items());
    let own = Router::new().route("/v1/data", axum::routing::get(served));
    let server = node_server_with(state, own).await;
    let (status, body) = get(&server, "/v1/components/cvc/data", None).await;
    assert_eq!(status, 200);
    assert_eq!(json(&body), serde_json::json!({ "served": "/v1/data" }));
    // The other collection is still SOVDd's.
    let (status, body) = get(&server, "/v1/components/cvc/operations", None).await;
    assert_eq!(status, 200);
    assert_eq!(ids(&json(&body)).len(), 3);
}

#[tokio::test]
async fn node_level_collections_need_what_the_node_document_needs() {
    let state = members(&["vm1", "vm2"])
        .with_authorizer(Arc::new(ScopedAuthorizer))
        .with_capability_extensions(contributed_items());
    let server = node_server(state).await;
    for path in ["/v1/components/cvc/data", "/v1/components/cvc/operations"] {
        for (token, expected) in [
            (Some("token-all"), 200),
            (Some("token-vm1"), 200),
            (Some("token-none"), 403),
            (None, 401),
        ] {
            let (status, _) = get(&server, path, token).await;
            assert_eq!(status, expected, "{path} {token:?}");
            let (status, _) = get(&server, "/v1/components/cvc", token).await;
            assert_eq!(status, expected, "node document {token:?}");
        }
    }
}

#[tokio::test]
async fn other_node_paths_are_not_served() {
    let server = node_server(members(&["vm1"])).await;
    for path in [
        "/v1/components/cvc/faults",
        "/v1/components/cvc/data-lists",
        "/v1/components/cvc/statusx",
        "/v1/components/cvc/subcomponents/cvc",
    ] {
        let (status, body) = get(&server, path, None).await;
        assert_eq!(status, 404, "{path}");
        assert!(
            json(&body).get("error_code").is_some(),
            "{path}: GenericError body"
        );
    }
}

#[tokio::test]
async fn an_unknown_member_answers_what_an_unknown_component_does() {
    let server = node_server(members(&["vm1"])).await;
    let reference = flat_server(members(&["vm1"])).await;
    for rest in ["", "/data", "/faults/f1"] {
        let flat = get(&reference, &format!("/v1/components/nope{rest}"), None).await;
        assert_eq!(flat.0, 404, "{rest}");
        assert_eq!(
            get(
                &server,
                &format!("/v1/components/cvc/subcomponents/nope{rest}"),
                None
            )
            .await,
            flat,
            "{rest}"
        );
    }
}

/// `dual_base.rs`'s legacy snapshot, served by a server with a node.
#[tokio::test]
async fn the_legacy_tree_is_unchanged_with_a_node() {
    let node = node_server(state()).await;
    let plain = open_server().await;

    let (status, body) = get(&node, "/vehicle/v1/components", None).await;
    assert_eq!(status, 200);
    let mut body = json(&body);
    body["items"]
        .as_array_mut()
        .unwrap()
        .sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    assert_eq!(body.to_string(), LEGACY_COMPONENTS);

    let (status, body) = get(&node, "/vehicle/v1/components/ecu_a/docs", None).await;
    assert_eq!(status, 200);
    assert_eq!(
        String::from_utf8(body).unwrap(),
        include_str!("fixtures/dual_base_legacy_component_docs.json")
            .replace("@@VERSION@@", env!("CARGO_PKG_VERSION"))
    );

    let resp = request(
        &node,
        reqwest::Method::POST,
        "/vehicle/v1/components/ecu_a/updates",
        Some(serde_json::json!({ "id": "u1" })),
    )
    .await;
    assert_eq!(location(&resp, 201), LEGACY_REGISTER_LOCATION);

    // Every other legacy response, the node-form paths included, is that of a
    // server without a node.
    for path in [
        "/vehicle/v1",
        "/vehicle/v1/docs",
        "/vehicle/v1/components/ecu_a",
        "/vehicle/v1/components/ecu_a/data",
        "/vehicle/v1/components/ecu_a/faults",
        "/vehicle/v1/components/cvc",
        "/vehicle/v1/components/cvc/subcomponents",
        "/vehicle/v1/components/cvc/subcomponents/ecu_a",
        "/vehicle/v1/components/cvc/subcomponents/ecu_a/data",
        "/vehicle/v1/components/cvc/updates",
        "/vehicle/v1/components/cvc/data",
    ] {
        assert_eq!(
            get(&node, path, None).await,
            get(&plain, path, None).await,
            "{path}"
        );
    }
}

const LEGACY_COMPONENTS: &str = r#"{"items":[{"href":"/vehicle/v1/components/ecu_a","id":"ecu_a","name":"ecu_a ecu","type":"ecu"},{"href":"/vehicle/v1/components/ecu_b","id":"ecu_b","name":"ecu_b ecu","type":"ecu"}]}"#;
const LEGACY_REGISTER_LOCATION: &str = "/vehicle/v1/components/ecu_a/updates/u1";

#[tokio::test]
async fn the_v1_docs_list_the_node_forms_only() {
    let server = node_server(members(&["vm1"])).await;
    let (status, body) = get(&server, "/v1/docs", None).await;
    assert_eq!(status, 200);
    let doc = json(&body);
    let paths = doc["paths"].as_object().expect("paths");
    for path in [
        "/v1/components",
        "/v1/docs",
        "/v1/components/cvc",
        "/v1/components/cvc/subcomponents",
        "/v1/components/cvc/subcomponents/{component_id}",
        "/v1/components/cvc/subcomponents/{component_id}/data/{param_id}",
        "/v1/components/cvc/updates",
        "/v1/components/cvc/updates/{update_id}/bulk-data",
        "/v1/components/cvc/data",
        "/v1/components/cvc/operations",
    ] {
        assert!(paths.contains_key(path), "{path} missing");
    }
    // Neither the flat member paths nor the root node-level ones.
    for path in paths.keys().filter(|p| p.starts_with("/v1/")) {
        let served = ["/v1/components", "/v1/docs", "/v1/"].contains(&path.as_str())
            || path == "/v1/components/cvc"
            || path.starts_with("/v1/components/cvc/");
        assert!(served, "{path} is not served");
    }
    // Their items are those of the flat paths of a server without a node.
    let (_, flat) = get(&flat_server(members(&["vm1"])).await, "/v1/docs", None).await;
    assert_eq!(
        paths["/v1/components/cvc/subcomponents/{component_id}/data/{param_id}"],
        json(&flat)["paths"]["/v1/components/{component_id}/data/{param_id}"]
    );

    // An embedder's root-form node-level data item is listed under the node.
    let item = serde_json::json!({ "get": { "summary": "Update state" } });
    let extensions = sovd_api::CapabilityExtensions::from_openapi(&serde_json::json!({
        "paths": { "/v1/data/x-update-state": item },
    }));
    let with_data = node_server(members(&["vm1"]).with_capability_extensions(extensions)).await;
    let (status, body) = get(&with_data, "/v1/docs", None).await;
    assert_eq!(status, 200);
    let doc = json(&body);
    assert!(doc["paths"].get("/v1/data/x-update-state").is_none());
    assert_eq!(doc["paths"]["/v1/components/cvc/data/x-update-state"], item);

    // A member's scoped doc lists its node-form paths.
    let (status, body) = get(&server, "/v1/components/cvc/subcomponents/vm1/docs", None).await;
    assert_eq!(status, 200);
    let doc = json(&body);
    let paths = doc["paths"].as_object().expect("paths");
    assert!(paths.contains_key("/v1/components/cvc/subcomponents/vm1/data/{param_id}"));
    for path in paths.keys() {
        assert!(
            path.starts_with("/v1/components/cvc/subcomponents/vm1"),
            "{path}"
        );
    }

    // The legacy doc is that of a server without a node.
    let (_, legacy) = get(&server, "/vehicle/v1/docs", None).await;
    assert!(!String::from_utf8_lossy(&legacy).contains("subcomponents"));
    assert_eq!(
        legacy,
        get(&open_server().await, "/vehicle/v1/docs", None).await.1
    );
}

#[tokio::test]
async fn the_component_scope_applies_through_the_node_form() {
    let state = members(&["vm1", "vm2"]).with_authorizer(Arc::new(ScopedAuthorizer));
    let server = node_server(state).await;

    let (status, _) = get(
        &server,
        "/v1/components/cvc/subcomponents/vm1/data",
        Some("token-vm1"),
    )
    .await;
    assert_eq!(status, 200);
    let path = "/v1/components/cvc/subcomponents/vm2/data";
    let (status, _) = get(&server, path, Some("token-vm1")).await;
    assert_eq!(status, 403);
    let (status, _) = get(&server, path, Some("token-all")).await;
    assert_eq!(status, 200);
    // The flat form is not served, whatever the token.
    for token in [Some("token-all"), Some("token-vm1"), None] {
        let (status, _) = get(&server, "/v1/components/vm1/data", token).await;
        assert_eq!(status, 404, "{token:?}");
    }

    // The listings show what the client may access; the node to a client that may
    // access any member.
    let (status, body) = get(&server, "/v1/components", Some("token-vm1")).await;
    assert_eq!(status, 200);
    assert_eq!(ids(&json(&body)), ["cvc"]);
    let (status, body) = get(
        &server,
        "/v1/components/cvc/subcomponents",
        Some("token-vm1"),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(ids(&json(&body)), ["vm1"]);
    let (status, body) = get(&server, "/v1/components/cvc", Some("token-vm1")).await;
    assert_eq!(status, 200);
    assert_eq!(json(&body)["id"], "cvc");

    let (status, body) = get(&server, "/v1/components", Some("token-none")).await;
    assert_eq!(status, 200);
    assert!(ids(&json(&body)).is_empty());
    let (status, _) = get(&server, "/v1/components/cvc", Some("token-none")).await;
    assert_eq!(status, 403);
    let (status, _) = get(&server, "/v1/components/cvc", None).await;
    assert_eq!(status, 401);
}

#[test]
fn a_node_needs_an_id_of_its_own() {
    let err = state()
        .with_node(NodeTopology::new("ecu_a", "Clash"))
        .err()
        .expect("refused");
    assert!(err.contains("ecu_a"), "{err}");
    for id in ["", "a/b", "{id}"] {
        assert!(
            state().with_node(NodeTopology::new(id, "Bad")).is_err(),
            "{id:?}"
        );
    }
    assert!(state().with_node(NodeTopology::new("cvc", "CVC")).is_ok());
}
