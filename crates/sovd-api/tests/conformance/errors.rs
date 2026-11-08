//! Error responses.

use reqwest::Method;
use sovd_api::Tree;

use crate::common::json;
use crate::{assert_generic_error, send, server};

/// ISO 17978-3 §5.8.2 / §5.8.3 Table 16: an unknown resource answers 404 and
/// a method the resource does not support answers 405, each with a
/// `GenericError` body.
#[tokio::test]
async fn not_found_and_method_not_allowed_are_generic_errors() {
    let server = server().await;
    for tree in Tree::ALL {
        let base = tree.base();
        for path in [
            format!("{base}/no-such"),
            format!("{}/no-such", tree.component("ecu_a")),
        ] {
            let resp = send(&server, Method::GET, &path, &[], None).await;
            assert_eq!(resp.status().as_u16(), 404, "{path}");
            let body = json(&resp.bytes().await.expect("body"));
            assert_generic_error(&body, &path);
        }

        let path = tree.component("ecu_a");
        let resp = send(&server, Method::DELETE, &path, &[], None).await;
        assert_eq!(resp.status().as_u16(), 405, "DELETE {path}");
        let body = json(&resp.bytes().await.expect("body"));
        assert_generic_error(&body, &path);
    }
}
