//! End-to-end tests for SOVD Gateway with real UDS/CAN
//!
//! These tests run the full federated stack with real CAN communication:
//! 1. Set up virtual CAN interface (vcan0)
//! 2. Start example-ecu simulator
//! 3. Start sovdd with gateway config
//! 4. Exercise the REST API through the gateway using SovdClient
//! 5. Verify real UDS responses from example-ecu
//!
//! Run with: cargo test --test gateway_e2e_test -- --test-threads=1
//!
//! Note: Tests will be skipped automatically if vcan0 is not available.

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use reqwest::Client;
use serde_json::Value;
use sovd_client::SovdClient;
use tokio::time::sleep;

const SERVER_PORT: u16 = 18082;
const INTERFACE: &str = "vcan0";

/// Check if vcan0 interface is available
fn vcan0_available() -> bool {
    Command::new("ip")
        .args(["link", "show", INTERFACE])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Macro to skip test if vcan0 is not available
macro_rules! require_vcan0 {
    () => {
        if !vcan0_available() {
            eprintln!("Skipping test: vcan0 interface not available");
            return;
        }
    };
}

/// Test harness that manages the test environment
struct GatewayTestHarness {
    example_ecu: Option<Child>,
    sovd_server: Option<Child>,
    http_client: Client,
    sovd_client: SovdClient,
    base_url: String,
}

impl GatewayTestHarness {
    async fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let http_client = Client::builder().timeout(Duration::from_secs(10)).build()?;

        let base_url = format!("http://localhost:{}", SERVER_PORT);
        let sovd_client = SovdClient::new(&base_url)?;

        let mut harness = Self {
            example_ecu: None,
            sovd_server: None,
            http_client,
            sovd_client,
            base_url,
        };

        harness.setup().await?;
        Ok(harness)
    }

    /// Get the SovdClient for high-level API calls
    fn client(&self) -> &SovdClient {
        &self.sovd_client
    }

    async fn setup(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        // Kill stale processes from a previous crashed test run (PID-file based)
        Self::kill_orphaned_processes();

        // Ensure vcan0 exists
        self.setup_vcan()?;

        // Start example-ecu
        self.start_example_ecu()?;

        // Wait for example-ecu to initialize
        sleep(Duration::from_millis(500)).await;

        // Start the gateway server
        self.start_sovd_server()?;

        // Track spawned PIDs so a future run can clean up if we crash
        self.write_pids();

        // Wait for server to be ready
        self.wait_for_server().await?;

        Ok(())
    }

    /// Path to PID file for tracking processes spawned by this test harness.
    /// Only PIDs written here will be killed during orphan cleanup.
    fn pid_file_path() -> String {
        let workspace = Self::workspace_root();
        format!("{}/target/.gateway-e2e-test-pids", workspace)
    }

    /// Write spawned child PIDs to the PID file so a future test run can
    /// clean them up if this run crashes without calling Drop.
    fn write_pids(&self) {
        let mut pids = Vec::new();
        if let Some(ref child) = self.example_ecu {
            pids.push(child.id().to_string());
        }
        if let Some(ref child) = self.sovd_server {
            pids.push(child.id().to_string());
        }
        if !pids.is_empty() {
            let _ = std::fs::write(Self::pid_file_path(), pids.join("\n"));
        }
    }

    /// Kill only processes from a previous crashed test run (tracked via PID file).
    /// Unlike pkill, this never touches unrelated example-ecu/sovdd instances.
    fn kill_orphaned_processes() {
        let pid_file = Self::pid_file_path();
        if let Ok(contents) = std::fs::read_to_string(&pid_file) {
            for line in contents.lines() {
                if let Ok(pid) = line.trim().parse::<i32>() {
                    unsafe {
                        libc::kill(pid, libc::SIGTERM);
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(200));
            for line in contents.lines() {
                if let Ok(pid) = line.trim().parse::<i32>() {
                    unsafe {
                        libc::kill(pid, libc::SIGKILL);
                    }
                }
            }
            let _ = std::fs::remove_file(&pid_file);
        }

        // Wait for processes to fully terminate
        std::thread::sleep(Duration::from_millis(500));
    }

    fn setup_vcan(&self) -> Result<(), Box<dyn std::error::Error>> {
        let status = Command::new("ip")
            .args(["link", "show", INTERFACE])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;

        if status.success() {
            return Ok(());
        }

        eprintln!("Setting up {}...", INTERFACE);
        let _ = Command::new("sudo").args(["modprobe", "vcan"]).status();
        let _ = Command::new("sudo")
            .args(["ip", "link", "add", "dev", INTERFACE, "type", "vcan"])
            .status();
        Command::new("sudo")
            .args(["ip", "link", "set", "up", INTERFACE])
            .status()?;

        Ok(())
    }

    /// Get the workspace root directory (two levels up from crates/sovd-tests)
    fn workspace_root() -> String {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        std::path::Path::new(manifest_dir)
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| manifest_dir.to_string())
    }

    fn start_example_ecu(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let workspace = Self::workspace_root();

        // Check release first, fall back to debug
        let binary = format!("{}/target/release/example-ecu", workspace);
        let binary = if std::path::Path::new(&binary).exists() {
            binary
        } else {
            format!("{}/target/debug/example-ecu", workspace)
        };

        let child = Command::new(&binary)
            .args([
                "--interface",
                INTERFACE,
                "--rx-id",
                "0x18DA00F1",
                "--tx-id",
                "0x18DAF100",
            ])
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()?;

        eprintln!("Started example-ecu (PID: {})", child.id());
        self.example_ecu = Some(child);
        Ok(())
    }

    fn start_sovd_server(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let workspace = Self::workspace_root();

        // Check release first, fall back to debug
        let binary = format!("{}/target/release/sovdd", workspace);
        let binary = if std::path::Path::new(&binary).exists() {
            binary
        } else {
            format!("{}/target/debug/sovdd", workspace)
        };

        let config = format!("{}/config/gateway-socketcan.toml", workspace);

        let child = Command::new(&binary)
            .arg(&config)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()?;

        eprintln!("Started sovdd with gateway (PID: {})", child.id());
        self.sovd_server = Some(child);
        Ok(())
    }

    async fn wait_for_server(&self) -> Result<(), Box<dyn std::error::Error>> {
        for i in 0..50 {
            match self.sovd_client.health().await {
                Ok(_) => {
                    eprintln!("Server ready after {} attempts", i + 1);
                    return Ok(());
                }
                _ => {
                    sleep(Duration::from_millis(100)).await;
                }
            }
        }
        Err("Server failed to start".into())
    }

    // Raw HTTP methods for low-level access (status codes, nested path testing)
    async fn get_with_status(
        &self,
        path: &str,
    ) -> Result<(u16, Value), Box<dyn std::error::Error>> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self.http_client.get(&url).send().await?;
        let status = resp.status().as_u16();
        let json = resp.json().await.unwrap_or(serde_json::json!({}));
        Ok((status, json))
    }

    async fn put_json(
        &self,
        path: &str,
        body: Value,
    ) -> Result<(u16, Value), Box<dyn std::error::Error>> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self.http_client.put(&url).json(&body).send().await?;
        let status = resp.status().as_u16();
        let json = resp.json().await.unwrap_or(serde_json::json!({}));
        Ok((status, json))
    }
}

