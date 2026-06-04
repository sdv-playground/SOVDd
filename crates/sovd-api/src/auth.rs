//! Client→SOVDd authentication — JWT-bearer validation.
//!
//! SOVDd validates an incoming `Authorization: Bearer <JWT>` against a
//! configured set of trusted issuers (OIDC), or a static dev token, or
//! nothing (disabled). It **never issues tokens** and never talks to an
//! HSM — "online (cloud IdP)" vs "offline (workshop)" is purely *which
//! issuers* sit in the trusted set, not a code difference here.
//!
//! Conformance: ISO 17978-3 C-030 / C-032 (authentication). Per-client
//! authorization / resource filtering (C-031) builds on the
//! [`ClientContext`] injected here and lands in a follow-up increment.
//!
//! See `ARCHITECTURE.md` §13 and `tasks/sovdd-auth-slice.md`.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header::AUTHORIZATION, Method};
use axum::middleware::Next;
use axum::response::Response;
use jsonwebtoken::{decode, decode_header, jwk::JwkSet, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use tokio::sync::RwLock;

use crate::error::ApiError;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Configuration — deserialized from the `[server.auth]` TOML section.
// ---------------------------------------------------------------------------

/// Authentication configuration (`[server.auth]`).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct AuthConfig {
    #[serde(default)]
    pub mode: AuthMode,
    /// Required when `mode = "static"`. Dev/CI only.
    #[serde(default)]
    pub static_token: Option<String>,
    /// Trusted OIDC issuers (`mode = "oidc"`). Multi-issuer from day one so
    /// the offline workshop-token issuer is purely additive later.
    #[serde(default)]
    pub issuers: Vec<IssuerConfig>,
    /// Permit serving authenticated requests over plain HTTP (no `[server.tls]`).
    /// Default false: the server refuses to start so bearer tokens never cross
    /// the wire in cleartext by accident. Set true only for loopback dev/CI.
    #[serde(default)]
    pub allow_insecure_transport: bool,
    /// `mode = "workshop-ca"`: path to the pinned workshop-CA cert bundle (PEM).
    #[serde(default)]
    pub ca_cert: Option<String>,
    /// `mode = "workshop-ca"`: this device's id — the expected token `aud`.
    #[serde(default)]
    pub device_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AuthMode {
    /// No authentication — the surface is open. Default, so dev/sim and the
    /// no-config mock server keep working unchanged.
    #[default]
    Disabled,
    /// Single shared bearer token compared verbatim. Dev/CI only.
    Static,
    /// Validate JWTs against one or more OIDC issuers' JWKS.
    Oidc,
    /// Validate delegated-cert (`x5c`) JWTs against a pinned workshop CA — the
    /// offline/workshop path. Requires `ca_cert` + `device_id`.
    #[serde(rename = "workshop-ca")]
    WorkshopCa,
}

/// A trusted OIDC issuer.
#[derive(Clone, Debug, Deserialize)]
pub struct IssuerConfig {
    /// Issuer URL; its `/.well-known/openid-configuration` is fetched to
    /// discover the JWKS endpoint.
    pub issuer: String,
    /// Expected `aud` claim for tokens minted by this issuer.
    pub audience: String,
}

// ---------------------------------------------------------------------------
// Per-request identity — injected into request extensions, consumed by the
// C-031 scope-enforcement increment.
// ---------------------------------------------------------------------------

/// Validated caller identity attached to each authenticated request.
#[derive(Clone, Debug)]
pub struct ClientContext {
    pub subject: String,
    pub scopes: Vec<String>,
}

impl ClientContext {
    fn anonymous() -> Self {
        Self {
            subject: "anonymous".to_string(),
            scopes: Vec::new(),
        }
    }

    /// Per-component coarse authorization (C-031): the caller may access
    /// `component_id` iff a scope of `component:<id>` or `component:*` is
    /// present. Scope is at the top-level component granularity — sub-entities
    /// inherit their parent component's grant.
    pub fn can_access_component(&self, component_id: &str) -> bool {
        self.scopes.iter().any(|s| {
            s == "component:*"
                || s.strip_prefix("component:")
                    .is_some_and(|c| c == component_id)
        })
    }

    /// Server-admin authorization (`/admin/*` — mutates the shared DidStore,
    /// which changes how every component's data is parsed). Requires an explicit
    /// `admin:*` / `admin` scope; ordinary `component:*` access does NOT grant it.
    pub fn can_admin(&self) -> bool {
        self.scopes.iter().any(|s| s == "admin:*" || s == "admin")
    }
}

// ---------------------------------------------------------------------------
// Runtime validator — built once at startup, shared via `AppState`.
// ---------------------------------------------------------------------------

/// Built once from [`AuthConfig`] and shared via [`AppState`].
pub struct AuthContext {
    validator: Validator,
}

enum Validator {
    Disabled,
    Static {
        token: String,
    },
    Oidc {
        manager: Arc<JwksManager>,
    },
    WorkshopCa {
        validator: crate::workshop_ca::WorkshopCaValidator,
    },
    /// Test-only: always authenticates, with a fixed scope set.
    #[cfg(test)]
    TestScoped {
        scopes: Vec<String>,
    },
}

impl Default for AuthContext {
    fn default() -> Self {
        Self {
            validator: Validator::Disabled,
        }
    }
}

impl AuthContext {
    /// A disabled context (open surface).
    pub fn disabled() -> Self {
        Self::default()
    }

    /// Test-only: a context that authenticates any request with fixed scopes.
    #[cfg(test)]
    pub(crate) fn test_scoped(scopes: Vec<&str>) -> Self {
        Self {
            validator: Validator::TestScoped {
                scopes: scopes.into_iter().map(String::from).collect(),
            },
        }
    }

    /// True when the surface is unauthenticated.
    pub fn is_open(&self) -> bool {
        matches!(self.validator, Validator::Disabled)
    }

    /// Build from config. For OIDC this performs JWKS discovery for every
    /// issuer (network I/O) and spawns a background refresh task. `Err`
    /// carries a human-readable misconfiguration / unreachable-issuer reason.
    pub async fn from_config(cfg: AuthConfig) -> Result<Self, String> {
        let validator = match cfg.mode {
            AuthMode::Disabled => Validator::Disabled,
            AuthMode::Static => {
                let token = cfg
                    .static_token
                    .filter(|t| !t.is_empty())
                    .ok_or("auth.mode = \"static\" requires a non-empty auth.static_token")?;
                Validator::Static { token }
            }
            AuthMode::Oidc => {
                if cfg.issuers.is_empty() {
                    return Err(
                        "auth.mode = \"oidc\" requires at least one [[server.auth.issuers]]"
                            .to_string(),
                    );
                }
                let client = reqwest::Client::new();
                let mut providers = Vec::with_capacity(cfg.issuers.len());
                for issuer in cfg.issuers {
                    let (jwks_uri, jwks) = discover_jwks(&client, &issuer.issuer).await?;
                    tracing::info!(
                        issuer = %issuer.issuer,
                        keys = jwks.keys.len(),
                        "Loaded JWKS for trusted issuer"
                    );
                    providers.push(ProviderState {
                        issuer: issuer.issuer,
                        audience: issuer.audience,
                        jwks: RwLock::new(jwks),
                        jwks_uri,
                        client: client.clone(),
                    });
                }
                let manager = Arc::new(JwksManager { providers });
                spawn_jwks_refresh(manager.clone());
                Validator::Oidc { manager }
            }
            AuthMode::WorkshopCa => {
                let ca_path = cfg
                    .ca_cert
                    .filter(|p| !p.is_empty())
                    .ok_or("auth.mode = \"workshop-ca\" requires auth.ca_cert (PEM path)")?;
                let device_id = cfg
                    .device_id
                    .filter(|d| !d.is_empty())
                    .ok_or("auth.mode = \"workshop-ca\" requires auth.device_id")?;
                let ca_pem = std::fs::read_to_string(&ca_path)
                    .map_err(|e| format!("read auth.ca_cert {ca_path}: {e}"))?;
                let validator =
                    crate::workshop_ca::WorkshopCaValidator::from_pem(&ca_pem, &device_id)?;
                Validator::WorkshopCa { validator }
            }
        };
        Ok(Self { validator })
    }

    /// Validate the `Authorization` header value, returning the caller
    /// identity. `Err` carries a human-readable reason (surfaced as 401).
    pub async fn authenticate(&self, header: Option<&str>) -> Result<ClientContext, String> {
        match &self.validator {
            Validator::Disabled => Ok(ClientContext::anonymous()),
            Validator::Static { token } => {
                let raw = bearer(header)?;
                if raw == token {
                    Ok(ClientContext {
                        subject: "static".to_string(),
                        // A static dev token is full-access (components + admin) —
                        // there is no scope info to narrow it.
                        scopes: vec!["component:*".to_string(), "admin:*".to_string()],
                    })
                } else {
                    Err("invalid bearer token".to_string())
                }
            }
            Validator::Oidc { manager } => {
                let raw = bearer(header)?;
                let claims = manager.validate_token(raw).await?;
                Ok(ClientContext {
                    subject: claims.sub.clone(),
                    scopes: claims.into_scopes(),
                })
            }
            Validator::WorkshopCa { validator } => {
                let raw = bearer(header)?;
                let (subject, scopes) = validator.validate(raw)?;
                Ok(ClientContext { subject, scopes })
            }
            #[cfg(test)]
            Validator::TestScoped { scopes } => Ok(ClientContext {
                subject: "test".to_string(),
                scopes: scopes.clone(),
            }),
        }
    }
}

/// Extract the raw token from an `Authorization: Bearer <token>` header value.
fn bearer(header: Option<&str>) -> Result<&str, String> {
    header
        .ok_or("missing Authorization header")?
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "malformed Authorization header (expected \"Bearer <token>\")".to_string())
}

