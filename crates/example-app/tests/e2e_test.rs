//! E2E tests for the example-app multi-layer proxy architecture.
//!
//! Architecture per test:
//!
//! ```text
//! Test Client (reqwest / SovdClient)
//!     │
//!     ▼
//! Supplier App TestServer (port X, random)
//!   ├── ExampleAppBackend
//!   │     ├── ManagedEcuBackend (sub-entity)
//!   │     │     └── SovdProxyBackend → HTTP → Upstream TestServer (port Y, random)
//!   │     │                                       └── MockUpstreamBackend
//!   │     ├── auth middleware (bearer token)
//!   │     └── synthetic params (engine_health_score, maintenance_hours)
//! ```
//!
//! Fully in-process — no vCAN or external processes required.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::middleware;
use chrono::Utc;
use example_app::auth::{auth_middleware, AuthToken};
use example_app::backend::ExampleAppBackend;
use example_app::managed_ecu::ManagedEcuBackend;
use serde_json::Value;
use sovd_api::{create_router, AppState};
use sovd_client::testing::TestServer;
use sovd_core::{
    BackendError, BackendResult, Capabilities, ClearFaultsResult, DataValue, DiagnosticBackend,
    EntityInfo, Fault, FaultFilter, FaultSeverity, FaultsResult, OperationExecution, OperationInfo,
    PackageInfo, PackageStatus, ParameterInfo, VerifyResult,
};
use sovd_proxy::SovdProxyBackend;
use tokio::sync::RwLock;

// =============================================================================
// Mock Upstream Backend
// =============================================================================

/// Mock backend simulating an upstream ECU that the example-app proxies to.
struct MockUpstreamBackend {
    info: EntityInfo,
    capabilities: Capabilities,
    packages: RwLock<HashMap<String, Vec<u8>>>,
}

