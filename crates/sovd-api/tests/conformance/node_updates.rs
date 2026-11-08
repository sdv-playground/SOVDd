//! Node-level software updates: `{base}/updates` on the server root
//! (ISO 17978-3 §7.18.1.1), an alias of the per-component collections. See
//! docs/x-update.md "Node-level collection".

use std::collections::HashMap;
use std::sync::Arc;

use reqwest::Method;
use serde_json::{json, Value};
use sovd_api::{create_router, AppState, Tree};
use sovd_client::testing::TestServer;
use sovd_core::{DiagnosticBackend, VerdictScope};

use crate::common::{BankedBackend, MockBackend, ScopedAuthorizer};
use crate::{header, poll_until, register, send, server, status, terminal, OTA};

/// A second update-capable component.
const OTA2: &str = "ota2";

/// `ecu_a` / `ecu_b`, [`OTA`] and [`OTA2`]; with `auth`, behind the scoped
/// test authorizer.
async fn node_server(auth: bool) -> TestServer {
    let mut backends: HashMap<String, Arc<dyn DiagnosticBackend>> = HashMap::new();
    for id in ["ecu_a", "ecu_b"] {
        backends.insert(id.to_string(), Arc::new(MockBackend::new(id)));
    }
    for id in [OTA, OTA2] {
        let mut ota = BankedBackend::new(id, VerdictScope::Update);
        ota.capabilities.bulk_data = true;
        backends.insert(id.to_string(), Arc::new(ota));
    }
    let mut state = AppState::new(backends);
    if auth {
        state = state.with_authorizer(Arc::new(ScopedAuthorizer));
    }
    TestServer::start(create_router(state))
        .await
        .expect("test server")
}

/// `{base}/updates/{id}`.
fn node_url(tree: Tree, id: &str) -> String {
    format!("{}/updates/{id}", tree.base())
}

/// POST `body` to `path` with an optional bearer token.
async fn post(
    server: &TestServer,
    path: &str,
    body: Value,
    token: Option<&str>,
) -> reqwest::Response {
    let mut req = reqwest::Client::new()
        .post(format!("{}{path}", server.base_url()))
        .json(&body);
    if let Some(token) = token {
        req = req.bearer_auth(token);
    }
    req.send().await.expect("request")
}

/// Register on the node-level collection.
async fn register_node(server: &TestServer, tree: Tree, body: Value) -> reqwest::Response {
    post(server, &format!("{}/updates", tree.base()), body, None).await
}

/// `GET {base}/updates?origin=proximity` items, with an optional token.
async fn node_items(server: &TestServer, tree: Tree, token: Option<&str>) -> Vec<String> {
    let auth = token.map(|t| format!("Bearer {t}"));
    let headers: Vec<(&str, &str)> = auth.iter().map(|a| ("authorization", a.as_str())).collect();
    let path = format!("{}/updates?origin=proximity", tree.base());
    let resp = send(server, Method::GET, &path, &headers, None).await;
    assert_eq!(resp.status().as_u16(), 200, "{path}");
    let body: Value = resp.json().await.expect("listing body");
    serde_json::from_value(body["items"].clone()).expect("items")
}

/// ISO 17978-3 §7.18.1.1 / §7.6.3: the root document references the
/// server-root `updates` collection, and it lists updates registered through
/// it and through a component collection.
#[tokio::test]
async fn root_references_node_collection() {
    for tree in Tree::ALL {
        let server = server().await;
        let base = tree.base();
        let resp = send(&server, Method::GET, &format!("{base}/"), &[], None).await;
        assert_eq!(resp.status().as_u16(), 200, "{base}/");
        let body: Value = resp.json().await.expect("root body");
        assert_eq!(body["updates"], format!("{base}/updates"), "{body}");

        let resp = register_node(&server, tree, json!({ "id": "u-node", "target": OTA })).await;
        assert_eq!(resp.status().as_u16(), 201, "{base}: node register");
        let resp = register(&server, tree, json!({ "id": "u-comp" })).await;
        assert_eq!(resp.status().as_u16(), 201, "{base}: component register");

        assert_eq!(node_items(&server, tree, None).await, ["u-comp", "u-node"]);
    }
}

