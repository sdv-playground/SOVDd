//! Bulk data (§7.20).

use reqwest::Method;
use serde_json::{json, Value};
use sovd_api::Tree;

use crate::{header, poll_until, register, send, server, terminal, update_url};

/// An update's URL on its component collection and on the node-level alias.
fn update_urls(tree: Tree, id: &str) -> [String; 2] {
    [
        update_url(tree, id),
        format!("{}/updates/{id}", tree.base()),
    ]
}

/// `GET {update}/bulk-data` items.
async fn parts(server: &sovd_client::testing::TestServer, update: &str) -> Vec<Value> {
    let resp = send(
        server,
        Method::GET,
        &format!("{update}/bulk-data"),
        &[],
        None,
    )
    .await;
    assert_eq!(resp.status().as_u16(), 200, "{update}/bulk-data");
    let body: Value = resp.json().await.expect("listing body");
    body["items"].as_array().expect("items").clone()
}

/// `POST {collection}` the manifest bytes with `headers`.
async fn post_part(
    server: &sovd_client::testing::TestServer,
    collection: &str,
    headers: Vec<(&str, &str)>,
) -> reqwest::Response {
    send(
        server,
        Method::POST,
        collection,
        &headers,
        Some(b"banked".to_vec()),
    )
    .await
}

/// ISO 17978-3 §7.20 Tables 301-303: a file is uploaded with `POST` to a
/// bulk-data collection, its name given by `Content-Disposition`; the server
/// answers 201 with `{id}` and a `Location` under that collection. The
/// listing carries Table 298 descriptors (under `/v1`) and the part is
/// accepted by `prepare` like a `PUT` one.
#[tokio::test]
async fn post_upload_to_update_bulk_data() {
    for tree in Tree::ALL {
        for update in update_urls(tree, "u1") {
            let server = server().await;
            let resp = register(&server, tree, json!({ "id": "u1" })).await;
            assert_eq!(resp.status().as_u16(), 201);
            let collection = format!("{update}/bulk-data");
            let disposition = (
                "content-disposition",
                "form-data; name=\"manifest\"; filename=\"manifest\"",
            );
            let post = |headers: Vec<(&'static str, &'static str)>| {
                post_part(&server, &collection, headers)
            };
            let resp = post(vec![("content-type", "application/cbor"), disposition]).await;
            assert_eq!(resp.status().as_u16(), 201, "POST {collection}");
            let location = format!("{collection}/manifest");
            assert_eq!(
                header(&resp, "location"),
                Some(location.clone()),
                "{collection}"
            );
            assert!(header(&resp, "etag").is_some(), "{collection}: ETag");
            let body: Value = resp.json().await.expect("upload body");
            assert_eq!(body, json!({ "id": "manifest" }), "{collection}");

            // An identical retry: 201, same Location.
            let resp = post(vec![("content-type", "application/cbor"), disposition]).await;
            assert_eq!(resp.status().as_u16(), 201, "POST {collection} again");
            assert_eq!(
                header(&resp, "location"),
                Some(location.clone()),
                "{collection}"
            );

            let items = parts(&server, &update).await;
            assert_eq!(items.len(), 1, "{collection}: {items:?}");
            let item = &items[0];
            assert_eq!(item["part_id"], "manifest", "{collection}: {item}");
            assert_eq!(item["href"], location, "{collection}: {item}");
            assert_eq!(item["size"], 6, "{collection}: {item}");
            if tree == Tree::V1 {
                assert_eq!(item["id"], "manifest", "{collection}: {item}");
                assert_eq!(item["mimetype"], "application/cbor", "{collection}: {item}");
                assert_eq!(item["hash"], item["sha256"], "{collection}: {item}");
                assert_eq!(item["hash_algorithm"], "sha256", "{collection}: {item}");
            } else {
                for member in ["id", "mimetype", "hash", "hash_algorithm"] {
                    assert!(item.get(member).is_none(), "{collection}: {item}");
                }
            }

            // No Content-Disposition, or a name that is no URL segment: 400.
            for headers in [
                vec![("content-type", "application/cbor")],
                vec![("content-disposition", "form-data; filename=\"a/b\"")],
                vec![("content-disposition", "attachment")],
            ] {
                let resp = post(headers.clone()).await;
                assert_eq!(resp.status().as_u16(), 400, "{collection}: {headers:?}");
                let body: Value = resp.json().await.expect("error body");
                crate::assert_generic_error(&body, &collection);
                assert_eq!(
                    body["error_code"], "incomplete-request",
                    "{collection}: {body}"
                );
            }

            let resp = send(
                &server,
                Method::PUT,
                &format!("{update}/prepare"),
                &[],
                None,
            )
            .await;
            assert_eq!(resp.status().as_u16(), 202, "{update}/prepare");
            let done = poll_until(&server, &update, terminal).await;
            assert_eq!(done["status"], "completed", "{update}: {done}");

            // Uploads end with prepare, for the POST as for the PUT.
            let resp = post(vec![disposition]).await;
            assert_eq!(
                resp.status().as_u16(),
                409,
                "POST {collection} after prepare"
            );
        }
    }
}