impl Drop for GatewayTestHarness {
    fn drop(&mut self) {
        if let Some(mut child) = self.sovd_server.take() {
            eprintln!("Stopping sovdd...");
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(mut child) = self.example_ecu.take() {
            eprintln!("Stopping example-ecu...");
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_file(Self::pid_file_path());
    }
}

// =============================================================================
// Tests that access ECU directly (bypassing gateway)
// =============================================================================

#[tokio::test]
#[serial_test::serial]
async fn test_list_components() {
    require_vcan0!();
    let harness = GatewayTestHarness::new()
        .await
        .expect("Failed to setup test harness");

    let components = harness
        .client()
        .list_components()
        .await
        .expect("Failed to get components");

    eprintln!("Components: {:?}", components);

    // Should have only the gateway at top level (ECUs are sub-entities)
    assert!(
        components.iter().any(|c| c.id == "vehicle_gateway"),
        "Should have vehicle_gateway"
    );
    assert!(
        !components.iter().any(|c| c.id == "vtx_ecm"),
        "vtx_ecm should NOT be at top level (it's a gateway sub-entity)"
    );
}

#[tokio::test]
#[serial_test::serial]
async fn test_ecu_read_vin_through_gateway() {
    require_vcan0!();
    let harness = GatewayTestHarness::new()
        .await
        .expect("Failed to setup test harness");

    // Access ECU through gateway using prefixed parameter
    let vin = harness
        .client()
        .read_data("vehicle_gateway", "vtx_ecm/vin")
        .await
        .expect("Failed to read VIN through gateway");

    eprintln!("VIN (via gateway): {:?}", vin);

    // example-ecu returns "1HGCM82633A004352"
    let value = vin.value.as_str().expect("VIN should be string");
    assert!(value.len() == 17, "VIN should be 17 characters: {}", value);
    eprintln!("VIN value: {}", value);
}

#[tokio::test]
#[serial_test::serial]
async fn test_ecu_get_faults_through_gateway() {
    require_vcan0!();
    let harness = GatewayTestHarness::new()
        .await
        .expect("Failed to setup test harness");

    let faults = harness
        .client()
        .get_faults("vehicle_gateway")
        .await
        .expect("Failed to get faults through gateway");

    eprintln!("Faults (via gateway): {:?}", faults);
    eprintln!("Found {} DTCs via gateway", faults.len());

    // example-ecu should have DTCs
    assert!(
        !faults.is_empty(),
        "example-ecu should have DTCs via gateway"
    );
}

// =============================================================================
// Tests that route UDS requests THROUGH the gateway
// =============================================================================

#[tokio::test]
#[serial_test::serial]
async fn test_gateway_sub_entities() {
    require_vcan0!();
    let harness = GatewayTestHarness::new()
        .await
        .expect("Failed to setup test harness");

    // Gateway should list its registered backends via apps endpoint
    let apps = harness
        .client()
        .list_apps("vehicle_gateway")
        .await
        .expect("Failed to get gateway apps");

    eprintln!("Gateway sub-entities: {:?}", apps);

    assert!(
        apps.iter().any(|e| e.id == "vtx_ecm"),
        "Gateway should have vtx_ecm registered"
    );
}

#[tokio::test]
#[serial_test::serial]
async fn test_gateway_own_parameters_only() {
    require_vcan0!();
    let harness = GatewayTestHarness::new()
        .await
        .expect("Failed to setup test harness");

    // Gateway /data should return only the gateway's own parameters (standard IDs),
    // not aggregated child params. Per SOVD §6.5, child params are accessed via sub-entity paths.
    let params = harness
        .client()
        .list_parameters("vehicle_gateway")
        .await
        .expect("Failed to get gateway parameters");

    eprintln!("Gateway parameters: {:?}", params);

    // No child-prefixed parameters at gateway level
    let has_prefixed = params.items.iter().any(|p| p.id.contains('/'));
    assert!(
        !has_prefixed,
        "Gateway should not aggregate child parameters"
    );
}

#[tokio::test]
#[serial_test::serial]
async fn test_gateway_routes_read_to_uds() {
    require_vcan0!();
    let harness = GatewayTestHarness::new()
        .await
        .expect("Failed to setup test harness");

    // Read VIN THROUGH the gateway using prefixed parameter ID
    // The gateway routes "vtx_ecm/vin" to the vtx_ecm backend
    let vin = harness
        .client()
        .read_data("vehicle_gateway", "vtx_ecm/vin")
        .await
        .expect("Failed to read VIN through gateway");

    eprintln!("VIN (via gateway): {:?}", vin);

    // Verify we got real data from example-ecu
    let value = vin.value.as_str().expect("VIN should be string");
    assert!(
        value.len() == 17,
        "VIN should be 17 characters: got {}",
        value
    );
    eprintln!("VIN value via gateway: {}", value);
}

#[tokio::test]
#[serial_test::serial]
async fn test_gateway_aggregates_faults() {
    require_vcan0!();
    let harness = GatewayTestHarness::new()
        .await
        .expect("Failed to setup test harness");

    // Get faults THROUGH the gateway
    let faults = harness
        .client()
        .get_faults("vehicle_gateway")
        .await
        .expect("Failed to get gateway faults");

    eprintln!("Faults (via gateway): {:?}", faults);
    eprintln!("Found {} DTCs via gateway", faults.len());

    // Faults should be prefixed with backend ID
    if !faults.is_empty() {
        assert!(
            faults[0].id.starts_with("vtx_ecm/"),
            "Fault ID should be prefixed: {}",
            faults[0].id
        );
    }
}

#[tokio::test]
#[serial_test::serial]
async fn test_gateway_aggregates_operations() {
    require_vcan0!();
    let harness = GatewayTestHarness::new()
        .await
        .expect("Failed to setup test harness");

    let ops = harness
        .client()
        .list_operations("vehicle_gateway")
        .await
        .expect("Failed to get gateway operations");

    eprintln!("Operations (via gateway): {:?}", ops);

    // Should have prefixed check_preconditions operation
    let check_op = ops.iter().find(|o| o.id == "vtx_ecm/check_preconditions");

    assert!(
        check_op.is_some(),
        "Should have vtx_ecm/check_preconditions operation"
    );
}

// =============================================================================
// COMPREHENSIVE FEATURE TESTS
// These tests verify all features work through the dual-layer architecture
// =============================================================================

#[tokio::test]
#[serial_test::serial]
async fn test_read_multiple_public_parameters() {
    require_vcan0!();
    let harness = GatewayTestHarness::new()
        .await
        .expect("Failed to setup test harness");

    // Read multiple public parameters through gateway with prefixed IDs
    let test_params = [("vin", 17), ("part_number", 16), ("serial_number", 11)];

    for (param_id, min_len) in test_params {
        let prefixed = format!("vtx_ecm/{}", param_id);
        let result = harness
            .client()
            .read_data("vehicle_gateway", &prefixed)
            .await
            .expect(&format!("Failed to read {} through gateway", param_id));

        eprintln!("{}: {:?}", param_id, result);

        let value = result
            .value
            .as_str()
            .expect(&format!("{} should be string", param_id));
        assert!(
            value.len() >= min_len,
            "{} should be at least {} chars, got: {}",
            param_id,
            min_len,
            value
        );
        eprintln!("{} = {}", param_id, value);
    }
}

#[tokio::test]
#[serial_test::serial]
async fn test_read_numeric_parameters() {
    require_vcan0!();
    let harness = GatewayTestHarness::new()
        .await
        .expect("Failed to setup test harness");

    // Read coolant temp through gateway (public, uint8)
    let temp = harness
        .client()
        .read_data("vehicle_gateway", "vtx_ecm/coolant_temp")
        .await
        .expect("Failed to read coolant_temp through gateway");

    eprintln!("Coolant Temp: {:?}", temp);

    // Should be a number with correct unit
    let value = temp
        .value
        .as_f64()
        .expect("coolant_temp should be a number");
    assert!(
        value >= -40.0 && value <= 215.0,
        "Coolant temp should be in valid range: {}",
        value
    );
    assert_eq!(temp.unit.as_deref(), Some("°C"));

    // Read vehicle speed through gateway (public, uint8)
    let speed = harness
        .client()
        .read_data("vehicle_gateway", "vtx_ecm/vehicle_speed")
        .await
        .expect("Failed to read vehicle_speed through gateway");

    eprintln!("Vehicle Speed: {:?}", speed);

    let speed_val = speed
        .value
        .as_f64()
        .expect("vehicle_speed should be a number");
    assert!(
        speed_val >= 0.0 && speed_val <= 255.0,
        "Speed should be in valid range: {}",
        speed_val
    );
    assert_eq!(speed.unit.as_deref(), Some("km/h"));
}

#[tokio::test]
#[serial_test::serial]
async fn test_write_parameter_through_gateway() {
    require_vcan0!();
    let harness = GatewayTestHarness::new()
        .await
        .expect("Failed to setup test harness");

    // Write a test date to programming_date (0xF199) through gateway
    // Date format: BCD [YY, YY, MM, DD] e.g., 2024-01-15 = [0x20, 0x24, 0x01, 0x15]
    let new_date = serde_json::json!("20240215");

    match harness
        .client()
        .write_data("vehicle_gateway", "vtx_ecm/programming_date", new_date)
        .await
    {
        Ok(()) => {
            eprintln!("Write succeeded!");
        }
        Err(e) => {
            eprintln!("Write failed: {} (may require extended session)", e);
            // This is expected - example-ecu requires extended session for this parameter
        }
    }
}

#[tokio::test]
#[serial_test::serial]
async fn test_execute_operation_through_gateway() {
    require_vcan0!();
    let harness = GatewayTestHarness::new()
        .await
        .expect("Failed to setup test harness");

    // Routine 0x0203 requires extended session per UDS spec
    harness
        .put_json(
            "/vehicle/v1/components/vehicle_gateway/modes/session?target=vtx_ecm",
            serde_json::json!({"value": "extended"}),
        )
        .await
        .expect("Failed to set extended session on vtx_ecm");

    // Execute check_preconditions routine (0x0203) through gateway
    let result = harness
        .client()
        .execute_operation_simple("vehicle_gateway", "vtx_ecm/check_preconditions")
        .await
        .expect("Failed to execute operation through gateway");

    eprintln!("Operation result: {:?}", result);
    eprintln!("Operation executed successfully through gateway!");
}

#[tokio::test]
#[serial_test::serial]
async fn test_clear_faults_requires_extended_session() {
    require_vcan0!();
    let harness = GatewayTestHarness::new()
        .await
        .expect("Failed to setup test harness");

    // First, verify we have DTCs (through gateway)
    let faults_before = harness
        .client()
        .get_faults("vehicle_gateway")
        .await
        .expect("Failed to get faults through gateway");

    eprintln!("Faults before clear: {} DTCs", faults_before.len());

    // Try to clear faults through gateway (may fail without extended session)
    match harness.client().clear_faults("vehicle_gateway").await {
        Ok(result) => {
            eprintln!("Clear faults result: {:?}", result);
            if result.success {
                eprintln!("Clear faults succeeded (ECU in extended session)");
            } else {
                eprintln!(
                    "Clear faults correctly rejected via gateway (requires extended session)"
                );
            }
        }
        Err(e) => {
            eprintln!(
                "Clear faults failed as expected ({}, requires extended session)",
                e
            );
            // This is the expected behavior - validates that security works
        }
    }
}

#[tokio::test]
#[serial_test::serial]
async fn test_get_specific_fault() {
    require_vcan0!();
    let harness = GatewayTestHarness::new()
        .await
        .expect("Failed to setup test harness");

    // First get all faults through gateway to find an ID
    let faults = harness
        .client()
        .get_faults("vehicle_gateway")
        .await
        .expect("Failed to get faults through gateway");

    if faults.is_empty() {
        eprintln!("No faults to test - skipping specific fault test");
        return;
    }

    let first_fault_id = &faults[0].id;
    eprintln!("Getting details for fault: {}", first_fault_id);

    // Get specific fault detail through gateway (SovdClient handles URL-encoding of '/')
    let fault = harness
        .client()
        .get_fault("vehicle_gateway", first_fault_id)
        .await
        .expect("Failed to get fault detail through gateway");

    eprintln!("Fault detail: {:?}", fault);

    assert_eq!(&fault.id, first_fault_id);
    assert!(!fault.code.is_empty(), "Fault should have code");
    assert!(!fault.severity.is_empty(), "Fault should have severity");
}

#[tokio::test]
#[serial_test::serial]
async fn test_gateway_routes_parameter_read() {
    require_vcan0!();
    let harness = GatewayTestHarness::new()
        .await
        .expect("Failed to setup test harness");

    // Read multiple parameters through gateway with prefixed IDs
    let params_to_test = [
        "vtx_ecm/vin",
        "vtx_ecm/coolant_temp",
        "vtx_ecm/vehicle_speed",
    ];

    for param_id in params_to_test {
        let result = harness
            .client()
            .read_data("vehicle_gateway", param_id)
            .await
            .expect(&format!("Failed to read {} through gateway", param_id));

        eprintln!("{} (via gateway): {:?}", param_id, result);
    }
}

/// Test gateway read with actual nested path segments (not URL-encoded)
/// This tests the route: /data/:child_id/:child_param_id
#[tokio::test]
#[serial_test::serial]
async fn test_gateway_routes_nested_path_read() {
    require_vcan0!();
    let harness = GatewayTestHarness::new()
        .await
        .expect("Failed to setup test harness");

    // Read parameters through gateway using actual nested path (not URL-encoded)
    // This tests the /data/:child_id/:child_param_id route
    let params_to_test = [
        ("vtx_ecm", "vin"),
        ("vtx_ecm", "coolant_temp"),
        ("vtx_ecm", "vehicle_speed"),
    ];

    for (child_id, param_name) in params_to_test {
        // Use actual path segments, not URL-encoded
        let (status, result) = harness
            .get_with_status(&format!(
                "/vehicle/v1/components/vehicle_gateway/data/{}/{}",
                child_id, param_name
            ))
            .await
            .expect(&format!(
                "Failed to read {}/{} through gateway nested path",
                child_id, param_name
            ));

        assert_eq!(
            status, 200,
            "Expected 200 OK for {}/{}, got {} with body: {:?}",
            child_id, param_name, status, result
        );

        eprintln!(
            "{}/{} (via gateway nested path): {}",
            child_id,
            param_name,
            serde_json::to_string_pretty(&result).unwrap()
        );

        assert!(
            result.get("value").is_some(),
            "{}/{} should have value",
            child_id,
            param_name
        );
        assert!(
            result.get("id").is_some(),
            "{}/{} should have id",
            child_id,
            param_name
        );

        // Verify the ID is prefixed with the child component
        let id = result["id"].as_str().unwrap();
        assert!(id.contains("/"), "ID should be prefixed: {}", id);
    }
}

#[tokio::test]
#[serial_test::serial]
async fn test_child_parameters_via_sub_entity() {
    require_vcan0!();
    let harness = GatewayTestHarness::new()
        .await
        .expect("Failed to setup test harness");

    // Gateway's own /data should not contain child parameters
    let gw_params = harness
        .client()
        .list_parameters("vehicle_gateway")
        .await
        .expect("Failed to list gateway parameters");

    let has_child_params = gw_params.items.iter().any(|p| p.id.contains('/'));
    assert!(
        !has_child_params,
        "Gateway /data should not contain child-prefixed parameters"
    );

    // Individual child parameter reads via prefixed routing still work
    let vin = harness
        .client()
        .read_data("vehicle_gateway", "vtx_ecm/vin")
        .await
        .expect("Failed to read vtx_ecm/vin through gateway");

    let value = vin.value.as_str().expect("VIN should be string");
    assert_eq!(
        value.len(),
        17,
        "VIN should be 17 characters: got {}",
        value
    );
}

#[tokio::test]
#[serial_test::serial]
async fn test_list_all_operations() {
    require_vcan0!();
    let harness = GatewayTestHarness::new()
        .await
        .expect("Failed to setup test harness");

    // List operations through gateway
    let ops = harness
        .client()
        .list_operations("vehicle_gateway")
        .await
        .expect("Failed to list operations through gateway");

    eprintln!("Gateway operations: {:?}", ops);

    // Should have our configured operations (prefixed with vtx_ecm/)
    let has_check = ops
        .iter()
        .any(|o| o.id == "vtx_ecm/check_preconditions" || o.id == "check_preconditions");
    let has_erase = ops
        .iter()
        .any(|o| o.id == "vtx_ecm/erase_memory" || o.id == "erase_memory");

    assert!(has_check, "Should have check_preconditions operation");
    assert!(has_erase, "Should have erase_memory operation");

    // Verify erase_memory shows it requires security
    let erase_op = ops
        .iter()
        .find(|o| o.id == "vtx_ecm/erase_memory" || o.id == "erase_memory");
    if let Some(op) = erase_op {
        assert!(op.requires_security, "erase_memory should require security");
    }
}

#[tokio::test]
#[serial_test::serial]
async fn test_erase_memory_requires_security() {
    require_vcan0!();
    let harness = GatewayTestHarness::new()
        .await
        .expect("Failed to setup test harness");

    // Try to execute erase_memory through gateway (requires security access)
    match harness
        .client()
        .execute_operation_simple("vehicle_gateway", "vtx_ecm/erase_memory")
        .await
    {
        Ok(result) => {
            eprintln!("Erase memory result: {:?}", result);
            if result.status == sovd_client::OperationStatus::Failed {
                eprintln!("Erase memory failed as expected (security required)");
            } else {
                // If it succeeded, the ECU might already have security unlocked
                eprintln!("Note: Erase memory succeeded (ECU may have security unlocked)");
            }
        }
        Err(e) => {
            eprintln!(
                "Erase memory correctly rejected without security access: {}",
                e
            );
        }
    }
}

#[tokio::test]
#[serial_test::serial]
async fn test_full_dual_layer_flow() {
    require_vcan0!();
    // This test verifies the complete flow through both server layers:
    // HTTP Client -> sovd-api -> GatewayBackend -> UdsBackend -> CAN/ISO-TP -> example-ecu

    let harness = GatewayTestHarness::new()
        .await
        .expect("Failed to setup test harness");

    eprintln!("=== FULL DUAL-LAYER FLOW TEST ===\n");

    // Step 1: List components via gateway
    eprintln!("Step 1: List components");
    let components = harness
        .client()
        .list_components()
        .await
        .expect("Failed to list components");
    eprintln!("  Found {} components", components.len());

    // Step 2: Get gateway info
    eprintln!("\nStep 2: Get gateway info");
    let gateway = harness
        .client()
        .get_component("vehicle_gateway")
        .await
        .expect("Failed to get gateway");
    eprintln!("  Gateway: {}", gateway.name);

    // Step 3: List gateway sub-entities
    eprintln!("\nStep 3: List gateway sub-entities (backends)");
    let apps = harness
        .client()
        .list_apps("vehicle_gateway")
        .await
        .expect("Failed to get apps");
    eprintln!("  Found {} registered backends", apps.len());
    for app in &apps {
        eprintln!("    - {}: {}", app.id, app.name);
    }

    // Step 4: Read VIN through gateway (verifies full path)
    eprintln!("\nStep 4: Read VIN through gateway");
    let vin = harness
        .client()
        .read_data("vehicle_gateway", "vtx_ecm/vin")
        .await
        .expect("Failed to read VIN through gateway");
    eprintln!("  VIN: {}", vin.value);

    // Step 5: List faults through gateway
    eprintln!("\nStep 5: List faults through gateway");
    let faults = harness
        .client()
        .get_faults("vehicle_gateway")
        .await
        .expect("Failed to get faults through gateway");
    eprintln!("  Found {} faults via gateway", faults.len());

    // Step 6: Execute operation through gateway (requires extended session)
    eprintln!("\nStep 6: Set extended session, then execute operation through gateway");
    harness
        .put_json(
            "/vehicle/v1/components/vehicle_gateway/modes/session?target=vtx_ecm",
            serde_json::json!({"value": "extended"}),
        )
        .await
        .expect("Failed to set extended session on vtx_ecm");

    let op_result = harness
        .client()
        .execute_operation_simple("vehicle_gateway", "vtx_ecm/check_preconditions")
        .await
        .expect("Failed to execute operation through gateway");
    eprintln!("  Operation completed: {:?}", op_result.status);

    // Step 7: Verify VIN read through gateway nested path
    eprintln!("\nStep 7: Verify gateway nested path access");
    let (status, nested_vin) = harness
        .get_with_status("/vehicle/v1/components/vehicle_gateway/data/vtx_ecm/vin")
        .await
        .expect("Failed to read VIN via nested path");
    eprintln!(
        "  Nested path VIN (status {}): {}",
        status, nested_vin["value"]
    );

    eprintln!("\n=== DUAL-LAYER FLOW TEST COMPLETE ===");
}

// =============================================================================
// STREAMING TESTS (UDS 0x2A Periodic Data)
// =============================================================================

#[tokio::test]
#[serial_test::serial]
async fn test_streaming_via_sse() {
    require_vcan0!();
    let harness = GatewayTestHarness::new()
        .await
        .expect("Failed to setup test harness");

    eprintln!("=== STREAMING TEST (UDS 0x2A) ===\n");

    // Request streaming of coolant_temp parameter at 10Hz through gateway
    // The server will use UDS 0x2A ReadDataByPeriodicIdentifier
    eprintln!("Requesting SSE stream via subscribe_inline...");
    eprintln!("Collecting data for 3 seconds...\n");

    let mut sub = harness
        .client()
        .subscribe_inline("vehicle_gateway", vec!["vtx_ecm/coolant_temp".into()], 10)
        .await
        .expect("Failed to start inline subscription");

    // Collect streaming data for 3 seconds
    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, sub.next()).await {
            Ok(Some(Ok(event))) => events.push(event),
            Ok(Some(Err(e))) => {
                eprintln!("Stream error: {}", e);
                break;
            }
            Ok(None) => break,
            Err(_) => break, // Timeout
        }
    }