impl MockUpstreamBackend {
    fn new(id: &str) -> Self {
        Self {
            info: EntityInfo {
                id: id.to_string(),
                name: format!("{} ECU", id),
                entity_type: "ecu".to_string(),
                description: Some("Mock upstream ECU for proxy testing".to_string()),
                href: format!("/vehicle/v1/components/{}", id),
                status: Some("online".to_string()),
            },
            capabilities: Capabilities::uds_ecu(),
            packages: RwLock::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl DiagnosticBackend for MockUpstreamBackend {
    fn entity_info(&self) -> &EntityInfo {
        &self.info
    }

    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    async fn list_parameters(&self) -> BackendResult<Vec<ParameterInfo>> {
        Ok(vec![
            ParameterInfo {
                id: "engine_rpm".to_string(),
                name: "Engine RPM".to_string(),
                description: None,
                unit: Some("rpm".to_string()),
                data_type: Some("float64".to_string()),
                read_only: true,
                href: String::new(),
                did: None,
            },
            ParameterInfo {
                id: "coolant_temperature".to_string(),
                name: "Coolant Temperature".to_string(),
                description: None,
                unit: Some("°C".to_string()),
                data_type: Some("float64".to_string()),
                read_only: true,
                href: String::new(),
                did: None,
            },
            ParameterInfo {
                id: "vin".to_string(),
                name: "VIN".to_string(),
                description: None,
                unit: None,
                data_type: Some("string".to_string()),
                read_only: true,
                href: String::new(),
                did: None,
            },
            ParameterInfo {
                id: "vehicle_speed".to_string(),
                name: "Vehicle Speed".to_string(),
                description: None,
                unit: Some("km/h".to_string()),
                data_type: Some("float64".to_string()),
                read_only: true,
                href: String::new(),
                did: None,
            },
        ])
    }

    async fn read_data(&self, param_ids: &[String]) -> BackendResult<Vec<DataValue>> {
        let mut results = Vec::new();
        for id in param_ids {
            let dv = match id.as_str() {
                "engine_rpm" => {
                    DataValue::from_float("engine_rpm", "Engine RPM", 3500.0).with_unit("rpm")
                }
                "coolant_temperature" => {
                    DataValue::from_float("coolant_temperature", "Coolant Temperature", 92.0)
                        .with_unit("°C")
                }
                "vin" => DataValue::from_string("vin", "VIN", "WF0XXXGCDX1234567"),
                "vehicle_speed" => {
                    DataValue::from_float("vehicle_speed", "Vehicle Speed", 60.0).with_unit("km/h")
                }
                _ => return Err(BackendError::ParameterNotFound(id.clone())),
            };
            results.push(dv);
        }
        Ok(results)
    }

    async fn get_faults(&self, _filter: Option<&FaultFilter>) -> BackendResult<FaultsResult> {
        Ok(FaultsResult {
            faults: vec![
                Fault {
                    id: "fault_1".to_string(),
                    code: "P0101".to_string(),
                    severity: FaultSeverity::Warning,
                    message: "Mass air flow sensor range/performance".to_string(),
                    category: Some("powertrain".to_string()),
                    first_occurrence: Some(Utc::now()),
                    last_occurrence: Some(Utc::now()),
                    occurrence_count: Some(3),
                    active: true,
                    status: None,
                    href: String::new(),
                },
                Fault {
                    id: "fault_2".to_string(),
                    code: "P0420".to_string(),
                    severity: FaultSeverity::Error,
                    message: "Catalyst system efficiency below threshold".to_string(),
                    category: Some("emissions".to_string()),
                    first_occurrence: Some(Utc::now()),
                    last_occurrence: Some(Utc::now()),
                    occurrence_count: Some(1),
                    active: false,
                    status: None,
                    href: String::new(),
                },
            ],
            status_availability_mask: None,
        })
    }

    async fn list_operations(&self) -> BackendResult<Vec<OperationInfo>> {
        Ok(vec![
            OperationInfo {
                id: "self_test".to_string(),
                name: "ECU Self Test".to_string(),
                description: Some("Run self-diagnostic".to_string()),
                parameters: vec![],
                requires_security: false,
                security_level: 0,
                href: String::new(),
            },
            OperationInfo {
                id: "clear_adaptation".to_string(),
                name: "Clear Adaptation Values".to_string(),
                description: Some("Reset learned values".to_string()),
                parameters: vec![],
                requires_security: true,
                security_level: 1,
                href: String::new(),
            },
        ])
    }

    async fn start_operation(
        &self,
        operation_id: &str,
        _params: &[u8],
    ) -> BackendResult<OperationExecution> {
        Ok(OperationExecution::completed_with_message(
            "exec_1",
            operation_id,
            "Operation completed",
        ))
    }

    // ---- Package management (for flash proxy testing) ----

    async fn receive_package(&self, data: &[u8]) -> BackendResult<String> {
        let id = format!("upstream_pkg_{}", uuid::Uuid::new_v4());
        self.packages
            .write()
            .await
            .insert(id.clone(), data.to_vec());
        Ok(id)
    }

    async fn list_packages(&self) -> BackendResult<Vec<PackageInfo>> {
        let pkgs = self.packages.read().await;
        Ok(pkgs
            .iter()
            .map(|(id, data)| PackageInfo {
                id: id.clone(),
                size: data.len(),
                target_ecu: None,
                version: None,
                status: PackageStatus::Pending,
                created_at: Some(Utc::now()),
            })
            .collect())
    }

    async fn verify_package(&self, package_id: &str) -> BackendResult<VerifyResult> {
        let pkgs = self.packages.read().await;
        if pkgs.contains_key(package_id) {
            Ok(VerifyResult {
                valid: true,
                checksum: Some("deadbeef".to_string()),
                algorithm: Some("mock".to_string()),
                error: None,
            })
        } else {
            Err(BackendError::EntityNotFound(package_id.to_string()))
        }
    }

    async fn clear_faults(&self, _group: Option<u32>) -> BackendResult<ClearFaultsResult> {
        Ok(ClearFaultsResult {
            success: true,
            cleared_count: 2,
            message: "All faults cleared".to_string(),
        })
    }
}

// =============================================================================
// Test Helpers
// =============================================================================

const AUTH_TOKEN: &str = "test-secret-token-123";
const SUPPLIER_ID: &str = "vortex_engine";
const UPSTREAM_ID: &str = "vtx_vx500";
const ECU_ID: &str = "vtx_vx500";

/// Test environment with both servers running.
struct TestEnv {
    /// The example-app server (the thing under test)
    supplier: TestServer,
    /// The upstream ECU server
    _upstream: TestServer,
    /// Raw HTTP client for auth testing
    http: reqwest::Client,
    /// Base URL of the example-app
    supplier_url: String,
    /// Direct reference to the backend for integration-level tests
    backend: Arc<ExampleAppBackend>,
    /// Direct reference to the managed ECU backend
    _managed_ecu: Arc<ManagedEcuBackend>,
}

/// Start both upstream and example-app servers.
/// If `auth_token` is Some, bearer token auth is enabled.
async fn setup(auth_token: Option<&str>) -> TestEnv {
    // 1. Start the upstream mock ECU server
    let upstream_backend = Arc::new(MockUpstreamBackend::new(UPSTREAM_ID));
    let upstream_state = AppState::single(
        UPSTREAM_ID.to_string(),
        upstream_backend as Arc<dyn DiagnosticBackend>,
    );
    let upstream_router = create_router(upstream_state);
    let upstream = TestServer::start(upstream_router)
        .await
        .expect("Failed to start upstream server");
    let upstream_url = upstream.base_url();

    // 2. Create the SovdProxyBackend pointing at the upstream
    let proxy = SovdProxyBackend::new(UPSTREAM_ID, &upstream_url, UPSTREAM_ID)
        .await
        .expect("Failed to create proxy backend");

    // 3. Create the ManagedEcuBackend wrapping the proxy
    let managed_ecu = Arc::new(
        ManagedEcuBackend::new(
            ECU_ID,
            "Vortex VX500 Engine ECU",
            SUPPLIER_ID,
            proxy,
            &upstream_url,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            None,
        )
        .expect("Failed to create managed ECU backend"),
    );

    // 4. Create the ExampleAppBackend wrapping the managed ECU
    let backend = Arc::new(ExampleAppBackend::new(
        SUPPLIER_ID,
        "Vortex Motors Engine App",
        ECU_ID,
        "Vortex VX500 Engine ECU",
        Some(managed_ecu.clone()),
    ));

    let state = AppState::single(
        SUPPLIER_ID.to_string(),
        backend.clone() as Arc<dyn DiagnosticBackend>,
    );
    let mut app = create_router(state);

    // 5. Apply auth middleware if token is configured
    if let Some(token) = auth_token {
        app = app
            .layer(middleware::from_fn(auth_middleware))
            .layer(axum::Extension(AuthToken(token.to_string())));
    }

    let supplier =
        TestServer::start_with_timeout(app, Duration::from_secs(10), Duration::from_secs(5))
            .await
            .expect("Failed to start supplier server");
    let supplier_url = supplier.base_url();

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("Failed to create HTTP client");

    TestEnv {
        supplier,
        _upstream: upstream,
        http,
        supplier_url,
        backend,
        _managed_ecu: managed_ecu,
    }
}

// =============================================================================
// Test 1: Health endpoint bypasses auth
// =============================================================================

#[tokio::test]
async fn test_health_no_auth() {
    let env = setup(Some(AUTH_TOKEN)).await;

    // /health should work without any auth header
    let resp = env
        .http
        .get(format!("{}/health", env.supplier_url))
        .send()
        .await
        .expect("health request failed");

    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "OK");
}

// =============================================================================
// Test 2: Unauthenticated request returns 401
// =============================================================================

#[tokio::test]
async fn test_auth_required() {
    let env = setup(Some(AUTH_TOKEN)).await;

    let resp = env
        .http
        .get(format!("{}/vehicle/v1/components", env.supplier_url))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 401);
}

// =============================================================================
// Test 3: Wrong bearer token returns 401
// =============================================================================

#[tokio::test]
async fn test_auth_wrong_token() {
    let env = setup(Some(AUTH_TOKEN)).await;

    let resp = env
        .http
        .get(format!("{}/vehicle/v1/components", env.supplier_url))
        .header("Authorization", "Bearer wrong-token")
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 401);
}