/// A node-level registration names its component in `target` (one of the
/// root `x-update.update-targets`); the 201 points into `{base}/updates`.
#[tokio::test]
async fn node_registration_requires_target() {
    for tree in Tree::ALL {
        let server = server().await;
        let base = tree.base();
        for body in [
            json!({ "id": "u1" }),
            json!({ "id": "u1", "target": "nope" }),
            json!({ "id": "u1", "target": "ecu_a" }),
        ] {
            let resp = register_node(&server, tree, body.clone()).await;
            assert_eq!(resp.status().as_u16(), 400, "{base}: {body}");
            let err: Value = resp.json().await.expect("error body");
            assert_eq!(err["error_code"], "incomplete-request", "{base}: {err}");
        }

        let resp = register_node(&server, tree, json!({ "id": "u1", "target": OTA })).await;
        assert_eq!(resp.status().as_u16(), 201, "{base}");
        let update = node_url(tree, "u1");
        assert_eq!(header(&resp, "location").as_deref(), Some(update.as_str()));
        let body: Value = resp.json().await.expect("register body");
        assert_eq!(body["id"], "u1", "{body}");
        assert_eq!(body["update_id"], body["id"], "{body}");
        for key in ["href", "bulk_data_href", "executions_href"] {
            let href = body[key].as_str().expect("href string");
            assert!(href.starts_with(&update), "{base}: {key} = {href}");
        }

        // Id rules as on the component collection: `/v1` mints, legacy refuses.
        let resp = register_node(&server, tree, json!({ "target": OTA })).await;
        let expected = match tree {
            Tree::V1 => 201,
            Tree::Legacy => 400,
        };
        assert_eq!(resp.status().as_u16(), expected, "{base}: no id");
    }
}

/// The lifecycle on the node-level collection: upload, prepare (202 +
/// `Location` into `{base}/updates`), status with `x-update.target` (`/v1`),
/// delete.
/// A deleted id is free again on the component collection.
#[tokio::test]
async fn node_lifecycle() {
    for tree in Tree::ALL {
        let server = server().await;
        let update = node_url(tree, "u1");
        let resp = register_node(&server, tree, json!({ "id": "u1", "target": OTA })).await;
        assert_eq!(resp.status().as_u16(), 201, "{update}");

        let resp = send(
            &server,
            Method::PUT,
            &format!("{update}/bulk-data/manifest"),
            &[("content-type", "application/octet-stream")],
            Some(b"banked".to_vec()),
        )
        .await;
        assert_eq!(resp.status().as_u16(), 201, "{update}: manifest");
        let body: Value = resp.json().await.expect("part body");
        assert_eq!(body["href"], format!("{update}/bulk-data/manifest"));

        let resp = send(
            &server,
            Method::GET,
            &format!("{update}/bulk-data"),
            &[],
            None,
        )
        .await;
        assert_eq!(resp.status().as_u16(), 200, "{update}/bulk-data");
        let body: Value = resp.json().await.expect("parts body");
        assert_eq!(
            body["items"][0]["href"],
            format!("{update}/bulk-data/manifest")
        );

        let resp = send(
            &server,
            Method::PUT,
            &format!("{update}/prepare"),
            &[],
            None,
        )
        .await;
        assert_eq!(resp.status().as_u16(), 202, "{update}/prepare");
        assert_eq!(header(&resp, "location"), Some(format!("{update}/status")));

        let done = poll_until(&server, &update, terminal).await;
        assert_eq!(done["status"], "completed", "{update}: {done}");
        // `x-update.target` is a `/v1` member; legacy bodies are unchanged.
        let target = match tree {
            Tree::V1 => json!(OTA),
            Tree::Legacy => Value::Null,
        };
        assert_eq!(status(&server, &update).await["x-update"]["target"], target);

        let resp = send(&server, Method::GET, &update, &[], None).await;
        assert_eq!(resp.status().as_u16(), 200, "GET {update}");
        let body: Value = resp.json().await.expect("detail body");
        assert_eq!(body["id"], "u1", "{body}");
        assert_eq!(body["x-update"]["target"], target, "{body}");

        let resp = send(&server, Method::DELETE, &update, &[], None).await;
        assert_eq!(resp.status().as_u16(), 204, "DELETE {update}");
        let resp = send(&server, Method::GET, &update, &[], None).await;
        assert_eq!(resp.status().as_u16(), 404, "GET {update} after DELETE");

        let resp = register(&server, tree, json!({ "id": "u1" })).await;
        assert_eq!(
            resp.status().as_u16(),
            201,
            "{update}: re-register on {OTA}"
        );
    }
}

