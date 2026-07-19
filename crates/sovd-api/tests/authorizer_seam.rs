//! Authorizer-seam integration test.
//!
//! Proves SOVDd's injection seam end to end: SOVDd computes the capability a
//! route requires and the component it addresses, hands them to an **injected**
//! [`Authorizer`], and enforces that authorizer's decision.
//!
//! The CA/issuer + minter + authorizer here are **test-only scaffolding** —
//! SOVDd ships none of them (it has only the `Authorizer` trait and its built-in
//! modes). Production injects an HSM-backed authorizer from the machine-manager
//! layer; the realistic ES256 / `x5c`-against-a-pinned-CA verification path is
//! covered by the `workshop_ca` unit tests. This fixture uses a compact HS256
//! minter purely to demonstrate the *seam*, and lives in `tests/` so none of it
//! becomes part of the SOVDd surface.

use std::collections::HashMap;
use std::sync::Arc;

use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use sovd_api::{create_router, AccessRequest, AppState, Authorizer, Capability, ClientContext};
use sovd_client::testing::TestServer;
use sovd_core::{
    BackendError, BackendResult, Capabilities, DataValue, DiagnosticBackend, EntityInfo,
    FaultFilter, FaultsResult, OperationExecution, OperationInfo, ParameterInfo,
};

// ---------------------------------------------------------------------------
// Test CA / issuer + minter — vendor-side; never shipped by SOVDd.
// ---------------------------------------------------------------------------

/// The test issuer's signing key — stands in for the CA/minter trust root. A
/// real deployment uses an ES256 leaf chaining to a pinned workshop CA, or an
/// HSM-issued key; HS256 keeps this fixture compact.
const ISSUER_SECRET: &[u8] = b"test-issuer-secret-not-for-production";

#[derive(Serialize, Deserialize)]
struct TokenClaims {
    sub: String,
    exp: usize,
    /// OAuth-style space-delimited capability + component scopes.
    scope: String,
}

/// The minter: issue a capability-bearing token signed by the test issuer.
fn mint(scopes: &[&str]) -> String {
    let claims = TokenClaims {
        sub: "test-operator".to_string(),
        exp: 9_999_999_999, // year 2286 — never expires within the test
        scope: scopes.join(" "),
    };
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(ISSUER_SECRET),
    )
    .expect("mint token")
}

// ---------------------------------------------------------------------------
// The injected authorizer — vendor-side; what an HSM-backed impl would be.
// ---------------------------------------------------------------------------

struct TestAuthorizer {
    key: DecodingKey,
}

impl TestAuthorizer {
    fn new() -> Self {
        Self {
            key: DecodingKey::from_secret(ISSUER_SECRET),
        }
    }
}

/// The scope string a capability requires; `None` means component scope alone
/// is enough (a plain read).
fn capability_scope(cap: Capability) -> Option<&'static str> {
    match cap {
        Capability::DataRead => Some("data:read"),
        Capability::DataWrite => Some("data:write"),
        Capability::OperationsExecute => Some("operations:execute"),
        Capability::ModesSet => Some("modes:set"),
        Capability::UpdateTransfer => Some("updates:transfer"),
        Capability::UpdateExecute => Some("updates:execute"),
        Capability::UpdateVerdict => Some("updates:verdict"),
        Capability::ResetExecute => Some("reset:execute"),
        Capability::FactoryReset => Some("factory-reset"),
        // Bare-hyphenated on purpose: `component:admin` would collide with the
        // `component:<id>` scope namespace (see the enum doc).
        Capability::ComponentAdmin => Some("component-admin"),
        Capability::Admin => Some("admin"),
        Capability::Read => None,
    }
}

#[async_trait::async_trait]
impl Authorizer for TestAuthorizer {
    async fn authorize(&self, req: &AccessRequest<'_>) -> Result<ClientContext, String> {
        // `AccessRequest.bearer` is the full `Authorization` header value — the
        // authorizer parses it (the built-in one strips `Bearer ` internally too).
        let token = req
            .bearer
            .and_then(|h| h.strip_prefix("Bearer ").map(str::trim))
            .filter(|t| !t.is_empty())
            .ok_or("missing or malformed bearer token")?;
        let mut validation = Validation::new(Algorithm::HS256);
        validation.validate_aud = false; // the compact test minter issues no `aud`
        let data = decode::<TokenClaims>(token, &self.key, &validation)
            .map_err(|e| format!("invalid token: {e}"))?;
        let ctx = ClientContext {
            subject: data.claims.sub,
            scopes: data
                .claims
                .scope
                .split_whitespace()
                .map(String::from)
                .collect(),
        };
        // Component scope (C-031): the route addresses a specific component.
        if let Some(component) = req.component {
            if !ctx.can_access_component(component) {
                return Err(format!("token has no scope for component '{component}'"));
            }
        }
        // Capability (verb) scope — the dimension the built-in authorizer leaves
        // to an injected one (docs/design/authorization.md §5).
        if let Some(needed) = capability_scope(req.capability) {
            if !ctx.scopes.iter().any(|s| s == needed) {
                return Err(format!("token lacks the '{needed}' capability"));
            }
        }
        Ok(ctx)
    }
}

