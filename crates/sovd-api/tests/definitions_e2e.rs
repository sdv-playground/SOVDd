//! E2E tests for DID definitions API using sovd-client
//!
//! Tests the full flow:
//! 1. Upload definitions via POST /admin/definitions
//! 2. Read data and verify conversion
//! 3. Test raw mode
//!
//! These tests use the sovd-client library to make requests,
//! ensuring the client stays in sync with the API.

use std::collections::HashMap;
use std::sync::Arc;

use sovd_client::testing::TestServer;
use sovd_conv::DidStore;
use sovd_core::{
    BackendError, BackendResult, Capabilities, DataValue, DiagnosticBackend, EntityInfo,
    FaultFilter, FaultsResult, OperationExecution, OperationInfo, ParameterInfo,
};

use sovd_api::{create_router, AppState};

// =============================================================================
// Mock Backend
// =============================================================================

/// Mock backend that returns configurable DID values
struct MockBackend {
    info: EntityInfo,
    capabilities: Capabilities,
    /// Map of DID → raw bytes to return
    did_values: HashMap<u16, Vec<u8>>,
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
            faults: vec![],
            status_availability_mask: None,
        })
    }

    async fn list_operations(&self) -> BackendResult<Vec<OperationInfo>> {
        Ok(vec![])
    }

    async fn start_operation(
        &self,
        operation_id: &str,
        _params: &[u8],
    ) -> BackendResult<OperationExecution> {
        Err(BackendError::OperationNotFound(operation_id.to_string()))
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
// Definition Management Tests
// =============================================================================

#[tokio::test]
async fn test_upload_definitions_yaml() {
    let server = create_test_server().await;
    let client = &server.client;

    let yaml = r#"
meta:
  name: Test ECU
  version: "1.0"

dids:
  0xF405:
    name: Coolant Temperature
    type: uint8
    scale: 1.0
    offset: -40.0
    unit: °C

  0xF40C:
    name: Engine RPM
    type: uint16
    scale: 0.25
    unit: rpm
"#;

    let result = client.upload_definitions(yaml).await.unwrap();
    assert_eq!(result.status, "ok");
    assert_eq!(result.loaded, 2);
}

#[tokio::test]
async fn test_list_definitions() {
    let server = create_test_server().await;
    let client = &server.client;

    // Upload first
    let yaml = r#"
dids:
  0xF405:
    name: Coolant Temperature
    type: uint8
"#;
    client.upload_definitions(yaml).await.unwrap();

    // List
    let defs = client.list_definitions().await.unwrap();
    assert_eq!(defs.count, 1);
    assert_eq!(defs.dids[0].did, "F405");
}

#[tokio::test]
async fn test_get_definition() {
    let server = create_test_server().await;
    let client = &server.client;

    // Upload
    let yaml = r#"
dids:
  0xF405:
    name: Coolant Temperature
    type: uint8
    scale: 1.0
    offset: -40.0
    unit: °C
"#;
    client.upload_definitions(yaml).await.unwrap();

    // Get single
    let def = client.get_definition("F405").await.unwrap();
    assert_eq!(def.did, "F405");
    assert_eq!(def.name.as_deref(), Some("Coolant Temperature"));
    assert_eq!(def.scale, Some(1.0));
    assert_eq!(def.offset, Some(-40.0));
}

#[tokio::test]
async fn test_delete_definition() {
    let server = create_test_server().await;
    let client = &server.client;

    // Upload
    let yaml = r#"
dids:
  0xF405:
    name: Coolant Temperature
    type: uint8
"#;
    client.upload_definitions(yaml).await.unwrap();

    // Delete
    client.delete_definition("F405").await.unwrap();

    // Verify deleted
    let result = client.get_definition("F405").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_clear_definitions() {
    let server = create_test_server().await;
    let client = &server.client;

    // Upload definitions
    let yaml = r#"
dids:
  0xF405:
    name: Coolant Temperature
    type: uint8
  0xF40C:
    name: Engine RPM
    type: uint16
"#;
    client.upload_definitions(yaml).await.unwrap();

    // Clear all
    let result = client.clear_definitions().await.unwrap();
    assert_eq!(result["cleared"], 2);

    // Verify empty
    let defs = client.list_definitions().await.unwrap();
    assert_eq!(defs.count, 0);
}

// =============================================================================
// Data Read Tests with Conversion
// =============================================================================

#[tokio::test]
async fn test_read_data_with_conversion() {
    let server = create_test_server().await;
    let client = &server.client;

    // Upload definitions
    let yaml = r#"
dids:
  0xF405:
    name: Coolant Temperature
    type: uint8
    scale: 1.0
    offset: -40.0
    unit: °C
"#;
    client.upload_definitions(yaml).await.unwrap();

    // Read data - should be converted
    let response = client.read_data("example_ecu", "F405").await.unwrap();
    assert_eq!(response.did.as_deref(), Some("F405"));
    assert_eq!(response.value, 92); // 132 - 40 = 92
    assert_eq!(response.unit.as_deref(), Some("°C"));
    assert_eq!(response.raw.as_deref(), Some("84")); // 132 in hex
    assert_eq!(response.converted, Some(true));
}

#[tokio::test]
async fn test_read_data_without_conversion() {
    let server = create_test_server().await;
    let client = &server.client;

    // Read data without uploading definitions - should return raw
    let response = client.read_data("example_ecu", "F405").await.unwrap();
    assert_eq!(response.did.as_deref(), Some("F405"));
    assert_eq!(response.value, "84"); // Raw hex
    assert_eq!(response.converted, Some(false));
}

#[tokio::test]
async fn test_read_data_raw_mode() {
    let server = create_test_server().await;
    let client = &server.client;

    // Upload definitions
    let yaml = r#"
dids:
  0xF405:
    name: Coolant Temperature
    type: uint8
    scale: 1.0
    offset: -40.0
"#;
    client.upload_definitions(yaml).await.unwrap();

    // Read with raw=true - should skip conversion
    let response = client.read_data_raw("example_ecu", "F405").await.unwrap();
    assert_eq!(response.value, "84"); // Raw hex despite conversion being registered
    assert_eq!(response.converted, Some(false));
}

#[tokio::test]
async fn test_read_rpm_with_scale() {
    let server = create_test_server().await;
    let client = &server.client;

    // Upload definitions
    let yaml = r#"
dids:
  0xF40C:
    name: Engine RPM
    type: uint16
    scale: 0.25
    unit: rpm
"#;
    client.upload_definitions(yaml).await.unwrap();

    // Read data
    let response = client.read_data("example_ecu", "F40C").await.unwrap();
    assert_eq!(response.did.as_deref(), Some("F40C"));
    assert_eq!(response.value, 1800); // 7200 * 0.25 = 1800
    assert_eq!(response.unit.as_deref(), Some("rpm"));
}

#[tokio::test]
async fn test_preloaded_did_store() {
    // Create DidStore with pre-loaded definitions
    let did_store = Arc::new(DidStore::new());
    did_store.register(
        0xF405,
        sovd_conv::DidDefinition::scaled(sovd_conv::DataType::Uint8, 1.0, -40.0)
            .with_name("Coolant Temperature")
            .with_unit("°C"),
    );

    let server = create_test_server_with_store(did_store).await;
    let client = &server.client;

    // Read data - should use preloaded definition
    let response = client.read_data("example_ecu", "F405").await.unwrap();
    assert_eq!(response.value, 92);
    assert_eq!(response.converted, Some(true));
}

#[tokio::test]
async fn test_list_parameters_shows_registered_dids() {
    let server = create_test_server().await;
    let client = &server.client;

    // Upload definitions
    let yaml = r#"
dids:
  0xF405:
    name: Coolant Temperature
    type: uint8
    unit: °C

  0xF40C:
    name: Engine RPM
    type: uint16
    unit: rpm
"#;
    client.upload_definitions(yaml).await.unwrap();

    // List parameters
    let params = client.list_parameters("example_ecu").await.unwrap();
    assert_eq!(params.count, 2);

    // Should be sorted by DID
    assert_eq!(params.items[0].did, "F405");
    assert_eq!(params.items[1].did, "F40C");
}

// =============================================================================
// Advanced Format Tests - Arrays, Enums, Bitfields, Maps, Histograms
// =============================================================================

/// Create a MockBackend with configurable DID values for advanced type tests
struct AdvancedMockBackend {
    info: EntityInfo,
    capabilities: Capabilities,
    did_values: HashMap<u16, Vec<u8>>,
}

impl AdvancedMockBackend {
    fn new() -> Self {
        let mut did_values = HashMap::new();

        // String: VIN (17 ASCII chars)
        did_values.insert(0xF190, b"WF0XXXGCDX1234567".to_vec());

        // Enum: Gear position = D (value 3)
        did_values.insert(0xF404, vec![3]);

        // Bitfield: engine_running=1, ac_on=0, check_engine=1 → 0b10000001 = 129
        did_values.insert(0xF410, vec![0b10000001]);

        // Array: 4 wheel speeds (FL=100, FR=100.5, RL=99.8, RR=100.2 km/h)
        // With scale 0.01: raw values are 10000, 10050, 9980, 10020
        did_values.insert(
            0xF421,
            vec![
                0x27, 0x10, // FL: 10000 BE
                0x27, 0x42, // FR: 10050 BE
                0x26, 0xFC, // RL: 9980 BE
                0x27, 0x24, // RR: 10020 BE
            ],
        );

        // 2D Map: 2x2 fuel map (values: 1.0, 1.1, 1.2, 1.3 ms)
        // With scale 0.1: raw values are 10, 11, 12, 13
        did_values.insert(0xF500, vec![10, 11, 12, 13]);

        // Little-endian uint32: Odometer = 123456.7 km
        // Raw value = 1234567, LE bytes: 0x87, 0xD6, 0x12, 0x00
        did_values.insert(0xF440, vec![0x87, 0xD6, 0x12, 0x00]);

        // Histogram: 3 bins with values [100, 200, 300] seconds
        did_values.insert(
            0xF600,
            vec![
                0x00, 0x00, 0x00, 0x64, // 100 BE u32
                0x00, 0x00, 0x00, 0xC8, // 200 BE u32
                0x00, 0x00, 0x01, 0x2C, // 300 BE u32
            ],
        );

        // Multi-bit bitfield (transmission status)
        did_values.insert(0xF414, vec![0x06, 0x55]);

        Self {
            info: EntityInfo {
                id: "adv_ecu".to_string(),
                name: "Advanced ECU".to_string(),
                entity_type: "ecu".to_string(),
                description: Some("ECU for advanced type tests".to_string()),
                href: "/vehicle/v1/components/adv_ecu".to_string(),
                status: Some("online".to_string()),
            },
            capabilities: Capabilities::default(),
            did_values,
        }
    }
}

#[async_trait::async_trait]
impl DiagnosticBackend for AdvancedMockBackend {
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
            .ok_or_else(|| BackendError::ParameterNotFound(format!("DID 0x{:04X}", did)))
    }

    async fn write_raw_did(&self, _did: u16, _data: &[u8]) -> BackendResult<()> {
        Ok(())
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

    async fn start_operation(
        &self,
        operation_id: &str,
        _params: &[u8],
    ) -> BackendResult<OperationExecution> {
        Err(BackendError::OperationNotFound(operation_id.to_string()))
    }
}

async fn create_advanced_test_server() -> TestServer {
    let backend = Arc::new(AdvancedMockBackend::new());
    let mut backends = HashMap::new();
    backends.insert("adv_ecu".to_string(), backend as Arc<dyn DiagnosticBackend>);

    let state = AppState::new(backends);
    let router = create_router(state);

    TestServer::start(router)
        .await
        .expect("Failed to start test server")
}

#[tokio::test]
async fn test_string_type() {
    let server = create_advanced_test_server().await;
    let client = &server.client;

    let yaml = r#"
dids:
  0xF190:
    name: VIN
    type: string
    length: 17
"#;
    client.upload_definitions(yaml).await.unwrap();

    let response = client.read_data("adv_ecu", "F190").await.unwrap();
    assert_eq!(response.did.as_deref(), Some("F190"));
    assert_eq!(response.value, "WF0XXXGCDX1234567");
    assert_eq!(response.converted, Some(true));
}

#[tokio::test]
async fn test_enum_type() {
    let server = create_advanced_test_server().await;
    let client = &server.client;

    let yaml = r#"
dids:
  0xF404:
    name: Gear Position
    type: uint8
    enum:
      0: P
      1: R
      2: N
      3: D
      4: S
"#;
    client.upload_definitions(yaml).await.unwrap();

    let response = client.read_data("adv_ecu", "F404").await.unwrap();
    assert_eq!(response.did.as_deref(), Some("F404"));
    assert_eq!(response.value["value"], 3);
    assert_eq!(response.value["label"], "D");
    assert_eq!(response.converted, Some(true));
}

#[tokio::test]
async fn test_bitfield_type() {
    let server = create_advanced_test_server().await;
    let client = &server.client;

    let yaml = r#"
dids:
  0xF410:
    name: Engine Status
    type: uint8
    bits:
      - name: engine_running
        bit: 0
      - name: ac_on
        bit: 1
      - name: check_engine
        bit: 7
"#;
    client.upload_definitions(yaml).await.unwrap();

    let response = client.read_data("adv_ecu", "F410").await.unwrap();
    assert_eq!(response.did.as_deref(), Some("F410"));
    assert_eq!(response.value["engine_running"], true);
    assert_eq!(response.value["ac_on"], false);
    assert_eq!(response.value["check_engine"], true);
    assert_eq!(response.converted, Some(true));
}

#[tokio::test]
async fn test_labeled_array_type() {
    let server = create_advanced_test_server().await;
    let client = &server.client;

    let yaml = r#"
dids:
  0xF421:
    name: Wheel Speeds
    type: uint16
    scale: 0.01
    unit: km/h
    array: 4
    labels: [FL, FR, RL, RR]
"#;
    client.upload_definitions(yaml).await.unwrap();

    let response = client.read_data("adv_ecu", "F421").await.unwrap();
    assert_eq!(response.did.as_deref(), Some("F421"));
    assert_eq!(response.unit.as_deref(), Some("km/h"));
    // Values: FL=100, FR=100.5, RL=99.8, RR=100.2
    assert_eq!(response.value["FL"], 100);
    assert_eq!(response.value["FR"], 100.5);
    assert_eq!(response.value["RL"], 99.8);
    assert_eq!(response.value["RR"], 100.2);
    assert_eq!(response.converted, Some(true));
}

#[tokio::test]
async fn test_2d_map_with_axes() {
    let server = create_advanced_test_server().await;
    let client = &server.client;

    let yaml = r#"
dids:
  0xF500:
    name: Fuel Map
    type: uint8
    scale: 0.1
    unit: ms
    map:
      rows: 2
      cols: 2
      row_axis:
        name: RPM
        unit: rpm
        breakpoints: [1000, 2000]
      col_axis:
        name: Load
        unit: "%"
        breakpoints: [0, 50]
"#;
    client.upload_definitions(yaml).await.unwrap();

    let response = client.read_data("adv_ecu", "F500").await.unwrap();
    assert_eq!(response.did.as_deref(), Some("F500"));
    assert_eq!(response.unit.as_deref(), Some("ms"));

    // Values: [[1.0, 1.1], [1.2, 1.3]]
    let values = &response.value["values"];
    assert_eq!(values[0][0], 1);
    assert_eq!(values[0][1], 1.1);
    assert_eq!(values[1][0], 1.2);
    assert_eq!(values[1][1], 1.3);

    // Axis metadata
    assert_eq!(response.value["row_axis"]["name"], "RPM");
    assert_eq!(response.value["col_axis"]["name"], "Load");
    assert_eq!(response.converted, Some(true));
}

#[tokio::test]
async fn test_little_endian_byte_order() {
    let server = create_advanced_test_server().await;
    let client = &server.client;

    let yaml = r#"
dids:
  0xF440:
    name: Odometer
    type: uint32
    byte_order: little
    scale: 0.1
    unit: km
"#;
    client.upload_definitions(yaml).await.unwrap();

    let response = client.read_data("adv_ecu", "F440").await.unwrap();
    assert_eq!(response.did.as_deref(), Some("F440"));
    assert_eq!(response.unit.as_deref(), Some("km"));
    // Raw LE bytes [0x87, 0xD6, 0x12, 0x00] = 1234567, * 0.1 = 123456.7
    assert_eq!(response.value, 123456.7);
    assert_eq!(response.converted, Some(true));
}

#[tokio::test]
async fn test_histogram_type() {
    let server = create_advanced_test_server().await;
    let client = &server.client;

    let yaml = r#"
dids:
  0xF600:
    name: RPM Histogram
    type: uint32
    unit: seconds
    histogram:
      bins: [0, 1000, 2000, 3000]
      labels: [idle, low, cruise]
      axis_name: RPM
      axis_unit: rpm
"#;
    client.upload_definitions(yaml).await.unwrap();

    let response = client.read_data("adv_ecu", "F600").await.unwrap();
    assert_eq!(response.did.as_deref(), Some("F600"));
    assert_eq!(response.unit.as_deref(), Some("seconds"));

    // Counts should be [100, 200, 300]
    let counts = response.value["counts"].as_array().unwrap();
    assert_eq!(counts[0], 100);
    assert_eq!(counts[1], 200);
    assert_eq!(counts[2], 300);

    // Labels
    let labels = response.value["labels"].as_array().unwrap();
    assert_eq!(labels[0], "idle");
    assert_eq!(labels[1], "low");
    assert_eq!(labels[2], "cruise");

    assert_eq!(response.converted, Some(true));
}

#[tokio::test]
async fn test_load_comprehensive_yaml_file() {
    let server = create_advanced_test_server().await;
    let client = &server.client;

    // Load the full engine_ecu.did.yaml file content
    let yaml = r#"
meta:
  name: Engine ECU
  version: "1.0"
  description: Comprehensive test definitions

dids:
  # Scalar with offset
  0xF405:
    name: Coolant Temperature
    type: uint8
    scale: 1.0
    offset: -40.0
    unit: °C
    min: -40
    max: 215

  # String
  0xF190:
    name: VIN
    type: string
    length: 17

  # Enum
  0xF404:
    name: Gear Position
    type: uint8
    enum:
      0: P
      1: R
      2: N
      3: D

  # Bitfield
  0xF410:
    name: Engine Status
    type: uint8
    bits:
      - name: engine_running
        bit: 0
      - name: mil_on
        bit: 1
      - name: check_engine
        bit: 7

  # Labeled array
  0xF421:
    name: Wheel Speeds
    type: uint16
    scale: 0.01
    unit: km/h
    array: 4
    labels: [FL, FR, RL, RR]

  # 2D Map
  0xF500:
    name: Fuel Map
    type: uint8
    scale: 0.1
    unit: ms
    map:
      rows: 2
      cols: 2
      row_axis:
        name: RPM
        unit: rpm
        breakpoints: [1000, 2000]
      col_axis:
        name: Load
        unit: "%"
        breakpoints: [0, 50]

  # Histogram
  0xF600:
    name: RPM Histogram
    type: uint32
    unit: seconds
    histogram:
      bins: [0, 1000, 2000, 3000]
      labels: [idle, low, cruise]

  # Little-endian
  0xF440:
    name: Odometer
    type: uint32
    byte_order: little
    scale: 0.1
    unit: km
"#;

    let result = client.upload_definitions(yaml).await.unwrap();
    assert_eq!(result.status, "ok");
    assert_eq!(result.loaded, 8);

    // Verify metadata was loaded
    let defs = client.list_definitions().await.unwrap();
    assert_eq!(defs.count, 8);
    assert_eq!(defs.meta.as_ref().unwrap()["name"], "Engine ECU");
    assert_eq!(defs.meta.as_ref().unwrap()["version"], "1.0");
}

#[tokio::test]
async fn test_multi_bit_bitfield() {
    let server = create_advanced_test_server().await;
    let client = &server.client;

    let yaml = r#"
dids:
  0xF414:
    name: Transmission Status
    type: uint16
    bits:
      - name: torque_converter_locked
        bit: 0
      - name: shift_in_progress
        bit: 1
      - name: sport_mode
        bit: 2
      - name: current_gear
        bit: 4
        width: 4
      - name: requested_gear
        bit: 8
        width: 4
"#;
    client.upload_definitions(yaml).await.unwrap();

    let response = client.read_data("adv_ecu", "F414").await.unwrap();
    assert_eq!(response.did.as_deref(), Some("F414"));

    // 0x55 = 0b01010101: TQ=1, shift=0, sport=1, gear=5
    // 0x06 = 0b00000110: req_gear=6
    assert_eq!(response.value["torque_converter_locked"], true);
    assert_eq!(response.value["shift_in_progress"], false);
    assert_eq!(response.value["sport_mode"], true);
    assert_eq!(response.value["current_gear"], 5);
    assert_eq!(response.value["requested_gear"], 6);
    assert_eq!(response.converted, Some(true));
}

// =============================================================================
// Full Workflow Test
// =============================================================================

#[tokio::test]
async fn test_full_workflow_with_client() {
    let server = create_test_server().await;
    let client = &server.client;

    // 1. Health check
    let health = client.health().await.unwrap();
    assert_eq!(health, "OK");

    // 2. List components
    let components = client.list_components().await.unwrap();
    assert_eq!(components.len(), 1);
    assert_eq!(components[0].id, "example_ecu");

    // 3. Upload definitions
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
    let upload_result = client.upload_definitions(yaml).await.unwrap();
    assert_eq!(upload_result.loaded, 2);

    // 4. List parameters
    let params = client.list_parameters("example_ecu").await.unwrap();
    assert_eq!(params.count, 2);

    // 5. Read converted data
    let temp = client.read_data("example_ecu", "F405").await.unwrap();
    assert_eq!(temp.value, 92); // 132 - 40

    // 6. Read VIN
    let vin = client.read_data("example_ecu", "F190").await.unwrap();
    assert_eq!(vin.value, "WF0XXXGCDX1234567");

    // 7. Read with raw mode
    let raw = client.read_data_raw("example_ecu", "F405").await.unwrap();
    assert_eq!(raw.converted, Some(false));

    // 8. Get definition
    let def = client.get_definition("F405").await.unwrap();
    assert_eq!(def.name.as_deref(), Some("Coolant Temperature"));

    // 9. Delete definition
    client.delete_definition("F405").await.unwrap();
    let defs = client.list_definitions().await.unwrap();
    assert_eq!(defs.count, 1);

    // 10. Clear all
    client.clear_definitions().await.unwrap();
    let defs = client.list_definitions().await.unwrap();
    assert_eq!(defs.count, 0);
}