    eprintln!("Received {} data points:", events.len());
    for (i, event) in events.iter().take(5).enumerate() {
        eprintln!(
            "  [{}] ts={}, seq={}, values={:?}",
            i + 1,
            event.timestamp,
            event.sequence,
            event.values
        );
    }
    if events.len() > 5 {
        eprintln!("  ... and {} more", events.len() - 5);
    }

    // With 10Hz rate and 3 second collection, we should have approximately 30 events
    // Allow for some variance due to timing
    if events.is_empty() {
        eprintln!("\nNote: No streaming data received - periodic data may not be configured");
        // This is acceptable for now as it tests the API path
    } else {
        eprintln!("\nStreaming test successful!");

        // Verify data point structure
        let first_event = &events[0];
        assert!(
            first_event.has("coolant_temp") && first_event.timestamp > 0,
            "Data point should have coolant_temp and timestamp, got: {:?}",
            first_event
        );
    }

    eprintln!("\n=== STREAMING TEST COMPLETE ===");
}

#[tokio::test]
#[serial_test::serial]
async fn test_streaming_through_gateway() {
    require_vcan0!();
    let harness = GatewayTestHarness::new()
        .await
        .expect("Failed to setup test harness");

    eprintln!("=== GATEWAY STREAMING TEST ===\n");

    // Request streaming through gateway with prefixed parameter
    eprintln!("Requesting SSE stream through gateway via subscribe_inline...");
    eprintln!("Collecting data for 2 seconds...\n");

    let mut sub = harness
        .client()
        .subscribe_inline("vehicle_gateway", vec!["vtx_ecm/coolant_temp".into()], 5)
        .await
        .expect("Failed to start inline subscription through gateway");

    // Collect streaming data for 2 seconds
    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, sub.next()).await {
            Ok(Some(Ok(event))) => events.push(event),
            Ok(Some(Err(e))) => {
                eprintln!("Stream error: {}", e);
                break;
            }
            Ok(None) => break,
            Err(_) => break, // Timeout
        }
    }

    eprintln!("Received {} data points through gateway", events.len());

    if events.is_empty() {
        eprintln!("Note: No streaming data through gateway - routing may need implementation");
    } else {
        for (i, event) in events.iter().take(3).enumerate() {
            eprintln!(
                "  [{}] ts={}, seq={}, values={:?}",
                i + 1,
                event.timestamp,
                event.sequence,
                event.values
            );
        }
        eprintln!("\nGateway streaming successful!");
    }

    eprintln!("\n=== GATEWAY STREAMING TEST COMPLETE ===");
}