// ---------------------------------------------------------------------------
// JWKS management (OIDC) — lifted from SOVD-security-helper.
// ---------------------------------------------------------------------------

struct ProviderState {
    issuer: String,
    audience: String,
    jwks: RwLock<JwkSet>,
    jwks_uri: String,
    client: reqwest::Client,
}

struct JwksManager {
    providers: Vec<ProviderState>,
}

/// Claims SOVDd extracts. `aud`/`exp`/`iss` are *validated* by jsonwebtoken
/// (see [`JwksManager::validate_token`]); `sub` and the scope claim are read.
#[derive(Debug, Deserialize)]
struct Claims {
    sub: String,
    /// OAuth-style space-delimited scopes.
    #[serde(default)]
    scope: Option<String>,
    /// Alternative array form.
    #[serde(default)]
    scopes: Option<Vec<String>>,
}

impl Claims {
    fn into_scopes(self) -> Vec<String> {
        if let Some(arr) = self.scopes {
            arr
        } else if let Some(s) = self.scope {
            s.split_whitespace().map(str::to_string).collect()
        } else {
            Vec::new()
        }
    }
}

/// Accepted JWT signature algorithms — asymmetric only (no symmetric `HS*`, so a
/// public verification key can never be misused as an HMAC secret). Pinned at the
/// app layer rather than trusting the token header's `alg` (alg-confusion defence).
const ASYMMETRIC_ALGS: &[Algorithm] = &[
    Algorithm::RS256,
    Algorithm::RS384,
    Algorithm::RS512,
    Algorithm::PS256,
    Algorithm::PS384,
    Algorithm::PS512,
    Algorithm::ES256,
    Algorithm::ES384,
    Algorithm::EdDSA,
];