// =============================================================================
// Test 4: Correct bearer token returns 200
// =============================================================================

#[tokio::test]
async fn test_auth_success() {
    let env = setup(Some(AUTH_TOKEN)).await;

    let resp = env
        .http
        .get(format!("{}/vehicle/v1/components", env.supplier_url))
        .header("Authorization", format!("Bearer {}", AUTH_TOKEN))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 200);
}

// =============================================================================
// Test 5: Entity type is "app", not "gateway" or "ecu"
// =============================================================================

#[tokio::test]
async fn test_entity_type_is_app() {
    let env = setup(None).await;

    let components = env.supplier.client().list_components().await.unwrap();
    assert_eq!(components.len(), 1);
    assert_eq!(components[0].id, SUPPLIER_ID);
    assert_eq!(
        components[0].component_type.as_deref(),
        Some("app"),
        "example-app entity_type must be 'app'"
    );
}

// =============================================================================
// Test 6: Capabilities — sub_entities: true, faults: false at app level
// =============================================================================

#[tokio::test]
async fn test_capabilities() {
    let env = setup(None).await;

    let component = env
        .supplier
        .client()
        .get_component(SUPPLIER_ID)
        .await
        .unwrap();

    let caps = component
        .capabilities
        .expect("capabilities should be present");

    assert!(caps.read_data, "should support read_data");
    assert!(!caps.faults, "should NOT support faults at app level");
    assert!(
        !caps.operations,
        "should NOT support operations at app level"
    );
    assert!(caps.sub_entities, "should have sub_entities");
}