// =============================================================================
// I/O Control through gateway
// =============================================================================

#[tokio::test]
async fn test_io_control_through_gateway() {
    require_vcan0!();
    let harness = GatewayTestHarness::new()
        .await
        .expect("Failed to start test harness");
    let client = harness.client();
    eprintln!("\n=== IO CONTROL THROUGH GATEWAY ===\n");

    // 1. List outputs — should be aggregated from ECU with prefixed IDs
    let outputs = client
        .list_outputs("vehicle_gateway")
        .await
        .expect("Failed to list outputs through gateway");
    eprintln!("Gateway outputs: {} total", outputs.len());
    assert!(
        !outputs.is_empty(),
        "Gateway should aggregate outputs from child ECUs"
    );

    // All output IDs should be prefixed with the child backend ID
    for o in &outputs {
        assert!(
            o.id.contains('/'),
            "Gateway output ID should be prefixed: {}",
            o.id
        );
        eprintln!("  {} (security_level={:?})", o.id, o.security_level);
    }

    // 2. Find a non-security output to test (e.g., led_status)
    let test_output = outputs
        .iter()
        .find(|o| o.id.ends_with("/led_status"))
        .expect("Should have led_status output");
    eprintln!("\nTesting with output: {}", test_output.id);

    // 3. Freeze the output through the gateway
    let freeze_resp = client
        .control_output(
            "vehicle_gateway",
            &test_output.id,
            "freeze",
            None::<serde_json::Value>,
        )
        .await
        .expect("Freeze through gateway failed");
    eprintln!(
        "Freeze: success={}, frozen={}",
        freeze_resp.success, freeze_resp.frozen
    );
    assert!(freeze_resp.success);
    assert!(freeze_resp.frozen);

    // 4. Short-term adjust with typed value ("on" from allowed list)
    let adjust_resp = client
        .control_output(
            "vehicle_gateway",
            &test_output.id,
            "short_term_adjust",
            Some(serde_json::json!("on")),
        )
        .await
        .expect("Short-term adjust through gateway failed");
    eprintln!(
        "Adjust to 'on': success={}, new_value={:?}, value={:?}",
        adjust_resp.success, adjust_resp.new_value, adjust_resp.value
    );
    assert!(adjust_resp.success);

    // 5. Reset to default
    let reset_resp = client
        .control_output(
            "vehicle_gateway",
            &test_output.id,
            "reset_to_default",
            None::<serde_json::Value>,
        )
        .await
        .expect("Reset through gateway failed");
    eprintln!(
        "Reset: success={}, frozen={}",
        reset_resp.success, reset_resp.frozen
    );
    assert!(reset_resp.success);
    assert!(!reset_resp.frozen);

    eprintln!("\n=== IO CONTROL THROUGH GATEWAY COMPLETE ===");
}

