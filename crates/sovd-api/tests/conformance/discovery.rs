//! Discovery: version-info, entity collections and capability documents.

use reqwest::Method;
use sovd_api::Tree;

use crate::common::{auth_server, get, json};
use crate::{header, send, server, OTA};

/// `MAJOR.MINOR.PATCH` with numeric parts, optionally followed by a SemVer
/// pre-release / build suffix.
fn is_semver(version: &str) -> bool {
    let core = version.split(['-', '+']).next().unwrap_or_default();
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// ISO 17978-3 §7.4.2: version-info lists every supported version (Table 37
/// `SOVDInfo`) with a SemVer `version` and its `base_uri`; `/v1` comes first.
#[tokio::test]
async fn version_info_lists_both_bases() {
    let server = server().await;
    let (status, body) = get(&server, "/version-info", None).await;
    assert_eq!(status, 200);
    let body = json(&body);
    let info = body["sovd_info"].as_array().expect("sovd_info array");
    assert_eq!(info.len(), Tree::ALL.len(), "{body}");
    assert_eq!(info[0]["base_uri"], "/v1");
    for (entry, tree) in info.iter().zip(Tree::ALL) {
        assert_eq!(entry["base_uri"], tree.base(), "{body}");
        let version = entry["version"].as_str().expect("version string");
        assert!(is_semver(version), "{tree:?}: {version}");
    }
}

/// ISO 17978-3 §7.6.2.1: the components collection returns `items[]`, each
/// with `id`, `name` and an `href` under the request's base.
#[tokio::test]
async fn components_collection_items() {
    let server = server().await;
    for tree in Tree::ALL {
        let base = tree.base();
        let (status, body) = get(&server, &format!("{base}/components"), None).await;
        assert_eq!(status, 200, "{base}");
        let body = json(&body);
        let items = body["items"].as_array().expect("items array");
        assert!(!items.is_empty(), "{base}: {body}");
        for item in items {
            assert!(item["id"].is_string(), "{base}: {item}");
            assert!(item["name"].is_string(), "{base}: {item}");
            let href = item["href"].as_str().expect("href");
            assert!(
                href.starts_with(&format!("{base}/components/")),
                "{base}: {href}"
            );
        }
    }
}

/// ISO 17978-3 §7.6.3 Table 53: `GET {base}/` returns the SOVD server's
/// capability document with `id`, `name` and references to its collections —
/// only those the server itself serves at root level (C1: entity collections
/// are not aggregated), plus the vendor `x-update.update-targets` list.
#[tokio::test]
async fn root_capability_document() {
    let server = server().await;
    for tree in Tree::ALL {
        let base = tree.base();
        let (status, body) = get(&server, &format!("{base}/"), None).await;
        assert_eq!(status, 200, "{base}/");
        let body = json(&body);
        assert!(body["id"].is_string(), "{base}: {body}");
        assert!(body["name"].is_string(), "{base}: {body}");
        assert_eq!(body["components"], format!("{base}/components"), "{body}");
        assert_eq!(
            body["x-update"]["update-targets"],
            serde_json::json!([OTA]),
            "{base}: {body}"
        );
        for key in ["data", "faults", "operations"] {
            assert!(body.get(key).is_none(), "{base}: root has `{key}`: {body}");
        }
    }

    // §5.4.4: the root document is a protected resource.
    let server = auth_server().await;
    for tree in Tree::ALL {
        let (status, _) = get(&server, &format!("{}/", tree.base()), None).await;
        assert_eq!(status, 401, "{}/", tree.base());
    }
}

/// ISO 17978-3 §7.6.3 Table 53 (C1): a component's capability document
/// references the `updates` and `bulk-data` collections it supports, and
/// omits them when it does not.
#[tokio::test]
async fn component_lists_updates_and_bulk_data() {
    let server = server().await;
    for tree in Tree::ALL {
        let component = tree.component(OTA);
        let (status, body) = get(&server, &component, None).await;
        assert_eq!(status, 200, "{component}");
        let body = json(&body);
        assert_eq!(body["updates"], format!("{component}/updates"), "{body}");
        assert_eq!(
            body["bulk-data"],
            format!("{component}/bulk-data"),
            "{body}"
        );

        let component = tree.component("ecu_a");
        let (status, body) = get(&server, &component, None).await;
        assert_eq!(status, 200, "{component}");
        let body = json(&body);
        for key in ["updates", "bulk-data"] {
            assert!(body.get(key).is_none(), "{component} has `{key}`: {body}");
        }
    }
}

/// ISO 17978-3 §5.2.1: the server supports `OPTIONS` on each resource and
/// returns the supported methods in `Allow`.
#[tokio::test]
async fn options_returns_allow() {
    let server = server().await;
    for tree in Tree::ALL {
        let path = format!("{}/components", tree.base());
        let resp = send(&server, Method::OPTIONS, &path, &[], None).await;
        assert!(resp.status().is_success(), "{path}: {}", resp.status());
        let allow = header(&resp, "allow").expect("Allow header");
        assert!(allow.contains("GET"), "{path}: {allow}");
    }
}
