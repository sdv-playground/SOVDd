//! Integration tests for sovd-client
//!
//! These tests spin up a real SOVD server and use the client to interact with it.
//! This ensures the client stays in sync with the API.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use sovd_api::{create_router, AppState};
use sovd_client::testing::TestServer;
use sovd_conv::DidStore;
use sovd_core::{
    BackendError, BackendResult, Capabilities, ClearFaultsResult, DataValue, DiagnosticBackend,
    EntityInfo, Fault, FaultFilter, FaultSeverity, FaultsResult, OperationExecution, OperationInfo,
    OperationStatus, ParameterInfo,
};

// =============================================================================
// Mock Backend
// =============================================================================

/// Mock backend for testing
struct MockBackend {
    info: EntityInfo,
    capabilities: Capabilities,
    did_values: HashMap<u16, Vec<u8>>,
    faults: Vec<Fault>,
    operations: Vec<OperationInfo>,
}

impl MockBackend {
    fn new(id: &str) -> Self {
        let mut did_values = HashMap::new();
        // Coolant temp: raw 132 → 92°C (with offset -40)
        did_values.insert(0xF405, vec![132]);
        // Engine RPM: raw 7200 → 1800 rpm (with scale 0.25)
        did_values.insert(0xF40C, vec![0x1C, 0x20]);
        // VIN
        did_values.insert(0xF190, b"WF0XXXGCDX1234567".to_vec());

        let faults = vec![Fault {
            id: "P0123".to_string(),
            code: "P0123".to_string(),
            message: "Throttle Position Sensor".to_string(),
            severity: FaultSeverity::Warning,
            category: Some("powertrain".to_string()),
            active: true,
            occurrence_count: Some(3),
            first_occurrence: None,
            last_occurrence: None,
            status: None,
            href: format!("/vehicle/v1/components/{}/faults/P0123", id),
        }];

        let operations = vec![OperationInfo {
            id: "reset".to_string(),
            name: "ECU Reset".to_string(),
            description: Some("Reset the ECU".to_string()),
            parameters: vec![],
            requires_security: false,
            security_level: 0,
            href: format!("/vehicle/v1/components/{}/operations/reset", id),
        }];

        Self {
            info: EntityInfo {
                id: id.to_string(),
                name: format!("{} ECU", id),
                entity_type: "ecu".to_string(),
                description: Some("Mock ECU for testing".to_string()),
                href: format!("/vehicle/v1/components/{}", id),
                status: Some("online".to_string()),
            },
            capabilities: Capabilities::default(),
            did_values,
            faults,
            operations,
        }
    }
}

#[async_trait]
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

    async fn read_data(&self, _param_ids: &[String]) -> BackendResult<Vec<DataValue>> {
        Ok(vec![])
    }

    async fn read_raw_did(&self, did: u16) -> BackendResult<Vec<u8>> {
        self.did_values
            .get(&did)
            .cloned()
            .ok_or_else(|| BackendError::ParameterNotFound(format!("DID 0x{:04X} not found", did)))
    }

    async fn write_raw_did(&self, _did: u16, _data: &[u8]) -> BackendResult<()> {
        Ok(())
    }

    async fn get_faults(&self, _filter: Option<&FaultFilter>) -> BackendResult<FaultsResult> {
        Ok(FaultsResult {
            faults: self.faults.clone(),
            status_availability_mask: None,
        })
    }

    async fn clear_faults(&self, _group: Option<u32>) -> BackendResult<ClearFaultsResult> {
        Ok(ClearFaultsResult {
            success: true,
            cleared_count: self.faults.len() as u32,
            message: "Cleared all faults".to_string(),
        })
    }

    async fn list_operations(&self) -> BackendResult<Vec<OperationInfo>> {
        Ok(self.operations.clone())
    }

    async fn start_operation(
        &self,
        operation_id: &str,
        _params: &[u8],
    ) -> BackendResult<OperationExecution> {
        if operation_id == "reset" {
            Ok(OperationExecution {
                execution_id: "exec-123".to_string(),
                operation_id: operation_id.to_string(),
                status: OperationStatus::Completed,
                result: Some(serde_json::json!({"success": true})),
                error: None,
                started_at: chrono::Utc::now(),
                completed_at: Some(chrono::Utc::now()),
            })
        } else {
            Err(BackendError::OperationNotFound(operation_id.to_string()))
        }
    }
}

