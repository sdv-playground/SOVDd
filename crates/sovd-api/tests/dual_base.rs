//! Dual base path — the entity API is served under both `/v1` and
//! `/vehicle/v1` over one shared `AppState`.
//!
//! Covers: `/version-info` advertises `/v1` first; the two bases return the
//! same bodies; the capability docs are public under both; the component-scope
//! authorization decision is identical under both (the security property); and
//! unknown paths / wrong methods get the `GenericError` body under both; every
//! URL a response emits (hrefs, `Location`, capability-doc `servers`/`paths`)
//! uses the base the request came in on; legacy responses are unchanged.
//!
//! Same in-process `TestServer` + mock backend pattern as `authorizer_seam.rs`;
//! the mock and helpers live in `common`.

mod common;

use common::*;
use serde_json::Value;
use sovd_api::Tree;

const BASES: [&str; 2] = ["/v1", "/vehicle/v1"];

/// `body` with every `/vehicle/v1` replaced by `/v1` — a legacy body mapped
/// onto the `/v1` tree.
fn as_v1(body: &[u8]) -> String {
    String::from_utf8(body.to_vec())
        .expect("UTF-8 body")
        .replace("/vehicle/v1", "/v1")
}

/// A legacy capability doc mapped onto `/v1`, minus the paths only the legacy
/// tree mounts (the untracked `status/restart/{exec_id}` stub).
fn legacy_docs_as_v1(body: &[u8]) -> serde_json::Value {
    let mut doc: serde_json::Value = serde_json::from_str(&as_v1(body)).expect("JSON doc");
    doc["paths"]
        .as_object_mut()
        .expect("paths")
        .retain(|path, _| !path.ends_with("/status/restart/{exec_id}"));
    doc
}

/// A `/v1` doc without its `/v1`-only operations (the part read-back and
/// execute), to compare with [`legacy_docs_as_v1`].
fn v1_docs_without_v1_only(body: &[u8]) -> serde_json::Value {
    let mut doc = json(body);
    let paths = doc["paths"].as_object_mut().expect("paths");
    for (path, item) in paths.iter_mut() {
        if path.ends_with("/bulk-data/{part_id}") {
            item.as_object_mut().expect("path item").remove("get");
        }
    }
    paths.retain(|path, _| !path.ends_with("/execute"));
    doc
}

// ---------------------------------------------------------------------------
// a. version-info
// ---------------------------------------------------------------------------

#[tokio::test]
async fn version_info_lists_v1_first() {
    let server = open_server().await;
    let (status, body) = get(&server, "/version-info", None).await;
    assert_eq!(status, 200);
    let body = json(&body);
    let info = body["sovd_info"].as_array().expect("sovd_info array");
    assert_eq!(info.len(), 2);
    assert_eq!(info[0]["base_uri"], "/v1");
    assert_eq!(info[1]["base_uri"], "/vehicle/v1");
    for entry in info {
        assert_eq!(entry["version"], "1.1.0");
    }
    // The advertised order is `Tree::ALL`.
    for (entry, tree) in info.iter().zip(Tree::ALL) {
        assert_eq!(entry["base_uri"], tree.base());
    }
}

// ---------------------------------------------------------------------------
// b. identical bodies under both bases
// ---------------------------------------------------------------------------

#[tokio::test]
async fn both_bases_serve_identical_bodies() {
    let server = open_server().await;
    for rel in ["/components", "/components/ecu_a"] {
        let (s1, b1) = get(&server, &format!("/v1{rel}"), None).await;
        let (s2, b2) = get(&server, &format!("/vehicle/v1{rel}"), None).await;
        assert_eq!((s1, s2), (200, 200), "{rel}");
        assert_eq!(
            String::from_utf8(b1).unwrap(),
            as_v1(&b2),
            "{rel}: bodies differ between bases beyond the base"
        );
    }
}

