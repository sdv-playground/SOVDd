//! Software updates (§7.18).

use reqwest::Method;
use serde_json::{json, Value};
use sovd_api::Tree;

use crate::{header, poll_until, register, send, server, status, update_url, upload_and_prepare};

/// ISO 17978-3 §7.18.9 / §7.18.5 / §7.18.7 / §7.18.8: register (201 +
/// `Location` under the base), prepare (202 + `Location` to the status),
/// read the status, and delete the update once prepare has settled (204).
#[tokio::test]
async fn lifecycle_happy_path() {
    for tree in Tree::ALL {
        let server = server().await;
        let update = update_url(tree, "u1");

        let resp = register(&server, tree, json!({ "id": "u1" })).await;
        assert_eq!(resp.status().as_u16(), 201, "{update}");
        assert_eq!(header(&resp, "location").as_deref(), Some(update.as_str()));

        let resp = send(
            &server,
            Method::PUT,
            &format!("{update}/bulk-data/manifest"),
            &[("content-type", "application/octet-stream")],
            Some(b"banked".to_vec()),
        )
        .await;
        assert_eq!(resp.status().as_u16(), 201, "{update}: manifest");
        let resp = send(
            &server,
            Method::PUT,
            &format!("{update}/prepare"),
            &[],
            None,
        )
        .await;
        assert_eq!(resp.status().as_u16(), 202, "{update}/prepare");
        assert_eq!(
            header(&resp, "location"),
            Some(format!("{update}/status")),
            "{update}/prepare"
        );

        let body = status(&server, &update).await;
        assert!(body["status"].is_string(), "{update}: {body}");
        let done = poll_until(&server, &update, crate::terminal).await;
        assert_eq!(done["status"], "completed", "{update}: {done}");

        let resp = send(&server, Method::DELETE, &update, &[], None).await;
        assert_eq!(resp.status().as_u16(), 204, "DELETE {update}");
        let resp = send(&server, Method::GET, &update, &[], None).await;
        assert_eq!(resp.status().as_u16(), 404, "GET {update} after DELETE");
    }
}

/// ISO 17978-3 §7.18.8 Table 275: deleting an update package that is in
/// phase execute answers 405.
#[tokio::test]
#[ignore = "F1: §7.18.8 — DELETE during the execute phase is accepted (204), not 405"]
async fn delete_during_execute_is_405() {
    for tree in Tree::ALL {
        let server = server().await;
        let update = update_url(tree, "u1");
        let resp = register(&server, tree, json!({ "id": "u1" })).await;
        assert_eq!(resp.status().as_u16(), 201);
        upload_and_prepare(&server, &update).await;
        let resp = send(
            &server,
            Method::PUT,
            &format!("{update}/execute?x-ota-control=orchestrated"),
            &[],
            None,
        )
        .await;
        assert_eq!(resp.status().as_u16(), 202, "{update}/execute");
        let body = poll_until(&server, &update, |b| {
            b["x-ota-substate"] == "awaiting-verdict"
        })
        .await;
        assert_eq!(body["phase"], "execute", "{update}: {body}");

        let resp = send(&server, Method::DELETE, &update, &[], None).await;
        assert_eq!(resp.status().as_u16(), 405, "DELETE {update} in execute");
    }
}

/// ISO 17978-3 §7.18.9 Table 277: the registration response body carries the
/// `id` of the update package.
#[tokio::test]
async fn registration_body_carries_id() {
    for tree in Tree::ALL {
        let server = server().await;
        let resp = register(&server, tree, json!({ "id": "u1" })).await;
        assert_eq!(resp.status().as_u16(), 201);
        let body: Value = resp.json().await.expect("register body");
        assert_eq!(body["id"], "u1", "{tree:?}: {body}");
    }
}

/// ISO 17978-3 §7.18.9: the id in the registration response is created by
/// the server, so a registration body without an id is accepted.
#[tokio::test]
#[ignore = "F3: §7.18.9 — POST /updates without an id is refused (400)"]
async fn registration_without_id_gets_server_id() {
    for tree in Tree::ALL {
        let server = server().await;
        let resp = register(&server, tree, json!({})).await;
        assert_eq!(resp.status().as_u16(), 201, "{tree:?}");
        let location = header(&resp, "location").expect("Location header");
        let body: Value = resp.json().await.expect("register body");
        let id = body["id"].as_str().expect("server-created id");
        assert!(!id.is_empty());
        assert_eq!(location, update_url(tree, id));
    }
}

/// docs/x-update.md (status member): a freshly registered update reports
/// `x-update.state` `REGISTERED`.
#[tokio::test]
#[ignore = "A3: docs/x-update.md — x-update.state after registration is READY, not REGISTERED"]
async fn registered_state_name() {
    for tree in Tree::ALL {
        let server = server().await;
        let update = update_url(tree, "u1");
        let resp = register(&server, tree, json!({ "id": "u1" })).await;
        assert_eq!(resp.status().as_u16(), 201);
        let body = status(&server, &update).await;
        assert_eq!(body["x-update"]["state"], "REGISTERED", "{update}: {body}");
    }
}