// ---------------------------------------------------------------------------
// Minimal backend so an authorized request reaches a 200.
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
    async fn read_data(&self, _ids: &[String]) -> BackendResult<Vec<DataValue>> {
        Ok(vec![])
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

async fn server_with_injected_authorizer() -> TestServer {
    let mut backends = HashMap::new();
    backends.insert(
        "vm1".to_string(),
        Arc::new(MockBackend::new("vm1")) as Arc<dyn DiagnosticBackend>,
    );
    let state = AppState::new(backends).with_authorizer(Arc::new(TestAuthorizer::new()));
    TestServer::start(create_router(state))
        .await
        .expect("test server")
}

async fn get_status(server: &TestServer, path: &str, token: Option<&str>) -> u16 {
    let mut req = reqwest::Client::new().get(format!("{}{path}", server.base_url()));
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    req.send().await.expect("request").status().as_u16()
}

// `GET .../data` → `DataRead` capability; a token with the component + the
// `data:read` capability is granted by the injected authorizer.
#[tokio::test]
async fn grants_with_component_and_capability() {
    let server = server_with_injected_authorizer().await;
    let token = mint(&["component:vm1", "data:read"]);
    assert_eq!(
        get_status(&server, "/vehicle/v1/components/vm1/data", Some(&token)).await,
        200
    );
}

// Same route, token missing `data:read` → the injected authorizer denies, so the
// capability dimension is genuinely enforced (the built-in authorizer would have
// allowed it on component scope alone).
#[tokio::test]
async fn denies_missing_capability() {
    let server = server_with_injected_authorizer().await;
    let token = mint(&["component:vm1"]);
    assert_eq!(
        get_status(&server, "/vehicle/v1/components/vm1/data", Some(&token)).await,
        401
    );
}

#[tokio::test]
async fn denies_wrong_component() {
    let server = server_with_injected_authorizer().await;
    let token = mint(&["component:other", "data:read"]);
    assert_eq!(
        get_status(&server, "/vehicle/v1/components/vm1/data", Some(&token)).await,
        401
    );
}

#[tokio::test]
async fn denies_missing_token() {
    let server = server_with_injected_authorizer().await;
    assert_eq!(
        get_status(&server, "/vehicle/v1/components/vm1/data", None).await,
        401
    );
}

// A plain component read needs no verb capability — only the component scope.
#[tokio::test]
async fn component_read_needs_only_component_scope() {
    let server = server_with_injected_authorizer().await;
    let token = mint(&["component:vm1"]);
    assert_eq!(
        get_status(&server, "/vehicle/v1/components/vm1", Some(&token)).await,
        200
    );
}

/// `ComponentAdmin` maps to no SOVDd route — the per-component admin-state op
/// (disable/enable) is a vendor route in the machine-manager layer; SOVDd owns
/// only the vocabulary. An embedder gates that route through this same seam, so
/// exercise the authorizer directly with a hand-built [`AccessRequest`].
async fn authorize_component_admin(
    authorizer: &TestAuthorizer,
    token: &str,
) -> Result<ClientContext, String> {
    let header = format!("Bearer {token}");
    // `reqwest::Method` is the same `http::Method` axum re-exports.
    let method = reqwest::Method::POST;
    let req = AccessRequest {
        bearer: Some(&header),
        method: &method,
        path: "/not-a-sovdd-route/admin-state",
        component: Some("vm1"),
        capability: Capability::ComponentAdmin,
    };
    authorizer.authorize(&req).await
}

#[tokio::test]
async fn component_admin_needs_its_bare_hyphenated_verb() {
    let authorizer = TestAuthorizer::new();

    // Component scope + the `component-admin` verb → granted.
    let granted =
        authorize_component_admin(&authorizer, &mint(&["component:vm1", "component-admin"])).await;
    assert!(granted.is_ok(), "got: {granted:?}");

    // Component scope alone → denied: the verb dimension is enforced.
    assert!(
        authorize_component_admin(&authorizer, &mint(&["component:vm1"]))
            .await
            .is_err()
    );

    // The namespace collision the hyphen avoids: `component:admin` is NOT the
    // admin verb — it is component access to an id "admin", so it neither
    // matches component vm1 nor supplies `component-admin`.
    assert!(
        authorize_component_admin(&authorizer, &mint(&["component:admin"]))
            .await
            .is_err()
    );
}
