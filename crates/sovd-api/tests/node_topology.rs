//! Node topology — a server configured with a node serves its backends as the
//! node's subcomponents (`{base}/components/{node}/subcomponents/{id}/...`).
//!
//! Covers: `/components` lists the node alone; the node document links its
//! subcomponents, and with a host backend serves the host's resources as its
//! own; the subcomponents listing; a subcomponent's entity API under the node
//! form, identical to the flat one of a server without a node; rendered URLs
//! take the node form; flat component paths answer 404; the component scope
//! applies to subcomponents as to components; refused configurations.

mod common;

use common::*;
use serde_json::Value;
use sovd_api::{create_router, with_node_topology, AppState, NodeTopology};
use sovd_client::testing::TestServer;
use std::sync::Arc;

const BASES: [&str; 2] = ["/v1", "/vehicle/v1"];

async fn node_server(node: NodeTopology, state: AppState) -> TestServer {
    let state = state.with_node(node).expect("valid node");
    let router = with_node_topology(create_router(state.clone()), &state);
    TestServer::start(router).await.expect("test server")
}

async fn node() -> TestServer {
    node_server(NodeTopology::new("node", "Node"), state()).await
}

async fn hosted() -> TestServer {
    node_server(
        NodeTopology::new("node", "Node").with_host("ecu_a"),
        state(),
    )
    .await
}

#[tokio::test]
async fn components_lists_the_node_alone() {
    let server = node().await;
    for base in BASES {
        let (status, body) = get(&server, &format!("{base}/components"), None).await;
        assert_eq!(status, 200, "{base}");
        let body = json(&body);
        let items = body["items"].as_array().expect("items");
        assert_eq!(items.len(), 1, "{base}: {body}");
        assert_eq!(items[0]["id"], "node");
        assert_eq!(items[0]["name"], "Node");
        assert_eq!(items[0]["type"], "node");
        assert_eq!(items[0]["href"], format!("{base}/components/node"));
    }
}

#[tokio::test]
async fn the_node_lists_its_backends_as_subcomponents() {
    let server = node().await;
    for base in BASES {
        let (status, body) = get(&server, &format!("{base}/components/node"), None).await;
        assert_eq!(status, 200, "{base}");
        assert_eq!(
            json(&body)["subcomponents"],
            format!("{base}/components/node/subcomponents")
        );

        let (status, body) = get(
            &server,
            &format!("{base}/components/node/subcomponents"),
            None,
        )
        .await;
        assert_eq!(status, 200, "{base}");
        let body = json(&body);
        let items: Vec<(&str, &str)> = body["items"]
            .as_array()
            .expect("items")
            .iter()
            .map(|i| (i["id"].as_str().unwrap(), i["href"].as_str().unwrap()))
            .collect();
        assert_eq!(
            items,
            vec![
                (
                    "ecu_a",
                    &*format!("{base}/components/node/subcomponents/ecu_a")
                ),
                (
                    "ecu_b",
                    &*format!("{base}/components/node/subcomponents/ecu_b")
                ),
            ]
        );
    }
}

#[tokio::test]
async fn a_subcomponent_serves_what_a_component_serves() {
    let flat = open_server().await;
    let node = node().await;
    for base in BASES {
        let (status, flat_body) = get(&flat, &format!("{base}/components/ecu_a/data"), None).await;
        assert_eq!(status, 200, "{base}");
        let (status, node_body) = get(
            &node,
            &format!("{base}/components/node/subcomponents/ecu_a/data"),
            None,
        )
        .await;
        assert_eq!(status, 200, "{base}");
        // The same resources, at the node-form URLs.
        let expected = String::from_utf8(flat_body).unwrap().replace(
            &format!("{base}/components/ecu_a"),
            &format!("{base}/components/node/subcomponents/ecu_a"),
        );
        assert_eq!(String::from_utf8(node_body).unwrap(), expected, "{base}");
    }
}