// =============================================================================
// Test Helpers
// =============================================================================

async fn create_test_server() -> TestServer {
    let backend = Arc::new(MockBackend::new("example_ecu"));
    let mut backends = HashMap::new();
    backends.insert(
        "example_ecu".to_string(),
        backend as Arc<dyn DiagnosticBackend>,
    );

    let state = AppState::new(backends);
    let router = create_router(state);

    TestServer::start(router)
        .await
        .expect("Failed to start test server")
}

async fn create_test_server_with_store(did_store: Arc<DidStore>) -> TestServer {
    let backend = Arc::new(MockBackend::new("example_ecu"));
    let mut backends = HashMap::new();
    backends.insert(
        "example_ecu".to_string(),
        backend as Arc<dyn DiagnosticBackend>,
    );

    let state = AppState::with_did_store(backends, did_store);
    let router = create_router(state);

    TestServer::start(router)
        .await
        .expect("Failed to start test server")
}

// =============================================================================
// Health Check Tests
// =============================================================================

#[tokio::test]
async fn test_health_check() {
    let server = create_test_server().await;

    let result = server.client.health().await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "OK");
}

// =============================================================================
// Component Tests
// =============================================================================

#[tokio::test]
async fn test_list_components() {
    let server = create_test_server().await;

    let components = server.client.list_components().await.unwrap();
    assert_eq!(components.len(), 1);
    assert_eq!(components[0].id, "example_ecu");
}

#[tokio::test]
async fn test_get_component() {
    let server = create_test_server().await;

    let component = server.client.get_component("example_ecu").await.unwrap();
    assert_eq!(component.id, "example_ecu");
    assert_eq!(component.name, "example_ecu ECU");
}

#[tokio::test]
async fn test_get_component_not_found() {
    let server = create_test_server().await;

    let result = server.client.get_component("nonexistent").await;
    assert!(result.is_err());
}

// =============================================================================
// Data/Parameter Tests
// =============================================================================

#[tokio::test]
async fn test_read_did_raw() {
    let server = create_test_server().await;

    // Read VIN (0xF190)
    let response = server.client.read_did("example_ecu", 0xF190).await.unwrap();
    assert_eq!(response.did.as_deref(), Some("F190"));
    // Value should be raw hex since no conversion is registered
    assert_eq!(response.converted, Some(false));
}

#[tokio::test]
async fn test_read_data_with_conversion() {
    // Create a DID store with conversions
    let did_store = Arc::new(DidStore::new());
    did_store.register(
        0xF405,
        sovd_conv::DidDefinition::scaled(sovd_conv::DataType::Uint8, 1.0, -40.0)
            .with_name("Coolant Temperature")
            .with_unit("°C"),
    );

    let server = create_test_server_with_store(did_store).await;

    // Read converted value
    let response = server
        .client
        .read_data("example_ecu", "F405")
        .await
        .unwrap();
    assert_eq!(response.converted, Some(true));
    // 132 - 40 = 92
    assert_eq!(response.value, 92);
    assert_eq!(response.unit.as_deref(), Some("°C"));
}

#[tokio::test]
async fn test_read_data_raw_mode() {
    // Create a DID store with conversions
    let did_store = Arc::new(DidStore::new());
    did_store.register(
        0xF405,
        sovd_conv::DidDefinition::scaled(sovd_conv::DataType::Uint8, 1.0, -40.0),
    );

    let server = create_test_server_with_store(did_store).await;

    // Read with raw=true should skip conversion
    let response = server
        .client
        .read_data_raw("example_ecu", "F405")
        .await
        .unwrap();
    assert_eq!(response.converted, Some(false));
    // Should be raw hex "84" (132 in hex)
    assert_eq!(response.value, "84");
}