impl JwksManager {
    /// Validate a JWT against all trusted issuers: match `kid`, then verify
    /// signature, expiry, audience, and issuer.
    async fn validate_token(&self, raw: &str) -> Result<Claims, String> {
        let header = decode_header(raw).map_err(|e| format!("invalid JWT header: {e}"))?;
        let kid = header
            .kid
            .as_deref()
            .ok_or("JWT missing 'kid' header claim")?;

        for provider in &self.providers {
            let jwks = provider.jwks.read().await;
            let Some(jwk) = jwks.find(kid) else { continue };

            let key = DecodingKey::from_jwk(jwk)
                .map_err(|e| format!("failed to build decoding key from JWK: {e}"))?;
            // Pin accepted algorithms (asymmetric only) at the app layer instead
            // of trusting the token header's `alg` — defence-in-depth against
            // algorithm-confusion. Also enforce nbf (not-before).
            let mut validation = Validation::new(Algorithm::ES256);
            validation.algorithms = ASYMMETRIC_ALGS.to_vec();
            validation.set_audience(&[&provider.audience]);
            validation.set_issuer(&[&provider.issuer]);
            validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
            validation.validate_nbf = true;

            let data = decode::<Claims>(raw, &key, &validation)
                .map_err(|e| format!("JWT validation failed: {e}"))?;
            return Ok(data.claims);
        }

        Err(format!("no trusted issuer has a key for kid '{kid}'"))
    }
}