/// Both bases serve the same value: `/v1` as an ISO 17978-3 `ReadValue`
/// (`{"id", "data"}`), the frozen `/vehicle/v1` as its complete `DidResponse`,
/// whose `timestamp` the data handler stamps with the read time.
#[tokio::test]
async fn both_bases_serve_the_same_data_resource() {
    let server = open_server().await;
    let (status, body) = get(&server, "/v1/components/ecu_a/data/rpm", None).await;
    assert_eq!(status, 200);
    assert_eq!(json(&body), serde_json::json!({ "id": "rpm", "data": 850 }));

    let (status, body) = get(&server, "/vehicle/v1/components/ecu_a/data/rpm", None).await;
    assert_eq!(status, 200);
    let mut body = json(&body);
    assert!(body["timestamp"].is_string(), "{body}");
    body.as_object_mut().unwrap().remove("timestamp");
    assert_eq!(
        body,
        serde_json::json!({
            "id": "rpm", "did": "", "value": 850, "unit": "rpm",
            "raw": "", "length": 0, "converted": false,
        })
    );
}

// ---------------------------------------------------------------------------
// c. capability docs public under both bases
// ---------------------------------------------------------------------------

#[tokio::test]
async fn docs_are_public_under_both_bases() {
    let server = auth_server().await;
    let mut globals = Vec::new();
    for base in BASES {
        let (status, body) = get(&server, &format!("{base}/docs"), None).await;
        assert_eq!(status, 200, "{base}/docs");
        assert_eq!(json(&body)["openapi"], "3.1.0");
        globals.push(body);
    }
    assert_eq!(
        v1_docs_without_v1_only(&globals[0]),
        legacy_docs_as_v1(&globals[1])
    );

    // Entity-scoped docs (router fallback) — public, and scoped the same way
    // under both bases.
    let mut scoped = Vec::new();
    for base in BASES {
        let (status, body) = get(&server, &format!("{base}/components/ecu_b/docs"), None).await;
        assert_eq!(status, 200, "{base}/components/ecu_b/docs");
        let body = json(&body);
        assert!(
            !body["paths"].as_object().expect("paths").is_empty(),
            "{base}: scoped doc has no paths"
        );
        scoped.push(body.to_string());
    }
    assert_eq!(
        v1_docs_without_v1_only(scoped[0].as_bytes()),
        legacy_docs_as_v1(scoped[1].as_bytes())
    );
}

// ---------------------------------------------------------------------------
// d. component-scope authorization under both bases (403 vs 401, same body)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn component_scope_enforced_identically_under_both_bases() {
    let server = auth_server().await;
    let token = Some("token-a");

    for rel in [
        "/components/ecu_b",
        "/components/ecu_b/data",
        "/components/ecu_b/data/rpm",
        "/components/ecu_b/faults",
    ] {
        let v1 = get(&server, &format!("/v1{rel}"), token).await;
        let legacy = get(&server, &format!("/vehicle/v1{rel}"), token).await;
        assert_eq!(legacy.0, 401, "{rel}: legacy base must refuse ecu_b");
        // ISO 17978-3 §5.4.4: `/v1` answers a valid-but-insufficient token
        // with 403; the legacy tree keeps 401. The body is the same.
        assert_eq!(v1.0, 403, "{rel}: /v1 must forbid ecu_b");
        assert_eq!(v1.1, legacy.1, "{rel}: error body differs between bases");
    }

    // No token at all — refused the same under both.
    let v1 = get(&server, "/v1/components/ecu_a", None).await;
    let legacy = get(&server, "/vehicle/v1/components/ecu_a", None).await;
    assert_eq!(legacy.0, 401);
    assert_eq!(v1, legacy);

    // The scoped component is reachable under both.
    for base in BASES {
        let (status, _) = get(&server, &format!("{base}/components/ecu_a"), token).await;
        assert_eq!(status, 200, "{base}/components/ecu_a");
        let (status, _) = get(&server, &format!("{base}/components/ecu_a/data/rpm"), token).await;
        assert_eq!(status, 200, "{base}/components/ecu_a/data/rpm");
    }
}

