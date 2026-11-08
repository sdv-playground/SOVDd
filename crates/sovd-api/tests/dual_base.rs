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
//! Same in-process `TestServer` + mock backend pattern as `authorizer_seam.rs`.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde_json::Value;
use sovd_api::{create_router, AccessRequest, AppState, Authorizer, ClientContext, Tree};
use sovd_client::testing::TestServer;
use sovd_core::{
    BackendError, BackendResult, Capabilities, DataValue, DiagnosticBackend, EntityInfo,
    FaultFilter, FaultsResult, OperationExecution, OperationInfo, PackageStream, ParameterInfo,
};

const BASES: [&str; 2] = ["/v1", "/vehicle/v1"];

// ---------------------------------------------------------------------------
// Mock backend
// ---------------------------------------------------------------------------

struct MockBackend {
    info: EntityInfo,
    capabilities: Capabilities,
}

impl MockBackend {
    fn new(id: &str) -> Self {
        Self {
            info: EntityInfo {
                id: id.to_string(),
                name: format!("{id} ecu"),
                entity_type: "ecu".to_string(),
                description: None,
                href: format!("/vehicle/v1/components/{id}"),
                status: Some("online".to_string()),
            },
            capabilities: Capabilities::default(),
        }
    }
}

#[async_trait::async_trait]
impl DiagnosticBackend for MockBackend {
    fn entity_info(&self) -> &EntityInfo {
        &self.info
    }
    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }
    async fn list_parameters(&self) -> BackendResult<Vec<ParameterInfo>> {
        Ok(vec![])
    }
    async fn read_data(&self, ids: &[String]) -> BackendResult<Vec<DataValue>> {
        // A fixed timestamp so the backend side is deterministic.
        let ts: DateTime<Utc> = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        Ok(ids
            .iter()
            .filter(|id| *id == "rpm")
            .map(|id| {
                let mut dv = DataValue::new(id.clone(), "Engine RPM", serde_json::json!(850))
                    .with_unit("rpm");
                dv.timestamp = ts;
                dv
            })
            .collect())
    }
    async fn get_faults(&self, _filter: Option<&FaultFilter>) -> BackendResult<FaultsResult> {
        Ok(FaultsResult {
            faults: vec![],
            status_availability_mask: None,
        })
    }
    async fn list_operations(&self) -> BackendResult<Vec<OperationInfo>> {
        Ok(vec![OperationInfo {
            id: "selftest".to_string(),
            name: "Self test".to_string(),
            description: None,
            parameters: vec![],
            requires_security: false,
            security_level: 0,
            href: String::new(),
        }])
    }
    async fn start_operation(&self, op: &str, _params: &[u8]) -> BackendResult<OperationExecution> {
        match op {
            "selftest" => Ok(OperationExecution::running("backend-exec", op)),
            _ => Err(BackendError::OperationNotFound(op.to_string())),
        }
    }
    async fn receive_package_part(
        &self,
        part_id: &str,
        _stream: PackageStream,
        _content_length: Option<u64>,
    ) -> BackendResult<String> {
        Ok(format!("file-{part_id}"))
    }
}

/// A test authorizer with the built-in component-scope decision: the bearer
/// value is looked up in a fixed token→scopes table, then a component-addressed
/// route needs the matching `component:<id>` scope.
struct ScopedAuthorizer;

#[async_trait::async_trait]
impl Authorizer for ScopedAuthorizer {
    async fn authorize(&self, req: &AccessRequest<'_>) -> Result<ClientContext, String> {
        let scopes = match req.bearer {
            Some("Bearer token-a") => vec!["component:ecu_a".to_string()],
            Some(_) => return Err("invalid token".to_string()),
            None => return Err("missing Authorization header".to_string()),
        };
        let ctx = ClientContext {
            subject: "tester".to_string(),
            scopes,
        };
        if let Some(component) = req.component {
            if !ctx.can_access_component(component) {
                return Err(format!("client not authorized for component '{component}'"));
            }
        }
        Ok(ctx)
    }
}

