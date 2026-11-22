//! Node topology — a server configured with a node also serves its backends on
//! `/v1` as the node's subcomponents
//! (`/v1/components/{node}/subcomponents/{id}/...`).
//!
//! Covers: without a node nothing changes; `/v1/components` lists the node
//! first and the backends still flat; the node document; the subcomponents
//! listing; a member's entity API under the node form, with node-form hrefs
//! and `Location`s; node-level resources under the node served by their root
//! forms' handlers, embedder routes included; other node paths 404; the flat
//! member paths carry the deprecation headers; the legacy tree is unchanged
//! with a node; the `/v1` capability doc lists the node paths; the component
//! scope applies through both forms; refused configurations.

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
/// answers the path it was routed as, the restart with a root-form `Location`.
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
    Router::new()
        .route("/v1/status", axum::routing::get(served))
        .route("/v1/status/restart", axum::routing::put(restart))
        .route(
            "/v1/operations/x-update-commit/executions",
            axum::routing::post(served),
        )
}

/// `state` as node `cvc`, its router (with the embedder's routes merged in)
/// wrapped in the node topology.
async fn node_server(state: AppState) -> TestServer {
    let state = state
        .with_node(NodeTopology::new("cvc", "Central vehicle computer"))
        .expect("valid node");
    let router = create_router(state.clone()).merge(embedder_routes());
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
        "/v1/components/ecu_a",
        "/v1/components/ecu_a/data",
        "/v1/components/ecu_a/faults",
        "/v1/components/ecu_a/subcomponents",
        "/v1/components/cvc/subcomponents/ecu_a",
        "/v1/docs",
        "/vehicle/v1/components/ecu_a",
    ] {
        let resp = send_json(&wrapped, reqwest::Method::GET, path, None).await;
        assert!(header(&resp, "deprecation").is_none(), "{path}");
        assert!(header(&resp, "link").is_none(), "{path}");
        let status = resp.status().as_u16();
        let body = resp.bytes().await.unwrap().to_vec();
        assert_eq!((status, body), get(&plain, path, None).await, "{path}");
    }
    let (status, body) = get(&wrapped, "/v1/components", None).await;
    assert_eq!(status, 200);
    let body = json(&body);
    let mut listed = ids(&body);
    listed.sort();
    assert_eq!(listed, ["ecu_a", "ecu_b"]);
}