// =============================================================================
// Test 7: App-level list_parameters returns only synthetic params
//         (sub-entity params are on their own route per SOVD standard)
// =============================================================================

#[tokio::test]
async fn test_list_parameters_includes_synthetic() {
    let env = setup(None).await;

    let params = env
        .supplier
        .client()
        .list_parameters(SUPPLIER_ID)
        .await
        .unwrap();

    let param_ids: Vec<&str> = params.items.iter().map(|p| p.id.as_str()).collect();

    // Synthetic params (at app level)
    assert!(
        param_ids.contains(&"engine_health_score"),
        "should include synthetic engine_health_score, got: {:?}",
        param_ids
    );
    assert!(
        param_ids.contains(&"maintenance_hours"),
        "should include synthetic maintenance_hours"
    );

    // Sub-entity params must NOT appear at the app level — they are
    // accessed via the sub-entity route (tested in test_list_parameters_via_sub_entity)
    let has_ecu_param = param_ids.iter().any(|id| id.contains("engine_rpm"));
    assert!(
        !has_ecu_param,
        "sub-entity params should not appear at app level, got: {:?}",
        param_ids
    );
}

// =============================================================================
// Test 7b: Parameters via sub-entity path
// =============================================================================

#[tokio::test]
async fn test_list_parameters_via_sub_entity() {
    let env = setup(None).await;

    let resp = env
        .http
        .get(format!(
            "{}/vehicle/v1/components/{}/apps/{}/data",
            env.supplier_url, SUPPLIER_ID, ECU_ID
        ))
        .send()
        .await
        .expect("sub-entity data request failed");

    assert_eq!(
        resp.status(),
        200,
        "should get 200 OK for sub-entity parameters"
    );
    let body: Value = resp.json().await.unwrap();
    let items = body["items"].as_array().expect("Expected items array");

    let param_ids: Vec<&str> = items.iter().filter_map(|p| p["id"].as_str()).collect();

    assert!(
        param_ids.contains(&"engine_rpm"),
        "sub-entity should include engine_rpm, got: {:?}",
        param_ids
    );
    assert!(
        param_ids.contains(&"coolant_temperature"),
        "sub-entity should include coolant_temperature, got: {:?}",
        param_ids
    );
}