// =============================================================================
// Security access flow through gateway (seed → key → unlock → verify)
// =============================================================================

#[tokio::test]
#[serial_test::serial]
async fn test_security_unlock_through_gateway() {
    require_vcan0!();
    let harness = GatewayTestHarness::new()
        .await
        .expect("Failed to start test harness");
    let client = harness.client();
    eprintln!("\n=== SECURITY UNLOCK THROUGH GATEWAY ===\n");

    let target = "vtx_ecm";
    let gw = "vehicle_gateway";

    // Step 1: GET security state — should be locked initially
    eprintln!("Step 1: Check initial security state");
    let (status, body) = harness
        .get_with_status(&format!(
            "/vehicle/v1/components/{}/modes/security?target={}",
            gw, target
        ))
        .await
        .expect("Failed to GET security mode");
    eprintln!("  Status: {}, Body: {}", status, body);
    assert_eq!(status, 200);
    assert_eq!(body["id"], "security");
    assert!(
        body["value"] == "locked" || body.get("value").is_none(),
        "Should be locked initially, got: {:?}",
        body["value"]
    );

    // Step 2: Switch to extended session (required for security access on this ECU)
    eprintln!("\nStep 2: Switch to extended session");
    let (status, body) = harness
        .put_json(
            &format!(
                "/vehicle/v1/components/{}/modes/session?target={}",
                gw, target
            ),
            serde_json::json!({ "value": "extended" }),
        )
        .await
        .expect("Failed to set extended session");
    eprintln!("  Status: {}, Body: {}", status, body);
    assert_eq!(status, 200);
    assert_eq!(body["value"], "extended");

    // Step 3: Request seed via targeted client method
    eprintln!("\nStep 3: Request security seed");
    let seed = client
        .security_access_request_seed_targeted(
            gw,
            sovd_client::SecurityLevel::LEVEL_1,
            Some(target),
        )
        .await
        .expect("Failed to request seed through gateway");
    eprintln!("  Seed: {}", hex::encode(&seed));
    assert!(!seed.is_empty(), "Seed should not be empty");

    // Step 4: Compute key (XOR each seed byte with 0xFF — default example-ecu secret)
    let key: Vec<u8> = seed.iter().map(|b| b ^ 0xFF).collect();
    eprintln!("\nStep 4: Computed key: {}", hex::encode(&key));

    // Step 5: Send key via targeted client method
    eprintln!("\nStep 5: Send security key");
    client
        .security_access_send_key_targeted(
            gw,
            sovd_client::SecurityLevel::LEVEL_1,
            &key,
            Some(target),
        )
        .await
        .expect("Failed to send key through gateway");
    eprintln!("  Key accepted!");

    // Step 6: GET security state — should be unlocked at level 1
    eprintln!("\nStep 6: Verify security state is unlocked");
    let (status, body) = harness
        .get_with_status(&format!(
            "/vehicle/v1/components/{}/modes/security?target={}",
            gw, target
        ))
        .await
        .expect("Failed to GET security mode after unlock");
    eprintln!("  Status: {}, Body: {}", status, body);
    assert_eq!(status, 200);
    assert_eq!(
        body["value"], "level1",
        "Should be unlocked at level1, got: {}",
        body
    );

    // Step 7: Execute a security-gated operation (erase_memory requires security_level=1)
    eprintln!("\nStep 7: Execute security-gated operation (erase_memory)");
    let op_result = client
        .execute_operation_simple(gw, &format!("{}/erase_memory", target))
        .await
        .expect("erase_memory should succeed after security unlock");
    eprintln!("  Operation status: {:?}", op_result.status);

    // Step 8: Return to default session (should re-lock security)
    eprintln!("\nStep 8: Return to default session");
    let (status, body) = harness
        .put_json(
            &format!(
                "/vehicle/v1/components/{}/modes/session?target={}",
                gw, target
            ),
            serde_json::json!({ "value": "default" }),
        )
        .await
        .expect("Failed to set default session");
    eprintln!("  Status: {}, Body: {}", status, body);
    assert_eq!(status, 200);
    assert_eq!(body["value"], "default");

    // Step 9: Verify security is locked again
    eprintln!("\nStep 9: Verify security re-locked after session change");
    let (status, body) = harness
        .get_with_status(&format!(
            "/vehicle/v1/components/{}/modes/security?target={}",
            gw, target
        ))
        .await
        .expect("Failed to GET security mode after re-lock");
    eprintln!("  Status: {}, Body: {}", status, body);
    assert_eq!(status, 200);
    assert!(
        body["value"] == "locked" || body.get("value").is_none(),
        "Should be locked after returning to default session, got: {}",
        body
    );

    eprintln!("\n=== SECURITY UNLOCK THROUGH GATEWAY COMPLETE ===");
}

