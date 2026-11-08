//! Dual base path — the entity API is served under both `/v1` and
//! `/vehicle/v1` over one shared `AppState`.
//!
//! Covers: `/version-info` advertises `/v1` first; the two bases return the
//! same bodies; the capability docs are public under both; the component-scope
//! authorization decision is identical under both (the security property); and
//! unknown paths / wrong methods get the `GenericError` body under both.
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
    FaultFilter, FaultsResult, OperationExecution, OperationInfo, ParameterInfo,
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
        Ok(vec![])
    }
    async fn start_operation(&self, op: &str, _params: &[u8]) -> BackendResult<OperationExecution> {
        Err(BackendError::OperationNotFound(op.to_string()))
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
        assert_eq!(b1, b2, "{rel}: bodies differ between bases");
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
    assert_eq!(globals[0], globals[1]);

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
        scoped.push(body);
    }
    assert_eq!(scoped[0], scoped[1]);
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