#[tokio::test]
async fn test_list_parameters_with_definitions() {
    let did_store = Arc::new(DidStore::new());
    did_store.register(
        0xF405,
        sovd_conv::DidDefinition::scaled(sovd_conv::DataType::Uint8, 1.0, -40.0)
            .with_name("Coolant Temperature"),
    );
    did_store.register(
        0xF40C,
        sovd_conv::DidDefinition::scaled(sovd_conv::DataType::Uint16, 0.25, 0.0)
            .with_name("Engine RPM"),
    );

    let server = create_test_server_with_store(did_store).await;

    let params = server.client.list_parameters("example_ecu").await.unwrap();
    assert_eq!(params.count, 2);
}

// =============================================================================
// Fault Tests
// =============================================================================

#[tokio::test]
async fn test_get_faults() {
    let server = create_test_server().await;

    let faults = server.client.get_faults("example_ecu").await.unwrap();
    assert_eq!(faults.len(), 1);
    assert_eq!(faults[0].code, "P0123");
    assert!(faults[0].active);
}

#[tokio::test]
async fn test_get_fault_detail() {
    let server = create_test_server().await;

    let fault = server
        .client
        .get_fault("example_ecu", "P0123")
        .await
        .unwrap();
    assert_eq!(fault.code, "P0123");
}

#[tokio::test]
async fn test_clear_faults() {
    let server = create_test_server().await;

    let result = server.client.clear_faults("example_ecu").await.unwrap();
    assert!(result.success);
}

// =============================================================================
// Operation Tests
// =============================================================================

#[tokio::test]
async fn test_list_operations() {
    let server = create_test_server().await;

    let operations = server.client.list_operations("example_ecu").await.unwrap();
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].id, "reset");
    assert_eq!(operations[0].name, "ECU Reset");
}

#[tokio::test]
async fn test_execute_operation() {
    let server = create_test_server().await;

    let result = server
        .client
        .execute_operation_simple("example_ecu", "reset")
        .await
        .unwrap();

    // Operation should be running or completed after start
    assert!(
        result.status == sovd_client::OperationStatus::Running
            || result.status == sovd_client::OperationStatus::Completed
    );
}

#[tokio::test]
async fn test_execute_operation_not_found() {
    let server = create_test_server().await;

    let result = server
        .client
        .execute_operation_simple("example_ecu", "nonexistent")
        .await;

    assert!(result.is_err());
}

// =============================================================================
// Admin/Definition Tests
// =============================================================================

#[tokio::test]
async fn test_upload_definitions() {
    let server = create_test_server().await;

    let yaml = r#"
dids:
  0xF405:
    name: Coolant Temperature
    type: uint8
    scale: 1.0
    offset: -40.0
    unit: °C
"#;

    let result = server.client.upload_definitions(yaml).await.unwrap();
    assert_eq!(result.status, "ok");
    assert_eq!(result.loaded, 1);
}

#[tokio::test]
async fn test_list_definitions() {
    let server = create_test_server().await;

    // Upload first
    let yaml = r#"
dids:
  0xF405:
    name: Coolant Temperature
    type: uint8
"#;
    server.client.upload_definitions(yaml).await.unwrap();

    // List
    let defs = server.client.list_definitions().await.unwrap();
    assert_eq!(defs.count, 1);
    assert_eq!(defs.dids[0].did, "F405");
}