// ---------------------------------------------------------------------------
// e. GenericError fallbacks under both bases
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unknown_path_and_wrong_method_give_generic_error() {
    let server = open_server().await;
    for base in BASES {
        let (status, body) = get(&server, &format!("{base}/no-such"), None).await;
        assert_eq!(status, 404, "{base}/no-such");
        let body = json(&body);
        assert!(body["error_code"].is_string(), "{base}: {body}");
        assert!(body["message"].is_string(), "{base}: {body}");
    }

    let (status, body) = send(
        &server,
        reqwest::Method::DELETE,
        "/v1/components/ecu_a",
        None,
    )
    .await;
    assert_eq!(status, 405);
    let body = json(&body);
    assert_eq!(body["vendor_code"], "method-not-allowed", "{body}");
    assert!(body["message"].is_string(), "{body}");
}

// ---------------------------------------------------------------------------
// f. every emitted URL uses the request's base
// ---------------------------------------------------------------------------

#[tokio::test]
async fn component_list_hrefs_use_request_base() {
    let server = open_server().await;
    for base in BASES {
        let (status, body) = get(&server, &format!("{base}/components"), None).await;
        assert_eq!(status, 200, "{base}");
        let body = json(&body);
        let items = body["items"].as_array().expect("items");
        assert_eq!(items.len(), 2, "{base}: {body}");
        for item in items {
            let href = item["href"].as_str().expect("href");
            assert!(
                href.starts_with(&format!("{base}/components/")),
                "{base}: {href}"
            );
        }
    }
}

#[tokio::test]
async fn component_urls_use_request_base() {
    let server = open_server().await;
    for base in BASES {
        let (status, body) = get(&server, &format!("{base}/components/ecu_a"), None).await;
        assert_eq!(status, 200, "{base}");
        let mut found = Vec::new();
        urls(&json(&body), &mut found);
        assert!(!found.is_empty(), "{base}: component emits no URLs");
        for url in found {
            assert!(url.starts_with(&format!("{base}/")), "{base}: {url}");
        }
    }
}

#[tokio::test]
async fn locations_use_request_base() {
    let server = open_server().await;
    for (base, id) in BASES.into_iter().zip(["ecu_a", "ecu_b"]) {
        let component = format!("{base}/components/{id}");

        // Register an update: 201 + Location under the request's base.
        let resp = request(
            &server,
            reqwest::Method::POST,
            &format!("{component}/updates"),
            Some(serde_json::json!({ "id": "u1" })),
        )
        .await;
        let loc = location(&resp, 201);
        assert_eq!(loc, format!("{component}/updates/u1"), "{base}");
        let mut found = Vec::new();
        urls(&resp.json().await.expect("register body"), &mut found);
        assert!(!found.is_empty());
        for url in found {
            assert!(
                url.starts_with(&format!("{component}/updates/")),
                "{base}: {url}"
            );
        }

        // Upload a part: its href, and the bulk-data listing's, are under the base.
        let resp = reqwest::Client::new()
            .put(format!(
                "{}{component}/updates/u1/bulk-data/p1",
                server.base_url()
            ))
            .body("bytes")
            .send()
            .await
            .expect("part upload");
        assert_eq!(resp.status().as_u16(), 201, "{base}");
        let body: Value = resp.json().await.expect("part body");
        assert_eq!(body["href"], format!("{component}/updates/u1/bulk-data/p1"));
        let (status, body) = get(&server, &format!("{component}/updates/u1/bulk-data"), None).await;
        assert_eq!(status, 200, "{base}");
        let mut found = Vec::new();
        urls(&json(&body), &mut found);
        assert!(!found.is_empty());
        for url in found {
            assert!(
                url.starts_with(&format!("{component}/updates/u1/bulk-data/")),
                "{base}: {url}"
            );
        }

        // Start an operation execution: 202 + Location under the base, and
        // GET-able there.
        let resp = request(
            &server,
            reqwest::Method::POST,
            &format!("{component}/operations/selftest/executions"),
            Some(serde_json::json!({})),
        )
        .await;
        let loc = location(&resp, 202);
        assert!(
            loc.starts_with(&format!("{component}/operations/selftest/executions/")),
            "{base}: {loc}"
        );
        let (status, _) = get(&server, &loc, None).await;
        assert_eq!(status, 200, "{base}: GET {loc}");
    }
}

