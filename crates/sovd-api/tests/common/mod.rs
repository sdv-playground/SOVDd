//! Shared fixtures for the sovd-api integration tests: mock backends, test
//! authorizer, and in-process `TestServer` helpers. Each test target pulls in
//! what it needs, so items unused by one target are expected.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde_json::Value;
use sovd_api::{create_router, AccessRequest, AppState, AuthError, Authorizer, ClientContext};
use sovd_client::testing::TestServer;
use sovd_core::{
    BackendError, BackendResult, Capabilities, DataValue, DiagnosticBackend, EntityInfo, Fault,
    FaultFilter, FaultSeverity, FaultsResult, FlashProgress, FlashState, FlashStatus,
    OperationExecution, OperationInfo, PackageStream, ParameterInfo, VerdictScope,
};

// ---------------------------------------------------------------------------
// Mock backend
// ---------------------------------------------------------------------------

pub struct MockBackend {
    pub info: EntityInfo,
    pub capabilities: Capabilities,
}

impl MockBackend {
    pub fn new(id: &str) -> Self {
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
        // A deliberately wrong backend href: the API must rebuild it.
        Ok(FaultsResult {
            faults: vec![Fault {
                id: "f1".to_string(),
                code: "P0101".to_string(),
                severity: FaultSeverity::default(),
                message: "Mass air flow".to_string(),
                category: None,
                first_occurrence: None,
                last_occurrence: None,
                occurrence_count: None,
                active: true,
                status: None,
                href: "/bogus/x".to_string(),
            }],
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
            href: "/bogus/x".to_string(),
        }])
    }
    async fn start_operation(&self, op: &str, _params: &[u8]) -> BackendResult<OperationExecution> {
        match op {
            "selftest" => Ok(OperationExecution::running("backend-exec", op)),
            _ => Err(BackendError::OperationNotFound(op.to_string())),
        }
    }
    /// One sub-entity, `app_a`: a plain mock with the same `selftest` op.
    async fn get_sub_entity(&self, id: &str) -> BackendResult<Arc<dyn DiagnosticBackend>> {
        match id {
            "app_a" => Ok(Arc::new(MockBackend::new(id))),
            _ => Err(BackendError::EntityNotFound(id.to_string())),
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
/// value is looked up in a fixed token→scopes table (`token-a`: `ecu_a` only,
/// `token-all`: every component, `token-none`: no component), then a component-addressed
/// route needs the matching `component:<id>` scope.
pub struct ScopedAuthorizer;

#[async_trait::async_trait]
impl Authorizer for ScopedAuthorizer {
    async fn authorize(&self, req: &AccessRequest<'_>) -> Result<ClientContext, AuthError> {
        let scopes = match req.bearer {
            Some("Bearer token-a") => vec!["component:ecu_a".to_string()],
            Some("Bearer token-all") => vec!["component:*".to_string()],
            Some("Bearer token-none") => vec![],
            Some(_) => return Err("invalid token".into()),
            None => return Err("missing Authorization header".into()),
        };
        let ctx = ClientContext {
            subject: "tester".to_string(),
            scopes,
        };
        if let Some(component) = req.component {
            if !ctx.can_access_component(component) {
                return Err(AuthError::Forbidden(format!(
                    "client not authorized for component '{component}'"
                )));
            }
        }
        Ok(ctx)
    }
}

pub fn state() -> AppState {
    let mut backends = HashMap::new();
    for id in ["ecu_a", "ecu_b"] {
        backends.insert(
            id.to_string(),
            Arc::new(MockBackend::new(id)) as Arc<dyn DiagnosticBackend>,
        );
    }
    AppState::new(backends)
}

pub async fn open_server() -> TestServer {
    TestServer::start(create_router(state()))
        .await
        .expect("test server")
}

pub async fn auth_server() -> TestServer {
    TestServer::start(create_router(
        state().with_authorizer(Arc::new(ScopedAuthorizer)),
    ))
    .await
    .expect("test server")
}

/// GET `path`, returning `(status, body bytes)`.
pub async fn get(server: &TestServer, path: &str, token: Option<&str>) -> (u16, Vec<u8>) {
    send(server, reqwest::Method::GET, path, token).await
}

pub async fn send(
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

pub fn json(body: &[u8]) -> Value {
    serde_json::from_slice(body).expect("JSON body")
}

/// Send `method path` with an optional JSON body; returns the response.
pub async fn request(
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
pub fn location(resp: &reqwest::Response, status: u16) -> String {
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
pub fn urls(value: &Value, out: &mut Vec<String>) {
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
// Mock backend: a banked executor whose `activate` lands straight in the live
// trial (`Activated`), so commit is accepted from awaiting-verdict without a
// simulated reboot
// ---------------------------------------------------------------------------

pub struct BankedBackend {
    pub info: EntityInfo,
    pub capabilities: Capabilities,
    pub scope: VerdictScope,
    pub flash_state: Mutex<FlashState>,
    pub next_id: Mutex<u64>,
}

impl BankedBackend {
    pub fn new(id: &str, scope: VerdictScope) -> Self {
        Self {
            info: EntityInfo {
                id: id.into(),
                name: format!("{id} mock"),
                entity_type: "ecu".into(),
                description: None,
                href: format!("/vehicle/v1/components/{id}"),
                status: Some("online".into()),
            },
            capabilities: Capabilities {
                software_update: true,
                ..Default::default()
            },
            scope,
            flash_state: Mutex::new(FlashState::Transferring),
            next_id: Mutex::new(0),
        }
    }
}

#[async_trait::async_trait]
impl DiagnosticBackend for BankedBackend {
    fn entity_info(&self) -> &EntityInfo {
        &self.info
    }
    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }
    fn update_shape(&self) -> &'static str {
        "banked"
    }
    fn verdict_scope(&self) -> VerdictScope {
        self.scope
    }
    async fn list_parameters(&self) -> BackendResult<Vec<ParameterInfo>> {
        Ok(Vec::new())
    }
    async fn read_data(&self, _: &[String]) -> BackendResult<Vec<DataValue>> {
        Ok(Vec::new())
    }
    async fn get_faults(&self, _: Option<&FaultFilter>) -> BackendResult<FaultsResult> {
        Ok(FaultsResult {
            faults: Vec::new(),
            status_availability_mask: None,
        })
    }
    async fn list_operations(&self) -> BackendResult<Vec<OperationInfo>> {
        Ok(Vec::new())
    }
    async fn start_operation(&self, op_id: &str, _: &[u8]) -> BackendResult<OperationExecution> {
        Err(BackendError::OperationNotFound(op_id.into()))
    }
    async fn receive_package_part(
        &self,
        _part_id: &str,
        stream: PackageStream,
        content_length: Option<u64>,
    ) -> BackendResult<String> {
        self.receive_package_stream(stream, content_length).await
    }
    async fn receive_package_stream(
        &self,
        mut stream: PackageStream,
        _content_length: Option<u64>,
    ) -> BackendResult<String> {
        use futures::StreamExt;
        while let Some(chunk) = stream.next().await {
            chunk.map_err(|e| BackendError::Internal(format!("stream: {e}")))?;
        }
        let mut counter = self.next_id.lock();
        *counter += 1;
        *self.flash_state.lock() = FlashState::AwaitingActivation;
        Ok(format!("pkg-{}", *counter))
    }
    async fn verify_part(&self, _file_id: &str, _expected_sha256: &str) -> BackendResult<()> {
        Ok(())
    }
    async fn start_flash(&self) -> BackendResult<String> {
        let mut counter = self.next_id.lock();
        *counter += 1;
        *self.flash_state.lock() = FlashState::Transferring;
        Ok(format!("xfer-{}", *counter))
    }
    async fn get_flash_status(&self, transfer_id: &str) -> BackendResult<FlashStatus> {
        Ok(FlashStatus {
            transfer_id: transfer_id.to_string(),
            package_id: "pkg".into(),
            state: *self.flash_state.lock(),
            progress: Some(FlashProgress {
                bytes_transferred: 100,
                bytes_total: 100,
                blocks_transferred: 1,
                blocks_total: 1,
                percent: 100.0,
            }),
            error: None,
        })
    }
    async fn get_activation_state(&self) -> BackendResult<sovd_core::ActivationState> {
        Ok(sovd_core::ActivationState {
            supports_rollback: true,
            state: *self.flash_state.lock(),
            active_version: None,
            previous_version: None,
            reset_kind: sovd_core::ResetKind::Local,
        })
    }
    async fn finalize_flash(&self) -> BackendResult<()> {
        *self.flash_state.lock() = FlashState::AwaitingReboot;
        Ok(())
    }
    async fn validate(&self) -> BackendResult<()> {
        *self.flash_state.lock() = FlashState::Validated;
        Ok(())
    }
    async fn activate(&self) -> BackendResult<()> {
        *self.flash_state.lock() = FlashState::Activated;
        Ok(())
    }
    async fn commit_flash(&self) -> BackendResult<()> {
        *self.flash_state.lock() = FlashState::Committed;
        Ok(())
    }
    async fn rollback_flash(&self) -> BackendResult<()> {
        *self.flash_state.lock() = FlashState::RolledBack;
        Ok(())
    }
    async fn abort_flash(&self, _transfer_id: &str) -> BackendResult<()> {
        Ok(())
    }
}
