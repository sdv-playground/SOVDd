//! Node-level data items from a provider (`AppState::with_node_data`): SOVDd
//! reads one under `GET /v1/data/{id}` as an ISO 17978-3 `ReadValue` and lists
//! them under `GET /v1/data` with the contributed capability rows, on a flat
//! server and, in the node form, on a node server. The frozen `/vehicle/v1` is
//! unaffected, and a server without a provider answers as before.

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use axum::http::Uri;
use axum::response::IntoResponse;
use axum::Router;
use chrono::Utc;
use common::*;
use serde_json::Value;
use sovd_api::{
    create_router, with_node_topology, ApiError, AppState, CapabilityExtensions, NodeTopology,
};
use sovd_client::testing::TestServer;
use sovd_core::{
    BackendError, BackendResult, DataValue, DiagnosticBackend, NodeData, ParameterInfo,
};

/// Serves `x-state` (an object), `x-temp` (a number in `degC` with raw bytes)
/// and `x-fail`, whose read fails with a transport error.
struct Provider;

const TRANSPORT_FAILURE: &str = "state service unreachable";

fn item(id: &str, name: &str) -> ParameterInfo {
    ParameterInfo {
        id: id.to_string(),
        name: name.to_string(),
        description: None,
        unit: None,
        data_type: None,
        read_only: true,
        href: String::new(),
        did: None,
        category: None,
    }
}

fn value(id: &str, value: Value, unit: Option<&str>, raw: Option<&str>) -> DataValue {
    DataValue {
        id: id.to_string(),
        name: id.to_string(),
        value,
        unit: unit.map(str::to_string),
        timestamp: Utc::now(),
        raw: raw.map(str::to_string),
        did: None,
        length: raw.map(|r| r.len() / 2),
    }
}

#[async_trait::async_trait]
impl NodeData for Provider {
    async fn list_data(&self) -> BackendResult<Vec<ParameterInfo>> {
        Ok(vec![
            item("x-state", "State"),
            item("x-temp", "Temperature"),
            item("x-fail", "Failing"),
        ])
    }
    async fn read_data(&self, id: &str) -> BackendResult<DataValue> {
        match id {
            "x-state" => Ok(value(
                "x-state",
                serde_json::json!({ "state": "idle" }),
                None,
                None,
            )),
            "x-temp" => Ok(value(
                "x-temp",
                serde_json::json!(41.5),
                Some("degC"),
                Some("29"),
            )),
            "x-fail" => Err(BackendError::Transport(TRANSPORT_FAILURE.to_string())),
            other => Err(BackendError::ParameterNotFound(format!(
                "Node data item not found: {other}"
            ))),
        }
    }
}

/// A state over mock backend `vm1`, with the capability rows of two node-level
/// data items: `x-row`, and `x-state`, which the provider serves too.
fn members() -> AppState {
    let mut backends: HashMap<String, Arc<dyn DiagnosticBackend>> = HashMap::new();
    backends.insert("vm1".to_string(), Arc::new(MockBackend::new("vm1")));
    let rows = serde_json::json!({
        "paths": {
            "/v1/data/x-row": { "get": { "summary": "Row" } },
            "/v1/data/x-state": { "get": { "summary": "Row state" } },
        }
    });
    AppState::new(backends).with_capability_extensions(CapabilityExtensions::from_openapi(&rows))
}

fn with_provider(state: AppState) -> AppState {
    state.with_node_data(Arc::new(Provider))
}

/// `state` without a node, with the embedder's `extra` routes merged in.
async fn flat(state: AppState, extra: Router) -> TestServer {
    let router = create_router(state.clone()).merge(extra);
    TestServer::start(with_node_topology(router, &state))
        .await
        .expect("test server")
}

/// `state` as node `hpc`, with the embedder's `extra` routes merged in.
async fn node(state: AppState, extra: Router) -> TestServer {
    let state = state
        .with_node(NodeTopology::new("hpc", "Node"))
        .expect("valid node");
    let router = create_router(state.clone()).merge(extra);
    TestServer::start(with_node_topology(router, &state))
        .await
        .expect("test server")
}