/// OIDC discovery document (only the field we need).
#[derive(Deserialize)]
struct OidcDiscovery {
    jwks_uri: String,
}

/// Fetch a provider's OIDC discovery document and its JWKS.
async fn discover_jwks(client: &reqwest::Client, issuer: &str) -> Result<(String, JwkSet), String> {
    let discovery_url = format!(
        "{}/.well-known/openid-configuration",
        issuer.trim_end_matches('/')
    );
    let discovery: OidcDiscovery = client
        .get(&discovery_url)
        .send()
        .await
        .map_err(|e| format!("failed to fetch OIDC discovery from {discovery_url}: {e}"))?
        .error_for_status()
        .map_err(|e| format!("OIDC discovery {discovery_url} returned an error: {e}"))?
        .json()
        .await
        .map_err(|e| format!("failed to parse OIDC discovery from {discovery_url}: {e}"))?;

    let jwks: JwkSet = client
        .get(&discovery.jwks_uri)
        .send()
        .await
        .map_err(|e| format!("failed to fetch JWKS from {}: {e}", discovery.jwks_uri))?
        .error_for_status()
        .map_err(|e| {
            format!(
                "JWKS endpoint {} returned an error: {e}",
                discovery.jwks_uri
            )
        })?
        .json()
        .await
        .map_err(|e| format!("failed to parse JWKS from {}: {e}", discovery.jwks_uri))?;

    Ok((discovery.jwks_uri, jwks))
}