/// Backend-supplied hrefs (here `/bogus/x`) never reach the wire: fault and
/// operation hrefs are rebuilt under the request's tree.
#[tokio::test]
async fn backend_hrefs_rebuilt_under_request_base() {
    let server = open_server().await;
    for tree in Tree::ALL {
        let component = tree.component("ecu_a");
        for rel in ["faults", "operations", "faults/f1"] {
            let (status, body) = get(&server, &format!("{component}/{rel}"), None).await;
            assert_eq!(status, 200, "{component}/{rel}");
            let mut found = Vec::new();
            urls(&json(&body), &mut found);
            assert!(!found.is_empty(), "{component}/{rel}: no hrefs");
            for url in found {
                assert!(
                    url.starts_with(&format!("{component}/")),
                    "{component}/{rel}: {url}"
                );
            }
        }
        let (_, body) = get(&server, &format!("{component}/faults/f1"), None).await;
        assert_eq!(json(&body)["href"], format!("{component}/faults/f1"));
    }
}

#[tokio::test]
async fn capability_docs_use_request_base() {
    let server = open_server().await;
    let mut scoped = Vec::new();
    for base in BASES {
        let (status, body) = get(&server, &format!("{base}/docs"), None).await;
        assert_eq!(status, 200, "{base}/docs");
        let doc = json(&body);
        assert_eq!(doc["servers"][0]["url"], base);
        let paths = doc["paths"].as_object().expect("paths");
        assert!(!paths.is_empty());
        for key in paths.keys() {
            // `/health` and `/version-info` are root-level: served off no base.
            if key == "/health" || key == "/version-info" {
                continue;
            }
            assert!(key.starts_with(&format!("{base}/")), "{base}/docs: {key}");
        }

        let (status, body) = get(&server, &format!("{base}/components/ecu_a/docs"), None).await;
        assert_eq!(status, 200, "{base}/components/ecu_a/docs");
        let doc = json(&body);
        assert_eq!(doc["servers"][0]["url"], base);
        let paths = doc["paths"].as_object().expect("paths");
        assert!(!paths.is_empty(), "{base}: scoped doc has no paths");
        for key in paths.keys() {
            assert!(
                key.starts_with(&format!("{base}/components/ecu_a")),
                "{base} scoped: {key}"
            );
        }
        scoped.push(body);
    }
    assert_eq!(
        v1_docs_without_v1_only(&scoped[0]),
        legacy_docs_as_v1(&scoped[1])
    );
}

// ---------------------------------------------------------------------------
// g. legacy responses unchanged
// ---------------------------------------------------------------------------

/// Byte snapshots of legacy responses, recorded by running these requests on
/// the commit before tree-aware URL rendering. `/components` is compared with
/// its items sorted by id (the listing walks a `HashMap`, so its order is
/// per-process); the docs fixture holds `@@VERSION@@` for the crate version.
#[tokio::test]
async fn legacy_responses_unchanged() {
    let server = open_server().await;

    let (status, body) = get(&server, "/vehicle/v1/components", None).await;
    assert_eq!(status, 200);
    let mut body = json(&body);
    body["items"]
        .as_array_mut()
        .unwrap()
        .sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    assert_eq!(body.to_string(), LEGACY_COMPONENTS);

    let (status, body) = get(&server, "/vehicle/v1/components/ecu_a/docs", None).await;
    assert_eq!(status, 200);
    assert_eq!(
        String::from_utf8(body).unwrap(),
        include_str!("fixtures/dual_base_legacy_component_docs.json")
            .replace("@@VERSION@@", env!("CARGO_PKG_VERSION"))
    );

    let resp = request(
        &server,
        reqwest::Method::POST,
        "/vehicle/v1/components/ecu_a/updates",
        Some(serde_json::json!({ "id": "u1" })),
    )
    .await;
    assert_eq!(location(&resp, 201), LEGACY_REGISTER_LOCATION);
}

const LEGACY_COMPONENTS: &str = r#"{"items":[{"href":"/vehicle/v1/components/ecu_a","id":"ecu_a","name":"ecu_a ecu","type":"ecu"},{"href":"/vehicle/v1/components/ecu_b","id":"ecu_b","name":"ecu_b ecu","type":"ecu"}]}"#;
const LEGACY_REGISTER_LOCATION: &str = "/vehicle/v1/components/ecu_a/updates/u1";