/// The flat and the node server with a provider, each with its path prefix of
/// the node-level data (`/v1/data` and `/v1/components/hpc/data`).
async fn servers() -> [(TestServer, &'static str); 2] {
    [
        (
            flat(with_provider(members()), Router::new()).await,
            "/v1/data",
        ),
        (
            node(with_provider(members()), Router::new()).await,
            "/v1/components/hpc/data",
        ),
    ]
}

/// The standard error body `err` answers.
async fn error_body(err: ApiError) -> (u16, Value) {
    let resp = err.into_response();
    let status = resp.status().as_u16();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    (status, json(&bytes))
}

async fn get_json(server: &TestServer, path: &str) -> (u16, Value) {
    let (status, body) = get(server, path, None).await;
    (status, json(&body))
}

#[tokio::test]
async fn a_node_level_item_reads_as_a_read_value() {
    let mut answers = Vec::new();
    for (server, data) in servers().await {
        let (status, body) = get_json(&server, &format!("{data}/x-state")).await;
        assert_eq!(status, 200, "{data}");
        assert_eq!(
            body,
            serde_json::json!({ "id": "x-state", "data": { "state": "idle" } }),
            "{data}"
        );
        answers.push(body);
    }
    // The node form answers what the flat server's root form does.
    assert_eq!(answers[0], answers[1]);
}

#[tokio::test]
async fn a_node_level_item_reads_with_its_schema() {
    for (server, data) in servers().await {
        let (status, body) = get_json(&server, &format!("{data}/x-temp?include-schema=true")).await;
        assert_eq!(status, 200, "{data}");
        assert_eq!(
            body,
            serde_json::json!({
                "id": "x-temp",
                "data": 41.5,
                "schema": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string" },
                        "data": { "type": "number", "x-sovd-unit": { "display_name": "degC" } },
                    }
                }
            }),
            "{data}"
        );
        let (status, body) = get_json(&server, &format!("{data}/x-temp")).await;
        assert_eq!(status, 200, "{data}");
        assert_eq!(body, serde_json::json!({ "id": "x-temp", "data": 41.5 }));
    }
}

#[tokio::test]
async fn a_raw_read_of_a_node_level_item_is_its_raw_bytes() {
    for (server, data) in servers().await {
        let (status, body) = get_json(&server, &format!("{data}/x-temp?raw=true")).await;
        assert_eq!(status, 200, "{data}");
        assert_eq!(body, serde_json::json!({ "id": "x-temp", "data": "29" }));
        // An item without raw bytes reads its value.
        let (status, body) = get_json(&server, &format!("{data}/x-state?raw=true")).await;
        assert_eq!(status, 200, "{data}");
        assert_eq!(body["data"], serde_json::json!({ "state": "idle" }));
    }
}

#[tokio::test]
async fn an_unknown_node_level_item_is_404() {
    let expected = error_body(ApiError::NotFound(
        "Node data item not found: x-nope".to_string(),
    ))
    .await;
    assert_eq!(expected.0, 404);
    for (server, data) in servers().await {
        assert_eq!(
            get_json(&server, &format!("{data}/x-nope")).await,
            expected,
            "{data}"
        );
    }
}

#[tokio::test]
async fn a_failing_read_answers_its_mapped_error() {
    let expected = error_body(ApiError::from(BackendError::Transport(
        TRANSPORT_FAILURE.to_string(),
    )))
    .await;
    assert_ne!(expected.0, 200);
    for (server, data) in servers().await {
        assert_eq!(
            get_json(&server, &format!("{data}/x-fail")).await,
            expected,
            "{data}"
        );
    }
}

#[tokio::test]
async fn the_listing_merges_the_provider_items_with_the_capability_rows() {
    for (server, data) in servers().await {
        let (status, body) = get_json(&server, data).await;
        assert_eq!(status, 200, "{data}");
        let item = |id: &str, name: &str| serde_json::json!({ "id": id, "name": name, "href": format!("{data}/{id}") });
        assert_eq!(
            body,
            serde_json::json!({ "items": [
                item("x-fail", "Failing"),
                item("x-row", "Row"),
                // In both: listed once, as the provider's.
                item("x-state", "State"),
                item("x-temp", "Temperature"),
            ]}),
            "{data}"
        );
    }
}

#[tokio::test]
async fn the_legacy_tree_is_unaffected_by_a_provider() {
    for paths in [
        ["/vehicle/v1/data", "/vehicle/v1/data/x-state"],
        ["/vehicle/v1/components/hpc/data", "/vehicle/v1/data/x-nope"],
    ] {
        for path in paths {
            let with = flat(with_provider(members()), Router::new()).await;
            let without = flat(members(), Router::new()).await;
            assert_eq!(
                get(&with, path, None).await,
                get(&without, path, None).await,
                "flat {path}"
            );
            let with = node(with_provider(members()), Router::new()).await;
            let without = node(members(), Router::new()).await;
            let answer = get(&with, path, None).await;
            assert_eq!(answer.0, 404, "{path}");
            assert_eq!(answer, get(&without, path, None).await, "node {path}");
        }
    }
}