/// Refresh every provider's JWKS hourly so issuer key rotation is picked up.
fn spawn_jwks_refresh(manager: Arc<JwksManager>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60 * 60));
        interval.tick().await; // skip the immediate first tick (fetched at startup)
        loop {
            interval.tick().await;
            for provider in &manager.providers {
                match provider.client.get(&provider.jwks_uri).send().await {
                    Ok(resp) => match resp.json::<JwkSet>().await {
                        Ok(new_jwks) => {
                            let count = new_jwks.keys.len();
                            *provider.jwks.write().await = new_jwks;
                            tracing::debug!(issuer = %provider.issuer, keys = count, "refreshed JWKS");
                        }
                        Err(e) => tracing::warn!(
                            issuer = %provider.issuer, error = %e,
                            "failed to parse refreshed JWKS"
                        ),
                    },
                    Err(e) => tracing::warn!(
                        issuer = %provider.issuer, error = %e,
                        "failed to fetch refreshed JWKS"
                    ),
                }
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Middleware.
// ---------------------------------------------------------------------------

/// Axum middleware that enforces JWT-bearer auth on protected routes.
///
/// CORS preflight and public resources (health, version-info, capability
/// docs, `.well-known/*`) pass through unauthenticated, per ISO 17978-3
/// §5.4.4. On success the validated [`ClientContext`] is inserted into the
/// request extensions for downstream handlers (C-031). A no-op when auth is
/// disabled.
pub async fn require_auth(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let auth = state.auth();
    if auth.is_open() || req.method() == Method::OPTIONS || is_public_path(req.uri().path()) {
        return Ok(next.run(req).await);
    }

    // Own the header value so the immutable borrow of `req` ends before we
    // mutate its extensions below.
    let header = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    let ctx = auth
        .authenticate(header.as_deref())
        .await
        .map_err(ApiError::Unauthorized)?;

    // Authorization (C-030 / C-031). Component-scoped paths require the matching
    // `component:<id>` scope; the server-admin surface requires an `admin` scope.
    let path = req.uri().path().to_owned();
    if let Some(component_id) = component_in_path(&path) {
        if !ctx.can_access_component(component_id) {
            return Err(ApiError::Unauthorized(format!(
                "client not authorized for component '{component_id}'"
            )));
        }
    } else if path.starts_with("/admin/") && !ctx.can_admin() {
        return Err(ApiError::Unauthorized(
            "client not authorized for /admin (requires an admin scope)".to_string(),
        ));
    }

    req.extensions_mut().insert(ctx);
    Ok(next.run(req).await)
}

/// Resources reachable without authentication (ISO 17978-3 §5.4.4).
fn is_public_path(path: &str) -> bool {
    matches!(path, "/health" | "/version-info" | "/vehicle/v1/docs")
        || path.ends_with("/docs")
        || path.starts_with("/.well-known/")
}

/// The top-level component id addressed by `path`, if any
/// (`/vehicle/v1/components/{id}[/...]`). `None` for the collection itself
/// (`/vehicle/v1/components`) and non-component paths — the collection is
/// scope-filtered in its handler instead (C-031 non-leakage).
fn component_in_path(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/vehicle/v1/components/")?;
    let id = rest.split('/').next().unwrap_or("");
    (!id.is_empty()).then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn disabled_allows_without_token() {
        let ctx = AuthContext::disabled();
        assert!(ctx.is_open());
        assert!(ctx.authenticate(None).await.is_ok());
    }

    #[tokio::test]
    async fn static_token_accept_and_reject() {
        let ctx = AuthContext::from_config(AuthConfig {
            mode: AuthMode::Static,
            static_token: Some("s3cret".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
        assert!(!ctx.is_open());
        let granted = ctx.authenticate(Some("Bearer s3cret")).await.unwrap();
        assert!(granted.can_access_component("any_component")); // static = full access
        assert!(granted.can_admin()); // ...including the /admin surface
        assert!(ctx.authenticate(Some("Bearer nope")).await.is_err());
        assert!(ctx.authenticate(None).await.is_err());
        assert!(ctx.authenticate(Some("Basic s3cret")).await.is_err());
    }

    #[tokio::test]
    async fn static_mode_requires_a_token() {
        let err = AuthContext::from_config(AuthConfig {
            mode: AuthMode::Static,
            static_token: None,
            ..Default::default()
        })
        .await
        .err()
        .unwrap();
        assert!(err.contains("static_token"), "got: {err}");
    }

    #[tokio::test]
    async fn oidc_mode_requires_an_issuer() {
        let err = AuthContext::from_config(AuthConfig {
            mode: AuthMode::Oidc,
            static_token: None,
            ..Default::default()
        })
        .await
        .err()
        .unwrap();
        assert!(err.contains("issuers"), "got: {err}");
    }

    #[test]
    fn public_paths_recognized() {
        assert!(is_public_path("/health"));
        assert!(is_public_path("/version-info"));
        assert!(is_public_path("/vehicle/v1/docs"));
        assert!(is_public_path("/vehicle/v1/components/engine_ecu/docs"));
        assert!(is_public_path("/.well-known/sovd-extensions"));
        assert!(!is_public_path(
            "/vehicle/v1/components/engine_ecu/data/rpm"
        ));
        assert!(!is_public_path("/vehicle/v1/components"));
    }

    #[test]
    fn bearer_parsing() {
        assert_eq!(bearer(Some("Bearer abc")).unwrap(), "abc");
        assert_eq!(bearer(Some("Bearer  abc  ")).unwrap(), "abc");
        assert!(bearer(None).is_err());
        assert!(bearer(Some("abc")).is_err());
        assert!(bearer(Some("Bearer ")).is_err());
    }

    #[test]
    fn scopes_from_claims() {
        let c = Claims {
            sub: "s".into(),
            scope: Some("read write admin".into()),
            scopes: None,
        };
        assert_eq!(c.into_scopes(), vec!["read", "write", "admin"]);

        let c = Claims {
            sub: "s".into(),
            scope: None,
            scopes: Some(vec!["x".into()]),
        };
        assert_eq!(c.into_scopes(), vec!["x"]);

        let c = Claims {
            sub: "s".into(),
            scope: None,
            scopes: None,
        };
        assert!(c.into_scopes().is_empty());
    }

    #[test]
    fn component_scope_checks() {
        let ctx = ClientContext {
            subject: "x".into(),
            scopes: vec!["component:engine_ecu".into(), "component:trans".into()],
        };
        assert!(ctx.can_access_component("engine_ecu"));
        assert!(ctx.can_access_component("trans"));
        assert!(!ctx.can_access_component("body"));

        let all = ClientContext {
            subject: "x".into(),
            scopes: vec!["component:*".into()],
        };
        assert!(all.can_access_component("anything"));

        let none = ClientContext {
            subject: "x".into(),
            scopes: Vec::new(),
        };
        assert!(!none.can_access_component("engine_ecu"));
    }

    #[test]
    fn admin_scope_required_and_distinct_from_component() {
        let comp = ClientContext {
            subject: "x".into(),
            scopes: vec!["component:*".into()],
        };
        assert!(!comp.can_admin(), "component:* must NOT grant admin");

        let adm = ClientContext {
            subject: "x".into(),
            scopes: vec!["admin:*".into()],
        };
        assert!(adm.can_admin());
        assert!(
            !adm.can_access_component("engine_ecu"),
            "admin:* is not component access"
        );
    }

    #[test]
    fn component_in_path_extraction() {
        assert_eq!(
            component_in_path("/vehicle/v1/components/engine_ecu"),
            Some("engine_ecu")
        );
        assert_eq!(
            component_in_path("/vehicle/v1/components/engine_ecu/data/rpm"),
            Some("engine_ecu")
        );
        assert_eq!(
            component_in_path("/vehicle/v1/components/gw/apps/child/data"),
            Some("gw")
        );
        assert_eq!(component_in_path("/vehicle/v1/components"), None);
        assert_eq!(component_in_path("/vehicle/v1/components/"), None);
        assert_eq!(component_in_path("/admin/definitions"), None);
        assert_eq!(component_in_path("/health"), None);
    }

    // --- router-level enforcement + enumeration filtering (C-031) ---

    struct MockBackend {
        info: sovd_core::EntityInfo,
        caps: sovd_core::Capabilities,
    }

    #[async_trait::async_trait]
    impl sovd_core::DiagnosticBackend for MockBackend {
        fn entity_info(&self) -> &sovd_core::EntityInfo {
            &self.info
        }
        fn capabilities(&self) -> &sovd_core::Capabilities {
            &self.caps
        }
        // The remaining required methods are never exercised by these tests
        // (only entity_info/capabilities are touched by list/get handlers).
        async fn list_parameters(&self) -> sovd_core::BackendResult<Vec<sovd_core::ParameterInfo>> {
            Err(sovd_core::BackendError::NotSupported("mock".to_string()))
        }
        async fn read_data(
            &self,
            _param_ids: &[String],
        ) -> sovd_core::BackendResult<Vec<sovd_core::DataValue>> {
            Err(sovd_core::BackendError::NotSupported("mock".to_string()))
        }
        async fn get_faults(
            &self,
            _filter: Option<&sovd_core::FaultFilter>,
        ) -> sovd_core::BackendResult<sovd_core::FaultsResult> {
            Err(sovd_core::BackendError::NotSupported("mock".to_string()))
        }
        async fn list_operations(&self) -> sovd_core::BackendResult<Vec<sovd_core::OperationInfo>> {
            Err(sovd_core::BackendError::NotSupported("mock".to_string()))
        }
        async fn start_operation(
            &self,
            _op_id: &str,
            _params: &[u8],
        ) -> sovd_core::BackendResult<sovd_core::OperationExecution> {
            Err(sovd_core::BackendError::NotSupported("mock".to_string()))
        }
    }

    fn mock(id: &str) -> std::sync::Arc<dyn sovd_core::DiagnosticBackend> {
        std::sync::Arc::new(MockBackend {
            info: sovd_core::EntityInfo {
                id: id.to_string(),
                name: id.to_string(),
                entity_type: "ecu".to_string(),
                description: None,
                href: format!("/vehicle/v1/components/{id}"),
                status: None,
            },
            caps: sovd_core::Capabilities::uds_ecu(),
        })
    }

    #[tokio::test]
    async fn scope_enforcement_and_filtering_e2e() {
        use crate::AppState;
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use std::sync::Arc;
        use tower::ServiceExt;

        let mut backends = std::collections::HashMap::new();
        backends.insert("engine_ecu".to_string(), mock("engine_ecu"));
        backends.insert("body_ecu".to_string(), mock("body_ecu"));

        let auth = Arc::new(AuthContext::test_scoped(vec!["component:engine_ecu"]));
        let app = crate::create_router(AppState::new(backends).with_auth(auth));

        // in-scope component → 200
        let r = app
            .clone()
            .oneshot(
                Request::get("/vehicle/v1/components/engine_ecu")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK, "in-scope component reachable");

        // out-of-scope component → 401
        let r = app
            .clone()
            .oneshot(
                Request::get("/vehicle/v1/components/body_ecu")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            r.status(),
            StatusCode::UNAUTHORIZED,
            "out-of-scope component denied"
        );

        // enumeration filtered to in-scope only (C-031 non-leakage)
        let r = app
            .clone()
            .oneshot(
                Request::get("/vehicle/v1/components")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let body = axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let items = json["items"].as_array().unwrap();
        assert_eq!(
            items.len(),
            1,
            "listing must not leak out-of-scope components"
        );
        assert_eq!(items[0]["id"], "engine_ecu");

        // public path → 200 without auth
        let r = app
            .clone()
            .oneshot(Request::get("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);

        // (The bus-discovery `POST /vehicle/v1/discovery` endpoint was
        // removed for C-025 — it was not a SOVD entity resource. C-031
        // non-leakage is now exercised solely through the `/components`
        // enumeration filter asserted above.)

        // P0 fix: /admin requires an admin scope — a component-only token is denied.
        let r = app
            .oneshot(
                Request::get("/admin/definitions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            r.status(),
            StatusCode::UNAUTHORIZED,
            "/admin needs an admin scope"
        );

        // ...and an admin-scoped token reaches it.
        let mut admin_backends = std::collections::HashMap::new();
        admin_backends.insert("engine_ecu".to_string(), mock("engine_ecu"));
        let admin_app = crate::create_router(
            AppState::new(admin_backends)
                .with_auth(Arc::new(AuthContext::test_scoped(vec!["admin:*"]))),
        );
        let r = admin_app
            .oneshot(
                Request::get("/admin/definitions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK, "admin scope reaches /admin");
    }
}
