//! End-to-end tests for the full multi-tier supplier OTA architecture
//!
//! Architecture under test:
//!
//! ```text
//! SovdClient (test)
//!     │
//!     ▼
//! Vehicle Gateway  (sovdd, port 18092)         ← SOVD HTTP aggregator, no CAN
//!   ├── proxy: uds_gw      → sovdd (port 18090)
//!   └── proxy: vortex_engine → example-app (port 18091)
//!                                │
//!                                └── proxy → sovdd (port 18090, component: vtx_vx500)
//!                                               │
//!                                               └── UDS/CAN → example-ecu (vcan1, 0x18DA03F1/0x18DAF103)
//! ```
//!
//! Run with: cargo test -p sovd-tests --test multilayer_e2e_test -- --test-threads=1 --nocapture
//!
//! Note: Tests will be skipped automatically if vcan1 is not available.

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use reqwest::Client;
use serde_json::Value;
use sovd_client::SovdClient;
use tokio::time::sleep;

const UDS_GW_PORT: u16 = 18090;
const SUPPLIER_APP_PORT: u16 = 18091;
const VEHICLE_GW_PORT: u16 = 18092;
const INTERFACE: &str = "vcan1";
const AUTH_TOKEN: &str = "test-auth-token";

/// Check if vcan1 interface is available
fn vcan1_available() -> bool {
    Command::new("ip")
        .args(["link", "show", INTERFACE])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Macro to skip test if vcan1 is not available
macro_rules! require_vcan1 {
    () => {
        if !vcan1_available() {
            eprintln!("Skipping test: vcan1 interface not available");
            return;
        }
    };
}

/// Test harness that manages the full 4-process multi-tier environment
struct MultilayerTestHarness {
    example_ecu: Option<Child>,
    uds_gateway: Option<Child>,
    example_app: Option<Child>,
    vehicle_gateway: Option<Child>,
    http_client: Client,
    vehicle_gw_client: SovdClient,
    _temp_dir: tempfile::TempDir,
}

impl MultilayerTestHarness {
    async fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let http_client = Client::builder().timeout(Duration::from_secs(10)).build()?;

        let vehicle_gw_url = format!("http://localhost:{}", VEHICLE_GW_PORT);
        let vehicle_gw_client = SovdClient::new(&vehicle_gw_url)?;

        let temp_dir = tempfile::TempDir::new()?;

        let mut harness = Self {
            example_ecu: None,
            uds_gateway: None,
            example_app: None,
            vehicle_gateway: None,
            http_client,
            vehicle_gw_client,
            _temp_dir: temp_dir,
        };

        harness.setup().await?;
        Ok(harness)
    }

    fn vehicle_gw_client(&self) -> &SovdClient {
        &self.vehicle_gw_client
    }

    async fn setup(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        Self::kill_orphaned_processes();
        self.setup_vcan()?;
        self.write_configs()?;

        // 1. Start example-ecu (vtx_vx500) on vcan1
        self.start_example_ecu()?;
        sleep(Duration::from_millis(500)).await;

        // 2. Start UDS gateway on port 18090
        self.start_uds_gateway()?;
        self.write_pids();
        self.wait_for_health(UDS_GW_PORT, "UDS gateway").await?;

        // 3. Start example-app on port 18091
        self.start_example_app()?;
        self.write_pids();
        self.wait_for_health(SUPPLIER_APP_PORT, "example-app")
            .await?;

        // 4. Start vehicle gateway on port 18092
        self.start_vehicle_gateway()?;
        self.write_pids();
        self.wait_for_health(VEHICLE_GW_PORT, "vehicle gateway")
            .await?;

        Ok(())
    }

    fn pid_file_path() -> String {
        let workspace = Self::workspace_root();
        format!("{}/target/.multilayer-e2e-test-pids", workspace)
    }

    fn write_pids(&self) {
        let mut pids = Vec::new();
        if let Some(ref child) = self.example_ecu {
            pids.push(child.id().to_string());
        }
        if let Some(ref child) = self.uds_gateway {
            pids.push(child.id().to_string());
        }
        if let Some(ref child) = self.example_app {
            pids.push(child.id().to_string());
        }
        if let Some(ref child) = self.vehicle_gateway {
            pids.push(child.id().to_string());
        }
        if !pids.is_empty() {
            let _ = std::fs::write(Self::pid_file_path(), pids.join("\n"));
        }
    }

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

    fn workspace_root() -> String {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        std::path::Path::new(manifest_dir)
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| manifest_dir.to_string())
    }

    /// Write minimal configs to the temp dir for UDS gateway and vehicle gateway
    fn write_configs(&self) -> Result<(), Box<dyn std::error::Error>> {
        // UDS gateway config — only the vtx_vx500
        let uds_gw_config = format!(
            r#"# Minimal UDS gateway for multilayer E2E test
[server]
port = {}

[gateway]
enabled = true
id = "uds_gw"
name = "UDS Gateway"

[ecu.vtx_vx500]
id = "vtx_vx500"
name = "Vortex VX500 Engine ECU"

[ecu.vtx_vx500.transport]
type = "socketcan"
interface = "{}"

[ecu.vtx_vx500.transport.isotp]
tx_id = "0x18DA03F1"
rx_id = "0x18DAF103"

[ecu.vtx_vx500.security]
secret = "cc"

[ecu.vtx_vx500.session]
transfer_data_block_counter_start = 1
transfer_data_block_counter_wrap = 1

[[ecu.vtx_vx500.parameters]]
id = "boost_pressure"
did = "0xF40C"
name = "Boost Pressure"
data_type = "uint16"
byte_order = "big"
scale = 0.1
unit = "kPa"

[[ecu.vtx_vx500.parameters]]
id = "exhaust_temp"
did = "0xF405"
name = "Exhaust Gas Temperature"
data_type = "uint8"
offset = -40.0
unit = "°C"

[[ecu.vtx_vx500.parameters]]
id = "fuel_rail_pressure"
did = "0xF40D"
name = "Fuel Rail Pressure"
data_type = "uint16"
byte_order = "big"
scale = 0.1
unit = "bar"

[[ecu.vtx_vx500.parameters]]
id = "dpf_soot_load"
did = "0xF40E"
name = "DPF Soot Load"
data_type = "uint8"
unit = "%"

[[ecu.vtx_vx500.parameters]]
id = "vin"
did = "0xF190"
name = "Vehicle Identification Number"
data_type = "string"

[[ecu.vtx_vx500.parameters]]
id = "programming_date"
did = "0xF199"
name = "Programming Date"
data_type = "string"
writable = true

[[ecu.vtx_vx500.operations]]
id = "dpf_regen"
name = "DPF Regeneration"
rid = "0x0203"
description = "Initiate diesel particulate filter regeneration"
security_level = 0

[[ecu.vtx_vx500.operations]]
id = "injector_calibration"
name = "Injector Calibration"
rid = "0x0204"
description = "Run injector calibration routine"
security_level = 1

[[ecu.vtx_vx500.operations]]
id = "firmware_commit"
name = "Firmware Commit"
rid = "0xFF01"
description = "Commit activated firmware"
security_level = 0

[[ecu.vtx_vx500.operations]]
id = "firmware_rollback"
name = "Firmware Rollback"
rid = "0xFF02"
description = "Rollback to previous firmware"
security_level = 0
"#,
            UDS_GW_PORT, INTERFACE
        );

        let uds_gw_path = self._temp_dir.path().join("uds-gateway.toml");
        std::fs::write(&uds_gw_path, uds_gw_config)?;

        // Vehicle gateway config — proxies to UDS gateway and example-app
        let vehicle_gw_config = format!(
            r#"# Minimal vehicle gateway for multilayer E2E test
[server]
port = {}

[gateway]
enabled = true
id = "vehicle_gateway"
name = "Vehicle Gateway"

[proxy.uds_gw]
name = "UDS Gateway"
url = "http://localhost:{}"
component_id = "uds_gw"

[proxy.vortex_engine]
name = "Vortex Motors Engine App"
url = "http://localhost:{}"
component_id = "vortex_engine"
auth_token = "{}"
"#,
            VEHICLE_GW_PORT, UDS_GW_PORT, SUPPLIER_APP_PORT, AUTH_TOKEN
        );

        let vehicle_gw_path = self._temp_dir.path().join("vehicle-gateway.toml");
        std::fs::write(&vehicle_gw_path, vehicle_gw_config)?;

        Ok(())
    }

    fn find_binary(name: &str) -> String {
        let workspace = Self::workspace_root();
        let release = format!("{}/target/release/{}", workspace, name);
        if std::path::Path::new(&release).exists() {
            release
        } else {
            format!("{}/target/debug/{}", workspace, name)
        }
    }

    fn start_example_ecu(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let workspace = Self::workspace_root();
        let binary = Self::find_binary("example-ecu");
        let config = format!(
            "{}/simulations/supplier_ota/config/ecu-supplier-engine.toml",
            workspace
        );

        let child = Command::new(&binary)
            .args(["--config", &config])
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()?;

        eprintln!("Started example-ecu vtx_vx500 (PID: {})", child.id());
        self.example_ecu = Some(child);
        Ok(())
    }

    fn start_uds_gateway(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let binary = Self::find_binary("sovdd");
        let config = self
            ._temp_dir
            .path()
            .join("uds-gateway.toml")
            .to_string_lossy()
            .to_string();

        let child = Command::new(&binary)
            .arg(&config)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()?;

        eprintln!(
            "Started UDS gateway on port {} (PID: {})",
            UDS_GW_PORT,
            child.id()
        );
        self.uds_gateway = Some(child);
        Ok(())
    }

    fn start_example_app(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let binary = Self::find_binary("example-app");

        let child = Command::new(&binary)
            .args([
                "--port",
                &SUPPLIER_APP_PORT.to_string(),
                "--upstream-url",
                &format!("http://localhost:{}", UDS_GW_PORT),
                "--upstream-component",
                "vtx_vx500",
                "--upstream-gateway",
                "uds_gw",
                "--auth-token",
                AUTH_TOKEN,
            ])
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()?;

        eprintln!(
            "Started example-app on port {} (PID: {})",
            SUPPLIER_APP_PORT,
            child.id()
        );
        self.example_app = Some(child);
        Ok(())
    }

    fn start_vehicle_gateway(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let binary = Self::find_binary("sovdd");
        let config = self
            ._temp_dir
            .path()
            .join("vehicle-gateway.toml")
            .to_string_lossy()
            .to_string();

        let child = Command::new(&binary)
            .arg(&config)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()?;

        eprintln!(
            "Started vehicle gateway on port {} (PID: {})",
            VEHICLE_GW_PORT,
            child.id()
        );
        self.vehicle_gateway = Some(child);
        Ok(())
    }

    async fn wait_for_health(
        &self,
        port: u16,
        label: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let url = format!("http://localhost:{}/health", port);
        for i in 0..50 {
            match self.http_client.get(&url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    eprintln!("{} ready after {} attempts", label, i + 1);
                    return Ok(());
                }
                _ => {
                    sleep(Duration::from_millis(100)).await;
                }
            }
        }
        Err(format!("{} failed to start on port {}", label, port).into())
    }

    // Raw HTTP helpers

    async fn get(&self, base_port: u16, path: &str) -> Result<Value, Box<dyn std::error::Error>> {
        let url = format!("http://localhost:{}{}", base_port, path);
        let resp = self.http_client.get(&url).send().await?;
        let json = resp.json().await?;
        Ok(json)
    }

    async fn get_with_status(
        &self,
        base_port: u16,
        path: &str,
    ) -> Result<(u16, Value), Box<dyn std::error::Error>> {
        let url = format!("http://localhost:{}{}", base_port, path);
        let resp = self.http_client.get(&url).send().await?;
        let status = resp.status().as_u16();
        let json = resp.json().await.unwrap_or(serde_json::json!({}));
        Ok((status, json))
    }

    async fn get_with_auth(
        &self,
        base_port: u16,
        path: &str,
        token: &str,
    ) -> Result<(u16, Value), Box<dyn std::error::Error>> {
        let url = format!("http://localhost:{}{}", base_port, path);
        let resp = self
            .http_client
            .get(&url)
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await?;
        let status = resp.status().as_u16();
        let json = resp.json().await.unwrap_or(serde_json::json!({}));
        Ok((status, json))
    }
}