#[tokio::test]
async fn test_get_definition() {
    let server = create_test_server().await;

    let yaml = r#"
dids:
  0xF405:
    name: Coolant Temperature
    type: uint8
    scale: 1.0
    offset: -40.0
    unit: °C
"#;
    server.client.upload_definitions(yaml).await.unwrap();

    let def = server.client.get_definition("F405").await.unwrap();
    assert_eq!(def.did, "F405");
    assert_eq!(def.name.as_deref(), Some("Coolant Temperature"));
    assert_eq!(def.scale, Some(1.0));
    assert_eq!(def.offset, Some(-40.0));
}

#[tokio::test]
async fn test_delete_definition() {
    let server = create_test_server().await;

    let yaml = r#"
dids:
  0xF405:
    name: Coolant Temperature
    type: uint8
"#;
    server.client.upload_definitions(yaml).await.unwrap();

    // Delete
    server.client.delete_definition("F405").await.unwrap();

    // Verify deleted
    let result = server.client.get_definition("F405").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_clear_definitions() {
    let server = create_test_server().await;

    let yaml = r#"
dids:
  0xF405:
    name: Coolant Temperature
    type: uint8
  0xF40C:
    name: Engine RPM
    type: uint16
"#;
    server.client.upload_definitions(yaml).await.unwrap();

    // Clear
    let result = server.client.clear_definitions().await.unwrap();
    assert_eq!(result["cleared"], 2);

    // Verify empty
    let defs = server.client.list_definitions().await.unwrap();
    assert_eq!(defs.count, 0);
}

// =============================================================================
// SOVD Semantic Name Tests
// =============================================================================

/// Test SOVD-compliant semantic parameter names
/// When definitions include an `id` field, parameters can be accessed by semantic name
#[tokio::test]
async fn test_sovd_semantic_parameter_names() {
    let server = create_test_server().await;

    // Upload definitions with SOVD-compliant semantic IDs
    let yaml = r#"
dids:
  0xF405:
    id: coolant_temperature
    name: Coolant Temperature
    type: uint8
    scale: 1.0
    offset: -40.0
    unit: °C
  0xF40C:
    id: engine_rpm
    name: Engine RPM
    type: uint16
    scale: 0.25
    unit: rpm
"#;
    server.client.upload_definitions(yaml).await.unwrap();

    // List parameters - should show semantic IDs
    let params = server.client.list_parameters("example_ecu").await.unwrap();
    assert_eq!(params.count, 2);

    // Find coolant_temperature
    let coolant = params.items.iter().find(|p| p.id == "coolant_temperature");
    assert!(coolant.is_some());
    let coolant = coolant.unwrap();
    assert_eq!(coolant.did, "F405");
    assert_eq!(coolant.name.as_deref(), Some("Coolant Temperature"));
    // href should use semantic id
    assert!(coolant.href.contains("coolant_temperature"));

    // Read by semantic name
    let temp = server
        .client
        .read_data("example_ecu", "coolant_temperature")
        .await
        .unwrap();
    assert_eq!(temp.value, 92); // 132 - 40

    // Read by raw DID (also works)
    let temp2 = server
        .client
        .read_data("example_ecu", "F405")
        .await
        .unwrap();
    assert_eq!(temp2.value, 92);

    // Read RPM by semantic name
    let rpm = server
        .client
        .read_data("example_ecu", "engine_rpm")
        .await
        .unwrap();
    assert_eq!(rpm.value, 1800); // 7200 * 0.25
}

/// Test fallback to DID when no semantic name is defined (private data scenario)
#[tokio::test]
async fn test_private_data_did_fallback() {
    let server = create_test_server().await;

    // Upload a definition WITHOUT an id (only name, like legacy format)
    let yaml = r#"
dids:
  0xF405:
    name: Coolant Temperature
    type: uint8
    scale: 1.0
    offset: -40.0
    unit: °C
"#;
    server.client.upload_definitions(yaml).await.unwrap();

    // List parameters - id should fall back to DID
    let params = server.client.list_parameters("example_ecu").await.unwrap();
    assert_eq!(params.count, 1);

    let param = &params.items[0];
    assert_eq!(param.id, "F405"); // Falls back to DID since no id specified
    assert_eq!(param.did, "F405");

    // Can still read by DID
    let temp = server
        .client
        .read_data("example_ecu", "F405")
        .await
        .unwrap();
    assert_eq!(temp.value, 92);
}

/// Test accessing unknown DIDs without server-side definitions (tester-only conversion)
#[tokio::test]
async fn test_raw_did_access_without_definition() {
    let server = create_test_server().await;

    // Don't upload any definitions
    // Read by raw DID - should still work but return unconverted data
    let response = server
        .client
        .read_data("example_ecu", "F405")
        .await
        .unwrap();

    // Value should be raw hex since no conversion is registered
    assert_eq!(response.converted, Some(false));
    // Raw bytes available for client-side conversion
    assert!(response.raw.is_some());
}

/// Test client-side conversion for private data
/// This demonstrates the workflow when server doesn't have definitions
#[tokio::test]
async fn test_client_side_conversion() {
    use sovd_conv::{DataType, DidDefinition, DidStore};

    let server = create_test_server().await;

    // Set up client-side conversions (private/proprietary definitions)
    let store = DidStore::new();
    store.register(
        0xF405,
        DidDefinition::scaled(DataType::Uint8, 1.0, -40.0).with_unit("°C"),
    );
    store.register(
        0xF40C,
        DidDefinition::scaled(DataType::Uint16, 0.25, 0.0).with_unit("rpm"),
    );

    // Read raw data from server (no server-side definitions)
    let response = server
        .client
        .read_data_raw("example_ecu", "F405")
        .await
        .unwrap();

    // Get raw bytes and apply client-side conversion
    let raw_bytes = response.raw_bytes().unwrap();
    let temp = store.decode(0xF405, &raw_bytes).unwrap();
    assert_eq!(temp, 92); // 132 - 40

    // Test RPM with 2-byte value
    let response = server
        .client
        .read_data_raw("example_ecu", "F40C")
        .await
        .unwrap();
    let raw_bytes = response.raw_bytes().unwrap();
    let rpm = store.decode(0xF40C, &raw_bytes).unwrap();
    assert_eq!(rpm, 1800); // 7200 * 0.25
}

// =============================================================================
// Full Workflow Test
// =============================================================================

#[tokio::test]
async fn test_full_diagnostic_workflow() {
    let server = create_test_server().await;

    // 1. Check health
    let health = server.client.health().await.unwrap();
    assert_eq!(health, "OK");

    // 2. List components
    let components = server.client.list_components().await.unwrap();
    assert!(!components.is_empty());
    let component_id = &components[0].id;

    // 3. Upload DID definitions
    let yaml = r#"
dids:
  0xF405:
    name: Coolant Temperature
    type: uint8
    scale: 1.0
    offset: -40.0
    unit: °C
  0xF190:
    name: VIN
    type: string
    length: 17
"#;
    server.client.upload_definitions(yaml).await.unwrap();

    // 4. Read converted data
    let temp = server.client.read_data(component_id, "F405").await.unwrap();
    assert_eq!(temp.value, 92); // 132 - 40

    // 5. Read VIN
    let vin = server.client.read_data(component_id, "F190").await.unwrap();
    assert_eq!(vin.value, "WF0XXXGCDX1234567");

    // 6. Get faults
    let faults = server.client.get_faults(component_id).await.unwrap();
    assert!(!faults.is_empty());

    // 7. List operations
    let operations = server.client.list_operations(component_id).await.unwrap();
    assert!(!operations.is_empty());

    // 8. Execute operation
    let result = server
        .client
        .execute_operation_simple(component_id, "reset")
        .await
        .unwrap();
    // Operation should be running or completed after start
    assert!(
        result.status == sovd_client::OperationStatus::Running
            || result.status == sovd_client::OperationStatus::Completed
    );
}