#[tokio::test]
async fn components_lists_the_node_first_and_the_members_flat() {
    let server = node_server(members(&["vm1", "vm2"])).await;
    let (status, body) = get(&server, "/v1/components", None).await;
    assert_eq!(status, 200);
    let body = json(&body);
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 3, "{body}");
    assert_eq!(
        items[0],
        serde_json::json!({
            "id": "cvc",
            "name": "Central vehicle computer",
            "type": "node",
            "href": "/v1/components/cvc",
        })
    );
    let mut flat: Vec<(&str, &str)> = items[1..]
        .iter()
        .map(|i| (i["id"].as_str().unwrap(), i["href"].as_str().unwrap()))
        .collect();
    flat.sort();
    assert_eq!(
        flat,
        [("vm1", "/v1/components/vm1"), ("vm2", "/v1/components/vm2")]
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
    assert!(body.get("data").is_none(), "{body}");
    assert!(body.get("capabilities").is_none(), "{body}");
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
async fn a_member_serves_through_the_nested_path_what_it_serves_flat() {
    let server = node_server(members(&["vm1", "vm2"])).await;
    for rest in ["", "/data", "/faults", "/operations"] {
        let (status, flat) = get(&server, &format!("/v1/components/vm1{rest}"), None).await;
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
        for url in found {
            assert!(
                url.starts_with("/v1/components/cvc/subcomponents/vm1"),
                "{rest}: {url} is not a node-form URL"
            );
        }
    }
    // The flat request still renders flat URLs.
    let (_, flat) = get(&server, "/v1/components/vm1", None).await;
    assert_eq!(json(&flat)["data"], "/v1/components/vm1/data");
}

#[tokio::test]
async fn a_member_update_through_the_nested_path_renders_nested_urls() {
    let server = node_server(banked()).await;
    let resp = send_json(
        &server,
        reqwest::Method::POST,
        "/v1/components/cvc/subcomponents/vm1/updates",
        Some(serde_json::json!({ "id": "u1" })),
    )
    .await;
    assert_eq!(
        location(&resp, 201),
        "/v1/components/cvc/subcomponents/vm1/updates/u1"
    );
    // A part upload: its href in the form of the request.
    for (base, form) in [
        ("/v1/components/cvc/subcomponents/vm1", "nested"),
        ("/v1/components/vm1", "flat"),
    ] {
        let resp = reqwest::Client::new()
            .put(format!(
                "{}{base}/updates/u1/bulk-data/p-{form}",
                server.base_url()
            ))
            .body("payload")
            .send()
            .await
            .expect("request");
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap();
        assert!((200..300).contains(&status), "{form}: {status} {body}");
        let part = format!("{base}/updates/u1/bulk-data/p-{form}");
        let mut found = Vec::new();
        urls(&serde_json::from_str(&body).unwrap(), &mut found);
        assert!(found.contains(&part), "{form}: {body}");
    }
}

#[tokio::test]
async fn node_level_updates_are_the_root_collection() {
    let server = node_server(banked()).await;
    let resp = send_json(
        &server,
        reqwest::Method::POST,
        "/v1/components/cvc/updates",
        Some(serde_json::json!({ "id": "u1", "target": "vm2" })),
    )
    .await;
    assert_eq!(location(&resp, 201), "/v1/components/cvc/updates/u1");

    // Registered once, seen through both forms; each renders its own form.
    let (status, root) = get(&server, "/v1/updates/u1", None).await;
    assert_eq!(status, 200);
    let (status, node) = get(&server, "/v1/components/cvc/updates/u1", None).await;
    assert_eq!(status, 200);
    let root = String::from_utf8(root).unwrap();
    assert_eq!(
        String::from_utf8(node).unwrap(),
        root.replace("/v1/updates", "/v1/components/cvc/updates")
            .replace("/v1/components/vm2", "/v1/components/cvc/subcomponents/vm2")
    );
    let (status, root) = get(&server, "/v1/updates", None).await;
    assert_eq!(status, 200);
    let (status, node) = get(&server, "/v1/components/cvc/updates", None).await;
    assert_eq!(status, 200);
    assert_eq!(
        String::from_utf8(node).unwrap(),
        String::from_utf8(root)
            .unwrap()
            .replace("/v1/updates", "/v1/components/cvc/updates")
    );
}

#[tokio::test]
async fn node_level_resources_reach_the_embedders_root_routes() {
    let server = node_server(members(&["vm1"])).await;

    let (status, body) = get(&server, "/v1/components/cvc/status", None).await;
    assert_eq!(status, 200);
    assert_eq!(json(&body)["served"], "/v1/status");

    for path in ["/v1/status/restart", "/v1/components/cvc/status/restart"] {
        let resp = send_json(&server, reqwest::Method::PUT, path, None).await;
        let node_form = path.starts_with("/v1/components/cvc");
        // The embedder's root-form `Location` follows the form of the request.
        assert_eq!(
            location(&resp, 202),
            if node_form {
                "/v1/components/cvc/status"
            } else {
                "/v1/status"
            },
            "{path}"
        );
        assert_eq!(
            resp.json::<Value>().await.unwrap()["served"],
            "/v1/status/restart"
        );
    }

    for path in [
        "/v1/operations/x-update-commit/executions",
        "/v1/components/cvc/operations/x-update-commit/executions",
    ] {
        let resp = send_json(&server, reqwest::Method::POST, path, None).await;
        assert_eq!(resp.status().as_u16(), 200, "{path}");
        assert_eq!(
            resp.json::<Value>().await.unwrap()["served"],
            "/v1/operations/x-update-commit/executions"
        );
    }
}

#[tokio::test]
async fn other_node_paths_are_not_served() {
    let server = node_server(members(&["vm1"])).await;
    for path in [
        "/v1/components/cvc/data",
        "/v1/components/cvc/faults",
        "/v1/components/cvc/status/x-boot-id",
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
    for rest in ["", "/data", "/faults/f1"] {
        let flat = get(&server, &format!("/v1/components/nope{rest}"), None).await;
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

#[tokio::test]
async fn flat_member_paths_are_deprecated_on_v1_with_a_node() {
    let server = node_server(members(&["vm1"])).await;
    let resp = send_json(
        &server,
        reqwest::Method::GET,
        "/v1/components/vm1/data",
        None,
    )
    .await;
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(header(&resp, "deprecation"), Some("true"));
    assert_eq!(
        header(&resp, "link"),
        Some("</v1/components/cvc/subcomponents/vm1/data>; rel=\"successor-version\"")
    );
    // Not the node form, not the node, not the legacy tree, not without a node.
    for path in [
        "/v1/components/cvc/subcomponents/vm1/data",
        "/v1/components/cvc",
        "/v1/components",
        "/vehicle/v1/components/vm1/data",
    ] {
        let resp = send_json(&server, reqwest::Method::GET, path, None).await;
        assert!(header(&resp, "deprecation").is_none(), "{path}");
        assert!(header(&resp, "link").is_none(), "{path}");
    }
    let server = flat_server(members(&["vm1"])).await;
    let resp = send_json(
        &server,
        reqwest::Method::GET,
        "/v1/components/vm1/data",
        None,
    )
    .await;
    assert!(header(&resp, "deprecation").is_none());
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
    ] {
        let resp = send_json(&node, reqwest::Method::GET, path, None).await;
        assert!(header(&resp, "deprecation").is_none(), "{path}");
        let status = resp.status().as_u16();
        let body = resp.bytes().await.unwrap().to_vec();
        assert_eq!((status, body), get(&plain, path, None).await, "{path}");
    }
}

const LEGACY_COMPONENTS: &str = r#"{"items":[{"href":"/vehicle/v1/components/ecu_a","id":"ecu_a","name":"ecu_a ecu","type":"ecu"},{"href":"/vehicle/v1/components/ecu_b","id":"ecu_b","name":"ecu_b ecu","type":"ecu"}]}"#;
const LEGACY_REGISTER_LOCATION: &str = "/vehicle/v1/components/ecu_a/updates/u1";

#[tokio::test]
async fn the_v1_docs_list_the_node_paths() {
    let server = node_server(members(&["vm1"])).await;
    let (status, body) = get(&server, "/v1/docs", None).await;
    assert_eq!(status, 200);
    let doc = json(&body);
    let paths = doc["paths"].as_object().expect("paths");
    for path in [
        "/v1/components/cvc",
        "/v1/components/cvc/subcomponents",
        "/v1/components/cvc/subcomponents/{component_id}",
        "/v1/components/cvc/subcomponents/{component_id}/data/{param_id}",
        "/v1/components/cvc/updates",
        "/v1/components/cvc/updates/{update_id}/bulk-data",
        // The flat and root forms stay listed.
        "/v1/components/{component_id}/data/{param_id}",
        "/v1/updates",
    ] {
        assert!(paths.contains_key(path), "{path} missing");
    }
    assert_eq!(
        paths["/v1/components/cvc/subcomponents/{component_id}/data/{param_id}"],
        paths["/v1/components/{component_id}/data/{param_id}"]
    );

    // The legacy doc is that of a server without a node.
    let (_, legacy) = get(&server, "/vehicle/v1/docs", None).await;
    assert!(!String::from_utf8_lossy(&legacy).contains("subcomponents"));
    assert_eq!(
        legacy,
        get(&open_server().await, "/vehicle/v1/docs", None).await.1
    );
}

#[tokio::test]
async fn the_component_scope_applies_through_both_forms() {
    let state = members(&["vm1", "vm2"]).with_authorizer(Arc::new(ScopedAuthorizer));
    let server = node_server(state).await;

    for path in [
        "/v1/components/vm1/data",
        "/v1/components/cvc/subcomponents/vm1/data",
    ] {
        let (status, _) = get(&server, path, Some("token-vm1")).await;
        assert_eq!(status, 200, "{path}");
    }
    for path in [
        "/v1/components/vm2/data",
        "/v1/components/cvc/subcomponents/vm2/data",
    ] {
        let (status, _) = get(&server, path, Some("token-vm1")).await;
        assert_eq!(status, 403, "{path}");
        let (status, _) = get(&server, path, Some("token-all")).await;
        assert_eq!(status, 200, "{path}");
    }

    // The listings show what the client may access; the node to a client that may
    // access any member.
    let (status, body) = get(&server, "/v1/components", Some("token-vm1")).await;
    assert_eq!(status, 200);
    assert_eq!(ids(&json(&body)), ["cvc", "vm1"]);
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
