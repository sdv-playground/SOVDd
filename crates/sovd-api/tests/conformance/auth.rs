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
/// token does not grant the addressed component is refused with 403 and a
/// GenericError body. The legacy tree keeps its historical 401.
#[tokio::test]
async fn out_of_scope_component_is_forbidden() {
    let server = auth_server().await;
    for tree in Tree::ALL {
        let component = tree.component("ecu_b");
        let (status, body) = get(&server, &component, Some("token-a")).await;
        let expected = match tree {
            Tree::V1 => 403,
            Tree::Legacy => 401,
        };
        assert_eq!(status, expected, "{component}");
        assert_eq!(
            json(&body)["error_code"],
            "insufficient-access-rights",
            "{component}"
        );
    }
}

/// ISO 17978-3 §5.4.4 (RFC 9110 §15.5.2): a 401 response carries a
/// `WWW-Authenticate` challenge; a presented but invalid token is flagged
/// `invalid_token` (RFC 6750 §3.1).
#[tokio::test]
async fn unauthorized_carries_www_authenticate() {
    let server = auth_server().await;
    for tree in Tree::ALL {
        let component = tree.component("ecu_a");
        let resp = reqwest::get(format!("{}{component}", server.base_url()))
            .await
            .expect("request");
        assert_eq!(resp.status().as_u16(), 401, "{component}");
        assert_eq!(
            header(&resp, "www-authenticate").as_deref(),
            Some(r#"Bearer realm="sovd""#),
            "{component}: no token"
        );

        let resp = reqwest::Client::new()
            .get(format!("{}{component}", server.base_url()))
            .bearer_auth("bogus")
            .send()
            .await
            .expect("request");
        assert_eq!(resp.status().as_u16(), 401, "{component}");
        assert_eq!(
            header(&resp, "www-authenticate").as_deref(),
            Some(r#"Bearer realm="sovd", error="invalid_token""#),
            "{component}: invalid token"
        );
    }
}