/// Ids are unique across the node-level collection: a node registration of
/// an id taken on any component is 409. An id registered on two components
/// through their own collections is ambiguous at node level (409).
#[tokio::test]
async fn node_ids_unique_and_ambiguity() {
    for tree in Tree::ALL {
        let server = node_server(false).await;
        let base = tree.base();
        let resp = register(&server, tree, json!({ "id": "u-dup" })).await;
        assert_eq!(resp.status().as_u16(), 201, "{base}");

        for target in [OTA, OTA2] {
            let resp =
                register_node(&server, tree, json!({ "id": "u-dup", "target": target })).await;
            assert_eq!(
                resp.status().as_u16(),
                409,
                "{base}: node register on {target}"
            );
            let err: Value = resp.json().await.expect("error body");
            assert_eq!(err["error_code"], "update-process-in-progress", "{err}");
        }

        let path = format!("{}/updates", tree.component(OTA2));
        let resp = post(&server, &path, json!({ "id": "u-dup" }), None).await;
        assert_eq!(resp.status().as_u16(), 201, "{path}");

        let path = format!("{}/status", node_url(tree, "u-dup"));
        let resp = send(&server, Method::GET, &path, &[], None).await;
        assert_eq!(resp.status().as_u16(), 409, "{path}");
        let err: Value = resp.json().await.expect("error body");
        assert_eq!(
            err["message"],
            "update id 'u-dup' is registered on several components (ota, ota2); \
             use the entity-level collection",
            "{err}"
        );

        let path = format!("{}/status", node_url(tree, "u-none"));
        let resp = send(&server, Method::GET, &path, &[], None).await;
        assert_eq!(resp.status().as_u16(), 404, "{path}");
    }
}

/// ISO 17978-3 §5.4.4 / C-031: the update's component scope applies at node
/// level — 403 under `/v1`, 401 on the legacy tree — and the listing leaves
/// out components the client may not access.
#[tokio::test]
async fn node_scope_follows_component() {
    let server = node_server(true).await;
    for tree in Tree::ALL {
        let base = tree.base();
        let id = format!("u-{}", base.replace('/', "-"));
        let path = format!("{}/updates", tree.component(OTA));
        let resp = post(&server, &path, json!({ "id": id }), Some("token-all")).await;
        assert_eq!(resp.status().as_u16(), 201, "{path}");
        let expected = match tree {
            Tree::V1 => 403,
            Tree::Legacy => 401,
        };

        let path = format!("{}/status", node_url(tree, &id));
        let resp = send(
            &server,
            Method::GET,
            &path,
            &[("authorization", "Bearer token-a")],
            None,
        )
        .await;
        assert_eq!(resp.status().as_u16(), expected, "{path}");
        let err: Value = resp.json().await.expect("error body");
        assert_eq!(err["error_code"], "insufficient-access-rights", "{err}");
        let resp = send(
            &server,
            Method::GET,
            &path,
            &[("authorization", "Bearer token-all")],
            None,
        )
        .await;
        assert_eq!(resp.status().as_u16(), 200, "{path}");

        let path = format!("{base}/updates");
        let resp = post(
            &server,
            &path,
            json!({ "id": "u-a", "target": OTA }),
            Some("token-a"),
        )
        .await;
        assert_eq!(resp.status().as_u16(), expected, "POST {path}");

        assert!(node_items(&server, tree, Some("token-a")).await.is_empty());
        assert!(node_items(&server, tree, Some("token-all"))
            .await
            .contains(&id));
    }
}