#[tokio::test]
async fn a_subcomponent_document_links_node_form_urls() {
    let server = node().await;
    let (status, body) = get(&server, "/v1/components/node/subcomponents/ecu_b", None).await;
    assert_eq!(status, 200);
    let body = json(&body);
    assert_eq!(body["id"], "ecu_b");
    let mut found = Vec::new();
    urls(&body, &mut found);
    assert!(!found.is_empty(), "{body}");
    for url in found {
        assert!(
            url.starts_with("/v1/components/node/subcomponents/ecu_b/"),
            "{url} is not a node-form URL"
        );
    }
}

#[tokio::test]
async fn flat_component_paths_are_not_served() {
    let server = node().await;
    for path in [
        "/v1/components/ecu_a",
        "/v1/components/ecu_a/data",
        "/vehicle/v1/components/ecu_b/faults",
        "/v1/components/node/subcomponents/no_such",
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
async fn a_hosted_node_serves_its_hosts_resources_as_its_own() {
    let server = hosted().await;
    let (status, body) = get(&server, "/v1/components/node", None).await;
    assert_eq!(status, 200);
    let body = json(&body);
    assert_eq!(body["id"], "node");
    assert_eq!(body["type"], "ecu", "the host's entity type");
    assert_eq!(body["data"], "/v1/components/node/data");
    assert_eq!(body["subcomponents"], "/v1/components/node/subcomponents");

    let (status, own) = get(&server, "/v1/components/node/data", None).await;
    assert_eq!(status, 200);
    let (status, hosts) = get(
        &server,
        "/v1/components/node/subcomponents/ecu_a/data",
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(json(&own)["items"], json(&hosts)["items"]);
}

#[tokio::test]
async fn a_node_without_a_host_has_no_resources_of_its_own() {
    let server = node().await;
    let (status, _) = get(&server, "/v1/components/node/data", None).await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn the_component_scope_applies_to_subcomponents() {
    let state = state().with_authorizer(Arc::new(ScopedAuthorizer));
    let server = node_server(NodeTopology::new("node", "Node"), state).await;

    // A client scoped to ecu_a sees the node and, of its subcomponents, ecu_a.
    let (status, body) = get(&server, "/v1/components", Some("token-a")).await;
    assert_eq!(status, 200);
    assert_eq!(json(&body)["items"][0]["id"], "node");
    let (status, body) = get(
        &server,
        "/v1/components/node/subcomponents",
        Some("token-a"),
    )
    .await;
    assert_eq!(status, 200);
    let ids: Vec<Value> = json(&body)["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].clone())
        .collect();
    assert_eq!(ids, vec![Value::from("ecu_a")]);

    let (status, _) = get(
        &server,
        "/v1/components/node/subcomponents/ecu_a/data",
        Some("token-a"),
    )
    .await;
    assert_eq!(status, 200);
    let (status, _) = get(
        &server,
        "/v1/components/node/subcomponents/ecu_b/data",
        Some("token-a"),
    )
    .await;
    assert_eq!(status, 403);
    let (status, _) = get(
        &server,
        "/v1/components/node/subcomponents/ecu_b/data",
        Some("token-all"),
    )
    .await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn a_server_without_a_node_is_unchanged() {
    // Wrapping a state without a node changes nothing, and nothing lists subcomponents.
    let state = state();
    let server = TestServer::start(with_node_topology(create_router(state.clone()), &state))
        .await
        .expect("test server");
    let (status, body) = get(&server, "/v1/components", None).await;
    assert_eq!(status, 200);
    assert_eq!(json(&body)["items"].as_array().unwrap().len(), 2);
    let (status, _) = get(&server, "/v1/components/ecu_a/data", None).await;
    assert_eq!(status, 200);
    let (status, _) = get(&server, "/v1/components/ecu_a/subcomponents", None).await;
    assert_eq!(status, 404);
}

#[test]
fn a_node_needs_an_id_of_its_own_and_a_host_that_is_a_backend() {
    assert!(state()
        .with_node(NodeTopology::new("ecu_a", "Clash"))
        .is_err());
    assert!(state()
        .with_node(NodeTopology::new("node", "Node").with_host("no_such"))
        .is_err());
    assert!(state()
        .with_node(NodeTopology::new("node", "Node").with_host("ecu_b"))
        .is_ok());
}