fn state() -> AppState {
    let mut backends = HashMap::new();
    for id in ["ecu_a", "ecu_b"] {
        backends.insert(
            id.to_string(),
            Arc::new(MockBackend::new(id)) as Arc<dyn DiagnosticBackend>,
        );
    }
    AppState::new(backends)
}

async fn open_server() -> TestServer {
    TestServer::start(create_router(state()))
        .await
        .expect("test server")
}

async fn auth_server() -> TestServer {
    TestServer::start(create_router(
        state().with_authorizer(Arc::new(ScopedAuthorizer)),
    ))
    .await
    .expect("test server")
}

/// GET `path`, returning `(status, body bytes)`.
async fn get(server: &TestServer, path: &str, token: Option<&str>) -> (u16, Vec<u8>) {
    send(server, reqwest::Method::GET, path, token).await
}

async fn send(
    server: &TestServer,
    method: reqwest::Method,
    path: &str,
    token: Option<&str>,
) -> (u16, Vec<u8>) {
    let mut req = reqwest::Client::new().request(method, format!("{}{path}", server.base_url()));
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await.expect("request");
    let status = resp.status().as_u16();
    (status, resp.bytes().await.expect("body").to_vec())
}

fn json(body: &[u8]) -> Value {
    serde_json::from_slice(body).expect("JSON body")
}

/// `body` with every `/vehicle/v1` replaced by `/v1` — a legacy body mapped
/// onto the `/v1` tree.
fn as_v1(body: &[u8]) -> String {
    String::from_utf8(body.to_vec())
        .expect("UTF-8 body")
        .replace("/vehicle/v1", "/v1")
}

/// Send `method path` with an optional JSON body; returns the response.
async fn request(
    server: &TestServer,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> reqwest::Response {
    let mut req = reqwest::Client::new().request(method, format!("{}{path}", server.base_url()));
    if let Some(body) = body {
        req = req.json(&body);
    }
    req.send().await.expect("request")
}

/// The `Location` header of `resp`, checking its status first.
fn location(resp: &reqwest::Response, status: u16) -> String {
    assert_eq!(resp.status().as_u16(), status, "{}", resp.url());
    resp.headers()
        .get(reqwest::header::LOCATION)
        .expect("Location header")
        .to_str()
        .expect("ASCII Location")
        .to_string()
}

/// Every URL `value` emits: string members keyed `href` / `*_href`, plus any
/// other string member that is an absolute path.
fn urls(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, member) in map {
                match member {
                    Value::String(s) if key == "href" || key.ends_with("_href") => {
                        out.push(s.clone())
                    }
                    Value::String(s) if s.starts_with('/') => out.push(s.clone()),
                    _ => urls(member, out),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| urls(item, out)),
        _ => {}
    }
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

/// The data handler stamps `timestamp` with the read time, so the bodies are
/// compared with that one member removed.
#[tokio::test]
async fn both_bases_serve_identical_data_resource() {
    let server = open_server().await;
    let mut bodies = Vec::new();
    for base in BASES {
        let (status, body) = get(&server, &format!("{base}/components/ecu_a/data/rpm"), None).await;
        assert_eq!(status, 200, "{base}");
        let mut body = json(&body);
        assert!(body["timestamp"].is_string(), "{base}: {body}");
        body.as_object_mut().unwrap().remove("timestamp");
        bodies.push(body);
    }
    assert_eq!(bodies[0], bodies[1]);
    assert_eq!(bodies[0]["value"], 850);
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
        String::from_utf8(globals[0].clone()).unwrap(),
        as_v1(&globals[1])
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
    assert_eq!(scoped[0], scoped[1].replace("/vehicle/v1", "/v1"));
}

// ---------------------------------------------------------------------------
// d. component-scope authorization identical under both bases
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
        assert_eq!(v1.0, legacy.0, "{rel}: status differs between bases");
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
        String::from_utf8(scoped[0].clone()).unwrap(),
        as_v1(&scoped[1])
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