// =============================================================================
// Test 8: Reading a synthetic parameter (engine_health_score)
// =============================================================================

#[tokio::test]
async fn test_read_synthetic_parameter() {
    let env = setup(None).await;

    let values = env
        .backend
        .read_data(&["engine_health_score".to_string()])
        .await
        .expect("read_data failed");

    assert_eq!(values.len(), 1);
    assert_eq!(values[0].id, "engine_health_score");
    let value = values[0].value.as_f64().unwrap();
    assert!(
        (0.0..=100.0).contains(&value),
        "engine_health_score should be 0-100, got {}",
        value
    );
    assert!(
        (value - 100.0).abs() < 0.01,
        "with RPM=3500 and temp=92, health should be 100.0, got {}",
        value
    );
}

// =============================================================================
// Test 9: Reading maintenance_hours returns a number >= 0
// =============================================================================

#[tokio::test]
async fn test_read_maintenance_hours() {
    let env = setup(None).await;

    let values = env
        .backend
        .read_data(&["maintenance_hours".to_string()])
        .await
        .expect("read_data failed");

    assert_eq!(values.len(), 1);
    assert_eq!(values[0].id, "maintenance_hours");
    let value = values[0].value.as_f64().unwrap();
    assert!(
        value >= 0.0,
        "maintenance_hours should be >= 0, got {}",
        value
    );
}

// =============================================================================
// Test 10: Sub-entities list returns managed ECU
// =============================================================================

#[tokio::test]
async fn test_list_sub_entities() {
    let env = setup(None).await;

    let apps = env.supplier.client().list_apps(SUPPLIER_ID).await.unwrap();

    assert_eq!(apps.len(), 1, "should have exactly 1 sub-entity");
    assert_eq!(apps[0].id, ECU_ID, "sub-entity should be the managed ECU");
}

// =============================================================================
// Test 11: Faults via sub-entity path
// =============================================================================

#[tokio::test]
async fn test_get_faults_via_sub_entity() {
    let env = setup(None).await;

    let resp = env
        .http
        .get(format!(
            "{}/vehicle/v1/components/{}/apps/{}/faults",
            env.supplier_url, SUPPLIER_ID, ECU_ID
        ))
        .send()
        .await
        .expect("faults request failed");

    assert_eq!(
        resp.status(),
        200,
        "should get 200 OK for sub-entity faults"
    );
    let body: Value = resp.json().await.unwrap();
    let items = body["items"].as_array().expect("Expected items array");
    assert_eq!(items.len(), 2, "should have 2 faults from upstream");
}

// =============================================================================
// Test 12: Operations via sub-entity path
// =============================================================================

#[tokio::test]
async fn test_list_operations_via_sub_entity() {
    let env = setup(None).await;

    let resp = env
        .http
        .get(format!(
            "{}/vehicle/v1/components/{}/apps/{}/operations",
            env.supplier_url, SUPPLIER_ID, ECU_ID
        ))
        .send()
        .await
        .expect("operations request failed");

    assert_eq!(
        resp.status(),
        200,
        "should get 200 OK for sub-entity operations"
    );
    let body: Value = resp.json().await.unwrap();
    let items = body["items"].as_array().expect("Expected items array");
    assert_eq!(items.len(), 2, "should have 2 operations from upstream");
    assert!(
        items.iter().any(|o| o["id"].as_str() == Some("self_test")),
        "should include self_test"
    );
    assert!(
        items
            .iter()
            .any(|o| o["id"].as_str() == Some("clear_adaptation")),
        "should include clear_adaptation"
    );
}

