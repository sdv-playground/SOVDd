//! Access control.

use sovd_api::Tree;

use crate::common::{auth_server, get, json};
use crate::header;

/// ISO 17978-3 §5.4.4: resources may be accessible without authorization
/// (the capability docs are), while a protected resource is refused to a
/// client that is not authenticated.
#[tokio::test]
async fn docs_public_protected_resource_refused() {
    let server = auth_server().await;
    for tree in Tree::ALL {
        let base = tree.base();
        let (status, body) = get(&server, &format!("{base}/docs"), None).await;
        assert_eq!(status, 200, "{base}/docs");
        assert_eq!(json(&body)["openapi"], "3.1.0");

        let component = tree.component("ecu_a");
        let (status, _) = get(&server, &component, None).await;
        assert!(matches!(status, 401 | 403), "{component}: {status}");
    }
}

/// ISO 17978-3 §5.4.4 / Table 16 status codes: an authenticated client whose
/// token does not grant the addressed component is refused with 403.
#[tokio::test]
#[ignore = "F4: §5.4.4 — out-of-scope component answers 401, not 403"]
async fn out_of_scope_component_is_forbidden() {
    let server = auth_server().await;
    for tree in Tree::ALL {
        let component = tree.component("ecu_b");
        let (status, _) = get(&server, &component, Some("token-a")).await;
        assert_eq!(status, 403, "{component}");
    }
}

/// ISO 17978-3 §5.4.4 (RFC 9110 §15.5.2): a 401 response carries a
/// `WWW-Authenticate` challenge.
#[tokio::test]
#[ignore = "F4: §5.4.4 — 401 responses carry no WWW-Authenticate"]
async fn unauthorized_carries_www_authenticate() {
    let server = auth_server().await;
    for tree in Tree::ALL {
        let component = tree.component("ecu_a");
        let resp = reqwest::get(format!("{}{component}", server.base_url()))
            .await
            .expect("request");
        assert_eq!(resp.status().as_u16(), 401, "{component}");
        assert!(
            header(&resp, "www-authenticate").is_some(),
            "{component}: no WWW-Authenticate"
        );
    }
}
