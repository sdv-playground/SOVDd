//! Operations (§7.14).

use reqwest::Method;
use sovd_api::Tree;

use crate::{header, send, server};

/// ISO 17978-3 §7.14.6 Table 184 / §7.14.7 Table 189: an asynchronous start
/// answers 202 with body `{id, status}`, and reading the execution returns
/// its `capability`.
#[tokio::test]
async fn execution_start_and_status_shape() {
    let server = server().await;
    for tree in Tree::ALL {
        let path = format!("{}/operations/selftest/executions", tree.component("ecu_a"));
        let resp = send(
            &server,
            Method::POST,
            &path,
            &[("content-type", "application/json")],
            Some(b"{}".to_vec()),
        )
        .await;
        assert_eq!(resp.status().as_u16(), 202, "{path}");
        let location = header(&resp, "location").expect("Location header");
        let body: serde_json::Value = resp.json().await.expect("202 body");
        assert!(body["id"].is_string(), "{path}: {body}");
        assert!(body["status"].is_string(), "{path}: {body}");
        // `execution_id` stays beside the spec `id` (client dependency).
        assert_eq!(body["id"], body["execution_id"], "{path}: {body}");

        let resp = send(&server, Method::GET, &location, &[], None).await;
        assert_eq!(resp.status().as_u16(), 200, "{location}");
        let body: serde_json::Value = resp.json().await.expect("execution body");
        assert!(body["capability"].is_string(), "{location}: {body}");
    }
}

/// ISO 17978-3 §7.14.6 Table 184 / §7.14.7 Table 189 on a sub-entity: the
/// app-scoped executions carry the same `id` and `capability`.
#[tokio::test]
async fn sub_entity_execution_start_and_status_shape() {
    let server = server().await;
    for tree in Tree::ALL {
        let path = format!(
            "{}/operations/selftest/executions",
            tree.app("ecu_a", "app_a")
        );
        let resp = send(
            &server,
            Method::POST,
            &path,
            &[("content-type", "application/json")],
            Some(b"{}".to_vec()),
        )
        .await;
        assert_eq!(resp.status().as_u16(), 202, "{path}");
        let location = header(&resp, "location").expect("Location header");
        let body: serde_json::Value = resp.json().await.expect("202 body");
        assert!(body["id"].is_string(), "{path}: {body}");
        assert_eq!(body["id"], body["execution_id"], "{path}: {body}");
        assert_eq!(body["capability"], "execute", "{path}: {body}");

        let resp = send(&server, Method::GET, &location, &[], None).await;
        assert_eq!(resp.status().as_u16(), 200, "{location}");
        let body: serde_json::Value = resp.json().await.expect("execution body");
        assert_eq!(body["id"], body["execution_id"], "{location}: {body}");
        assert_eq!(body["capability"], "execute", "{location}: {body}");
    }
}