// =============================================================================
// Helper: set the sub-entity session to programming (required for OTA ops)
// =============================================================================

async fn set_programming_session(env: &TestEnv) {
    let resp = env
        .http
        .put(format!(
            "{}/vehicle/v1/components/{}/apps/{}/modes/session",
            env.supplier_url, SUPPLIER_ID, ECU_ID
        ))
        .json(&serde_json::json!({"value": "programming"}))
        .send()
        .await
        .expect("set session request failed");
    assert_eq!(resp.status(), 200, "should switch to programming session");
}

// =============================================================================
// Test 13: Upload package via sub-entity path
// =============================================================================

#[tokio::test]
async fn test_receive_package() {
    let env = setup(None).await;
    set_programming_session(&env).await;

    // Create a valid package (>= 16 bytes, first 4 bytes not all zeros)
    let package_data: Vec<u8> = vec![0x01, 0x02, 0x03, 0x04]
        .into_iter()
        .chain(vec![0xAB; 28])
        .collect();

    let resp = env
        .http
        .post(format!(
            "{}/vehicle/v1/components/{}/apps/{}/files",
            env.supplier_url, SUPPLIER_ID, ECU_ID
        ))
        .body(package_data.clone())
        .send()
        .await
        .expect("upload request failed");

    assert_eq!(resp.status(), 201, "upload should return 201 Created");
    let body: Value = resp.json().await.unwrap();

    let file_id = body["file_id"].as_str().unwrap();
    assert!(!file_id.is_empty(), "should return a non-empty file_id");
    assert_eq!(body["size"].as_u64().unwrap(), package_data.len() as u64);
}

// =============================================================================
// Test 14: Verify uploaded package
// =============================================================================

#[tokio::test]
async fn test_verify_package() {
    let env = setup(None).await;
    set_programming_session(&env).await;

    // Upload a valid package (first 4 bytes non-zero, >= 16 bytes)
    let package_data: Vec<u8> = vec![0x01, 0x02, 0x03, 0x04]
        .into_iter()
        .chain(vec![0xAB; 28])
        .collect();

    let upload_resp = env
        .http
        .post(format!(
            "{}/vehicle/v1/components/{}/apps/{}/files",
            env.supplier_url, SUPPLIER_ID, ECU_ID
        ))
        .body(package_data)
        .send()
        .await
        .expect("upload failed");

    let upload_body: Value = upload_resp.json().await.unwrap();
    let file_id = upload_body["file_id"].as_str().unwrap();

    // Now verify
    let verify_resp = env
        .http
        .post(format!(
            "{}/vehicle/v1/components/{}/apps/{}/files/{}/verify",
            env.supplier_url, SUPPLIER_ID, ECU_ID, file_id
        ))
        .send()
        .await
        .expect("verify request failed");

    assert_eq!(verify_resp.status(), 200);
    let verify_body: Value = verify_resp.json().await.unwrap();
    assert_eq!(verify_body["valid"], true, "package should be valid");
    assert!(
        verify_body["checksum"].as_str().is_some(),
        "should return a checksum"
    );
}

// =============================================================================
// Test 15: Upload tiny package (<16 bytes) → error
// =============================================================================

#[tokio::test]
async fn test_reject_small_package() {
    let env = setup(None).await;
    set_programming_session(&env).await;

    // Package smaller than 16 bytes
    let small_package = vec![0x01, 0x02, 0x03];

    let resp = env
        .http
        .post(format!(
            "{}/vehicle/v1/components/{}/apps/{}/files",
            env.supplier_url, SUPPLIER_ID, ECU_ID
        ))
        .body(small_package)
        .send()
        .await
        .expect("upload request failed");

    assert_eq!(
        resp.status(),
        400,
        "should reject packages smaller than 16 bytes"
    );
}