/// The legacy `PUT {update}/bulk-data/{part_id}` still uploads, and its part
/// is listed like a `POST` one, with the default mimetype.
#[tokio::test]
async fn put_and_post_parts_list_alike() {
    for tree in Tree::ALL {
        for update in update_urls(tree, "u1") {
            let server = server().await;
            let resp = register(&server, tree, json!({ "id": "u1" })).await;
            assert_eq!(resp.status().as_u16(), 201);
            let resp = send(
                &server,
                Method::PUT,
                &format!("{update}/bulk-data/put-part"),
                &[("content-type", "application/cbor")],
                Some(b"bytes".to_vec()),
            )
            .await;
            assert_eq!(resp.status().as_u16(), 201, "{update}: PUT");
            let body: Value = resp.json().await.expect("PUT body");
            assert_eq!(body["part_id"], "put-part", "{update}: {body}");
            assert_eq!(
                body["href"],
                format!("{update}/bulk-data/put-part"),
                "{update}"
            );
            let resp = send(
                &server,
                Method::POST,
                &format!("{update}/bulk-data"),
                &[("content-disposition", "form-data; name=\"post-part\"")],
                Some(b"bytes".to_vec()),
            )
            .await;
            assert_eq!(resp.status().as_u16(), 201, "{update}: POST");
            assert_eq!(
                header(&resp, "location"),
                Some(format!("{update}/bulk-data/post-part")),
                "{update}"
            );

            let mut items = parts(&server, &update).await;
            assert_eq!(items.len(), 2, "{update}: {items:?}");
            for (item, name) in items.iter_mut().zip(["put-part", "post-part"]) {
                assert_eq!(item["part_id"], name, "{update}: {item}");
                if tree == Tree::V1 {
                    assert_eq!(
                        item["mimetype"], "application/octet-stream",
                        "{update}: {item}"
                    );
                }
                let obj = item.as_object_mut().expect("item object");
                obj.remove("part_id");
                obj.remove("id");
                obj.remove("href");
            }
            assert_eq!(items[0], items[1], "{update}");
        }
    }
}

/// §5.2.1: `OPTIONS` on an update's bulk-data collection lists the upload.
#[tokio::test]
async fn options_on_update_bulk_data_allows_post() {
    for tree in Tree::ALL {
        let server = server().await;
        let resp = register(&server, tree, json!({ "id": "u1" })).await;
        assert_eq!(resp.status().as_u16(), 201);
        let path = format!("{}/bulk-data", update_url(tree, "u1"));
        let resp = send(&server, Method::OPTIONS, &path, &[], None).await;
        assert!(resp.status().is_success(), "{path}: {}", resp.status());
        let allow = header(&resp, "allow").expect("Allow header");
        assert!(allow.contains("POST"), "{path}: {allow}");
        assert!(allow.contains("GET"), "{path}: {allow}");
    }
}