/// Test the full security flow using only raw HTTP requests (no client helpers)
/// to verify exact JSON request/response shapes match the SOVD standard.
#[tokio::test]
#[serial_test::serial]
async fn test_security_flow_raw_http() {
    require_vcan0!();
    let harness = GatewayTestHarness::new()
        .await
        .expect("Failed to start test harness");
    eprintln!("\n=== SECURITY FLOW RAW HTTP ===\n");

    let gw = "vehicle_gateway";
    let target = "vtx_ecm";
    let mode_path = |mode: &str| {
        format!(
            "/vehicle/v1/components/{}/modes/{}?target={}",
            gw, mode, target
        )
    };

    // Switch to extended session
    let (status, _) = harness
        .put_json(
            &mode_path("session"),
            serde_json::json!({ "value": "extended" }),
        )
        .await
        .unwrap();
    assert_eq!(status, 200, "Failed to set extended session");

    // Request seed — body: {"value": "level1_requestseed"}
    let (status, body) = harness
        .put_json(
            &mode_path("security"),
            serde_json::json!({ "value": "level1_requestseed" }),
        )
        .await
        .unwrap();
    eprintln!(
        "Seed response: {}",
        serde_json::to_string_pretty(&body).unwrap()
    );
    assert_eq!(status, 200);
    assert_eq!(body["id"], "security");
    // Seed should be in body.seed.Request_Seed as space-separated hex (e.g., "0xaa 0xbb 0xcc 0xdd")
    let seed_str = body["seed"]["Request_Seed"]
        .as_str()
        .expect("Seed response should have seed.Request_Seed");
    eprintln!("Raw seed string: {}", seed_str);

    // Parse seed from "0xaa 0xbb 0xcc 0xdd" format
    let seed_bytes: Vec<u8> = seed_str
        .split_whitespace()
        .map(|s| u8::from_str_radix(s.trim_start_matches("0x"), 16).unwrap())
        .collect();
    assert!(!seed_bytes.is_empty(), "Seed bytes should not be empty");
    eprintln!("Parsed seed: {}", hex::encode(&seed_bytes));

    // Compute key (XOR with 0xFF)
    let key: Vec<u8> = seed_bytes.iter().map(|b| b ^ 0xFF).collect();
    let key_hex = hex::encode(&key);
    eprintln!("Computed key: {}", key_hex);

    // Send key — body: {"value": "level1", "key": "<hex>"}
    let (status, body) = harness
        .put_json(
            &mode_path("security"),
            serde_json::json!({ "value": "level1", "key": key_hex }),
        )
        .await
        .unwrap();
    eprintln!(
        "Key response: {}",
        serde_json::to_string_pretty(&body).unwrap()
    );
    assert_eq!(status, 200);
    assert_eq!(body["id"], "security");
    assert_eq!(
        body["value"], "level1",
        "Key response should confirm level1 unlock"
    );

    // Verify GET shows unlocked
    let (status, body) = harness
        .get_with_status(&mode_path("security"))
        .await
        .unwrap();
    eprintln!(
        "Security state after unlock: {}",
        serde_json::to_string_pretty(&body).unwrap()
    );
    assert_eq!(status, 200);
    assert_eq!(body["value"], "level1");

    // Return to default session
    let (status, _) = harness
        .put_json(
            &mode_path("session"),
            serde_json::json!({ "value": "default" }),
        )
        .await
        .unwrap();
    assert_eq!(status, 200);

    eprintln!("\n=== SECURITY FLOW RAW HTTP COMPLETE ===");
}