#[tokio::test]
async fn without_a_provider_v1_data_answers_as_before() {
    // A flat server has no node-level data: both paths fall back to 404.
    let server = flat(members(), Router::new()).await;
    for path in ["/v1/data", "/v1/data/x-state"] {
        assert_eq!(
            get_json(&server, path).await,
            error_body(ApiError::NotFound(format!("No resource at {path}"))).await,
            "{path}"
        );
    }
    // A node server lists the capability rows and serves no item itself.
    let server = node(members(), Router::new()).await;
    let (status, body) = get_json(&server, "/v1/components/hpc/data").await;
    assert_eq!(status, 200);
    assert_eq!(
        body,
        serde_json::json!({ "items": [
            { "id": "x-row", "name": "Row", "href": "/v1/components/hpc/data/x-row" },
            { "id": "x-state", "name": "Row state", "href": "/v1/components/hpc/data/x-state" },
        ]})
    );
    let path = "/v1/components/hpc/data/x-state";
    assert_eq!(
        get_json(&server, path).await,
        error_body(ApiError::NotFound(format!("No resource at {path}"))).await
    );
}

#[tokio::test]
async fn an_embedder_literal_route_wins_next_to_the_provider() {
    async fn served(uri: Uri) -> impl IntoResponse {
        axum::Json(serde_json::json!({ "served": uri.path() }))
    }
    let own = || Router::new().route("/v1/data/special", axum::routing::get(served));
    for (server, data) in [
        (flat(with_provider(members()), own()).await, "/v1/data"),
        (
            node(with_provider(members()), own()).await,
            "/v1/components/hpc/data",
        ),
    ] {
        let (status, body) = get_json(&server, &format!("{data}/special")).await;
        assert_eq!(status, 200, "{data}");
        assert_eq!(body, serde_json::json!({ "served": "/v1/data/special" }));
        // The provider still serves the rest.
        let (status, body) = get_json(&server, &format!("{data}/x-state")).await;
        assert_eq!(status, 200, "{data}");
        assert_eq!(body["id"], "x-state");
    }
}

#[tokio::test]
async fn node_level_data_needs_what_the_node_document_needs() {
    let auth = |state: AppState| state.with_authorizer(Arc::new(ScopedAuthorizer));
    for (server, data) in [
        (
            flat(with_provider(auth(members())), Router::new()).await,
            "/v1/data",
        ),
        (
            node(with_provider(auth(members())), Router::new()).await,
            "/v1/components/hpc/data",
        ),
    ] {
        for path in [data.to_string(), format!("{data}/x-state")] {
            for (token, expected) in [
                (Some("token-all"), 200),
                (Some("token-vm1"), 200),
                (Some("token-none"), 403),
                (None, 401),
            ] {
                let (status, _) = get(&server, &path, token).await;
                assert_eq!(status, expected, "{path} {token:?}");
            }
        }
    }
}

#[tokio::test]
async fn the_v1_docs_describe_the_node_level_data_with_a_provider_only() {
    let docs = |server: TestServer, path: &'static str| async move {
        let (status, body) = get_json(&server, path).await;
        assert_eq!(status, 200, "{path}");
        body["paths"].clone()
    };
    // Flat: the root forms, on `/v1` only.
    let with = docs(
        flat(with_provider(members()), Router::new()).await,
        "/v1/docs",
    )
    .await;
    let without = docs(flat(members(), Router::new()).await, "/v1/docs").await;
    assert!(with["/v1/data"]["get"].is_object(), "{with}");
    assert!(with["/v1/data/{data_id}"]["get"].is_object(), "{with}");
    assert!(without.get("/v1/data").is_none());
    assert!(without.get("/v1/data/{data_id}").is_none());
    // Node: the node forms.
    let with = docs(
        node(with_provider(members()), Router::new()).await,
        "/v1/docs",
    )
    .await;
    assert!(
        with["/v1/components/hpc/data/{data_id}"]["get"].is_object(),
        "{with}"
    );
    assert!(with.get("/v1/data/{data_id}").is_none());
    // The legacy document is unchanged.
    for (with, without) in [
        (
            flat(with_provider(members()), Router::new()).await,
            flat(members(), Router::new()).await,
        ),
        (
            node(with_provider(members()), Router::new()).await,
            node(members(), Router::new()).await,
        ),
    ] {
        assert_eq!(
            get(&with, "/vehicle/v1/docs", None).await,
            get(&without, "/vehicle/v1/docs", None).await
        );
    }
}
