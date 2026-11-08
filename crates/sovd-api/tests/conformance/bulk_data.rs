//! Bulk data (§7.20).

use reqwest::Method;
use serde_json::json;
use sovd_api::Tree;

use crate::{header, register, send, server, update_url};

/// ISO 17978-3 §7.20: a file is uploaded with `POST` to a bulk-data
/// collection, its name given by `Content-Disposition`; the server answers
/// 201 with a `Location` under that collection.
#[tokio::test]
#[ignore = "F13: §7.20 — no POST upload to an update's bulk-data collection (only PUT per part)"]
async fn post_upload_to_update_bulk_data() {
    let server = server().await;
    for tree in Tree::ALL {
        let resp = register(&server, tree, json!({ "id": "u1" })).await;
        assert_eq!(resp.status().as_u16(), 201);
        let collection = format!("{}/bulk-data", update_url(tree, "u1"));
        let resp = send(
            &server,
            Method::POST,
            &collection,
            &[
                ("content-type", "application/octet-stream"),
                ("content-disposition", "attachment; filename=\"pkg.bin\""),
            ],
            Some(b"bytes".to_vec()),
        )
        .await;
        assert_eq!(resp.status().as_u16(), 201, "POST {collection}");
        let location = header(&resp, "location").expect("Location header");
        assert!(
            location.starts_with(&format!("{collection}/")),
            "{collection}: {location}"
        );
    }
}