impl Drop for MultilayerTestHarness {
    fn drop(&mut self) {
        // Shut down in reverse order
        if let Some(mut child) = self.vehicle_gateway.take() {
            eprintln!("Stopping vehicle gateway...");
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(mut child) = self.example_app.take() {
            eprintln!("Stopping example-app...");
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(mut child) = self.uds_gateway.take() {
            eprintln!("Stopping UDS gateway...");
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
// Test 1: Vehicle gateway lists sub-entities
// =============================================================================

#[tokio::test]
#[serial_test::serial]
async fn test_vehicle_gw_lists_sub_entities() {
    require_vcan1!();
    let harness = MultilayerTestHarness::new()
        .await
        .expect("Failed to setup test harness");

    let apps = harness
        .vehicle_gw_client()
        .list_apps("vehicle_gateway")
        .await
        .expect("Failed to list vehicle gateway sub-entities");

    eprintln!("Vehicle GW sub-entities: {:?}", apps);

    assert!(
        apps.iter().any(|a| a.id == "uds_gw"),
        "Vehicle GW should have uds_gw sub-entity"
    );
    assert!(
        apps.iter().any(|a| a.id == "vortex_engine"),
        "Vehicle GW should have vortex_engine sub-entity"
    );
}

// =============================================================================
// Test 1b: Nested gateway discovery (uds_gw sub-entities through vehicle_gateway)
// =============================================================================

#[tokio::test]
#[serial_test::serial]
async fn test_nested_gateway_discovery() {
    require_vcan1!();
    let harness = MultilayerTestHarness::new()
        .await
        .expect("Failed to setup test harness");

    // uds_gw is a sub-entity of vehicle_gateway and is itself a gateway.
    // Its sub-entities should be discoverable via the nested apps endpoint.
    let uds_gw_apps = harness
        .vehicle_gw_client()
        .list_sub_entity_apps("vehicle_gateway", "uds_gw")
        .await
        .expect("Failed to list uds_gw sub-entities through vehicle gateway");

    eprintln!("UDS GW sub-entities (via vehicle GW): {:?}", uds_gw_apps);

    assert!(
        uds_gw_apps.iter().any(|a| a.id == "vtx_vx500"),
        "uds_gw should have vtx_vx500 sub-entity, got: {:?}",
        uds_gw_apps.iter().map(|a| &a.id).collect::<Vec<_>>()
    );

    // vortex_engine is an app entity with sub_entities: true.
    // Its managed ECU (vtx_vx500) should be discoverable via nested apps.
    let vortex_apps = harness
        .vehicle_gw_client()
        .list_sub_entity_apps("vehicle_gateway", "vortex_engine")
        .await
        .expect("Failed to list vortex_engine sub-entities through vehicle gateway");

    eprintln!(
        "Vortex Engine sub-entities (via vehicle GW): {:?}",
        vortex_apps
    );

    assert!(
        vortex_apps.iter().any(|a| a.id == "vtx_vx500"),
        "vortex_engine should have vtx_vx500 managed ECU sub-entity, got: {:?}",
        vortex_apps.iter().map(|a| &a.id).collect::<Vec<_>>()
    );
}

// =============================================================================
// Test 2: Supplier app visible through vehicle gateway
// =============================================================================

#[tokio::test]
#[serial_test::serial]
async fn test_example_app_visible_through_vehicle_gw() {
    require_vcan1!();
    let harness = MultilayerTestHarness::new()
        .await
        .expect("Failed to setup test harness");

    let components = harness
        .vehicle_gw_client()
        .list_components()
        .await
        .expect("Failed to list components through vehicle GW");

    eprintln!("Components through vehicle GW: {:?}", components);

    assert!(
        components.iter().any(|c| c.id == "vehicle_gateway"),
        "Should have vehicle_gateway component"
    );

    // The vortex_engine should appear as a sub-entity / app via the gateway
    let apps = harness
        .vehicle_gw_client()
        .list_apps("vehicle_gateway")
        .await
        .expect("Failed to list apps");

    assert!(
        apps.iter().any(|a| a.id == "vortex_engine"),
        "vortex_engine should be listed as an app through vehicle GW"
    );
}

// =============================================================================
// Test 3: Supplier app entity_type is "app" when queried through vehicle GW
// =============================================================================

#[tokio::test]
#[serial_test::serial]
async fn test_example_app_entity_type_via_vehicle_gw() {
    require_vcan1!();
    let harness = MultilayerTestHarness::new()
        .await
        .expect("Failed to setup test harness");

    // Query the vortex_engine component detail through the example-app directly
    let (status, detail) = harness
        .get_with_auth(
            SUPPLIER_APP_PORT,
            "/vehicle/v1/components/vortex_engine",
            AUTH_TOKEN,
        )
        .await
        .expect("Failed to get vortex_engine component detail");

    assert_eq!(status, 200, "Should get 200 for component detail");

    eprintln!(
        "Supplier app detail: {}",
        serde_json::to_string_pretty(&detail).unwrap()
    );

    let entity_type = detail
        .get("type")
        .or_else(|| detail.get("entity_type"))
        .and_then(|v| v.as_str());

    assert_eq!(
        entity_type,
        Some("app"),
        "vortex_engine entity_type should be 'app'"
    );
}

// =============================================================================
// Test 4: Read ECU param through example-app sub-entity path
// =============================================================================

#[tokio::test]
#[serial_test::serial]
async fn test_read_ecu_param_through_example_app() {
    require_vcan1!();
    let harness = MultilayerTestHarness::new()
        .await
        .expect("Failed to setup test harness");

    // Read boost_pressure via the managed ECU sub-entity path
    // (example-app → proxy → UDS GW → example-ecu)
    let (status, result) = harness
        .get_with_auth(
            SUPPLIER_APP_PORT,
            "/vehicle/v1/components/vortex_engine/apps/vtx_vx500/data/boost_pressure",
            AUTH_TOKEN,
        )
        .await
        .expect("Failed to read boost_pressure through example-app sub-entity");

    eprintln!(
        "boost_pressure via example-app sub-entity (status {}): {}",
        status,
        serde_json::to_string_pretty(&result).unwrap()
    );

    assert_eq!(
        status, 200,
        "Should get 200 OK reading boost_pressure via sub-entity"
    );
    assert!(
        result.get("value").is_some(),
        "Response should contain a value"
    );
}

// =============================================================================
// Test 5: Read ECU param through full 4-tier chain
// =============================================================================

#[tokio::test]
#[serial_test::serial]
async fn test_read_ecu_param_through_full_chain() {
    require_vcan1!();
    let harness = MultilayerTestHarness::new()
        .await
        .expect("Failed to setup test harness");

    // Read vortex_engine/vtx_vx500/boost_pressure through vehicle gateway
    // Vehicle GW → example-app → sub-entity → proxy → UDS GW → example-ecu via CAN
    let result = harness
        .get(
            VEHICLE_GW_PORT,
            "/vehicle/v1/components/vehicle_gateway/data/vortex_engine%2Fvtx_vx500%2Fboost_pressure",
        )
        .await
        .expect("Failed to read boost_pressure through full chain");

    eprintln!(
        "boost_pressure via full chain: {}",
        serde_json::to_string_pretty(&result).unwrap()
    );

    assert!(
        result.get("value").is_some(),
        "Full-chain response should contain a value for boost_pressure"
    );
}

// =============================================================================
// Test 5b: Raw read through full 4-tier chain (client-side conversion)
// =============================================================================

#[tokio::test]
#[serial_test::serial]
async fn test_raw_read_through_full_chain() {
    require_vcan1!();
    let harness = MultilayerTestHarness::new()
        .await
        .expect("Failed to setup test harness");

    // Read boost_pressure with ?raw=true through the full chain
    // Vehicle GW → example-app → sub-entity → proxy → UDS GW → example-ecu via CAN
    let result = harness
        .get(
            VEHICLE_GW_PORT,
            "/vehicle/v1/components/vehicle_gateway/data/vortex_engine%2Fvtx_vx500%2Fboost_pressure?raw=true",
        )
        .await
        .expect("Failed to raw-read boost_pressure through full chain");

    eprintln!(
        "boost_pressure raw via full chain: {}",
        serde_json::to_string_pretty(&result).unwrap()
    );

    // Raw response must include the raw hex bytes
    let raw = result["raw"].as_str().unwrap_or("");
    assert!(
        !raw.is_empty(),
        "Raw read should return non-empty raw hex bytes, got: {}",
        serde_json::to_string_pretty(&result).unwrap()
    );

    // The value field in raw mode should be the hex string, not converted
    let value = result["value"].as_str().unwrap_or("");
    assert_eq!(
        value, raw,
        "In raw mode, value should equal the raw hex bytes"
    );

    // Should also include the DID identifier
    let did = result["did"].as_str().unwrap_or("");
    assert!(
        !did.is_empty(),
        "Raw read should include the DID identifier"
    );

    // Length should be > 0
    let length = result["length"].as_u64().unwrap_or(0);
    assert!(length > 0, "Raw read should report byte length");

    eprintln!("Raw read OK: did={}, raw={}, length={}", did, raw, length);
}

// =============================================================================
// Test 6: Supplier app synthetic params visible via vehicle GW
// =============================================================================

#[tokio::test]
#[serial_test::serial]
async fn test_example_app_synthetic_params_via_vehicle_gw() {
    require_vcan1!();
    let harness = MultilayerTestHarness::new()
        .await
        .expect("Failed to setup test harness");

    // Per SOVD standard, gateway's own parameter list is empty — child parameters
    // are accessed via sub-entity routes. Verify the gateway data endpoint is empty.
    let gw_params = harness
        .get(
            VEHICLE_GW_PORT,
            "/vehicle/v1/components/vehicle_gateway/data",
        )
        .await
        .expect("Failed to list params through vehicle GW");

    let gw_items = gw_params["items"].as_array().expect("Expected items array");
    assert!(
        gw_items.is_empty(),
        "Gateway should not aggregate child parameters (SOVD §6.5)"
    );

    // Access vortex_engine's synthetic params via sub-entity route
    let params = harness
        .get_with_auth(
            SUPPLIER_APP_PORT,
            "/vehicle/v1/components/vortex_engine/data",
            AUTH_TOKEN,
        )
        .await
        .expect("Failed to list params from example-app");

    let (status, body) = params;
    eprintln!(
        "Example-app params (status {}): {}",
        status,
        serde_json::to_string_pretty(&body).unwrap()
    );

    assert_eq!(status, 200, "Should get 200 OK");
    let items = body["items"].as_array().expect("Expected items array");

    let has_health_score = items.iter().any(|p| {
        p["id"]
            .as_str()
            .map_or(false, |id| id.contains("engine_health_score"))
    });
    let has_maintenance = items.iter().any(|p| {
        p["id"]
            .as_str()
            .map_or(false, |id| id.contains("maintenance_hours"))
    });

    assert!(
        has_health_score,
        "Should have engine_health_score param from example-app"
    );
    assert!(
        has_maintenance,
        "Should have maintenance_hours param from example-app"
    );
}

// =============================================================================
// Test 7: Faults through example-app sub-entity path
// =============================================================================

#[tokio::test]
#[serial_test::serial]
async fn test_faults_through_example_app() {
    require_vcan1!();
    let harness = MultilayerTestHarness::new()
        .await
        .expect("Failed to setup test harness");

    // Get faults via the managed ECU sub-entity path
    let (status, faults) = harness
        .get_with_auth(
            SUPPLIER_APP_PORT,
            "/vehicle/v1/components/vortex_engine/apps/vtx_vx500/faults",
            AUTH_TOKEN,
        )
        .await
        .expect("Failed to get faults through example-app sub-entity");

    eprintln!(
        "Faults via example-app sub-entity (status {}): {}",
        status,
        serde_json::to_string_pretty(&faults).unwrap()
    );

    assert_eq!(status, 200, "Should get 200 OK for faults via sub-entity");

    let items = faults["items"].as_array().expect("Expected items array");
    eprintln!("Found {} DTCs through example-app sub-entity", items.len());

    // Verify the response structure is correct (items array + total_count)
    assert!(
        faults.get("total_count").is_some(),
        "Faults response should contain total_count"
    );
}

// =============================================================================
// Test 8: Operations through example-app sub-entity path
// =============================================================================

#[tokio::test]
#[serial_test::serial]
async fn test_operations_through_example_app() {
    require_vcan1!();
    let harness = MultilayerTestHarness::new()
        .await
        .expect("Failed to setup test harness");

    // List operations via the managed ECU sub-entity path
    let (status, ops) = harness
        .get_with_auth(
            SUPPLIER_APP_PORT,
            "/vehicle/v1/components/vortex_engine/apps/vtx_vx500/operations",
            AUTH_TOKEN,
        )
        .await
        .expect("Failed to list operations through example-app sub-entity");

    eprintln!(
        "Operations via example-app sub-entity (status {}): {}",
        status,
        serde_json::to_string_pretty(&ops).unwrap()
    );

    assert_eq!(
        status, 200,
        "Should get 200 OK for operations via sub-entity"
    );

    let items = ops["items"].as_array().expect("Expected items array");
    eprintln!(
        "Found {} operations through example-app sub-entity",
        items.len()
    );

    // Should have at least the dpf_regen operation from the upstream ECU
    let has_dpf_regen = items.iter().any(|o| {
        o["id"]
            .as_str()
            .map_or(false, |id| id.contains("dpf_regen"))
    });

    assert!(
        has_dpf_regen,
        "Should have dpf_regen operation proxied from example-ecu"
    );
}

// =============================================================================
// Test 9: Supplier app auth enforcement
// =============================================================================

#[tokio::test]
#[serial_test::serial]
async fn test_example_app_auth_enforced() {
    require_vcan1!();
    let harness = MultilayerTestHarness::new()
        .await
        .expect("Failed to setup test harness");

    // Request WITHOUT auth token → should get 401
    let (status_no_auth, _) = harness
        .get_with_status(
            SUPPLIER_APP_PORT,
            "/vehicle/v1/components/vortex_engine/data",
        )
        .await
        .expect("Failed to send unauthenticated request");

    eprintln!("No auth token → status {}", status_no_auth);
    assert_eq!(
        status_no_auth, 401,
        "Request without auth token should return 401"
    );

    // Request WITH correct auth token → should get 200
    let (status_with_auth, _) = harness
        .get_with_auth(
            SUPPLIER_APP_PORT,
            "/vehicle/v1/components/vortex_engine/data",
            AUTH_TOKEN,
        )
        .await
        .expect("Failed to send authenticated request");

    eprintln!("With auth token → status {}", status_with_auth);
    assert_eq!(
        status_with_auth, 200,
        "Request with correct auth token should return 200"
    );
}

// =============================================================================
// Test 10: Full multilayer flow (comprehensive walkthrough)
// =============================================================================

#[tokio::test]
#[serial_test::serial]
async fn test_full_multilayer_flow() {
    require_vcan1!();
    let harness = MultilayerTestHarness::new()
        .await
        .expect("Failed to setup test harness");

    eprintln!("=== FULL MULTILAYER FLOW TEST ===\n");

    // Step 1: List components at vehicle gateway
    eprintln!("Step 1: List components at vehicle gateway");
    let components = harness
        .vehicle_gw_client()
        .list_components()
        .await
        .expect("Failed to list components");
    eprintln!("  Found {} components", components.len());
    assert!(
        components.iter().any(|c| c.id == "vehicle_gateway"),
        "Should have vehicle_gateway"
    );

    // Step 2: Get sub-entities of vehicle gateway
    eprintln!("\nStep 2: List vehicle gateway sub-entities");
    let apps = harness
        .vehicle_gw_client()
        .list_apps("vehicle_gateway")
        .await
        .expect("Failed to list apps");
    eprintln!("  Found {} sub-entities:", apps.len());
    for app in &apps {
        eprintln!("    - {} ({})", app.id, app.name);
    }
    assert!(
        apps.len() >= 2,
        "Should have at least uds_gw and vortex_engine"
    );

    // Step 3: List parameters through vehicle gateway
    eprintln!("\nStep 3: List parameters through vehicle gateway");
    let params = harness
        .get(
            VEHICLE_GW_PORT,
            "/vehicle/v1/components/vehicle_gateway/data",
        )
        .await
        .expect("Failed to list params");
    let param_items = params["items"].as_array().expect("Expected items array");
    eprintln!("  Found {} parameters", param_items.len());

    // Step 4: Read a parameter through the full chain (via sub-entity)
    eprintln!("\nStep 4: Read boost_pressure through full chain");
    let bp = harness
        .get(
            VEHICLE_GW_PORT,
            "/vehicle/v1/components/vehicle_gateway/data/vortex_engine%2Fvtx_vx500%2Fboost_pressure",
        )
        .await
        .expect("Failed to read boost_pressure");
    eprintln!("  boost_pressure: {}", bp["value"]);
    assert!(bp.get("value").is_some(), "Should have value");

    // Step 5: List faults through vehicle gateway
    eprintln!("\nStep 5: List faults through vehicle gateway");
    let faults = harness
        .get(
            VEHICLE_GW_PORT,
            "/vehicle/v1/components/vehicle_gateway/faults",
        )
        .await
        .expect("Failed to list faults through vehicle GW");
    let fault_items = faults["items"].as_array().expect("Expected items array");
    eprintln!("  Found {} faults via vehicle gateway", fault_items.len());

    // Step 6: List operations through vehicle gateway
    eprintln!("\nStep 6: List operations through vehicle gateway");
    let ops = harness
        .get(
            VEHICLE_GW_PORT,
            "/vehicle/v1/components/vehicle_gateway/operations",
        )
        .await
        .expect("Failed to list operations through vehicle GW");
    let op_items = ops["items"].as_array().expect("Expected items array");
    eprintln!("  Found {} operations via vehicle gateway", op_items.len());

    // Verify faults and operations are prefixed with backend IDs
    if !fault_items.is_empty() {
        let first_fault_id = fault_items[0]["id"].as_str().unwrap_or("");
        assert!(
            first_fault_id.contains('/'),
            "Fault IDs should be prefixed with backend ID: {}",
            first_fault_id
        );
    }
    if !op_items.is_empty() {
        let first_op_id = op_items[0]["id"].as_str().unwrap_or("");
        assert!(
            first_op_id.contains('/'),
            "Operation IDs should be prefixed with backend ID: {}",
            first_op_id
        );
    }

    eprintln!("\n=== FULL MULTILAYER FLOW TEST COMPLETE ===");
}
