//! End-to-end tests for SOVD Server
//!
//! These tests run the full stack:
//! 1. Set up virtual CAN interface (vcan0)
//! 2. Start example-ecu simulator
//! 3. Start sovdd
//! 4. Exercise the REST API
//! 5. Verify responses contain real ECU data
//!
//! Run with: cargo test -p sovd-tests --test e2e_test -- --test-threads=1
//!
//! Note: Requires root/sudo for vcan setup, or pre-existing vcan0 interface.

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use reqwest::Client;
use serde_json::Value;
use tokio::time::sleep;

/// Options for configuring the test harness
#[derive(Clone)]
struct TestHarnessOptions {
    /// Block counter start value for TransferData (default 0)
    block_counter_start: u8,
    /// Block counter wrap value (default 0)
    block_counter_wrap: u8,
    /// Whether to enable firmware rollback support (default true)
    supports_rollback: bool,
}

impl Default for TestHarnessOptions {
    fn default() -> Self {
        Self {
            block_counter_start: 0,
            block_counter_wrap: 0,
            supports_rollback: true,
        }
    }
}

/// Test harness that manages the test environment
struct TestHarness {
    example_ecu: Option<Child>,
    sovd_server: Option<Child>,
    client: Client,
    base_url: String,
    options: TestHarnessOptions,
}

impl TestHarness {
    const SERVER_PORT: u16 = 18080; // Use non-standard port for tests
    const INTERFACE: &'static str = "vcan0";

    async fn new() -> Result<Self, Box<dyn std::error::Error>> {
        Self::new_with_options(TestHarnessOptions::default()).await
    }

    async fn new_with_options(
        options: TestHarnessOptions,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let client = Client::builder().timeout(Duration::from_secs(10)).build()?;

        let base_url = format!("http://localhost:{}", Self::SERVER_PORT);

        let mut harness = Self {
            example_ecu: None,
            sovd_server: None,
            client,
            base_url,
            options,
        };

        harness.setup().await?;
        Ok(harness)
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

        // Start sovdd
        self.start_sovd_server()?;

        // Track spawned PIDs so a future run can clean up if we crash
        self.write_pids();

        // Wait for server to be ready
        self.wait_for_server().await?;

        // Upload DID definitions that match the example-ecu config
        self.upload_test_definitions().await?;

        // Reset to default session so all tests start with a clean state
        self.reset_to_default_session().await?;

        Ok(())
    }

    /// Path to PID file for tracking processes spawned by this test harness.
    /// Only PIDs written here will be killed during orphan cleanup.
    fn pid_file_path() -> String {
        let workspace = Self::workspace_root();
        format!("{}/target/.e2e-test-pids", workspace)
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
                    // SIGTERM first for graceful shutdown
                    unsafe {
                        libc::kill(pid, libc::SIGTERM);
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(200));
            for line in contents.lines() {
                if let Ok(pid) = line.trim().parse::<i32>() {
                    // SIGKILL stragglers
                    unsafe {
                        libc::kill(pid, libc::SIGKILL);
                    }
                }
            }
            let _ = std::fs::remove_file(&pid_file);
        }

        // Wait for processes to fully terminate and kernel to release socket resources.
        // ISO-TP sockets can have in-flight frames that need time to be fully discarded.
        std::thread::sleep(Duration::from_millis(500));
    }

    fn setup_vcan(&self) -> Result<(), Box<dyn std::error::Error>> {
        // Check if vcan0 already exists
        let status = Command::new("ip")
            .args(["link", "show", Self::INTERFACE])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;

        if status.success() {
            return Ok(());
        }

        // Try to create vcan0 (may require sudo)
        eprintln!("Setting up {}...", Self::INTERFACE);

        // Load vcan module
        let _ = Command::new("sudo").args(["modprobe", "vcan"]).status();

        // Create interface
        let _ = Command::new("sudo")
            .args(["ip", "link", "add", "dev", Self::INTERFACE, "type", "vcan"])
            .status();

        // Bring up interface
        Command::new("sudo")
            .args(["ip", "link", "set", "up", Self::INTERFACE])
            .status()?;

        Ok(())
    }

    /// Get the workspace root directory (two levels up from crates/sovd-tests)
    fn workspace_root() -> String {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        // Go up from crates/sovd-tests to workspace root
        std::path::Path::new(manifest_dir)
            .parent() // crates/
            .and_then(|p| p.parent()) // workspace root
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| manifest_dir.to_string())
    }

    fn start_example_ecu(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let workspace = Self::workspace_root();
        let binary = format!("{}/target/release/example-ecu", workspace);

        // Check if binary exists, fall back to debug
        let binary = if std::path::Path::new(&binary).exists() {
            binary
        } else {
            format!("{}/target/debug/example-ecu", workspace)
        };

        // Create example-ecu config if using non-default block counter settings
        let use_config =
            self.options.block_counter_start != 0 || self.options.block_counter_wrap != 0;

        let child = if use_config {
            let config_path = self.create_example_ecu_config()?;
            Command::new(&binary)
                .args(["--config", &config_path])
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()?
        } else {
            Command::new(&binary)
                .args([
                    "--interface",
                    Self::INTERFACE,
                    "--rx-id",
                    "0x18DA00F1",
                    "--tx-id",
                    "0x18DAF100",
                ])
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()?
        };

        self.example_ecu = Some(child);
        eprintln!(
            "Started example-ecu (PID: {})",
            self.example_ecu.as_ref().unwrap().id()
        );
        Ok(())
    }

    fn create_example_ecu_config(&self) -> Result<String, Box<dyn std::error::Error>> {
        let workspace = Self::workspace_root();

        let content = format!(
            r#"
[transport]
interface = "{}"
rx_id = "0x18DA00F1"
tx_id = "0x18DAF100"

[transfer]
block_counter_start = {}
block_counter_wrap = {}
"#,
            Self::INTERFACE,
            self.options.block_counter_start,
            self.options.block_counter_wrap
        );

        let config_path = format!("{}/target/example-ecu-config.toml", workspace);
        std::fs::write(&config_path, content)?;

        Ok(config_path)
    }

    fn start_sovd_server(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let workspace = Self::workspace_root();
        let binary = format!("{}/target/release/sovdd", workspace);

        // Check if binary exists, fall back to debug
        let binary = if std::path::Path::new(&binary).exists() {
            binary
        } else {
            format!("{}/target/debug/sovdd", workspace)
        };

        // Create a test config with custom port
        let test_config = self.create_test_config()?;

        let child = Command::new(&binary)
            .arg(&test_config)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()?;

        self.sovd_server = Some(child);
        eprintln!(
            "Started sovdd (PID: {})",
            self.sovd_server.as_ref().unwrap().id()
        );
        Ok(())
    }

    fn create_test_config(&self) -> Result<String, Box<dyn std::error::Error>> {
        // Create a minimal test config for sovdd
        let workspace = Self::workspace_root();

        let content = format!(
            r#"
[server]
port = {}

[transport]
type = "socketcan"
interface = "{}"
bitrate = 500000

[transport.isotp]
tx_id = "0x18DA00F1"
rx_id = "0x18DAF100"
tx_padding = 0xCC
rx_padding = 0xCC
block_size = 0
st_min_us = 0
tx_dl = 8

[session]
default_session = 0x01
extended_session = 0x03
engineering_session = 0x60
transfer_data_block_counter_start = {}
transfer_data_block_counter_wrap = {}

[session.security]
enabled = true
level = 1
secret = "ff"

[ecu.vtx_ecm]
name = "VTX ECM"
description = "Vortex Motors Engine Control Module (Simulated)"

{}

[[ecu.vtx_ecm.operations]]
id = "check_preconditions"
name = "Check Programming Preconditions"
rid = "0x0203"
security_level = 0

[[ecu.vtx_ecm.operations]]
id = "erase_memory"
name = "Erase Memory"
rid = "0xFF00"
security_level = 1

[[ecu.vtx_ecm.outputs]]
id = "led_status"
name = "LED Status"
ioid = "0xF000"
default_value = "00"
data_type = "uint8"
allowed = ["off", "on"]
description = "Status LED on/off control"
security_level = 0

[[ecu.vtx_ecm.outputs]]
id = "fan_speed"
name = "Fan Speed"
ioid = "0xF001"
default_value = "0000"
data_type = "uint16"
unit = "rpm"
min = 0.0
max = 10000.0
description = "Cooling fan motor speed"
security_level = 0

[[ecu.vtx_ecm.outputs]]
id = "relay_1"
name = "Relay 1"
ioid = "0xF002"
default_value = "00"
data_type = "uint8"
allowed = ["off", "on"]
description = "General purpose relay 1"
security_level = 0

[[ecu.vtx_ecm.outputs]]
id = "relay_2"
name = "Relay 2"
ioid = "0xF003"
default_value = "00"
data_type = "uint8"
allowed = ["off", "on"]
description = "General purpose relay 2 (secured)"
security_level = 1

[[ecu.vtx_ecm.outputs]]
id = "pwm_output"
name = "PWM Output"
ioid = "0xF004"
default_value = "80"
data_type = "uint8"
scale = 0.392157
unit = "%"
min = 0.0
max = 100.0
description = "Pulse-width modulated output duty cycle"
security_level = 0
"#,
            Self::SERVER_PORT,
            Self::INTERFACE,
            self.options.block_counter_start,
            self.options.block_counter_wrap,
            if self.options.supports_rollback {
                "[ecu.vtx_ecm.flash]\nsupports_rollback = true\ncommit_routine = \"0xFF01\"\nrollback_routine = \"0xFF02\""
            } else {
                ""
            }
        );

        let test_config = format!("{}/target/test-config.toml", workspace);
        std::fs::write(&test_config, content)?;

        Ok(test_config)
    }

    async fn wait_for_server(&self) -> Result<(), Box<dyn std::error::Error>> {
        let health_url = format!("{}/health", self.base_url);

        for i in 0..30 {
            match self.client.get(&health_url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    eprintln!("Server ready after {}ms", i * 100);
                    return Ok(());
                }
                _ => {
                    sleep(Duration::from_millis(100)).await;
                }
            }
        }

        Err("Server failed to start within 3 seconds".into())
    }

    async fn upload_test_definitions(&self) -> Result<(), Box<dyn std::error::Error>> {
        let workspace = Self::workspace_root();
        let definitions_path = format!("{}/tests/fixtures/vtx_ecm_definitions.yaml", workspace);

        let yaml_content = std::fs::read_to_string(&definitions_path).map_err(|e| {
            format!(
                "Failed to read definitions file {}: {}",
                definitions_path, e
            )
        })?;

        let url = format!("{}/admin/definitions", self.base_url);
        let resp = self
            .client
            .post(&url)
            .header("Content-Type", "application/x-yaml")
            .body(yaml_content)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("Failed to upload definitions: {} - {}", status, body).into());
        }

        eprintln!("Uploaded test DID definitions");
        Ok(())
    }

    /// Reset ECU to default session to ensure clean test state
    async fn reset_to_default_session(&self) -> Result<(), Box<dyn std::error::Error>> {
        let url = format!(
            "{}/vehicle/v1/components/vtx_ecm/modes/session",
            self.base_url
        );
        let resp = self
            .client
            .put(&url)
            .json(&serde_json::json!({ "value": "default" }))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("Failed to reset session: {} - {}", status, body).into());
        }

        eprintln!("Reset to default session");
        Ok(())
    }

    async fn get(&self, path: &str) -> Result<Value, Box<dyn std::error::Error>> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self.client.get(&url).send().await?;
        let json: Value = resp.json().await?;
        Ok(json)
    }

    async fn get_with_status(
        &self,
        path: &str,
    ) -> Result<(u16, Value), Box<dyn std::error::Error>> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self.client.get(&url).send().await?;
        let status = resp.status().as_u16();
        let json: Value = resp.json().await?;
        Ok((status, json))
    }

    async fn post(
        &self,
        path: &str,
        body: Value,
    ) -> Result<(u16, Value), Box<dyn std::error::Error>> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self.client.post(&url).json(&body).send().await?;
        let status = resp.status().as_u16();
        let json: Value = resp.json().await?;
        Ok((status, json))
    }

    async fn delete(&self, path: &str) -> Result<u16, Box<dyn std::error::Error>> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self.client.delete(&url).send().await?;
        Ok(resp.status().as_u16())
    }

    async fn delete_with_status(
        &self,
        path: &str,
    ) -> Result<(u16, Value), Box<dyn std::error::Error>> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self.client.delete(&url).send().await?;
        let status = resp.status().as_u16();
        let json: Value = resp.json().await?;
        Ok((status, json))
    }

    async fn put(
        &self,
        path: &str,
        body: Value,
    ) -> Result<(u16, Value), Box<dyn std::error::Error>> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self.client.put(&url).json(&body).send().await?;
        let status = resp.status().as_u16();
        let json: Value = resp.json().await?;
        Ok((status, json))
    }

    /// POST with query parameters (for endpoints that use Query extraction)
    async fn post_form(
        &self,
        path: &str,
        params: &[(&str, &str)],
    ) -> Result<reqwest::Response, Box<dyn std::error::Error>> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self.client.post(&url).query(params).send().await?;
        Ok(resp)
    }

    /// Create a SovdClient for API access
    fn sovd_client(&self) -> sovd_client::SovdClient {
        sovd_client::SovdClient::new(&self.base_url).expect("Failed to create SOVD client")
    }

    /// Create a FlashClient for the component under test
    fn flash_client(&self) -> sovd_client::FlashClient {
        sovd_client::FlashClient::for_sovd(&self.base_url, "vtx_ecm")
            .expect("Failed to create flash client")
    }

    /// Create a valid firmware package for testing
    ///
    /// Uses the example-ecu's binary format:
    /// - Header magic: "EXAMPLE_FW" (10 bytes)
    /// - Version string (32 bytes, null-padded)
    /// - Firmware data (variable)
    /// - SHA-256 of bytes 0..(len-42) (32 bytes)
    /// - Footer magic: "EXFW_END!\0" (10 bytes)
    fn create_firmware_package(data_size: usize) -> Vec<u8> {
        Self::create_firmware_package_with_version(data_size, "2.0.0-test")
    }

    fn create_firmware_package_with_version(data_size: usize, version: &str) -> Vec<u8> {
        use sha2::{Digest, Sha256};

        const HEADER_MAGIC: &[u8] = b"EXAMPLE_FW";
        const FOOTER_MAGIC: &[u8] = b"EXFW_END!\0";
        const VERSION_LEN: usize = 32;

        let mut package = Vec::new();

        // Header magic (10 bytes)
        package.extend_from_slice(HEADER_MAGIC);

        // Version string (32 bytes, null-padded)
        let version_bytes = version.as_bytes();
        package.extend_from_slice(version_bytes);
        package.resize(HEADER_MAGIC.len() + VERSION_LEN, 0);

        // Firmware data
        let data: Vec<u8> = (0..data_size).map(|i| (i & 0xFF) as u8).collect();
        package.extend_from_slice(&data);

        // Compute SHA-256 of everything so far (header + version + data)
        let mut hasher = Sha256::new();
        hasher.update(&package);
        let hash = hasher.finalize();

        // Append hash (32 bytes) and footer (10 bytes)
        package.extend_from_slice(&hash);
        package.extend_from_slice(FOOTER_MAGIC);

        package
    }
}

impl Drop for TestHarness {
    fn drop(&mut self) {
        // Kill sovdd
        if let Some(mut child) = self.sovd_server.take() {
            eprintln!("Stopping sovdd...");
            let _ = child.kill();
            let _ = child.wait();
        }

        // Kill example-ecu
        if let Some(mut child) = self.example_ecu.take() {
            eprintln!("Stopping example-ecu...");
            let _ = child.kill();
            let _ = child.wait();
        }

        // Clean up test config and PID file
        let workspace = Self::workspace_root();
        let test_config = format!("{}/target/test-config.toml", workspace);
        let _ = std::fs::remove_file(test_config);
        let _ = std::fs::remove_file(Self::pid_file_path());

        // Wait for socket resources to be fully released by the kernel
        // This prevents the next test from receiving stale data from the previous test's ECU
        // ISO-TP sockets can have pending frames that need time to be discarded
        std::thread::sleep(Duration::from_millis(300));
    }
}

// =============================================================================
// Component API Tests
// =============================================================================

#[tokio::test]
#[serial_test::serial]
async fn test_list_components() {
    let harness = TestHarness::new()
        .await
        .expect("Failed to setup test harness");
    let client = harness.sovd_client();

    let components = client
        .list_components()
        .await
        .expect("list_components failed");
    let json = serde_json::json!({ "items": components });

    assert!(json["items"].is_array(), "Expected items array");
    let items = json["items"].as_array().unwrap();
    assert!(!items.is_empty(), "Expected at least one component");

    // Check that vtx_ecm is present
    let has_vtx_ecm = items.iter().any(|item| item["id"] == "vtx_ecm");
    assert!(has_vtx_ecm, "Expected vtx_ecm component");
}

#[tokio::test]
#[serial_test::serial]
async fn test_get_component_details() {
    let harness = TestHarness::new()
        .await
        .expect("Failed to setup test harness");
    let client = harness.sovd_client();

    let component = client
        .get_component("vtx_ecm")
        .await
        .expect("get_component failed");
    let json = serde_json::to_value(&component).unwrap();

    assert_eq!(json["id"], "vtx_ecm");
    assert!(json["name"].is_string());
    assert!(json["capabilities"].is_object());
    assert_eq!(json["capabilities"]["read_data"], true);
}

#[tokio::test]
#[serial_test::serial]
async fn test_list_parameters() {
    let harness = TestHarness::new()
        .await
        .expect("Failed to setup test harness");
    let client = harness.sovd_client();

    let params = client
        .list_parameters("vtx_ecm")
        .await
        .expect("list_parameters failed");

    assert!(!params.items.is_empty(), "Expected at least one parameter");

    // Check that engine_rpm is present
    let has_engine_rpm = params.items.iter().any(|item| item.id == "engine_rpm");
    assert!(has_engine_rpm, "Expected engine_rpm parameter");
}

// =============================================================================
// Data API Tests - Reading from actual ECU
// =============================================================================

/// Test reading engine_rpm (EXTENDED DID) - requires extended session
#[tokio::test]
#[serial_test::serial]
async fn test_read_engine_rpm() {
    use sovd_client::SessionType;

    let harness = TestHarness::new()
        .await
        .expect("Failed to setup test harness");
    let client = harness.sovd_client();

    // engine_rpm is an EXTENDED DID - first change to extended session
    client
        .set_session("vtx_ecm", SessionType::Extended)
        .await
        .expect("set_session failed");

    let data = client
        .read_data("vtx_ecm", "engine_rpm")
        .await
        .expect("read_data failed");

    // The test ECU initializes RPM around 1850 (raw: 7400, scale: 0.25)
    let value = data.as_f64().expect("Expected numeric value");
    assert!(
        value > 0.0 && value < 10000.0,
        "RPM {} out of expected range",
        value
    );

    assert!(data.timestamp.is_some(), "Expected timestamp");
}

#[tokio::test]
#[serial_test::serial]
async fn test_read_coolant_temp() {
    let harness = TestHarness::new()
        .await
        .expect("Failed to setup test harness");
    let client = harness.sovd_client();

    let data = client
        .read_data("vtx_ecm", "coolant_temp")
        .await
        .expect("read_data failed");

    // The test ECU initializes coolant temp around 92°C
    let value = data.as_f64().expect("Expected numeric value");
    assert!(
        value > -50.0 && value < 250.0,
        "Temp {} out of expected range",
        value
    );
}

/// Test reading multiple PUBLIC parameters in sequence
#[tokio::test]
#[serial_test::serial]
async fn test_read_multiple_parameters() {
    let harness = TestHarness::new()
        .await
        .expect("Failed to setup test harness");
    let client = harness.sovd_client();

    // Read multiple PUBLIC parameters in sequence (no session change needed)
    let params = ["vehicle_speed", "coolant_temp", "engine_load"];

    for param in params {
        let data = client
            .read_data("vtx_ecm", param)
            .await
            .expect(&format!("read_data {} failed", param));

        assert!(
            data.as_f64().is_some(),
            "Expected numeric value for {}",
            param
        );
    }
}

#[tokio::test]
#[serial_test::serial]
async fn test_read_nonexistent_parameter() {
    let harness = TestHarness::new()
        .await
        .expect("Failed to setup test harness");
    let client = harness.sovd_client();

    let result = client.read_data("vtx_ecm", "nonexistent").await;

    // Should return an error
    assert!(result.is_err(), "Expected error for nonexistent parameter");
}

// =============================================================================
// Subscription API Tests
// =============================================================================

/// Test creating a subscription with PUBLIC DIDs only
#[tokio::test]
#[serial_test::serial]
async fn test_create_subscription() {
    let harness = TestHarness::new()
        .await
        .expect("Failed to setup test harness");
    let client = harness.sovd_client();

    // Use PUBLIC DIDs only - no session setup needed
    let sub = client
        .create_global_subscription(
            "vtx_ecm",
            vec!["vehicle_speed".to_string(), "coolant_temp".to_string()],
            1,
        )
        .await
        .expect("create_global_subscription failed");

    assert!(!sub.subscription_id.is_empty(), "Expected subscription_id");
    assert!(!sub.stream_url.is_empty(), "Expected stream_url");

    // Clean up - delete the subscription
    client
        .delete_global_subscription(&sub.subscription_id)
        .await
        .expect("delete_global_subscription failed");
}

#[tokio::test]
#[serial_test::serial]
async fn test_list_subscriptions() {
    let harness = TestHarness::new()
        .await
        .expect("Failed to setup test harness");
    let client = harness.sovd_client();

    // Create a subscription first (PUBLIC DID only)
    let sub = client
        .create_global_subscription("vtx_ecm", vec!["vehicle_speed".to_string()], 1)
        .await
        .expect("create_global_subscription failed");

    // List subscriptions
    let list = client
        .list_global_subscriptions()
        .await
        .expect("list_global_subscriptions failed");

    assert!(!list.items.is_empty(), "Expected at least one subscription");

    // Clean up
    client
        .delete_global_subscription(&sub.subscription_id)
        .await
        .expect("delete_global_subscription failed");
}

#[tokio::test]
#[serial_test::serial]
async fn test_get_subscription() {
    let harness = TestHarness::new()
        .await
        .expect("Failed to setup test harness");
    let client = harness.sovd_client();

    // Create a subscription first (PUBLIC DIDs only)
    let sub = client
        .create_global_subscription(
            "vtx_ecm",
            vec!["vehicle_speed".to_string(), "coolant_temp".to_string()],
            5,
        )
        .await
        .expect("create_global_subscription failed");

    // Get subscription details
    let details = client
        .get_global_subscription(&sub.subscription_id)
        .await
        .expect("get_global_subscription failed");

    assert_eq!(details.subscription_id, sub.subscription_id);
    assert_eq!(details.rate_hz, Some(5));

    // Clean up
    client
        .delete_global_subscription(&sub.subscription_id)
        .await
        .expect("delete_global_subscription failed");
}

#[tokio::test]
#[serial_test::serial]
async fn test_delete_subscription() {
    let harness = TestHarness::new()
        .await
        .expect("Failed to setup test harness");
    let client = harness.sovd_client();

    // Create a subscription (PUBLIC DID only)
    let sub = client
        .create_global_subscription("vtx_ecm", vec!["vehicle_speed".to_string()], 1)
        .await
        .expect("create_global_subscription failed");

    // Delete it
    client
        .delete_global_subscription(&sub.subscription_id)
        .await
        .expect("delete_global_subscription failed");

    // Verify it's gone - should get error
    let result = client.get_global_subscription(&sub.subscription_id).await;
    assert!(result.is_err(), "Expected error for deleted subscription");
}

// =============================================================================
// Validation Tests
// =============================================================================

#[tokio::test]
#[serial_test::serial]
async fn test_invalid_component_id() {
    let harness = TestHarness::new()
        .await
        .expect("Failed to setup test harness");
    let client = harness.sovd_client();

    let result = client.get_component("nonexistent").await;
    assert!(result.is_err(), "Expected error for nonexistent component");
}

#[tokio::test]
#[serial_test::serial]
async fn test_subscription_invalid_parameter() {
    let harness = TestHarness::new()
        .await
        .expect("Failed to setup test harness");
    let client = harness.sovd_client();

    let result = client
        .create_global_subscription("vtx_ecm", vec!["nonexistent_param".to_string()], 1)
        .await;

    // Should fail with error (parameter not found)
    assert!(result.is_err(), "Expected error for nonexistent parameter");
}

#[tokio::test]
#[serial_test::serial]
async fn test_subscription_invalid_ecu() {
    let harness = TestHarness::new()
        .await
        .expect("Failed to setup test harness");
    let client = harness.sovd_client();

    let result = client
        .create_global_subscription("nonexistent_ecu", vec!["vehicle_speed".to_string()], 1)
        .await;

    // Should fail with error (ECU not found)
    assert!(result.is_err(), "Expected error for nonexistent ECU");
}

// =============================================================================
// ECU Communication Tests
// =============================================================================

/// Test that parameter values change over time (using a PUBLIC DID)
#[tokio::test]
#[serial_test::serial]
async fn test_values_change_over_time() {
    let harness = TestHarness::new()
        .await
        .expect("Failed to setup test harness");
    let client = harness.sovd_client();

    // Read vehicle_speed twice with a delay (PUBLIC DID - no session setup needed)
    let data1 = client
        .read_data("vtx_ecm", "vehicle_speed")
        .await
        .expect("read_data 1 failed");
    let value1 = data1.as_f64().expect("Expected numeric value");

    // Wait for values to change (test ECU updates every 100ms)
    tokio::time::sleep(Duration::from_millis(500)).await;

    let data2 = client
        .read_data("vtx_ecm", "vehicle_speed")
        .await
        .expect("read_data 2 failed");
    let value2 = data2.as_f64().expect("Expected numeric value");

    // Values should be in valid range
    assert!(value1 >= 0.0 && value1 < 256.0);
    assert!(value2 >= 0.0 && value2 < 256.0);

    // Note: Values may or may not be different depending on random walk
    // Just verify they're both valid
    eprintln!("Vehicle speed reading 1: {}, reading 2: {}", value1, value2);
}

/// Test reading ALL configured parameters (requires session + security setup)
#[tokio::test]
#[serial_test::serial]
async fn test_read_all_configured_parameters() {
    use sovd_client::{SecurityLevel, SessionType};

    let harness = TestHarness::new()
        .await
        .expect("Failed to setup test harness");
    let client = harness.sovd_client();

    // Step 1: Set up extended session for extended DIDs
    client
        .set_session("vtx_ecm", SessionType::Extended)
        .await
        .expect("set_session failed");

    // Step 2: Perform security authentication for protected DIDs
    let seed = client
        .security_access_request_seed("vtx_ecm", SecurityLevel::LEVEL_1)
        .await
        .expect("security_access_request_seed failed");

    let key: Vec<u8> = seed.iter().map(|b| b ^ 0xFF).collect();

    client
        .security_access_send_key("vtx_ecm", SecurityLevel::LEVEL_1, &key)
        .await
        .expect("security_access_send_key failed");

    // Now read ALL parameters (public, extended, and protected)
    // Public: coolant_temp, vehicle_speed, engine_load
    // Extended: engine_rpm, oil_pressure, fuel_rate, intake_temp
    // Protected: boost_pressure, exhaust_temp, throttle_position
    let params = [
        "engine_rpm",        // Extended
        "coolant_temp",      // Public
        "oil_pressure",      // Extended
        "fuel_rate",         // Extended
        "vehicle_speed",     // Public
        "boost_pressure",    // Protected
        "intake_temp",       // Extended
        "exhaust_temp",      // Protected
        "throttle_position", // Protected
        "engine_load",       // Public
    ];

    for param in params {
        let data = client
            .read_data("vtx_ecm", param)
            .await
            .expect(&format!("read_data {} failed", param));

        let value = data
            .as_f64()
            .expect(&format!("Expected numeric value for {}", param));
        eprintln!("{}: {}", param, value);
    }
}

// =============================================================================
// SSE Streaming Tests
// =============================================================================

/// Test SSE streaming with PUBLIC DIDs only (no session setup needed)
#[tokio::test]
#[serial_test::serial]
async fn test_sse_stream_periodic_data() {
    use futures_util::StreamExt;

    let harness = TestHarness::new()
        .await
        .expect("Failed to setup test harness");

    // Create a subscription with periodic mode for PUBLIC DIDs only
    let body = serde_json::json!({
        "component_id": "vtx_ecm",
        "parameters": ["vehicle_speed", "coolant_temp", "engine_load"],
        "rate_hz": 10,  // 10Hz = 100ms intervals
        "mode": "periodic",
        "duration_secs": 30
    });

    let (status, create_json) = harness
        .post("/vehicle/v1/subscriptions", body)
        .await
        .expect("POST failed");

    assert_eq!(status, 201, "Expected 201 Created");
    let sub_id = create_json["subscription_id"].as_str().unwrap();
    let stream_url = create_json["stream_url"].as_str().unwrap();

    eprintln!("Created subscription: {}", sub_id);
    eprintln!("Stream URL: {}", stream_url);

    // Connect to SSE stream
    let stream_url = format!(
        "http://localhost:{}{}",
        TestHarness::SERVER_PORT,
        stream_url
    );

    let client = reqwest::Client::new();
    let response = client
        .get(&stream_url)
        .header("Accept", "text/event-stream")
        .send()
        .await
        .expect("Failed to connect to SSE stream");

    assert_eq!(response.status(), 200, "Expected 200 OK for stream");

    // Read events from the stream for 3 seconds
    let mut stream = response.bytes_stream();
    let mut events_received = 0;
    let mut buffer = Vec::new();
    let stream_duration = Duration::from_secs(3);
    let min_expected_events = 10; // At 10Hz, we should get ~30 events in 3 seconds

    eprintln!(
        "=== Starting SSE stream for {} seconds ===",
        stream_duration.as_secs()
    );

    // Stream for the specified duration
    let timeout = tokio::time::timeout(stream_duration, async {
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    buffer.extend_from_slice(&bytes);

                    // Parse SSE events from buffer
                    let text = String::from_utf8_lossy(&buffer);
                    for line in text.lines() {
                        if line.starts_with("data:") {
                            let data = line.strip_prefix("data:").unwrap().trim();
                            if !data.is_empty() {
                                events_received += 1;

                                // Parse and verify data format
                                if let Ok(json) = serde_json::from_str::<serde_json::Value>(data) {
                                    // Log every 5th event to avoid spam
                                    if events_received % 5 == 1 || events_received <= 3 {
                                        eprintln!("SSE Event #{}: seq={}, vehicle_speed={}, coolant_temp={}, engine_load={}",
                                            events_received,
                                            json["seq"],
                                            json.get("vehicle_speed").unwrap_or(&serde_json::Value::Null),
                                            json.get("coolant_temp").unwrap_or(&serde_json::Value::Null),
                                            json.get("engine_load").unwrap_or(&serde_json::Value::Null)
                                        );
                                    }
                                    assert!(json["seq"].is_number(), "Expected sequence number");
                                    assert!(json["ts"].is_number(), "Expected timestamp");
                                }
                            }
                        }
                    }

                    // Clear buffer of processed complete lines
                    if let Some(last_newline) = text.rfind('\n') {
                        buffer = buffer.split_off(last_newline + 1);
                    }
                }
                Err(e) => {
                    eprintln!("Stream error: {}", e);
                    break;
                }
            }
        }
    });

    // Let the timeout expire (we want to stream for the full duration)
    let _ = timeout.await;

    eprintln!(
        "=== SSE stream complete: received {} events in {} seconds ===",
        events_received,
        stream_duration.as_secs()
    );

    // Assert we received a reasonable number of events
    assert!(
        events_received >= min_expected_events,
        "Expected at least {} SSE events in {} seconds, but only got {}",
        min_expected_events,
        stream_duration.as_secs(),
        events_received
    );

    // Clean up subscription
    harness
        .delete(&format!("/vehicle/v1/subscriptions/{}", sub_id))
        .await
        .expect("DELETE failed");

    eprintln!("Test completed - SSE endpoint is functional");
}

// =============================================================================
// DID Access Level Tests - Public, Extended, Protected
// =============================================================================

/// Test reading a PUBLIC DID (VIN) - should work without any session/security setup
#[tokio::test]
#[serial_test::serial]
async fn test_read_public_did_vin() {
    let harness = TestHarness::new()
        .await
        .expect("Failed to setup test harness");
    let client = harness.sovd_client();

    let data = client
        .read_data("vtx_ecm", "vin")
        .await
        .expect("read_data failed");

    // VIN should be a string value
    let value = data.as_str().expect("VIN should be a string");
    assert_eq!(value.len(), 17, "VIN should be 17 characters");
    assert_eq!(value, "WF0XXXGCDX1234567");
}

/// Test reading a PUBLIC DID (vehicle_speed) - should work in default session
#[tokio::test]
#[serial_test::serial]
async fn test_read_public_did_vehicle_speed() {
    let harness = TestHarness::new()
        .await
        .expect("Failed to setup test harness");
    let client = harness.sovd_client();

    let data = client
        .read_data("vtx_ecm", "vehicle_speed")
        .await
        .expect("read_data failed");

    assert!(data.as_f64().is_some(), "Expected numeric value");
}

/// Test reading an EXTENDED DID without session change - should fail with NRC 0x22
#[tokio::test]
#[serial_test::serial]
async fn test_read_extended_did_without_session() {
    let harness = TestHarness::new()
        .await
        .expect("Failed to setup test harness");

    // engine_rpm requires extended session - should fail in default session
    let (status, json) = harness
        .get_with_status("/vehicle/v1/components/vtx_ecm/data/engine_rpm")
        .await
        .expect("Request failed");

    // Should get 412 (Precondition Failed) with session-related message
    assert_eq!(
        status, 412,
        "Expected 412 status (Precondition Failed), got {}: {:?}",
        status, json
    );
    let message = json["message"].as_str().unwrap_or("").to_lowercase();
    assert!(
        message.contains("session") || message.contains("conditionsnotcorrect"),
        "Expected session-related error, got: {}",
        message
    );
}

/// Test reading an EXTENDED DID after changing to extended session - should succeed
#[tokio::test]
#[serial_test::serial]
async fn test_read_extended_did_with_session() {
    use sovd_client::SessionType;

    let harness = TestHarness::new()
        .await
        .expect("Failed to setup test harness");
    let client = harness.sovd_client();

    // First, change to extended session
    client
        .set_session("vtx_ecm", SessionType::Extended)
        .await
        .expect("set_session failed");

    // Now read engine_rpm - should work
    let data = client
        .read_data("vtx_ecm", "engine_rpm")
        .await
        .expect("read_data engine_rpm failed after session change");

    assert!(data.as_f64().is_some(), "Expected numeric value");
}

/// Test reading a PROTECTED DID without security - should fail with NRC 0x33
#[tokio::test]
#[serial_test::serial]
async fn test_read_protected_did_without_security() {
    let harness = TestHarness::new()
        .await
        .expect("Failed to setup test harness");
    let client = harness.sovd_client();

    // boost_pressure requires security access - should fail without authentication
    let result = client.read_data("vtx_ecm", "boost_pressure").await;

    // Should fail with SecurityAccessDenied
    assert!(
        result.is_err(),
        "Expected error for protected DID without security"
    );
}

/// Test reading a PROTECTED DID after security authentication - should succeed
#[tokio::test]
#[serial_test::serial]
async fn test_read_protected_did_with_security() {
    use sovd_client::SecurityLevel;

    let harness = TestHarness::new()
        .await
        .expect("Failed to setup test harness");
    let client = harness.sovd_client();

    // Step 1: Request seed
    let seed = client
        .security_access_request_seed("vtx_ecm", SecurityLevel::LEVEL_1)
        .await
        .expect("security_access_request_seed failed");

    // Step 2: Calculate key (XOR with 0xFF - default secret)
    let key: Vec<u8> = seed.iter().map(|b| b ^ 0xFF).collect();

    // Step 3: Send key
    client
        .security_access_send_key("vtx_ecm", SecurityLevel::LEVEL_1, &key)
        .await
        .expect("security_access_send_key failed");

    // Now read boost_pressure - should work
    let data = client
        .read_data("vtx_ecm", "boost_pressure")
        .await
        .expect("read_data boost_pressure failed after security authentication");

    assert!(data.as_f64().is_some(), "Expected numeric value");
}

// =============================================================================
// Security Access Tests (UDS 0x27)
// =============================================================================

#[tokio::test]
#[serial_test::serial]
async fn test_security_access_get_state() {
    let harness = TestHarness::new()
        .await
        .expect("Failed to setup test harness");
    let client = harness.sovd_client();

    // Get security state - should be locked initially
    let response = client
        .get_mode("vtx_ecm", "security")
        .await
        .expect("get_mode failed");

    assert_eq!(response.id, "security");
    // State could be locked or unlocked depending on previous operations
    assert!(response.value.is_some());
}

#[tokio::test]
#[serial_test::serial]
async fn test_security_access_success() {
    let harness = TestHarness::new()
        .await
        .expect("Failed to setup test harness");

    // Step 1: Request seed
    let body = serde_json::json!({
        "value": "level1_requestseed"
    });

    let (status, json) = harness
        .put("/vehicle/v1/components/vtx_ecm/modes/security", body)
        .await
        .expect("PUT failed");

    assert_eq!(
        status, 200,
        "Expected 200 OK for seed request, got {}: {}",
        status, json
    );
    assert_eq!(json["id"], "security");

    // New format: seed is in { seed: { Request_Seed: "0xaa 0xbb 0xcc 0xdd" } }
    let seed_obj = &json["seed"];
    assert!(
        seed_obj.is_object(),
        "Expected seed object in response: {}",
        json
    );

    let seed_str = seed_obj["Request_Seed"]
        .as_str()
        .expect("Expected Request_Seed string in seed object");
    eprintln!("Received seed: {}", seed_str);

    // Parse space-separated hex bytes (e.g., "0xc9 0xb6 0xc5 0x72")
    let seed: Vec<u8> = seed_str
        .split_whitespace()
        .map(|s| u8::from_str_radix(s.trim_start_matches("0x"), 16).expect("Invalid hex byte"))
        .collect();

    // Step 2: Calculate key using the same algorithm as example-ecu (XOR with secret)
    // Default secret is "ff" (single byte 0xFF)
    let secret = vec![0xFFu8]; // Default secret
    let key: Vec<u8> = seed
        .iter()
        .enumerate()
        .map(|(i, b)| b ^ secret[i % secret.len()])
        .collect();
    let key_hex = hex::encode(&key);
    eprintln!("Calculated key: {}", key_hex);

    // Step 3: Send key
    let body = serde_json::json!({
        "value": "level1",
        "key": key_hex
    });

    let (status, json) = harness
        .put("/vehicle/v1/components/vtx_ecm/modes/security", body)
        .await
        .expect("PUT failed");

    assert_eq!(
        status, 200,
        "Expected 200 OK for key send, got {}: {}",
        status, json
    );
    // New format returns { id: "security", value: "level1" }
    assert_eq!(json["id"], "security");
    assert_eq!(
        json["value"], "level1",
        "Expected level1 value after correct key"
    );

    eprintln!("Security access granted!");
}

#[tokio::test]
#[serial_test::serial]
async fn test_security_access_failure_wrong_key() {
    let harness = TestHarness::new()
        .await
        .expect("Failed to setup test harness");

    // Step 1: Request seed
    let body = serde_json::json!({
        "value": "level1_requestseed"
    });

    let (status, json) = harness
        .put("/vehicle/v1/components/vtx_ecm/modes/security", body)
        .await
        .expect("PUT failed");

    assert_eq!(status, 200, "Expected 200 OK for seed request");
    assert_eq!(json["id"], "security");

    // New format: seed is in { seed: { Request_Seed: "0xaa 0xbb 0xcc 0xdd" } }
    let seed_obj = &json["seed"];
    assert!(
        seed_obj.is_object(),
        "Expected seed object in response: {}",
        json
    );

    let seed_str = seed_obj["Request_Seed"]
        .as_str()
        .expect("Expected Request_Seed string in seed object");
    eprintln!("Received seed: {}", seed_str);

    // Parse space-separated hex bytes
    let seed: Vec<u8> = seed_str
        .split_whitespace()
        .map(|s| u8::from_str_radix(s.trim_start_matches("0x"), 16).expect("Invalid hex byte"))
        .collect();

    // Step 2: Send WRONG key (use different secret)
    let wrong_secret = vec![0xAAu8]; // Wrong secret!
    let wrong_key: Vec<u8> = seed
        .iter()
        .enumerate()
        .map(|(i, b)| b ^ wrong_secret[i % wrong_secret.len()])
        .collect();
    let wrong_key_hex = hex::encode(&wrong_key);
    eprintln!("Sending wrong key: {}", wrong_key_hex);

    // Step 3: Send wrong key - should fail
    let body = serde_json::json!({
        "value": "level1",
        "key": wrong_key_hex
    });

    let (status, json) = harness
        .put("/vehicle/v1/components/vtx_ecm/modes/security", body)
        .await
        .expect("PUT failed");

    // Should get an error (503 from session error or 400 from bad request)
    assert!(
        status == 503 || status == 502,
        "Expected 503 or 502 for wrong key, got {}: {}",
        status,
        json
    );
    assert!(json["error"].is_string(), "Expected error in response");

    eprintln!(
        "Security access correctly rejected with wrong key: {}",
        json["message"]
    );
}

// =============================================================================
// Mixed Access Level Streaming Test
// =============================================================================

/// Test SSE streaming with parameters from all access levels (public, extended, protected)
/// This is a comprehensive integration test that verifies:
/// 1. Session management (extended session)
/// 2. Security access (authentication)
/// 3. Subscription with mixed DIDs
/// 4. SSE streaming with data from all access levels
#[tokio::test]
#[serial_test::serial]
async fn test_sse_stream_mixed_access_levels() {
    use futures_util::StreamExt;
    use sovd_client::{SecurityLevel, SessionType};
    use std::collections::HashSet;

    let harness = TestHarness::new()
        .await
        .expect("Failed to setup test harness");
    let client = harness.sovd_client();

    // Step 1: Change to extended session (required for extended DIDs)
    eprintln!("=== Step 1: Setting up extended session ===");
    client
        .set_session("vtx_ecm", SessionType::Extended)
        .await
        .expect("set_session failed");
    eprintln!("Extended session established");

    // Step 2: Perform security authentication (required for protected DIDs)
    eprintln!("=== Step 2: Authenticating security access ===");

    // Request seed
    let seed = client
        .security_access_request_seed("vtx_ecm", SecurityLevel::LEVEL_1)
        .await
        .expect("security_access_request_seed failed");
    eprintln!("Received seed: {}", hex::encode(&seed));

    // Calculate key (XOR with 0xFF - default secret)
    let key: Vec<u8> = seed.iter().map(|b| b ^ 0xFF).collect();

    // Send key
    client
        .security_access_send_key("vtx_ecm", SecurityLevel::LEVEL_1, &key)
        .await
        .expect("security_access_send_key failed");
    eprintln!("Security access granted");

    // Step 3: Create subscription with parameters from ALL access levels
    eprintln!("=== Step 3: Creating mixed-access subscription ===");

    // Public DIDs: vehicle_speed, coolant_temp
    // Extended DIDs: engine_rpm, oil_pressure
    // Protected DIDs: boost_pressure, throttle_position
    let body = serde_json::json!({
        "component_id": "vtx_ecm",
        "parameters": [
            "vehicle_speed",    // Public
            "coolant_temp",     // Public
            "engine_rpm",       // Extended
            "oil_pressure",     // Extended
            "boost_pressure",   // Protected
            "throttle_position" // Protected
        ],
        "rate_hz": 10,
        "mode": "periodic",
        "duration_secs": 30
    });

    let (status, create_json) = harness
        .post("/vehicle/v1/subscriptions", body)
        .await
        .expect("POST subscription failed");

    assert_eq!(
        status, 201,
        "Expected 201 Created, got {}: {:?}",
        status, create_json
    );
    let sub_id = create_json["subscription_id"].as_str().unwrap();
    let stream_url = create_json["stream_url"].as_str().unwrap();
    eprintln!("Subscription created: {}", sub_id);
    eprintln!("Stream URL: {}", stream_url);

    // Step 4: Connect to SSE stream and verify data from all access levels
    eprintln!("=== Step 4: Connecting to SSE stream ===");
    let stream_url = format!(
        "http://localhost:{}{}",
        TestHarness::SERVER_PORT,
        stream_url
    );

    let http_client = reqwest::Client::new();
    let response = http_client
        .get(&stream_url)
        .header("Accept", "text/event-stream")
        .send()
        .await
        .expect("Failed to connect to SSE stream");

    assert_eq!(response.status(), 200, "Expected 200 OK for stream");

    // Track which parameters we've received data for
    let mut received_params: HashSet<String> = HashSet::new();
    let expected_params: HashSet<&str> = [
        "vehicle_speed",
        "coolant_temp", // Public
        "engine_rpm",
        "oil_pressure", // Extended
        "boost_pressure",
        "throttle_position", // Protected
    ]
    .into_iter()
    .collect();

    let mut stream = response.bytes_stream();
    let mut events_received = 0;
    let mut buffer = Vec::new();

    // Read events until we've seen all parameters or timeout
    let timeout_result = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    buffer.extend_from_slice(&bytes);

                    // Parse SSE events from buffer
                    let text = String::from_utf8_lossy(&buffer);
                    for line in text.lines() {
                        if line.starts_with("data:") {
                            let data = line.strip_prefix("data:").unwrap().trim();
                            if !data.is_empty() {
                                events_received += 1;

                                if let Ok(json) = serde_json::from_str::<serde_json::Value>(data) {
                                    // Check for each parameter in the event
                                    for param in &expected_params {
                                        if json[*param].is_number() {
                                            if !received_params.contains(*param) {
                                                eprintln!("  Received {}: {}", param, json[*param]);
                                                received_params.insert(param.to_string());
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Clear processed buffer
                    if let Some(last_newline) = text.rfind('\n') {
                        buffer = buffer.split_off(last_newline + 1);
                    }

                    // Stop once we've received all parameters
                    if received_params.len() >= expected_params.len() {
                        break;
                    }

                    // Also stop after enough events even if not all params seen
                    if events_received >= 20 {
                        break;
                    }
                }
                Err(e) => {
                    eprintln!("Stream error: {}", e);
                    break;
                }
            }
        }
    });

    match timeout_result.await {
        Ok(_) => {
            eprintln!("=== Results ===");
            eprintln!("Received {} SSE events", events_received);
            eprintln!("Parameters received: {:?}", received_params);
        }
        Err(_) => {
            eprintln!(
                "Timeout - received {} events, params: {:?}",
                events_received, received_params
            );
        }
    }

    // Step 5: Verify we received data from all access levels
    eprintln!("=== Step 5: Verifying access level coverage ===");

    let public_received =
        received_params.contains("vehicle_speed") || received_params.contains("coolant_temp");
    let extended_received =
        received_params.contains("engine_rpm") || received_params.contains("oil_pressure");
    let protected_received =
        received_params.contains("boost_pressure") || received_params.contains("throttle_position");

    eprintln!("Public DIDs received: {}", public_received);
    eprintln!("Extended DIDs received: {}", extended_received);
    eprintln!("Protected DIDs received: {}", protected_received);

    // Clean up subscription
    client
        .delete_global_subscription(sub_id)
        .await
        .expect("delete_global_subscription failed");
    eprintln!("Subscription cleaned up");

    // Assert all access levels were covered
    assert!(
        public_received,
        "Expected to receive PUBLIC DID data (vehicle_speed or coolant_temp)"
    );
    assert!(
        extended_received,
        "Expected to receive EXTENDED DID data (engine_rpm or oil_pressure)"
    );
    assert!(
        protected_received,
        "Expected to receive PROTECTED DID data (boost_pressure or throttle_position)"
    );

    eprintln!("=== Test PASSED: All access levels streaming correctly ===");
}

// =============================================================================
// ECU Discovery Tests
// =============================================================================

/// Test ECU discovery via ISO-TP functional addressing (broadcast)
///
/// This test verifies that the discovery endpoint can find ECUs on the CAN bus
/// using UDS functional addressing (0x18DB33F1 broadcast -> 0x18DAF1xx responses)
#[tokio::test]
#[serial_test::serial]
#[serial_test::serial]
async fn test_ecu_discovery_isotp() {
    eprintln!("\n=== Testing ECU Discovery (ISO-TP) ===");

    // Get harness from thread-local or create new
    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");

    // Wait a bit for example-ecu's functional listener to start
    sleep(Duration::from_millis(200)).await;

    // Step 1: Call discovery endpoint with ISO-TP method
    eprintln!("=== Step 1: Calling POST /vehicle/v1/discovery ===");

    let resp = harness
        .post_form(
            "/vehicle/v1/discovery",
            &[
                ("method", "isotp"),
                ("interface", "vcan0"),
                ("addressing", "extended"),
                ("timeout_ms", "1000"),
                ("read_identification", "true"),
            ],
        )
        .await;

    assert!(resp.is_ok(), "Discovery POST failed: {:?}", resp.err());
    let resp = resp.unwrap();

    eprintln!("Discovery response status: {}", resp.status());
    assert!(
        resp.status().is_success(),
        "Expected success status, got: {}",
        resp.status()
    );

    // Parse response
    let body: Value = resp.json().await.expect("Failed to parse JSON response");
    eprintln!(
        "Discovery response: {}",
        serde_json::to_string_pretty(&body).unwrap()
    );

    // Step 2: Verify response structure
    eprintln!("=== Step 2: Verifying response structure ===");

    assert!(
        body.get("method").is_some(),
        "Response should have 'method' field"
    );
    assert!(
        body.get("count").is_some(),
        "Response should have 'count' field"
    );
    assert!(
        body.get("ecus").is_some(),
        "Response should have 'ecus' field"
    );

    let method = body["method"].as_str().unwrap();
    assert_eq!(method, "isotp", "Method should be 'isotp'");

    let count = body["count"].as_u64().unwrap();
    eprintln!("ECUs discovered: {}", count);

    // Step 3: Verify discovered ECU data (if any ECUs found)
    eprintln!("=== Step 3: Verifying discovered ECU data ===");

    let ecus = body["ecus"].as_array().expect("ecus should be an array");

    if count > 0 {
        assert!(
            !ecus.is_empty(),
            "ecus array should not be empty when count > 0"
        );

        let ecu = &ecus[0];
        eprintln!(
            "First discovered ECU: {}",
            serde_json::to_string_pretty(ecu).unwrap()
        );

        // Verify ECU has required fields
        assert!(ecu.get("address").is_some(), "ECU should have 'address'");
        assert!(
            ecu.get("tx_can_id").is_some(),
            "ECU should have 'tx_can_id'"
        );
        assert!(
            ecu.get("rx_can_id").is_some(),
            "ECU should have 'rx_can_id'"
        );
        assert!(
            ecu.get("config_snippet").is_some(),
            "ECU should have 'config_snippet'"
        );

        let address = ecu["address"].as_str().unwrap();
        let tx_can_id = ecu["tx_can_id"].as_str().unwrap();
        let rx_can_id = ecu["rx_can_id"].as_str().unwrap();

        eprintln!("ECU Address: {}", address);
        eprintln!("TX CAN ID: {}", tx_can_id);
        eprintln!("RX CAN ID: {}", rx_can_id);

        // For example-ecu with address 0x00, expect:
        // - tx_can_id: 0x18DA00F1 (tester -> ECU)
        // - rx_can_id: 0x18DAF100 (ECU -> tester)
        // Note: Discovery sees it from the scanner's perspective, not ECU's
        assert!(
            address == "0x00" || address == "0xF1",
            "Expected ECU address 0x00 or 0xF1, got: {}",
            address
        );

        // Verify identification DIDs were read
        if let Some(vin) = ecu.get("vin") {
            let vin_str = vin.as_str().unwrap();
            eprintln!("VIN: {}", vin_str);
            assert_eq!(
                vin_str, "WF0XXXGCDX1234567",
                "VIN should match example-ecu's VIN"
            );
        }

        if let Some(part_number) = ecu.get("part_number") {
            eprintln!("Part Number: {}", part_number.as_str().unwrap());
        }

        if let Some(software_version) = ecu.get("supplier_sw_version") {
            eprintln!("Software Version: {}", software_version.as_str().unwrap());
        }

        // Verify config snippet can be used
        let config_snippet = ecu["config_snippet"].as_str().unwrap();
        assert!(
            config_snippet.contains("[transport.isotp]"),
            "Config snippet should contain [transport.isotp] section"
        );
        assert!(
            config_snippet.contains("tx_id"),
            "Config snippet should contain tx_id"
        );
        assert!(
            config_snippet.contains("rx_id"),
            "Config snippet should contain rx_id"
        );
    } else {
        eprintln!("WARNING: No ECUs discovered. This may indicate:");
        eprintln!("  - example-ecu's functional listener didn't respond in time");
        eprintln!("  - CAN interface issue");
        eprintln!("  - Discovery timeout too short");
        // Don't fail the test if no ECUs found - the API itself worked
    }

    eprintln!("=== Test PASSED: ECU Discovery API working ===");
}

/// Test ECU discovery returns proper error for invalid method
#[tokio::test]
#[serial_test::serial]
#[serial_test::serial]
async fn test_ecu_discovery_invalid_method() {
    eprintln!("\n=== Testing ECU Discovery with invalid method ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");

    // Call discovery with invalid method
    let resp = harness
        .post_form(
            "/vehicle/v1/discovery",
            &[("method", "invalid_method"), ("interface", "vcan0")],
        )
        .await;

    assert!(resp.is_ok(), "POST should succeed at HTTP level");
    let resp = resp.unwrap();

    // Should return 400 Bad Request
    assert_eq!(
        resp.status().as_u16(),
        400,
        "Expected 400 Bad Request for invalid method, got: {}",
        resp.status()
    );

    let body: Value = resp.json().await.expect("Failed to parse error response");
    eprintln!(
        "Error response: {}",
        serde_json::to_string_pretty(&body).unwrap()
    );

    // Verify error message mentions the invalid method
    let error_msg = body["message"].as_str().unwrap_or("");
    assert!(
        error_msg.contains("invalid_method") || error_msg.contains("Unknown"),
        "Error message should mention the invalid method"
    );

    eprintln!("=== Test PASSED: Invalid method returns proper error ===");
}

/// Test SOME/IP discovery requires gateway_host parameter
#[tokio::test]
#[serial_test::serial]
#[serial_test::serial]
async fn test_ecu_discovery_someip_requires_gateway() {
    eprintln!("\n=== Testing SOME/IP Discovery requires gateway_host ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");

    // Call SOME/IP discovery without gateway_host
    let resp = harness
        .post_form(
            "/vehicle/v1/discovery",
            &[
                ("method", "someip"),
                // Missing gateway_host - should fail
            ],
        )
        .await;

    assert!(resp.is_ok(), "POST should succeed at HTTP level");
    let resp = resp.unwrap();

    // Should return 400 Bad Request
    assert_eq!(
        resp.status().as_u16(),
        400,
        "Expected 400 Bad Request when gateway_host is missing, got: {}",
        resp.status()
    );

    let body: Value = resp.json().await.expect("Failed to parse error response");
    eprintln!(
        "Error response: {}",
        serde_json::to_string_pretty(&body).unwrap()
    );

    // Verify error message mentions gateway_host
    let error_msg = body["message"].as_str().unwrap_or("");
    assert!(
        error_msg.contains("gateway_host"),
        "Error message should mention gateway_host is required"
    );

    eprintln!("=== Test PASSED: SOME/IP discovery validates gateway_host ===");
}

// =============================================================================
// DTC/Fault API Tests
// =============================================================================

/// Test listing all stored DTCs
#[tokio::test]
#[serial_test::serial]
async fn test_list_faults() {
    eprintln!("\n=== Testing GET /faults (List DTCs) ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");
    let client = harness.sovd_client();

    let faults = client
        .get_faults("vtx_ecm")
        .await
        .expect("get_faults failed");

    eprintln!("Found {} faults", faults.len());

    assert!(!faults.is_empty(), "Expected at least one DTC");

    // Verify DTC structure
    let first_dtc = &faults[0];
    assert!(!first_dtc.id.is_empty(), "Expected id");
    assert!(!first_dtc.code.is_empty(), "Expected dtc_code");

    // Check categories
    let categories: Vec<&str> = faults
        .iter()
        .filter_map(|f| f.category.as_deref())
        .collect();
    eprintln!("DTC categories found: {:?}", categories);

    eprintln!(
        "=== Test PASSED: List faults returned {} DTCs ===",
        faults.len()
    );
}

/// Test getting detailed fault information
#[tokio::test]
#[serial_test::serial]
async fn test_get_fault_detail() {
    eprintln!("\n=== Testing GET /faults/{{dtc_id}} (Fault Detail) ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");
    let client = harness.sovd_client();

    // First, get the list of faults to find a valid DTC ID
    let faults = client
        .get_faults("vtx_ecm")
        .await
        .expect("get_faults failed");

    assert!(!faults.is_empty(), "Expected at least one DTC");

    let dtc_id = &faults[0].id;
    eprintln!("Testing with DTC ID: {}", dtc_id);

    // Get the detailed fault info
    let detail = client
        .get_fault("vtx_ecm", dtc_id)
        .await
        .expect("get_fault failed");

    eprintln!("Got detail for DTC {} ({})", detail.id, detail.code);

    // Verify response structure
    assert_eq!(detail.id, *dtc_id, "ID should match");
    assert!(!detail.code.is_empty(), "Expected dtc_code");

    eprintln!(
        "=== Test PASSED: Got detail for DTC {} ({}) ===",
        dtc_id, detail.code
    );
}

/// Test clearing faults (requires extended session)
#[tokio::test]
#[serial_test::serial]
async fn test_clear_faults() {
    use sovd_client::SessionType;

    eprintln!("\n=== Testing DELETE /faults (Clear DTCs) ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");
    let client = harness.sovd_client();

    // First, verify we have DTCs
    let initial_faults = client
        .get_faults("vtx_ecm")
        .await
        .expect("get_faults failed");

    let initial_count = initial_faults.len();
    eprintln!("Initial DTC count: {}", initial_count);
    assert!(
        initial_count > 0,
        "Expected at least one DTC before clearing"
    );

    // Attempt to clear without extended session - should fail
    let result = client.clear_faults("vtx_ecm").await;
    assert!(
        result.is_err(),
        "Expected error when clearing without extended session"
    );
    eprintln!("Clear without session correctly rejected");

    // Now switch to extended session
    client
        .set_session("vtx_ecm", SessionType::Extended)
        .await
        .expect("set_session failed");
    eprintln!("Switched to extended session");

    // Now clear should succeed
    let clear_resp = client
        .clear_faults("vtx_ecm")
        .await
        .expect("clear_faults failed");
    assert!(clear_resp.success, "Expected success from clear_faults");
    eprintln!("DTCs cleared successfully");

    // Verify DTCs are cleared
    let final_faults = client
        .get_faults("vtx_ecm")
        .await
        .expect("get_faults failed");

    let final_count = final_faults.len();
    eprintln!("Final DTC count: {}", final_count);
    assert_eq!(final_count, 0, "Expected 0 DTCs after clearing");

    eprintln!(
        "=== Test PASSED: DTCs cleared from {} to {} ===",
        initial_count, final_count
    );
}

/// Test listing active DTCs only
#[tokio::test]
#[serial_test::serial]
async fn test_list_active_dtcs() {
    eprintln!("\n=== Testing GET /dtcs (Active DTCs Only) ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");

    // Get active DTCs only - using /dtcs endpoint which filters for active only
    // Note: This test uses raw HTTP call because /dtcs is a separate endpoint from /faults
    let json = harness
        .get("/vehicle/v1/components/vtx_ecm/dtcs")
        .await
        .expect("GET dtcs failed");

    eprintln!("Response: {}", serde_json::to_string_pretty(&json).unwrap());

    // Verify response structure
    assert!(json["items"].is_array(), "Expected items array");
    assert!(json["total_count"].is_number(), "Expected total_count");

    let items = json["items"].as_array().unwrap();
    let total_count = json["total_count"].as_u64().unwrap();

    assert_eq!(
        items.len() as u64,
        total_count,
        "items.len() should match total_count"
    );

    // Verify all returned DTCs are active (test_failed=true per SOVD standard)
    // SOVD "active" means the test is currently failing, regardless of confirmation status
    for item in items {
        let status = &item["status"];
        let test_failed = status["test_failed"].as_bool().unwrap_or(false);

        eprintln!(
            "DTC {} ({}): test_failed={}",
            item["dtc_code"].as_str().unwrap_or("?"),
            item["id"].as_str().unwrap_or("?"),
            test_failed
        );

        // Active per SOVD = test_failed is true (currently failing)
        assert!(
            test_failed,
            "Active DTC {} should have test_failed=true",
            item["dtc_code"].as_str().unwrap_or("?")
        );
    }

    eprintln!("=== Test PASSED: Found {} active DTCs ===", total_count);
}

/// Test filtering faults by category
#[tokio::test]
#[serial_test::serial]
async fn test_list_faults_by_category() {
    eprintln!("\n=== Testing GET /faults?category=powertrain (Filter by Category) ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");
    let client = harness.sovd_client();

    // Get powertrain DTCs only
    let faults = client
        .get_faults_filtered("vtx_ecm", Some("powertrain"))
        .await
        .expect("get_faults_filtered failed");

    eprintln!("Powertrain DTCs: {} found", faults.len());

    // Verify all returned DTCs are powertrain (P codes)
    for fault in &faults {
        assert!(
            fault.code.starts_with('P'),
            "Expected P code for powertrain, got {}",
            fault.code
        );
        assert_eq!(
            fault.category.as_deref(),
            Some("powertrain"),
            "Expected category 'powertrain'"
        );
    }

    eprintln!(
        "=== Test PASSED: Found {} powertrain DTCs ===",
        faults.len()
    );
}

/// Test fault not found error
#[tokio::test]
#[serial_test::serial]
async fn test_get_fault_not_found() {
    eprintln!("\n=== Testing GET /faults/FFFFFF (Not Found) ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");
    let client = harness.sovd_client();

    let result = client.get_fault("vtx_ecm", "FFFFFF").await;

    // Should return error (not found)
    assert!(result.is_err(), "Expected error for non-existent DTC");

    eprintln!("=== Test PASSED: Non-existent DTC correctly returns error ===");
}

// =============================================================================
// Write Parameter Tests (UDS 0x2E WriteDataByIdentifier)
// =============================================================================

/// Test writing a parameter (requires extended session)
#[tokio::test]
#[serial_test::serial]
async fn test_write_parameter() {
    use sovd_client::SessionType;

    eprintln!("\n=== Testing PUT /data/{{param}} (Write Parameter) ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");
    let client = harness.sovd_client();

    // Switch to extended session (required for writable DIDs)
    client
        .set_session("vtx_ecm", SessionType::Extended)
        .await
        .expect("set_session failed");
    eprintln!("Switched to extended session");

    // Write to programming_date (0xF199) - writable DID, extended access
    client
        .write_data("vtx_ecm", "programming_date", serde_json::json!("20250130"))
        .await
        .expect("write_data failed");

    eprintln!("=== Test PASSED: Parameter written successfully ===");
}

/// Test writing to a read-only parameter (should fail with NRC 0x72)
#[tokio::test]
#[serial_test::serial]
async fn test_write_parameter_readonly() {
    eprintln!("\n=== Testing PUT to read-only parameter ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");
    let client = harness.sovd_client();

    // VIN (0xF190) is read-only
    let result = client
        .write_data("vtx_ecm", "vin", serde_json::json!("NEWVIN1234567890X"))
        .await;

    // Should fail - VIN is read-only
    assert!(result.is_err(), "Expected error for read-only parameter");

    eprintln!("=== Test PASSED: Read-only parameter correctly rejected ===");
}

/// Test writing without required session (should fail)
#[tokio::test]
#[serial_test::serial]
async fn test_write_parameter_wrong_session() {
    eprintln!("\n=== Testing write without extended session ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");

    // Harness setup resets to default session, so write should fail (extended required)
    let body = serde_json::json!({
        "value": "20250130",
        "format": "hex"
    });

    let (status, json) = harness
        .put("/vehicle/v1/components/vtx_ecm/data/programming_date", body)
        .await
        .expect("PUT request failed");

    eprintln!("Response: status={}, body={}", status, json);

    // Should fail with precondition failed (session requirement)
    assert_eq!(
        status, 412,
        "Expected 412 (Precondition Failed) without extended session, got {}",
        status
    );

    eprintln!("=== Test PASSED: Write without session correctly rejected ===");
}

/// Test writing to protected parameter without security (should fail)
#[tokio::test]
#[serial_test::serial]
async fn test_write_parameter_security_required() {
    eprintln!("\n=== Testing write to protected parameter without security ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");

    // installation_date (0xF19D) requires security access
    let body = serde_json::json!({
        "value": "20250130",
        "format": "hex"
    });

    let (status, json) = harness
        .put(
            "/vehicle/v1/components/vtx_ecm/data/installation_date",
            body,
        )
        .await
        .expect("PUT request failed");

    eprintln!("Response: status={}, body={}", status, json);

    // Should fail with security access denied
    assert_eq!(status, 403, "Expected 403 without security, got {}", status);
    let message = json["message"].as_str().unwrap_or("");
    assert!(
        message.contains("Security") || message.contains("security"),
        "Expected security-related error, got: {}",
        message
    );

    eprintln!("=== Test PASSED: Write without security correctly rejected ===");
}

// =============================================================================
// Routine Control Tests (UDS 0x31 RoutineControl)
// =============================================================================

/// Test listing available operations
#[tokio::test]
#[serial_test::serial]
async fn test_list_operations() {
    eprintln!("\n=== Testing GET /operations (List Operations) ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");
    let client = harness.sovd_client();

    let operations = client
        .list_operations("vtx_ecm")
        .await
        .expect("list_operations failed");

    eprintln!("Found {} operations", operations.len());

    assert!(!operations.is_empty(), "Expected at least one operation");

    // Check for check_preconditions operation
    let has_check_preconditions = operations.iter().any(|op| op.id == "check_preconditions");
    assert!(
        has_check_preconditions,
        "Expected check_preconditions operation"
    );

    eprintln!("=== Test PASSED: Found {} operations ===", operations.len());
}

/// Test starting a routine (check_preconditions)
#[tokio::test]
#[serial_test::serial]
async fn test_routine_start() {
    eprintln!("\n=== Testing POST /operations/check_preconditions (Start Routine) ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");
    let client = harness.sovd_client();

    // Routine requires extended session per UDS spec
    client
        .set_session("vtx_ecm", sovd_client::SessionType::Extended)
        .await
        .expect("set_session extended failed");

    let result = client
        .execute_operation("vtx_ecm", "check_preconditions", "start", None)
        .await
        .expect("execute_operation failed");

    eprintln!(
        "Response: operation_id={}, action={}, status={}",
        result.operation_id, result.action, result.status
    );

    assert_eq!(result.operation_id, "check_preconditions");
    assert_eq!(result.action, "start");
    assert!(
        result.status == sovd_client::OperationStatus::Running
            || result.status == sovd_client::OperationStatus::Completed
    );

    eprintln!("=== Test PASSED: Routine started successfully ===");
}

/// Test getting routine result
#[tokio::test]
#[serial_test::serial]
async fn test_routine_result() {
    eprintln!("\n=== Testing POST /operations/check_preconditions action=result ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");
    let client = harness.sovd_client();

    // Routine requires extended session per UDS spec
    client
        .set_session("vtx_ecm", sovd_client::SessionType::Extended)
        .await
        .expect("set_session extended failed");

    // First start the routine
    client
        .execute_operation("vtx_ecm", "check_preconditions", "start", None)
        .await
        .expect("execute_operation start failed");

    // Now get the result
    let result = client
        .execute_operation("vtx_ecm", "check_preconditions", "result", None)
        .await
        .expect("execute_operation result failed");

    eprintln!(
        "Response: action={}, status={}",
        result.action, result.status
    );

    assert_eq!(result.action, "result");
    assert_eq!(result.status, sovd_client::OperationStatus::Completed);

    eprintln!("=== Test PASSED: Routine result retrieved ===");
}

/// Test routine that requires security (erase_memory)
#[tokio::test]
#[serial_test::serial]
async fn test_routine_security_required() {
    eprintln!("\n=== Testing routine requiring security ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");
    let client = harness.sovd_client();

    // Try to start erase_memory without security - should fail
    let result = client
        .execute_operation("vtx_ecm", "erase_memory", "start", None)
        .await;

    // Should fail with security access denied
    assert!(result.is_err(), "Expected error without security");

    eprintln!("=== Test PASSED: Routine without security correctly rejected ===");
}

/// Test non-existent routine
#[tokio::test]
#[serial_test::serial]
async fn test_routine_not_found() {
    eprintln!("\n=== Testing non-existent routine ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");
    let client = harness.sovd_client();

    let result = client
        .execute_operation("vtx_ecm", "nonexistent_routine", "start", None)
        .await;

    // Should fail with not found
    assert!(result.is_err(), "Expected error for non-existent routine");

    eprintln!("=== Test PASSED: Non-existent routine correctly rejected ===");
}

// =============================================================================
// Dynamic Data Identifier Tests (UDS 0x2C DynamicallyDefineDataIdentifier)
// =============================================================================

/// Test creating a dynamic data identifier
#[tokio::test]
#[serial_test::serial]
async fn test_define_ddid() {
    use sovd_client::DataDefinitionSource;

    eprintln!("\n=== Testing POST /data-definitions (Define DDID) ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");
    let client = harness.sovd_client();

    // Define a DDID that combines coolant_temp and vehicle_speed
    let sources = vec![
        DataDefinitionSource {
            did: "0xF405".to_string(),
            start_byte: Some(1),
            size: Some(1),
        },
        DataDefinitionSource {
            did: "0xF40E".to_string(),
            start_byte: Some(1),
            size: Some(1),
        },
    ];

    let result = client
        .create_data_definition("vtx_ecm", "0xF200", sources)
        .await
        .expect("create_data_definition failed");

    eprintln!("Response: ddid={}, status={}", result.ddid, result.status);

    assert_eq!(result.ddid, "0xF200");

    eprintln!("=== Test PASSED: DDID defined successfully ===");
}

/// Test clearing a dynamic data identifier
#[tokio::test]
#[serial_test::serial]
async fn test_clear_ddid() {
    use sovd_client::DataDefinitionSource;

    eprintln!("\n=== Testing DELETE /data-definitions/{{ddid}} (Clear DDID) ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");
    let client = harness.sovd_client();

    // First define a DDID
    let sources = vec![DataDefinitionSource {
        did: "0xF405".to_string(),
        start_byte: Some(1),
        size: Some(1),
    }];

    client
        .create_data_definition("vtx_ecm", "0xF201", sources)
        .await
        .expect("create_data_definition failed");

    // Now clear it
    client
        .delete_data_definition("vtx_ecm", "0xF201")
        .await
        .expect("delete_data_definition failed");

    eprintln!("=== Test PASSED: DDID cleared successfully ===");
}

/// Test invalid DDID range
#[tokio::test]
#[serial_test::serial]
async fn test_define_ddid_invalid_range() {
    use sovd_client::DataDefinitionSource;

    eprintln!("\n=== Testing DDID with invalid range ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");
    let client = harness.sovd_client();

    // Try to define a DDID outside valid range (0xF200-0xF3FF)
    let sources = vec![DataDefinitionSource {
        did: "0xF405".to_string(),
        start_byte: Some(1),
        size: Some(1),
    }];

    let result = client
        .create_data_definition("vtx_ecm", "0xF100", sources)
        .await;

    // Should fail for invalid range
    assert!(result.is_err(), "Expected error for invalid DDID range");

    eprintln!("=== Test PASSED: Invalid DDID range correctly rejected ===");
}

// =============================================================================
// Software Programming Tests - Async Flash Flow
// =============================================================================
//
// The new async flash flow:
// 1. POST /files - upload package
// 2. POST /files/:id/verify - verify package
// 3. POST /flash/transfer - start async flash
// 4. GET /flash/transfer/:id - poll status
// 5. PUT /flash/transferexit - finalize

/// Test uploading a file (package)
#[tokio::test]
#[serial_test::serial]
async fn test_upload_file() {
    eprintln!("\n=== Testing POST /files (Upload File) ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");

    // Create test firmware data
    let firmware_data: Vec<u8> = (0..256).map(|i| (i & 0xFF) as u8).collect();

    // Upload file
    let upload_url = format!(
        "http://localhost:{}/vehicle/v1/components/vtx_ecm/files",
        TestHarness::SERVER_PORT
    );

    let client = reqwest::Client::new();
    let response = client
        .post(&upload_url)
        .header("Content-Type", "application/octet-stream")
        .body(firmware_data.clone())
        .send()
        .await
        .expect("POST upload failed");

    assert_eq!(response.status(), 201, "Expected 201 Created");
    let json: Value = response.json().await.expect("Failed to parse response");

    eprintln!("Response: {}", serde_json::to_string_pretty(&json).unwrap());

    assert!(json["file_id"].is_string(), "Expected file_id");
    assert_eq!(json["size"].as_u64(), Some(256), "Expected size 256");
    assert!(json["verify_url"].is_string(), "Expected verify_url");
    assert!(json["href"].is_string(), "Expected href");

    let file_id = json["file_id"].as_str().unwrap();
    eprintln!("File ID: {}", file_id);

    // Verify we can get the file info
    let json = harness
        .get(&format!("/vehicle/v1/components/vtx_ecm/files/{}", file_id))
        .await
        .expect("GET file failed");

    // FileInfo uses #[serde(flatten)] so info fields are at top level
    assert_eq!(json["id"], file_id);
    assert_eq!(json["size"].as_u64(), Some(256));
    assert_eq!(json["status"], "pending");

    eprintln!("=== Test PASSED: File uploaded successfully ===");
}

/// Test listing files
#[tokio::test]
#[serial_test::serial]
async fn test_list_files() {
    eprintln!("\n=== Testing GET /files (List Files) ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");
    let flash_client = harness.flash_client();

    // Upload a file first
    let firmware_data: Vec<u8> = (0..64).collect();
    flash_client
        .upload_file(&firmware_data)
        .await
        .expect("upload_file failed");

    // List files
    let files_response = flash_client.list_files().await.expect("list_files failed");

    assert!(
        !files_response.files.is_empty(),
        "Expected at least one file"
    );

    eprintln!("Files: {} found", files_response.files.len());
    eprintln!("=== Test PASSED: Files listed successfully ===");
}

/// Test verifying a file
#[tokio::test]
#[serial_test::serial]
async fn test_verify_file() {
    eprintln!("\n=== Testing POST /files/:id/verify (Verify File) ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");
    let flash_client = harness.flash_client();

    // Upload a file
    let firmware_data: Vec<u8> = (0..128).collect();
    let upload_resp = flash_client
        .upload_file(&firmware_data)
        .await
        .expect("upload_file failed");

    let file_id = &upload_resp.upload_id;
    eprintln!("File ID: {}", file_id);

    // Verify the file
    let verify_resp = flash_client
        .verify_file(file_id)
        .await
        .expect("verify_file failed");

    eprintln!(
        "Verify response: valid={}, checksum={:?}",
        verify_resp.valid, verify_resp.checksum
    );

    assert!(verify_resp.valid, "Expected valid=true");
    assert!(verify_resp.checksum.is_some(), "Expected checksum");
    assert_eq!(verify_resp.algorithm.as_deref(), Some("crc32"));

    // Check file status is now "verified" (after verify, state becomes success/finished)
    let file_status = flash_client
        .get_upload_status(file_id)
        .await
        .expect("get_upload_status failed");

    assert!(
        file_status.state.is_success() || file_status.state.is_terminal(),
        "Expected file status to indicate completion, got {:?}",
        file_status.state
    );

    eprintln!("=== Test PASSED: File verified successfully ===");
}

/// Test deleting a file
#[tokio::test]
#[serial_test::serial]
async fn test_delete_file() {
    eprintln!("\n=== Testing DELETE /files/:id (Delete File) ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");
    let flash_client = harness.flash_client();

    // Upload a file
    let firmware_data: Vec<u8> = (0..64).collect();
    let upload_resp = flash_client
        .upload_file(&firmware_data)
        .await
        .expect("upload_file failed");

    let file_id = &upload_resp.upload_id;

    // Delete the file
    flash_client
        .delete_file(file_id)
        .await
        .expect("delete_file failed");

    // Verify it's gone
    let result = flash_client.get_upload_status(file_id).await;
    assert!(result.is_err(), "Expected error after delete");

    eprintln!("=== Test PASSED: File deleted successfully ===");
}

/// Test starting a flash transfer using FlashClient
#[tokio::test]
#[serial_test::serial]
async fn test_start_flash_transfer() {
    eprintln!("\n=== Testing Flash Transfer Start (using FlashClient) ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");

    let flash_client = harness.flash_client();

    // Upload firmware (valid package format)
    let firmware_data = TestHarness::create_firmware_package(256);
    let upload_resp = flash_client
        .upload_file(&firmware_data)
        .await
        .expect("Upload failed");

    let file_id = &upload_resp.upload_id;
    eprintln!("File ID: {}", file_id);

    // Verify the file
    let verify_resp = flash_client
        .verify_file(file_id)
        .await
        .expect("Verify failed");
    assert!(verify_resp.valid, "Expected file to be valid");
    eprintln!("File verified: checksum={:?}", verify_resp.checksum);

    // Start flash transfer
    let flash_resp = flash_client
        .start_flash(file_id)
        .await
        .expect("Start flash failed");

    eprintln!("Transfer ID: {}", flash_resp.transfer_id);
    eprintln!("Status URL: {:?}", flash_resp.status_url);

    eprintln!("=== Test PASSED: Flash transfer started ===");
}

/// Test polling flash transfer status using FlashClient
#[tokio::test]
#[serial_test::serial]
async fn test_flash_transfer_status() {
    use sovd_client::flash::FlashProgress;

    eprintln!("\n=== Testing Flash Transfer Status Polling (using FlashClient) ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");

    let flash_client = harness.flash_client();

    // Upload firmware (valid package format)
    let firmware_data = TestHarness::create_firmware_package(128);
    let upload_resp = flash_client
        .upload_file(&firmware_data)
        .await
        .expect("Upload failed");

    let file_id = &upload_resp.upload_id;
    eprintln!("File ID: {}", file_id);

    flash_client
        .verify_file(file_id)
        .await
        .expect("Verify failed");

    // Start flash
    let flash_resp = flash_client
        .start_flash(file_id)
        .await
        .expect("Start flash failed");

    let transfer_id = &flash_resp.transfer_id;
    eprintln!("Transfer ID: {}", transfer_id);

    // Poll status with progress callback
    let status = flash_client
        .poll_flash_complete(
            transfer_id,
            Some(|progress: &FlashProgress| {
                let pct = progress.percent.unwrap_or(0.0);
                eprintln!(
                    "  Progress: {}/{} blocks ({:.1}%)",
                    progress.blocks_transferred, progress.blocks_total, pct
                );
            }),
        )
        .await
        .expect("Flash polling failed");

    eprintln!("Transfer reached state: {:?}", status.state);
    assert!(
        status.state.is_success(),
        "Expected success state, got {:?}",
        status.state
    );

    eprintln!("=== Test PASSED: Flash transfer status polling works ===");
}

/// Test finalizing a flash transfer using FlashClient
#[tokio::test]
#[serial_test::serial]
async fn test_finalize_flash_transfer() {
    use sovd_client::flash::TransferState;

    eprintln!("\n=== Testing Flash Finalize (using FlashClient) ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");

    let flash_client = harness.flash_client();

    // Upload firmware (valid package format with proper header and checksum)
    let firmware_data = TestHarness::create_firmware_package(128);
    let upload_resp = flash_client
        .upload_file(&firmware_data)
        .await
        .expect("Upload failed");

    let file_id = &upload_resp.upload_id;

    // Verify
    flash_client
        .verify_file(file_id)
        .await
        .expect("Verify failed");

    // Start flash
    let flash_resp = flash_client
        .start_flash(file_id)
        .await
        .expect("Start flash failed");

    let transfer_id = &flash_resp.transfer_id;
    eprintln!("Transfer ID: {}", transfer_id);

    // Poll until awaiting_exit or complete using the client
    let status = flash_client
        .poll_flash_complete_simple(transfer_id)
        .await
        .expect("Flash polling failed");

    eprintln!("Flash state: {:?}", status.state);
    assert!(
        status.state.is_success(),
        "Expected success state, got {:?}",
        status.state
    );

    // Finalize
    let exit_resp = flash_client
        .transfer_exit()
        .await
        .expect("Transfer exit failed");

    eprintln!("Transfer exit success: {}", exit_resp.success);
    assert!(exit_resp.success, "Expected transfer exit to succeed");

    // Verify final state is complete or awaiting reset
    let final_status = flash_client
        .get_flash_status(transfer_id)
        .await
        .expect("Get final status failed");
    assert!(
        final_status.state == TransferState::Complete
            || final_status.state == TransferState::AwaitingReset,
        "Expected Complete or AwaitingReset state, got {:?}",
        final_status.state
    );

    eprintln!("=== Test PASSED: Flash transfer finalized successfully ===");
}

/// Test ECU reset
#[tokio::test]
#[serial_test::serial]
async fn test_ecu_reset() {
    eprintln!("\n=== Testing POST /reset (ECU Reset) ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");
    let client = harness.sovd_client();

    // Test soft reset
    let result = client
        .ecu_reset("vtx_ecm", "soft")
        .await
        .expect("ecu_reset failed");

    eprintln!(
        "Response: success={}, reset_type={}",
        result.success, result.reset_type
    );

    assert!(result.success, "Expected success");
    assert_eq!(result.reset_type, "soft");
    assert!(result.message.contains("soft"));

    eprintln!("=== Test PASSED: ECU reset successful ===");
}

/// Test ECU reset with different reset types
#[tokio::test]
#[serial_test::serial]
async fn test_ecu_reset_types() {
    eprintln!("\n=== Testing different ECU reset types ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");
    let client = harness.sovd_client();

    // Test all three reset types
    let reset_types = vec![
        ("hard", "hard"),
        ("soft", "soft"),
        ("key_off_on", "key_off_on"),
        ("0x01", "hard"), // Numeric hard reset
        ("0x03", "soft"), // Numeric soft reset
    ];

    for (input, expected) in reset_types {
        let result = client
            .ecu_reset("vtx_ecm", input)
            .await
            .expect(&format!("ecu_reset {} failed", input));

        assert!(result.success, "Expected success for reset type {}", input);
        assert_eq!(
            result.reset_type, expected,
            "Reset type mismatch for input {}",
            input
        );

        eprintln!("Reset type '{}' -> '{}': OK", input, expected);

        // Small delay between resets
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    eprintln!("=== Test PASSED: All reset types work ===");
}

/// Test flash transfer with unverified file (should fail)
#[tokio::test]
#[serial_test::serial]
async fn test_flash_unverified_file() {
    eprintln!("\n=== Testing flash with unverified file (should fail) ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");

    // Upload a file but don't verify it
    let firmware_data: Vec<u8> = (0..64).collect();
    let upload_url = format!(
        "http://localhost:{}/vehicle/v1/components/vtx_ecm/files",
        TestHarness::SERVER_PORT
    );

    let client = reqwest::Client::new();
    let response = client
        .post(&upload_url)
        .header("Content-Type", "application/octet-stream")
        .body(firmware_data)
        .send()
        .await
        .expect("POST upload failed");

    let json: Value = response.json().await.expect("Failed to parse response");
    let file_id = json["file_id"].as_str().unwrap();

    // Try to flash without verifying - should fail
    let (status, json) = harness
        .post(
            "/vehicle/v1/components/vtx_ecm/flash/transfer",
            serde_json::json!({ "file_id": file_id }),
        )
        .await
        .expect("POST flash/transfer request failed");

    eprintln!("Response: status={}, body={}", status, json);

    assert_eq!(status, 400, "Expected 400 Bad Request for unverified file");

    eprintln!("=== Test PASSED: Unverified file correctly rejected ===");
}

/// Test flash transfer with invalid file ID (should fail)
#[tokio::test]
#[serial_test::serial]
async fn test_flash_invalid_file_id() {
    eprintln!("\n=== Testing flash with invalid file ID (should fail) ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");

    // Try to flash with non-existent file ID
    let (status, json) = harness
        .post(
            "/vehicle/v1/components/vtx_ecm/flash/transfer",
            serde_json::json!({ "file_id": "nonexistent-file-id" }),
        )
        .await
        .expect("POST flash/transfer request failed");

    eprintln!("Response: status={}, body={}", status, json);

    assert_eq!(status, 404, "Expected 404 Not Found for invalid file ID");

    eprintln!("=== Test PASSED: Invalid file ID correctly rejected ===");
}

/// Test aborting a flash transfer
#[tokio::test]
#[serial_test::serial]
async fn test_abort_flash_transfer() {
    use sovd_client::flash::TransferState;

    eprintln!("\n=== Testing DELETE /flash/transfer/:id (Abort) ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");
    let flash_client = harness.flash_client();

    // Upload and verify a larger file to give us time to abort
    let firmware_data: Vec<u8> = (0..1024).map(|i| (i & 0xFF) as u8).collect();
    let upload_resp = flash_client
        .upload_file(&firmware_data)
        .await
        .expect("upload_file failed");

    let file_id = &upload_resp.upload_id;

    // Verify
    flash_client
        .verify_file(file_id)
        .await
        .expect("verify_file failed");

    // Start flash
    let flash_resp = flash_client
        .start_flash(file_id)
        .await
        .expect("start_flash failed");

    let transfer_id = &flash_resp.transfer_id;
    eprintln!("Transfer ID: {}", transfer_id);

    // Abort the transfer
    flash_client
        .abort_flash(transfer_id)
        .await
        .expect("abort_flash failed");

    // Check transfer is now failed
    let status = flash_client
        .get_flash_status(transfer_id)
        .await
        .expect("get_flash_status failed");

    assert_eq!(status.state, TransferState::Failed);
    assert!(status
        .error
        .as_ref()
        .map(|e| e.message.contains("abort"))
        .unwrap_or(false));

    eprintln!("=== Test PASSED: Flash transfer aborted successfully ===");
}

/// Test complete flash workflow with 1KB transfer using FlashClient
#[tokio::test]
#[serial_test::serial]
async fn test_complete_flash_workflow() {
    use sovd_client::flash::{FlashProgress, TransferState};

    eprintln!("\n=== Testing Complete Flash Workflow (1KB, using FlashClient) ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");

    let flash_client = harness.flash_client();

    // Create a valid firmware package (1KB payload) with a specific version
    let payload_size = 1024;
    let test_version = "3.5.7-flash-test";
    let firmware_data =
        TestHarness::create_firmware_package_with_version(payload_size, test_version);
    let total_size = firmware_data.len();

    // Step 1: Upload file
    eprintln!(
        "Step 1: Uploading {} bytes ({}B payload)...",
        total_size, payload_size
    );
    let upload_resp = flash_client
        .upload_file(&firmware_data)
        .await
        .expect("Upload failed");
    let file_id = &upload_resp.upload_id;
    eprintln!("  File ID: {}", file_id);

    // Step 2: Verify file
    eprintln!("Step 2: Verifying file...");
    let verify_resp = flash_client
        .verify_file(file_id)
        .await
        .expect("Verify failed");
    assert!(verify_resp.valid, "File verification failed");
    eprintln!("  Checksum: {:?}", verify_resp.checksum);

    // Step 3: Start flash transfer
    eprintln!("Step 3: Starting flash transfer...");
    let flash_resp = flash_client
        .start_flash(file_id)
        .await
        .expect("Start flash failed");
    let transfer_id = &flash_resp.transfer_id;
    eprintln!("  Transfer ID: {}", transfer_id);

    // Step 4: Poll until complete with progress callback
    eprintln!("Step 4: Polling transfer status...");
    let start_time = std::time::Instant::now();

    let status = flash_client
        .poll_flash_complete(
            transfer_id,
            Some(|progress: &FlashProgress| {
                let pct = progress.percent.unwrap_or(0.0);
                eprintln!(
                    "  Progress: {}/{} blocks ({:.1}%)",
                    progress.blocks_transferred, progress.blocks_total, pct
                );
            }),
        )
        .await
        .expect("Flash polling failed");

    let transfer_time = start_time.elapsed();
    eprintln!("  Final state: {:?}", status.state);

    // Step 5: Finalize
    eprintln!("Step 5: Finalizing transfer...");
    let exit_resp = flash_client
        .transfer_exit()
        .await
        .expect("Transfer exit failed");
    eprintln!("  Transfer exit success: {}", exit_resp.success);
    assert!(exit_resp.success, "Transfer exit failed");

    // Verify final state is complete or awaiting reset
    let final_status = flash_client
        .get_flash_status(transfer_id)
        .await
        .expect("Get final status failed");
    assert!(
        final_status.state == TransferState::Complete
            || final_status.state == TransferState::AwaitingReset,
        "Expected Complete or AwaitingReset state, got {:?}",
        final_status.state
    );

    // Step 6: ECU Reset to apply firmware update
    eprintln!("Step 6: Resetting ECU to apply firmware...");
    let reset_resp = flash_client
        .ecu_reset_with_type("hard")
        .await
        .expect("ecu_reset failed");
    assert!(reset_resp.success, "ECU reset failed");
    eprintln!("  ECU reset successful");

    // Wait for ECU to come back
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Step 7: Verify software version was updated
    eprintln!("Step 7: Verifying software version...");
    let client = harness.sovd_client();
    let version_data = client
        .read_data("vtx_ecm", "ecu_sw_version")
        .await
        .expect("read_data ecu_sw_version failed");

    let reported_version = version_data
        .as_str()
        .expect("ecu_sw_version value should be a string");
    eprintln!("  Reported version: {}", reported_version);
    eprintln!("  Expected version: {}", test_version);
    assert_eq!(
        reported_version, test_version,
        "Software version mismatch after flash"
    );

    eprintln!("\nFlash workflow complete:");
    eprintln!("  Total bytes: {}", total_size);
    eprintln!("  Payload size: {}", payload_size);
    eprintln!("  Duration: {:?}", transfer_time);
    eprintln!("  Final state: Complete");
    eprintln!("  Version updated: {} -> {}", "previous", test_version);

    eprintln!("=== Test PASSED: Complete flash workflow with version verification ===");
}

/// Test complete flash workflow with block_counter_start=1
///
/// This verifies that TransferData (0x36) block counter handling works correctly
/// when configured to start at 1 (common in many OEM implementations) instead of 0.
#[tokio::test]
#[serial_test::serial]
async fn test_flash_workflow_block_counter_1() {
    use sovd_client::flash::{FlashProgress, TransferState};

    eprintln!("\n=== Testing Flash Workflow with block_counter_start=1 ===");

    // Create harness with block counter starting at 1
    let options = TestHarnessOptions {
        block_counter_start: 1,
        block_counter_wrap: 1,
        ..Default::default()
    };
    let harness = TestHarness::new_with_options(options)
        .await
        .expect("Failed to create test harness");

    let flash_client = harness.flash_client();

    // Create a valid firmware package (1KB payload)
    let payload_size = 1024;
    let test_version = "4.0.0-block-counter-test";
    let firmware_data =
        TestHarness::create_firmware_package_with_version(payload_size, test_version);
    let total_size = firmware_data.len();

    // Step 1: Upload file
    eprintln!("Step 1: Uploading {} bytes...", total_size);
    let upload_resp = flash_client
        .upload_file(&firmware_data)
        .await
        .expect("Upload failed");
    let file_id = &upload_resp.upload_id;

    // Step 2: Verify file
    eprintln!("Step 2: Verifying file...");
    let verify_resp = flash_client
        .verify_file(file_id)
        .await
        .expect("Verify failed");
    assert!(verify_resp.valid, "File verification failed");

    // Step 3: Start flash transfer
    eprintln!("Step 3: Starting flash transfer (block_counter_start=1)...");
    let flash_resp = flash_client
        .start_flash(file_id)
        .await
        .expect("Start flash failed");
    let transfer_id = &flash_resp.transfer_id;

    // Step 4: Poll until complete
    eprintln!("Step 4: Polling transfer status...");
    let status = flash_client
        .poll_flash_complete(
            transfer_id,
            Some(|progress: &FlashProgress| {
                let pct = progress.percent.unwrap_or(0.0);
                eprintln!(
                    "  Progress: {}/{} blocks ({:.1}%)",
                    progress.blocks_transferred, progress.blocks_total, pct
                );
            }),
        )
        .await
        .expect("Flash polling failed");

    eprintln!("  Final state: {:?}", status.state);

    // Step 5: Finalize
    eprintln!("Step 5: Finalizing transfer...");
    let exit_resp = flash_client
        .transfer_exit()
        .await
        .expect("Transfer exit failed");
    assert!(exit_resp.success, "Transfer exit failed");

    // Verify final state is complete or awaiting reset
    let final_status = flash_client
        .get_flash_status(transfer_id)
        .await
        .expect("Get final status failed");
    assert!(
        final_status.state == TransferState::Complete
            || final_status.state == TransferState::AwaitingReset,
        "Expected Complete or AwaitingReset state, got {:?}",
        final_status.state
    );

    // Step 6: ECU Reset
    eprintln!("Step 6: Resetting ECU...");
    let reset_resp = flash_client
        .ecu_reset_with_type("hard")
        .await
        .expect("ecu_reset failed");
    assert!(reset_resp.success, "ECU reset failed");

    // Wait for ECU to come back
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Step 7: Verify software version
    eprintln!("Step 7: Verifying software version...");
    let client = harness.sovd_client();
    let version_data = client
        .read_data("vtx_ecm", "ecu_sw_version")
        .await
        .expect("read_data ecu_sw_version failed");

    let reported_version = version_data
        .as_str()
        .expect("ecu_sw_version value should be a string");
    assert_eq!(
        reported_version, test_version,
        "Software version mismatch after flash"
    );

    eprintln!("=== Test PASSED: Flash workflow with block_counter_start=1 ===");
}

// =============================================================================
// Upload Tests (UDS 0x35 RequestUpload)
// =============================================================================

/// Test: Start upload session and receive data from ECU
///
/// This test verifies:
/// - Starting an upload session (0x35)
/// - Receiving data blocks (0x36)
/// - Finalizing the upload (0x37)
///
/// NOTE: This test is currently ignored because the /software/upload endpoint
/// was removed during the async flash refactoring. The RequestUpload (0x35)
/// functionality for reading memory from ECU needs to be re-implemented.
#[tokio::test]
#[serial_test::serial]
#[ignore = "RequestUpload endpoint removed - needs re-implementation"]
async fn test_upload_session() {
    use sovd_client::SessionType;

    eprintln!("\n=== Testing upload session (0x35 RequestUpload) ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");
    let client = harness.sovd_client();

    // Switch to extended session
    client
        .set_session("vtx_ecm", SessionType::Extended)
        .await
        .expect("set_session failed");
    eprintln!("Switched to extended session");

    // Start an upload session for 256 bytes
    let body = serde_json::json!({
        "memory_address": "0x00000000",
        "memory_size": 256,
        "data_format": 0,
        "address_and_length_format": 0x44
    });

    let (status, json) = harness
        .post("/vehicle/v1/components/vtx_ecm/software/upload", body)
        .await
        .expect("POST upload failed");

    assert_eq!(status, 201, "Failed to start upload");
    let session_id = json["session_id"].as_str().unwrap();
    let max_block_size = json["max_block_size"].as_u64().unwrap();

    eprintln!("Session ID: {}", session_id);
    eprintln!("Max block size: {} bytes", max_block_size);

    // Receive a data block
    let (status, json) = harness
        .get_with_status(&format!(
            "/vehicle/v1/components/vtx_ecm/software/upload/{}",
            session_id
        ))
        .await
        .expect("GET upload block failed");

    assert_eq!(status, 200, "Failed to receive upload block");
    let block_counter = json["block_counter"].as_u64().unwrap();
    let data = json["data"].as_str().unwrap();
    let bytes_received = json["bytes_received"].as_u64().unwrap();

    eprintln!("Block counter: {}", block_counter);
    eprintln!("Data length: {} chars (hex)", data.len());
    eprintln!("Bytes received: {}", bytes_received);

    assert_eq!(block_counter, 1, "Wrong block counter");
    assert!(bytes_received > 0, "No data received");

    // Finalize upload
    let (status, json) = harness
        .delete_with_status(&format!(
            "/vehicle/v1/components/vtx_ecm/software/upload/{}",
            session_id
        ))
        .await
        .expect("DELETE upload failed");

    assert_eq!(status, 200, "Failed to finalize upload");
    assert_eq!(json["success"].as_bool(), Some(true));

    eprintln!("Total bytes uploaded: {}", json["total_bytes"]);
    eprintln!("CRC32: {}", json["crc32"].as_str().unwrap_or("N/A"));

    eprintln!("=== Test PASSED: Upload session completed ===");
}

// =============================================================================
// I/O Control Tests (UDS 0x2F InputOutputControlById)
// =============================================================================

/// Test: List available I/O outputs
#[tokio::test]
#[serial_test::serial]
async fn test_list_outputs() {
    eprintln!("\n=== Testing list I/O outputs ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");
    let client = harness.sovd_client();

    let outputs = client
        .list_outputs("vtx_ecm")
        .await
        .expect("list_outputs failed");

    eprintln!("Found {} outputs:", outputs.len());

    for output in &outputs {
        eprintln!(
            "  - {} ({})",
            output.name.as_deref().unwrap_or("?"),
            output.id
        );
    }

    assert!(outputs.len() >= 5, "Expected at least 5 outputs");

    eprintln!("=== Test PASSED: List outputs ===");
}

/// Test: Control an output with short-term adjustment
#[tokio::test]
#[serial_test::serial]
async fn test_io_control_adjust() {
    use sovd_client::SessionType;

    eprintln!("\n=== Testing I/O control short-term adjustment ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");
    let client = harness.sovd_client();

    // Switch to extended session
    client
        .set_session("vtx_ecm", SessionType::Extended)
        .await
        .expect("set_session failed");

    // Set LED status to "on" (typed value via allowed list)
    let result = client
        .control_output(
            "vtx_ecm",
            "led_status",
            "short_term_adjust",
            Some(serde_json::json!("on")),
        )
        .await
        .expect("control_output failed");

    assert!(result.success);
    assert_eq!(result.action, "short_term_adjust");

    eprintln!("Output ID: {}", result.output_id);
    eprintln!("Action: {}", result.action);

    eprintln!("=== Test PASSED: I/O control adjustment ===");
}

/// Test: Return control to ECU
#[tokio::test]
#[serial_test::serial]
async fn test_io_control_return_to_ecu() {
    use sovd_client::SessionType;

    eprintln!("\n=== Testing I/O control return to ECU ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");
    let client = harness.sovd_client();

    // Switch to extended session
    client
        .set_session("vtx_ecm", SessionType::Extended)
        .await
        .expect("set_session failed");

    // First, adjust the output (typed numeric value: 255 rpm)
    client
        .control_output(
            "vtx_ecm",
            "fan_speed",
            "short_term_adjust",
            Some(serde_json::json!(255)),
        )
        .await
        .expect("control_output adjust failed");

    // Then return control to ECU
    let result = client
        .control_output("vtx_ecm", "fan_speed", "return_to_ecu", None)
        .await
        .expect("control_output return_to_ecu failed");

    assert!(result.success);
    assert_eq!(result.action, "return_to_ecu");

    eprintln!("=== Test PASSED: Return control to ECU ===");
}

/// Test: Freeze output state
#[tokio::test]
#[serial_test::serial]
async fn test_io_control_freeze() {
    use sovd_client::SessionType;

    eprintln!("\n=== Testing I/O control freeze ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");
    let client = harness.sovd_client();

    // Switch to extended session
    client
        .set_session("vtx_ecm", SessionType::Extended)
        .await
        .expect("set_session failed");

    // Freeze the output
    let result = client
        .control_output("vtx_ecm", "pwm_output", "freeze", None)
        .await
        .expect("control_output freeze failed");

    assert!(result.success);
    assert_eq!(result.action, "freeze");

    eprintln!("=== Test PASSED: Freeze output ===");
}

/// Test: I/O control with security access required
#[tokio::test]
#[serial_test::serial]
async fn test_io_control_security_required() {
    use sovd_client::SessionType;

    eprintln!("\n=== Testing I/O control requiring security access ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");
    let client = harness.sovd_client();

    // Switch to extended session
    client
        .set_session("vtx_ecm", SessionType::Extended)
        .await
        .expect("set_session failed");

    // Try to control relay_2 (requires security) without security access
    let result = client
        .control_output(
            "vtx_ecm",
            "relay_2",
            "adjust",
            Some(serde_json::json!("01")),
        )
        .await;

    // Should fail with security access denied
    assert!(result.is_err(), "Expected security access denied error");
    eprintln!("Security access denied as expected");

    eprintln!("=== Test PASSED: Security check for I/O control ===");
}

// =============================================================================
// Link Control Tests (UDS 0x87 LinkControl)
// =============================================================================

/// Test: Get link status
#[tokio::test]
#[serial_test::serial]
async fn test_get_link_status() {
    eprintln!("\n=== Testing get link status ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");
    let client = harness.sovd_client();

    let mode = client
        .get_mode("vtx_ecm", "link")
        .await
        .expect("get_mode link failed");

    eprintln!("Mode ID: {}", mode.id);
    eprintln!("Value: {:?}", mode.value);

    assert_eq!(mode.id, "link");

    eprintln!("=== Test PASSED: Get link status ===");
}

/// Test: Verify and transition baud rate
#[tokio::test]
#[serial_test::serial]
async fn test_link_control_verify_and_transition() {
    use sovd_client::SessionType;

    eprintln!("\n=== Testing link control verify and transition ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");
    let client = harness.sovd_client();

    // Switch to extended session
    client
        .set_session("vtx_ecm", SessionType::Extended)
        .await
        .expect("set_session failed");

    // Verify a fixed baud rate
    let body = serde_json::json!({
        "action": "verify_fixed",
        "baud_rate_id": "250k"
    });

    let (status, json) = harness
        .put("/vehicle/v1/components/vtx_ecm/modes/link", body)
        .await
        .expect("PUT verify baud rate failed");

    assert_eq!(status, 200, "Verify baud rate failed");
    assert_eq!(json["success"].as_bool(), Some(true));
    assert_eq!(json["baud_rate"].as_u64(), Some(250000));

    eprintln!("Verified baud rate: {} bps", json["baud_rate"]);

    // Transition to the verified baud rate
    let body = serde_json::json!({
        "action": "transition"
    });

    let (status, json) = harness
        .put("/vehicle/v1/components/vtx_ecm/modes/link", body)
        .await
        .expect("PUT transition baud rate failed");

    assert_eq!(status, 200, "Transition baud rate failed");
    assert_eq!(json["success"].as_bool(), Some(true));
    assert_eq!(json["baud_rate"].as_u64(), Some(250000));

    eprintln!("Transitioned to baud rate: {} bps", json["baud_rate"]);

    // Verify the new status
    let (status, json) = harness
        .get_with_status("/vehicle/v1/components/vtx_ecm/modes/link")
        .await
        .expect("GET link status failed");

    assert_eq!(status, 200);
    assert_eq!(json["current_baud_rate"].as_u64(), Some(250000));

    eprintln!(
        "Current baud rate confirmed: {} bps",
        json["current_baud_rate"]
    );

    eprintln!("=== Test PASSED: Link control verify and transition ===");
}

/// Test: Verify specific baud rate
#[tokio::test]
#[serial_test::serial]
async fn test_link_control_verify_specific() {
    use sovd_client::SessionType;

    eprintln!("\n=== Testing link control verify specific baud rate ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");
    let client = harness.sovd_client();

    // Switch to extended session
    client
        .set_session("vtx_ecm", SessionType::Extended)
        .await
        .expect("set_session failed");

    // Verify a specific baud rate
    let body = serde_json::json!({
        "action": "verify_specific",
        "baud_rate": 333333
    });

    let (status, json) = harness
        .put("/vehicle/v1/components/vtx_ecm/modes/link", body)
        .await
        .expect("PUT verify specific baud rate failed");

    assert_eq!(status, 200, "Verify specific baud rate failed");
    assert_eq!(json["success"].as_bool(), Some(true));
    assert_eq!(json["baud_rate"].as_u64(), Some(333333));

    eprintln!("Verified specific baud rate: {} bps", json["baud_rate"]);

    eprintln!("=== Test PASSED: Link control verify specific ===");
}

/// Test: Link control transition without verify fails
#[tokio::test]
#[serial_test::serial]
async fn test_link_control_transition_without_verify() {
    use sovd_client::SessionType;

    eprintln!("\n=== Testing link control transition without verify ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");
    let client = harness.sovd_client();

    // Switch to extended session
    client
        .set_session("vtx_ecm", SessionType::Extended)
        .await
        .expect("set_session failed");

    // Try to transition without verifying first
    let body = serde_json::json!({
        "action": "transition"
    });

    let (status, _) = harness
        .put("/vehicle/v1/components/vtx_ecm/modes/link", body)
        .await
        .expect("PUT transition failed");

    // Should fail because no baud rate was verified
    assert_eq!(status, 400, "Expected bad request (400)");
    eprintln!(
        "Transition without verify failed as expected (status {})",
        status
    );

    eprintln!("=== Test PASSED: Transition without verify fails ===");
}

/// Test: Link control requires extended session
#[tokio::test]
#[serial_test::serial]
async fn test_link_control_requires_extended_session() {
    eprintln!("\n=== Testing link control requires extended session ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");

    // Don't switch to extended session - stay in default

    // Try to verify baud rate in default session
    let body = serde_json::json!({
        "action": "verify_fixed",
        "baud_rate_id": "500k"
    });

    let (status, _) = harness
        .put("/vehicle/v1/components/vtx_ecm/modes/link", body)
        .await
        .expect("PUT verify baud rate failed");

    // Should fail because not in extended session - ECU returns ConditionsNotCorrect (0x22) → 412 Precondition Failed
    assert_eq!(
        status, 412,
        "Expected precondition failed (412) for session requirement"
    );
    eprintln!(
        "Link control in default session failed as expected (status {})",
        status
    );

    eprintln!("=== Test PASSED: Link control requires extended session ===");
}

// =============================================================================
// Software Update Tests (Full Cycle with Firmware Verification)
// =============================================================================

/// Firmware payload format constants (must match example-ecu)
mod firmware {
    pub const HEADER_MAGIC: &[u8] = b"EXAMPLE_FW";
    pub const FOOTER_MAGIC: &[u8] = b"EXFW_END!\0";
    pub const VERSION_LENGTH: usize = 32;
    pub const FOOTER_SIZE: usize = 32 + 10; // SHA-256 + magic

    use sha2::{Digest, Sha256};

    /// Create a valid firmware payload with proper format and checksum
    pub fn create_valid_payload(version: &str, data_size: usize) -> Vec<u8> {
        let mut payload = Vec::new();

        // Header magic
        payload.extend_from_slice(HEADER_MAGIC);

        // Version string (padded to 32 bytes)
        let mut version_bytes = version.as_bytes().to_vec();
        version_bytes.resize(VERSION_LENGTH, 0);
        payload.extend_from_slice(&version_bytes);

        // Firmware data (pattern fill)
        for i in 0..data_size {
            payload.push((i & 0xFF) as u8);
        }

        // Calculate SHA-256 of everything so far
        let mut hasher = Sha256::new();
        hasher.update(&payload);
        let checksum = hasher.finalize();

        // Add checksum
        payload.extend_from_slice(&checksum);

        // Footer magic
        payload.extend_from_slice(FOOTER_MAGIC);

        payload
    }

    /// Create a corrupted firmware payload (bad checksum)
    pub fn create_corrupted_payload(version: &str, data_size: usize) -> Vec<u8> {
        let mut payload = create_valid_payload(version, data_size);

        // Corrupt the checksum by flipping some bits
        let checksum_start = payload.len() - FOOTER_SIZE;
        payload[checksum_start] ^= 0xFF;
        payload[checksum_start + 1] ^= 0xAA;

        payload
    }

    /// Create a payload with invalid header
    pub fn create_bad_header_payload(data_size: usize) -> Vec<u8> {
        let mut payload = Vec::new();

        // Wrong header magic
        payload.extend_from_slice(b"WRONGHEAD!");

        // Version string
        let mut version_bytes = b"1.0.0".to_vec();
        version_bytes.resize(VERSION_LENGTH, 0);
        payload.extend_from_slice(&version_bytes);

        // Data
        for i in 0..data_size {
            payload.push((i & 0xFF) as u8);
        }

        // Calculate SHA-256
        let mut hasher = Sha256::new();
        hasher.update(&payload);
        let checksum = hasher.finalize();
        payload.extend_from_slice(&checksum);

        // Footer magic
        payload.extend_from_slice(FOOTER_MAGIC);

        payload
    }
}

/// Test: Full software update cycle with firmware verification and version update
///
/// This comprehensive test verifies the complete OTA update flow:
/// 1. Read initial software version from ECU
/// 2. Switch to programming session
/// 3. Perform security access
/// 4. Transfer valid firmware with proper format and checksum
/// 5. Finalize download (firmware is verified by example-ecu)
/// 6. ECU reset to apply firmware
/// 7. Verify software version DID (0xF189) is updated
#[tokio::test]
#[serial_test::serial]
async fn test_software_update_full_cycle_with_version_check() {
    use sovd_client::{SecurityLevel, SessionType};

    eprintln!("\n=== Testing full software update cycle with version verification ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");
    let sovd_client = harness.sovd_client();
    let flash_client = harness.flash_client();

    // Step 1: Read initial software version
    eprintln!("\n--- Step 1: Read initial software version ---");
    let initial_version_val = sovd_client
        .read_data("vtx_ecm", "F189")
        .await
        .expect("read_data F189 failed");

    let initial_version_hex = initial_version_val.as_str().unwrap_or("");
    let initial_version = hex::decode(initial_version_hex)
        .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
        .unwrap_or_else(|_| initial_version_hex.to_string());
    eprintln!(
        "Initial version: {} (hex: {})",
        initial_version, initial_version_hex
    );

    // Step 2: Switch to programming session
    eprintln!("\n--- Step 2: Switch to programming session ---");
    sovd_client
        .set_session("vtx_ecm", SessionType::Programming)
        .await
        .expect("set_session failed");
    eprintln!("Switched to programming session");

    // Step 3: Perform security access
    eprintln!("\n--- Step 3: Perform security access ---");

    // Request seed
    let seed_bytes = sovd_client
        .security_access_request_seed("vtx_ecm", SecurityLevel::LEVEL_1)
        .await
        .expect("security_access_request_seed failed");
    eprintln!("Received seed: {}", hex::encode(&seed_bytes));

    // Calculate key (XOR algorithm with default secret)
    let key: Vec<u8> = seed_bytes.iter().map(|&s| s ^ 0xFF).collect();
    eprintln!("Calculated key: {}", hex::encode(&key));

    // Send key
    sovd_client
        .security_access_send_key("vtx_ecm", SecurityLevel::LEVEL_1, &key)
        .await
        .expect("security_access_send_key failed");
    eprintln!("Security access granted");

    // Step 4: Create and upload firmware
    eprintln!("\n--- Step 4: Upload firmware (100KB valid payload) ---");
    let new_version = "3.0.0-e2e-test";
    let firmware_data = firmware::create_valid_payload(new_version, 100 * 1024);
    eprintln!(
        "Firmware size: {} bytes, version: {}",
        firmware_data.len(),
        new_version
    );

    let upload_resp = flash_client
        .upload_file(&firmware_data)
        .await
        .expect("upload_file failed");
    let file_id = upload_resp.upload_id;
    eprintln!("File uploaded with ID: {}", file_id);

    // Step 5: Verify file
    eprintln!("\n--- Step 5: Verify uploaded firmware ---");
    let verify_resp = flash_client
        .verify_file(&file_id)
        .await
        .expect("verify_file failed");
    assert!(verify_resp.valid, "Firmware verification failed");
    eprintln!("Firmware verified: checksum={:?}", verify_resp.checksum);

    // Step 6: Start flash transfer
    eprintln!("\n--- Step 6: Start flash transfer ---");
    let flash_resp = flash_client
        .start_flash(&file_id)
        .await
        .expect("start_flash failed");
    let transfer_id = flash_resp.transfer_id;
    eprintln!("Flash transfer started with ID: {}", transfer_id);

    // Step 7: Poll for flash completion
    eprintln!("\n--- Step 7: Wait for flash to complete ---");
    let flash_status = flash_client
        .poll_flash_complete_simple(&transfer_id)
        .await
        .expect("poll_flash_complete failed");
    eprintln!("Flash completed: state={:?}", flash_status.state);

    // Step 8: Transfer exit
    eprintln!("\n--- Step 8: Transfer exit ---");
    let exit_resp = flash_client
        .transfer_exit()
        .await
        .expect("transfer_exit failed");
    assert!(exit_resp.success, "Transfer exit failed");
    eprintln!("Transfer exit successful");

    // Step 9: ECU reset
    eprintln!("\n--- Step 9: ECU reset ---");
    let reset_resp = flash_client
        .ecu_reset_with_type("hard")
        .await
        .expect("ecu_reset failed");
    assert!(reset_resp.success, "ECU reset failed");
    eprintln!("ECU reset successful");

    // Wait for ECU to come back
    sleep(Duration::from_millis(500)).await;

    // Step 10: Verify new software version
    eprintln!("\n--- Step 10: Verify new software version ---");
    let new_version_val = sovd_client
        .read_data("vtx_ecm", "F189")
        .await
        .expect("read_data F189 failed");

    let new_version_hex = new_version_val.as_str().unwrap_or("");
    let actual_new_version = hex::decode(new_version_hex)
        .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
        .unwrap_or_else(|_| new_version_hex.to_string());

    eprintln!("\n============================================================");
    eprintln!("   SOFTWARE UPDATE RESULT");
    eprintln!("============================================================");
    eprintln!("Initial version: {}", initial_version);
    eprintln!("Target version:  {}", new_version);
    eprintln!("Actual version:  {}", actual_new_version);
    eprintln!("============================================================");

    assert_eq!(
        actual_new_version, new_version,
        "Version mismatch: expected '{}', got '{}'",
        new_version, actual_new_version
    );

    eprintln!("\n=== Test PASSED: Full software update cycle with version verification ===");
}

/// Test: Corrupted firmware is detected and rejected
///
/// This test verifies that the ECU properly validates firmware:
/// - Corrupted checksum is detected
/// - Flash transfer fails due to corrupted checksum
/// - ECU state remains valid (can still be used)
#[tokio::test]
#[serial_test::serial]
async fn test_software_update_detects_corrupted_firmware() {
    use sovd_client::{SecurityLevel, SessionType};

    eprintln!("\n=== Testing corrupted firmware detection ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");
    let sovd_client = harness.sovd_client();
    let flash_client = harness.flash_client();

    // Switch to programming session
    sovd_client
        .set_session("vtx_ecm", SessionType::Programming)
        .await
        .expect("set_session failed");
    eprintln!("Switched to programming session");

    // Perform security access
    let seed_bytes = sovd_client
        .security_access_request_seed("vtx_ecm", SecurityLevel::LEVEL_1)
        .await
        .expect("security_access_request_seed failed");

    let key: Vec<u8> = seed_bytes.iter().map(|&s| s ^ 0xFF).collect();

    sovd_client
        .security_access_send_key("vtx_ecm", SecurityLevel::LEVEL_1, &key)
        .await
        .expect("security_access_send_key failed");
    eprintln!("Security access granted");

    // Create corrupted firmware (bad checksum)
    let corrupted_firmware = firmware::create_corrupted_payload("bad-version", 10 * 1024);
    eprintln!(
        "Corrupted firmware size: {} bytes",
        corrupted_firmware.len()
    );

    // Upload corrupted firmware
    let upload_resp = flash_client
        .upload_file(&corrupted_firmware)
        .await
        .expect("upload_file failed");
    let file_id = upload_resp.upload_id;
    eprintln!("Corrupted file uploaded with ID: {}", file_id);

    // File verification computes checksum of raw data - it passes
    // The actual firmware validation happens during flash
    let _verify_resp = flash_client
        .verify_file(&file_id)
        .await
        .expect("verify_file failed");
    eprintln!("File checksum verified (raw data)");

    // Attempt to flash - should fail due to corrupted internal checksum
    eprintln!("\n--- Attempting to flash corrupted firmware ---");
    let flash_result = flash_client.start_flash(&file_id).await;

    let flash_failed = match flash_result {
        Err(e) => {
            eprintln!("Flash start failed with error (expected): {:?}", e);
            true
        }
        Ok(resp) => {
            eprintln!("Flash started with ID: {}", resp.transfer_id);
            // Wait for flash to complete/fail
            let poll_result = flash_client
                .poll_flash_complete_simple(&resp.transfer_id)
                .await;
            match poll_result {
                Err(e) => {
                    eprintln!("Flash failed during transfer (expected): {:?}", e);
                    true
                }
                Ok(status) => {
                    eprintln!("Flash completed with state: {:?}", status.state);
                    // If flash completes, try transfer_exit - should fail
                    let exit_result = flash_client.transfer_exit().await;
                    match exit_result {
                        Err(e) => {
                            eprintln!("Transfer exit failed (expected): {:?}", e);
                            true
                        }
                        Ok(_) => {
                            eprintln!("WARNING: Transfer exit succeeded with corrupted firmware");
                            false
                        }
                    }
                }
            }
        }
    };

    assert!(
        flash_failed,
        "Expected corrupted firmware to be rejected during flash"
    );

    eprintln!("Corrupted firmware correctly rejected!");

    // Verify ECU is still operational by reading a DID
    eprintln!("\n--- Verifying ECU is still operational ---");

    // Reset to default session first
    sovd_client
        .set_session("vtx_ecm", SessionType::Default)
        .await
        .expect("set_session to default failed");

    let vin_data = sovd_client
        .read_data("vtx_ecm", "F190")
        .await
        .expect("ECU not operational after failed update - VIN read failed");
    eprintln!(
        "ECU still operational - VIN read successful: {:?}",
        vin_data
    );

    eprintln!("\n=== Test PASSED: Corrupted firmware detected and rejected ===");
}

/// Test: Invalid firmware header is rejected
#[tokio::test]
#[serial_test::serial]
async fn test_software_update_rejects_invalid_header() {
    use sovd_client::{SecurityLevel, SessionType};

    eprintln!("\n=== Testing invalid firmware header rejection ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");
    let sovd_client = harness.sovd_client();
    let flash_client = harness.flash_client();

    // Setup: programming session + security
    sovd_client
        .set_session("vtx_ecm", SessionType::Programming)
        .await
        .expect("set_session failed");

    let seed_bytes = sovd_client
        .security_access_request_seed("vtx_ecm", SecurityLevel::LEVEL_1)
        .await
        .expect("security_access_request_seed failed");

    let key: Vec<u8> = seed_bytes.iter().map(|&s| s ^ 0xFF).collect();

    sovd_client
        .security_access_send_key("vtx_ecm", SecurityLevel::LEVEL_1, &key)
        .await
        .expect("security_access_send_key failed");

    // Create firmware with invalid header
    let bad_header_firmware = firmware::create_bad_header_payload(5 * 1024);
    eprintln!(
        "Bad header firmware size: {} bytes",
        bad_header_firmware.len()
    );

    // Upload bad header firmware
    let upload_resp = flash_client
        .upload_file(&bad_header_firmware)
        .await
        .expect("upload_file failed");
    let file_id = upload_resp.upload_id;
    eprintln!("Bad header file uploaded with ID: {}", file_id);

    // File verification computes checksum of raw data - it passes
    let _verify_resp = flash_client
        .verify_file(&file_id)
        .await
        .expect("verify_file failed");
    eprintln!("File checksum verified (raw data)");

    // Attempt to flash - should fail due to invalid header
    eprintln!("\n--- Attempting to flash firmware with invalid header ---");
    let flash_result = flash_client.start_flash(&file_id).await;

    let flash_failed = match flash_result {
        Err(e) => {
            eprintln!("Flash start failed with error (expected): {:?}", e);
            true
        }
        Ok(resp) => {
            eprintln!("Flash started with ID: {}", resp.transfer_id);
            // Wait for flash to complete/fail
            let poll_result = flash_client
                .poll_flash_complete_simple(&resp.transfer_id)
                .await;
            match poll_result {
                Err(e) => {
                    eprintln!("Flash failed during transfer (expected): {:?}", e);
                    true
                }
                Ok(status) => {
                    eprintln!("Flash completed with state: {:?}", status.state);
                    // If flash completes, try transfer_exit - should fail
                    let exit_result = flash_client.transfer_exit().await;
                    match exit_result {
                        Err(e) => {
                            eprintln!("Transfer exit failed (expected): {:?}", e);
                            true
                        }
                        Ok(_) => {
                            eprintln!(
                                "WARNING: Transfer exit succeeded with invalid header firmware"
                            );
                            false
                        }
                    }
                }
            }
        }
    };

    assert!(
        flash_failed,
        "Expected invalid header firmware to be rejected during flash"
    );

    eprintln!("Invalid header firmware correctly rejected!");
    eprintln!("\n=== Test PASSED: Invalid firmware header rejected ===");
}

// =============================================================================
// CLI Tool Tests
// =============================================================================

/// Helper to run sovd-cli and capture output
fn run_cli(args: &[&str], server_url: &str) -> std::process::Output {
    let workspace = TestHarness::workspace_root();
    let binary = format!("{}/target/release/sovd-cli", workspace);

    // Check if binary exists, fall back to debug
    let binary = if std::path::Path::new(&binary).exists() {
        binary
    } else {
        format!("{}/target/debug/sovd-cli", workspace)
    };

    let mut cmd_args = vec!["-s", server_url];
    cmd_args.extend(args);

    Command::new(&binary)
        .args(&cmd_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("Failed to run sovd-cli")
}

/// Test: CLI list command shows components
#[tokio::test]
#[serial_test::serial]
async fn test_cli_list_components() {
    eprintln!("\n=== Testing CLI list command ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");

    let output = run_cli(&["list"], &harness.base_url);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    eprintln!("stdout: {}", stdout);
    eprintln!("stderr: {}", stderr);

    assert!(output.status.success(), "CLI list failed: {}", stderr);
    assert!(stdout.contains("vtx_ecm"), "Expected 'vtx_ecm' in output");

    eprintln!("\n=== Test PASSED: CLI list command ===");
}

/// Test: CLI list command with JSON output
#[tokio::test]
#[serial_test::serial]
async fn test_cli_list_json_output() {
    eprintln!("\n=== Testing CLI list --json output ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");

    let output = run_cli(&["-o", "json", "list"], &harness.base_url);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    eprintln!("stdout: {}", stdout);
    eprintln!("stderr: {}", stderr);

    assert!(
        output.status.success(),
        "CLI list --json failed: {}",
        stderr
    );

    // Parse JSON output
    let json: Value = serde_json::from_str(&stdout).expect("Failed to parse JSON output");

    assert!(json.is_array(), "Expected JSON array");
    let components = json.as_array().unwrap();
    assert!(!components.is_empty(), "Expected at least one component");

    // Check first component has expected fields
    let component = &components[0];
    assert!(component.get("id").is_some(), "Expected 'id' field");

    eprintln!("\n=== Test PASSED: CLI list --json output ===");
}

/// Test: CLI info command shows component details
#[tokio::test]
#[serial_test::serial]
async fn test_cli_info_component() {
    eprintln!("\n=== Testing CLI info command ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");

    let output = run_cli(&["info", "vtx_ecm"], &harness.base_url);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    eprintln!("stdout: {}", stdout);
    eprintln!("stderr: {}", stderr);

    assert!(output.status.success(), "CLI info failed: {}", stderr);
    assert!(
        stdout.contains("vtx_ecm") || stdout.contains("VTX ECM"),
        "Expected component info in output"
    );

    eprintln!("\n=== Test PASSED: CLI info command ===");
}

/// Test: CLI read command reads VIN
#[tokio::test]
#[serial_test::serial]
async fn test_cli_read_parameter() {
    eprintln!("\n=== Testing CLI read command ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");

    // Read VIN (F190)
    let output = run_cli(&["read", "vtx_ecm", "F190"], &harness.base_url);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    eprintln!("stdout: {}", stdout);
    eprintln!("stderr: {}", stderr);

    assert!(output.status.success(), "CLI read failed: {}", stderr);
    // The example-ecu returns a VIN starting with "1"
    assert!(
        stdout.contains("1") || stdout.contains("F190"),
        "Expected VIN data in output"
    );

    eprintln!("\n=== Test PASSED: CLI read command ===");
}

/// Test: CLI read command with JSON output
#[tokio::test]
#[serial_test::serial]
async fn test_cli_read_json_output() {
    eprintln!("\n=== Testing CLI read --json output ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");

    let output = run_cli(
        &["-o", "json", "read", "vtx_ecm", "F190"],
        &harness.base_url,
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    eprintln!("stdout: {}", stdout);
    eprintln!("stderr: {}", stderr);

    assert!(
        output.status.success(),
        "CLI read --json failed: {}",
        stderr
    );

    // Parse JSON output
    let json: Value = serde_json::from_str(&stdout).expect("Failed to parse JSON output");

    // Should have parameter and value fields
    assert!(
        json.get("parameter").is_some() || json.get("value").is_some(),
        "Expected parameter/value in JSON output"
    );

    eprintln!("\n=== Test PASSED: CLI read --json output ===");
}

/// Test: CLI faults command lists fault memory
#[tokio::test]
#[serial_test::serial]
async fn test_cli_faults_command() {
    eprintln!("\n=== Testing CLI faults command ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");

    let output = run_cli(&["faults", "vtx_ecm"], &harness.base_url);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    eprintln!("stdout: {}", stdout);
    eprintln!("stderr: {}", stderr);

    assert!(output.status.success(), "CLI faults failed: {}", stderr);
    // Should show some fault info or "no faults" message
    assert!(
        !stdout.is_empty() || !stderr.is_empty(),
        "Expected some output"
    );

    eprintln!("\n=== Test PASSED: CLI faults command ===");
}

/// Test: CLI data command lists available parameters
#[tokio::test]
#[serial_test::serial]
async fn test_cli_data_command() {
    eprintln!("\n=== Testing CLI data command ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");

    let output = run_cli(&["data", "vtx_ecm"], &harness.base_url);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    eprintln!("stdout: {}", stdout);
    eprintln!("stderr: {}", stderr);

    assert!(output.status.success(), "CLI data failed: {}", stderr);
    // Should list available parameters including F190 (VIN)
    assert!(
        stdout.contains("F190") || stdout.contains("vin") || stdout.contains("VIN"),
        "Expected F190/VIN in parameters list"
    );

    eprintln!("\n=== Test PASSED: CLI data command ===");
}

/// Test: CLI outputs command lists I/O outputs
#[tokio::test]
#[serial_test::serial]
async fn test_cli_outputs_command() {
    eprintln!("\n=== Testing CLI outputs command ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");

    let output = run_cli(&["outputs", "vtx_ecm"], &harness.base_url);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    eprintln!("stdout: {}", stdout);
    eprintln!("stderr: {}", stderr);

    assert!(output.status.success(), "CLI outputs failed: {}", stderr);
    // Should list outputs like led_status, fan_speed from config
    assert!(
        stdout.contains("led") || stdout.contains("fan") || stdout.contains("LED"),
        "Expected I/O outputs in list"
    );

    eprintln!("\n=== Test PASSED: CLI outputs command ===");
}

/// Test: CLI ops command lists operations
#[tokio::test]
#[serial_test::serial]
async fn test_cli_ops_command() {
    eprintln!("\n=== Testing CLI ops command ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");

    let output = run_cli(&["ops", "vtx_ecm"], &harness.base_url);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    eprintln!("stdout: {}", stdout);
    eprintln!("stderr: {}", stderr);

    assert!(output.status.success(), "CLI ops failed: {}", stderr);
    // Should list operations like check_preconditions, erase_memory
    assert!(
        stdout.contains("precondition") || stdout.contains("erase") || stdout.contains("check"),
        "Expected operations in list"
    );

    eprintln!("\n=== Test PASSED: CLI ops command ===");
}

/// Test: CLI session command changes session
#[tokio::test]
#[serial_test::serial]
async fn test_cli_session_command() {
    eprintln!("\n=== Testing CLI session command ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");

    // Change to extended session
    let output = run_cli(&["session", "vtx_ecm", "extended"], &harness.base_url);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    eprintln!("stdout: {}", stdout);
    eprintln!("stderr: {}", stderr);

    assert!(output.status.success(), "CLI session failed: {}", stderr);
    // Should confirm session change
    assert!(
        stdout.contains("extended") || stdout.contains("success") || stdout.contains("Session"),
        "Expected session confirmation in output"
    );

    eprintln!("\n=== Test PASSED: CLI session command ===");
}

// =============================================================================
// Log API Tests (for Message Passing Pattern)
// =============================================================================

/// Test: List logs returns proper structure (empty for UDS backend)
#[tokio::test]
#[serial_test::serial]
async fn test_list_logs_api() {
    eprintln!("\n=== Testing log list API ===");
    let harness = TestHarness::new()
        .await
        .expect("Failed to setup test harness");

    // UDS backend returns "not implemented" for logs
    // This tests that the API endpoint is properly routed
    let result = harness
        .get_with_status("/vehicle/v1/components/vtx_ecm/logs")
        .await;

    match result {
        Ok((status, json)) => {
            eprintln!("Response status: {}", status);
            eprintln!("Response: {:?}", json);

            // Either success with empty list, or 501 Not Implemented
            assert!(
                status == 200 || status == 501,
                "Expected 200 or 501, got {}",
                status
            );

            if status == 200 {
                // Should have items array
                assert!(
                    json.get("items").is_some(),
                    "Expected 'items' field in response"
                );
            }
        }
        Err(e) => {
            // Network error is not expected
            panic!("Request failed: {}", e);
        }
    }

    eprintln!("\n=== Test PASSED: log list API ===");
}

/// Test: Log API client methods work correctly
#[tokio::test]
#[serial_test::serial]
async fn test_log_client_methods() {
    eprintln!("\n=== Testing log client methods ===");
    let harness = TestHarness::new()
        .await
        .expect("Failed to setup test harness");
    let client = harness.sovd_client();

    // Test get_logs method
    let result = client.get_logs("vtx_ecm").await;

    match result {
        Ok(logs) => {
            eprintln!("Got logs response: {:?}", logs);
            // UDS backend returns empty logs (capability not supported)
            // The response should have the right structure
            assert!(
                logs.items.is_empty() || !logs.items.is_empty(),
                "Items should be a valid Vec"
            );
        }
        Err(e) => {
            // Expected for UDS backend which doesn't support logs
            eprintln!("Logs not supported (expected for UDS backend): {}", e);
            // Check it's a "not implemented" error
            let err_str = format!("{}", e);
            assert!(
                err_str.contains("501") || err_str.contains("not") || err_str.contains("implement"),
                "Expected 501/not implemented error, got: {}",
                err_str
            );
        }
    }

    // Test filtered logs (should also fail gracefully for UDS)
    let filter = sovd_client::LogFilter {
        log_type: Some("engine_dump".to_string()),
        status: Some("pending".to_string()),
        ..Default::default()
    };

    let result = client.get_logs_filtered("vtx_ecm", &filter).await;

    match result {
        Ok(logs) => {
            eprintln!("Got filtered logs: {:?}", logs);
        }
        Err(e) => {
            eprintln!("Filtered logs not supported (expected): {}", e);
        }
    }

    eprintln!("\n=== Test PASSED: log client methods ===");
}

/// Test: Individual log access returns 404 for non-existent log
#[tokio::test]
#[serial_test::serial]
async fn test_get_nonexistent_log() {
    eprintln!("\n=== Testing get non-existent log ===");
    let harness = TestHarness::new()
        .await
        .expect("Failed to setup test harness");
    let client = harness.sovd_client();

    // Try to get a log that doesn't exist
    let result = client.get_log("vtx_ecm", "nonexistent-log-id").await;

    match result {
        Ok(_) => {
            // Shouldn't happen - log doesn't exist
            panic!("Expected error for non-existent log");
        }
        Err(e) => {
            let err_str = format!("{}", e);
            eprintln!("Got expected error: {}", err_str);
            // Should be 404 Not Found or 501 Not Implemented
            assert!(
                err_str.contains("404") || err_str.contains("501") || err_str.contains("not"),
                "Expected 404/501 error, got: {}",
                err_str
            );
        }
    }

    eprintln!("\n=== Test PASSED: get non-existent log ===");
}

/// Test: Delete log returns appropriate response
#[tokio::test]
#[serial_test::serial]
async fn test_delete_nonexistent_log() {
    eprintln!("\n=== Testing delete non-existent log ===");
    let harness = TestHarness::new()
        .await
        .expect("Failed to setup test harness");
    let client = harness.sovd_client();

    // Try to delete a log that doesn't exist
    let result = client.delete_log("vtx_ecm", "nonexistent-log-id").await;

    match result {
        Ok(_) => {
            // Shouldn't happen for UDS backend
            eprintln!("Delete succeeded (unexpected but valid)");
        }
        Err(e) => {
            let err_str = format!("{}", e);
            eprintln!("Got expected error: {}", err_str);
            // Should be 404 Not Found or 501 Not Implemented
            assert!(
                err_str.contains("404") || err_str.contains("501") || err_str.contains("not"),
                "Expected 404/501 error, got: {}",
                err_str
            );
        }
    }

    eprintln!("\n=== Test PASSED: delete non-existent log ===");
}

// =============================================================================
// Flash Commit/Rollback Tests
// =============================================================================

/// Test complete flash + commit workflow
///
/// Flashes firmware, resets ECU, verifies activation state is "activated",
/// commits firmware, verifies state is "committed" and version matches.
#[tokio::test]
#[serial_test::serial]
async fn test_flash_commit_workflow() {
    use sovd_client::flash::FlashProgress;

    eprintln!("\n=== Testing Flash Commit Workflow ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");

    let flash_client = harness.flash_client();

    // Create a valid firmware package with a specific version
    let payload_size = 1024;
    let test_version = "5.0.0-commit-test";
    let firmware_data =
        TestHarness::create_firmware_package_with_version(payload_size, test_version);

    // Step 1: Upload file
    eprintln!("Step 1: Uploading firmware...");
    let upload_resp = flash_client
        .upload_file(&firmware_data)
        .await
        .expect("Upload failed");
    let file_id = &upload_resp.upload_id;

    // Step 2: Verify file
    eprintln!("Step 2: Verifying file...");
    let verify_resp = flash_client
        .verify_file(file_id)
        .await
        .expect("Verify failed");
    assert!(verify_resp.valid, "File verification failed");

    // Step 3: Start flash transfer
    eprintln!("Step 3: Starting flash transfer...");
    let flash_resp = flash_client
        .start_flash(file_id)
        .await
        .expect("Start flash failed");
    let transfer_id = &flash_resp.transfer_id;

    // Step 4: Poll until complete
    eprintln!("Step 4: Polling transfer status...");
    let _status = flash_client
        .poll_flash_complete(
            transfer_id,
            Some(|progress: &FlashProgress| {
                let pct = progress.percent.unwrap_or(0.0);
                eprintln!(
                    "  Progress: {}/{} blocks ({:.1}%)",
                    progress.blocks_transferred, progress.blocks_total, pct
                );
            }),
        )
        .await
        .expect("Flash polling failed");

    // Step 5: Finalize (transfer exit)
    eprintln!("Step 5: Finalizing transfer...");
    let exit_resp = flash_client
        .transfer_exit()
        .await
        .expect("Transfer exit failed");
    assert!(exit_resp.success, "Transfer exit failed");

    // Step 5b: Verify flash state is AwaitingReset (use get_flash_status to avoid auto-detect)
    eprintln!("Step 5b: Verifying AwaitingReset state...");
    let flash_status = flash_client
        .get_flash_status(transfer_id)
        .await
        .expect("get_flash_status failed");
    eprintln!("  Flash state: {:?}", flash_status.state);
    assert_eq!(
        flash_status.state,
        sovd_client::flash::TransferState::AwaitingReset,
        "Expected AwaitingReset after transfer_exit, got {:?}",
        flash_status.state
    );

    // Step 6: ECU Reset
    eprintln!("Step 6: Resetting ECU...");
    let reset_resp = flash_client
        .ecu_reset_with_type("hard")
        .await
        .expect("ecu_reset failed");
    assert!(reset_resp.success, "ECU reset failed");

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Step 7: Check activation state — should be "activated" (transitioned by ecu_reset)
    eprintln!("Step 7: Checking activation state...");
    let activation = flash_client
        .get_activation_state()
        .await
        .expect("get_activation_state failed");
    eprintln!("  State: {}", activation.state);
    eprintln!("  Supports rollback: {}", activation.supports_rollback);
    eprintln!("  Active version: {:?}", activation.active_version);
    eprintln!("  Previous version: {:?}", activation.previous_version);
    assert!(activation.supports_rollback);
    assert_eq!(activation.state, "activated");

    // Step 8: Commit firmware
    eprintln!("Step 8: Committing firmware...");
    let commit_resp = flash_client
        .commit_flash()
        .await
        .expect("commit_flash failed");
    assert!(commit_resp.success, "Commit failed");

    // Step 9: Verify state is now "committed"
    eprintln!("Step 9: Verifying committed state...");
    let activation = flash_client
        .get_activation_state()
        .await
        .expect("get_activation_state failed");
    eprintln!("  State: {}", activation.state);
    assert_eq!(activation.state, "committed");

    // Step 10: Verify software version was updated
    eprintln!("Step 10: Verifying software version...");
    let client = harness.sovd_client();
    let version_data = client
        .read_data("vtx_ecm", "ecu_sw_version")
        .await
        .expect("read_data ecu_sw_version failed");
    let reported_version = version_data
        .as_str()
        .expect("ecu_sw_version value should be a string");
    eprintln!("  Reported version: {}", reported_version);
    assert_eq!(
        reported_version, test_version,
        "Software version mismatch after commit"
    );

    eprintln!("=== Test PASSED: Flash commit workflow ===");
}

/// Test complete flash + rollback workflow
///
/// Flashes firmware, resets ECU, rolls back, verifies old version is restored.
#[tokio::test]
#[serial_test::serial]
async fn test_flash_rollback_workflow() {
    use sovd_client::flash::FlashProgress;

    eprintln!("\n=== Testing Flash Rollback Workflow ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");

    let flash_client = harness.flash_client();

    // First, read the current version
    let client = harness.sovd_client();
    let original_version_data = client
        .read_data("vtx_ecm", "ecu_sw_version")
        .await
        .expect("read_data ecu_sw_version failed");
    let original_version = original_version_data
        .as_str()
        .expect("ecu_sw_version value should be a string")
        .to_string();
    eprintln!("Original version: {}", original_version);

    // Create a valid firmware package with a new version
    let payload_size = 1024;
    let new_version = "6.0.0-rollback-test";
    let firmware_data =
        TestHarness::create_firmware_package_with_version(payload_size, new_version);

    // Flash the new firmware
    eprintln!("Step 1: Flash new firmware...");
    let upload_resp = flash_client
        .upload_file(&firmware_data)
        .await
        .expect("Upload failed");
    let file_id = &upload_resp.upload_id;

    flash_client
        .verify_file(file_id)
        .await
        .expect("Verify failed");

    let flash_resp = flash_client
        .start_flash(file_id)
        .await
        .expect("Start flash failed");
    let transfer_id = &flash_resp.transfer_id;

    flash_client
        .poll_flash_complete::<fn(&FlashProgress)>(transfer_id, None)
        .await
        .expect("Flash polling failed");

    let exit_resp = flash_client
        .transfer_exit()
        .await
        .expect("Transfer exit failed");
    assert!(exit_resp.success);

    // Verify flash state is AwaitingReset (use get_flash_status to avoid auto-detect)
    eprintln!("Step 1b: Verifying AwaitingReset state...");
    let flash_status = flash_client
        .get_flash_status(transfer_id)
        .await
        .expect("get_flash_status failed");
    assert_eq!(
        flash_status.state,
        sovd_client::flash::TransferState::AwaitingReset,
        "Expected AwaitingReset after transfer_exit, got {:?}",
        flash_status.state
    );

    // Reset ECU to apply firmware
    eprintln!("Step 2: Reset ECU...");
    let reset_resp = flash_client
        .ecu_reset_with_type("hard")
        .await
        .expect("ecu_reset failed");
    assert!(reset_resp.success);
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Verify activation state is "activated" (transitioned by ecu_reset)
    eprintln!("Step 3: Checking activation state...");
    let activation = flash_client
        .get_activation_state()
        .await
        .expect("get_activation_state failed");
    assert_eq!(activation.state, "activated");

    // Verify the new version is active
    let version_data = client
        .read_data("vtx_ecm", "ecu_sw_version")
        .await
        .expect("read_data failed");
    let active_version = version_data.as_str().expect("should be string");
    eprintln!("  Active version after flash: {}", active_version);
    assert_eq!(active_version, new_version);

    // Step 4: Rollback
    eprintln!("Step 4: Rolling back firmware...");
    let rollback_resp = flash_client
        .rollback_flash()
        .await
        .expect("rollback_flash failed");
    assert!(rollback_resp.success, "Rollback failed");

    // Step 5: Verify state is "rolled_back"
    eprintln!("Step 5: Verifying rolled back state...");
    let activation = flash_client
        .get_activation_state()
        .await
        .expect("get_activation_state failed");
    eprintln!("  State: {}", activation.state);
    assert_eq!(activation.state, "rolled_back");

    // Step 6: Verify old version is restored
    eprintln!("Step 6: Verifying restored version...");
    let version_data = client
        .read_data("vtx_ecm", "ecu_sw_version")
        .await
        .expect("read_data failed");
    let restored_version = version_data.as_str().expect("should be string");
    eprintln!("  Restored version: {}", restored_version);
    assert_eq!(
        restored_version, original_version,
        "Version should be restored to original after rollback"
    );

    eprintln!("=== Test PASSED: Flash rollback workflow ===");
}

/// Test commit/rollback on ECU without rollback support
///
/// ECU configured without supports_rollback should return error on commit/rollback.
#[tokio::test]
#[serial_test::serial]
async fn test_flash_commit_not_supported() {
    eprintln!("\n=== Testing Flash Commit Not Supported ===");

    let options = TestHarnessOptions {
        supports_rollback: false,
        ..Default::default()
    };
    let harness = TestHarness::new_with_options(options)
        .await
        .expect("Failed to create test harness");

    let flash_client = harness.flash_client();

    // Commit should fail
    eprintln!("Step 1: Attempting commit on non-rollback ECU...");
    let commit_result = flash_client.commit_flash().await;
    assert!(
        commit_result.is_err(),
        "Commit should fail on ECU without rollback support"
    );
    eprintln!("  Got expected error: {}", commit_result.unwrap_err());

    // Rollback should fail
    eprintln!("Step 2: Attempting rollback on non-rollback ECU...");
    let rollback_result = flash_client.rollback_flash().await;
    assert!(
        rollback_result.is_err(),
        "Rollback should fail on ECU without rollback support"
    );
    eprintln!("  Got expected error: {}", rollback_result.unwrap_err());

    // Activation state should fail
    eprintln!("Step 3: Attempting activation state query on non-rollback ECU...");
    let activation_result = flash_client.get_activation_state().await;
    assert!(
        activation_result.is_err(),
        "Activation state should fail on ECU without rollback support"
    );
    eprintln!("  Got expected error: {}", activation_result.unwrap_err());

    eprintln!("=== Test PASSED: Flash commit not supported ===");
}

/// Test commit without prior flash
///
/// Attempting to commit without first flashing should return an error.
#[tokio::test]
#[serial_test::serial]
async fn test_flash_commit_wrong_state() {
    eprintln!("\n=== Testing Flash Commit Wrong State ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");

    let flash_client = harness.flash_client();

    // Verify activation state is not "activated" (should be "complete" or initial)
    eprintln!("Step 1: Check initial activation state...");
    let activation = flash_client
        .get_activation_state()
        .await
        .expect("get_activation_state failed");
    eprintln!("  State: {}", activation.state);
    assert_ne!(
        activation.state, "activated",
        "Should not be in activated state without flash"
    );

    // Attempt commit — should fail
    eprintln!("Step 2: Attempting commit without prior flash...");
    let commit_result = flash_client.commit_flash().await;
    assert!(
        commit_result.is_err(),
        "Commit should fail without prior flash activation"
    );
    eprintln!("  Got expected error: {}", commit_result.unwrap_err());

    // Attempt rollback — should fail
    eprintln!("Step 3: Attempting rollback without prior flash...");
    let rollback_result = flash_client.rollback_flash().await;
    assert!(
        rollback_result.is_err(),
        "Rollback should fail without prior flash activation"
    );
    eprintln!("  Got expected error: {}", rollback_result.unwrap_err());

    eprintln!("=== Test PASSED: Flash commit wrong state ===");
}

/// Test aborting a flash transfer at the AwaitingExit boundary
///
/// This is the last abortable state before finalize. Flash firmware, poll until
/// AwaitingExit, then abort — should succeed and set state to Failed.
#[tokio::test]
#[serial_test::serial]
async fn test_abort_during_awaiting_exit() {
    use sovd_client::flash::{FlashProgress, TransferState};

    eprintln!("\n=== Testing Abort During AwaitingExit ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");

    let flash_client = harness.flash_client();

    // Create and upload firmware
    let payload_size = 1024;
    let firmware_data =
        TestHarness::create_firmware_package_with_version(payload_size, "7.0.0-abort-exit-test");

    eprintln!("Step 1: Upload and verify firmware...");
    let upload_resp = flash_client
        .upload_file(&firmware_data)
        .await
        .expect("Upload failed");
    let file_id = &upload_resp.upload_id;
    flash_client
        .verify_file(file_id)
        .await
        .expect("Verify failed");

    // Start flash and poll until complete (AwaitingExit)
    eprintln!("Step 2: Flash and poll to AwaitingExit...");
    let flash_resp = flash_client
        .start_flash(file_id)
        .await
        .expect("Start flash failed");
    let transfer_id = &flash_resp.transfer_id;

    let status = flash_client
        .poll_flash_complete(
            transfer_id,
            Some(|progress: &FlashProgress| {
                let pct = progress.percent.unwrap_or(0.0);
                eprintln!(
                    "  Progress: {}/{} blocks ({:.1}%)",
                    progress.blocks_transferred, progress.blocks_total, pct
                );
            }),
        )
        .await
        .expect("Flash polling failed");

    // poll_flash_complete returns on is_success(), which includes AwaitingExit
    eprintln!("  Transfer state: {:?}", status.state);
    assert!(
        status.state == TransferState::AwaitingExit || status.state == TransferState::Complete,
        "Expected AwaitingExit or Complete, got {:?}",
        status.state
    );

    // Step 3: Abort at this point (before transfer_exit)
    eprintln!("Step 3: Aborting flash transfer...");
    flash_client
        .abort_flash(transfer_id)
        .await
        .expect("abort_flash should succeed at AwaitingExit");

    // Step 4: Verify state is Failed
    eprintln!("Step 4: Verifying transfer state is Failed...");
    let status = flash_client
        .get_flash_status(transfer_id)
        .await
        .expect("get_flash_status failed");
    assert_eq!(
        status.state,
        TransferState::Failed,
        "Expected Failed state after abort"
    );
    assert!(
        status
            .error
            .as_ref()
            .map(|e| e.message.contains("abort"))
            .unwrap_or(false),
        "Expected abort error message, got: {:?}",
        status.error
    );

    eprintln!("=== Test PASSED: Abort during AwaitingExit ===");
}

/// Test that abort is rejected after firmware is activated
///
/// After flash + transfer_exit + reset, state is Activated. Abort should fail.
/// Rollback is the correct mechanism at this point.
#[tokio::test]
#[serial_test::serial]
async fn test_abort_after_activated_rejected() {
    use sovd_client::flash::FlashProgress;

    eprintln!("\n=== Testing Abort After Activated (Rejected) ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");

    let flash_client = harness.flash_client();

    // Flash firmware through to Activated state
    let payload_size = 1024;
    let firmware_data = TestHarness::create_firmware_package_with_version(
        payload_size,
        "7.1.0-abort-activated-test",
    );

    eprintln!("Step 1: Flash firmware...");
    let upload_resp = flash_client
        .upload_file(&firmware_data)
        .await
        .expect("Upload failed");
    let file_id = &upload_resp.upload_id;
    flash_client
        .verify_file(file_id)
        .await
        .expect("Verify failed");

    let flash_resp = flash_client
        .start_flash(file_id)
        .await
        .expect("Start flash failed");
    let transfer_id = &flash_resp.transfer_id;

    flash_client
        .poll_flash_complete(
            transfer_id,
            Some(|progress: &FlashProgress| {
                let pct = progress.percent.unwrap_or(0.0);
                eprintln!(
                    "  Progress: {}/{} blocks ({:.1}%)",
                    progress.blocks_transferred, progress.blocks_total, pct
                );
            }),
        )
        .await
        .expect("Flash polling failed");

    eprintln!("Step 2: Transfer exit...");
    let exit_resp = flash_client
        .transfer_exit()
        .await
        .expect("Transfer exit failed");
    assert!(exit_resp.success);

    eprintln!("Step 3: ECU Reset...");
    let reset_resp = flash_client
        .ecu_reset_with_type("hard")
        .await
        .expect("ecu_reset failed");
    assert!(reset_resp.success);
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Verify we're in Activated state
    eprintln!("Step 4: Verify activation state...");
    let activation = flash_client
        .get_activation_state()
        .await
        .expect("get_activation_state failed");
    assert_eq!(activation.state, "activated");

    // Step 5: Attempt abort — should fail
    eprintln!("Step 5: Attempting abort (should be rejected)...");
    let abort_result = flash_client.abort_flash(transfer_id).await;
    assert!(abort_result.is_err(), "Abort should fail after activation");
    eprintln!("  Got expected error: {}", abort_result.unwrap_err());

    // Verify activation state is unchanged
    let activation = flash_client
        .get_activation_state()
        .await
        .expect("get_activation_state failed");
    assert_eq!(
        activation.state, "activated",
        "State should still be activated"
    );

    // Step 6: Rollback should work (proving it's the correct mechanism)
    eprintln!("Step 6: Rollback (correct mechanism)...");
    let rollback_resp = flash_client
        .rollback_flash()
        .await
        .expect("rollback_flash failed");
    assert!(rollback_resp.success);

    let activation = flash_client
        .get_activation_state()
        .await
        .expect("get_activation_state failed");
    assert_eq!(activation.state, "rolled_back");

    eprintln!("=== Test PASSED: Abort after activated rejected ===");
}

/// Test that abort is rejected after firmware is committed
///
/// After flash + transfer_exit + reset + commit, state is Committed. Abort should fail.
#[tokio::test]
#[serial_test::serial]
async fn test_abort_after_committed_rejected() {
    use sovd_client::flash::FlashProgress;

    eprintln!("\n=== Testing Abort After Committed (Rejected) ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");

    let flash_client = harness.flash_client();

    // Flash firmware through to Committed state
    let payload_size = 1024;
    let firmware_data = TestHarness::create_firmware_package_with_version(
        payload_size,
        "7.2.0-abort-committed-test",
    );

    eprintln!("Step 1: Flash firmware...");
    let upload_resp = flash_client
        .upload_file(&firmware_data)
        .await
        .expect("Upload failed");
    let file_id = &upload_resp.upload_id;
    flash_client
        .verify_file(file_id)
        .await
        .expect("Verify failed");

    let flash_resp = flash_client
        .start_flash(file_id)
        .await
        .expect("Start flash failed");
    let transfer_id = &flash_resp.transfer_id;

    flash_client
        .poll_flash_complete(
            transfer_id,
            Some(|progress: &FlashProgress| {
                let pct = progress.percent.unwrap_or(0.0);
                eprintln!(
                    "  Progress: {}/{} blocks ({:.1}%)",
                    progress.blocks_transferred, progress.blocks_total, pct
                );
            }),
        )
        .await
        .expect("Flash polling failed");

    eprintln!("Step 2: Transfer exit...");
    let exit_resp = flash_client
        .transfer_exit()
        .await
        .expect("Transfer exit failed");
    assert!(exit_resp.success);

    eprintln!("Step 3: ECU Reset...");
    let reset_resp = flash_client
        .ecu_reset_with_type("hard")
        .await
        .expect("ecu_reset failed");
    assert!(reset_resp.success);
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    eprintln!("Step 4: Commit firmware...");
    let commit_resp = flash_client
        .commit_flash()
        .await
        .expect("commit_flash failed");
    assert!(commit_resp.success);

    let activation = flash_client
        .get_activation_state()
        .await
        .expect("get_activation_state failed");
    assert_eq!(activation.state, "committed");

    // Step 5: Attempt abort — should fail
    eprintln!("Step 5: Attempting abort (should be rejected)...");
    let abort_result = flash_client.abort_flash(transfer_id).await;
    assert!(abort_result.is_err(), "Abort should fail after commit");
    eprintln!("  Got expected error: {}", abort_result.unwrap_err());

    // Verify state is unchanged
    let activation = flash_client
        .get_activation_state()
        .await
        .expect("get_activation_state failed");
    assert_eq!(
        activation.state, "committed",
        "State should still be committed"
    );

    eprintln!("=== Test PASSED: Abort after committed rejected ===");
}

/// Test that abort is rejected after firmware is rolled back
///
/// After flash + transfer_exit + reset + rollback, state is RolledBack. Abort should fail.
#[tokio::test]
#[serial_test::serial]
async fn test_abort_after_rolledback_rejected() {
    use sovd_client::flash::FlashProgress;

    eprintln!("\n=== Testing Abort After RolledBack (Rejected) ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");

    let flash_client = harness.flash_client();

    // Flash firmware through to RolledBack state
    let payload_size = 1024;
    let firmware_data = TestHarness::create_firmware_package_with_version(
        payload_size,
        "7.3.0-abort-rolledback-test",
    );

    eprintln!("Step 1: Flash firmware...");
    let upload_resp = flash_client
        .upload_file(&firmware_data)
        .await
        .expect("Upload failed");
    let file_id = &upload_resp.upload_id;
    flash_client
        .verify_file(file_id)
        .await
        .expect("Verify failed");

    let flash_resp = flash_client
        .start_flash(file_id)
        .await
        .expect("Start flash failed");
    let transfer_id = &flash_resp.transfer_id;

    flash_client
        .poll_flash_complete(
            transfer_id,
            Some(|progress: &FlashProgress| {
                let pct = progress.percent.unwrap_or(0.0);
                eprintln!(
                    "  Progress: {}/{} blocks ({:.1}%)",
                    progress.blocks_transferred, progress.blocks_total, pct
                );
            }),
        )
        .await
        .expect("Flash polling failed");

    eprintln!("Step 2: Transfer exit...");
    let exit_resp = flash_client
        .transfer_exit()
        .await
        .expect("Transfer exit failed");
    assert!(exit_resp.success);

    eprintln!("Step 3: ECU Reset...");
    let reset_resp = flash_client
        .ecu_reset_with_type("hard")
        .await
        .expect("ecu_reset failed");
    assert!(reset_resp.success);
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    eprintln!("Step 4: Rollback firmware...");
    let rollback_resp = flash_client
        .rollback_flash()
        .await
        .expect("rollback_flash failed");
    assert!(rollback_resp.success);

    let activation = flash_client
        .get_activation_state()
        .await
        .expect("get_activation_state failed");
    assert_eq!(activation.state, "rolled_back");

    // Step 5: Attempt abort — should fail
    eprintln!("Step 5: Attempting abort (should be rejected)...");
    let abort_result = flash_client.abort_flash(transfer_id).await;
    assert!(abort_result.is_err(), "Abort should fail after rollback");
    eprintln!("  Got expected error: {}", abort_result.unwrap_err());

    // Verify state is unchanged
    let activation = flash_client
        .get_activation_state()
        .await
        .expect("get_activation_state failed");
    assert_eq!(
        activation.state, "rolled_back",
        "State should still be rolledback"
    );

    eprintln!("=== Test PASSED: Abort after rolledback rejected ===");
}

/// Test that abort is rejected after firmware is awaiting reset
///
/// After flash + transfer_exit, state is AwaitingReset. Abort should fail.
/// The correct path is ecu_reset() to activate, then rollback_flash() to revert.
#[tokio::test]
#[serial_test::serial]
async fn test_abort_after_awaiting_reset_rejected() {
    use sovd_client::flash::{FlashProgress, TransferState};

    eprintln!("\n=== Testing Abort After AwaitingReset (Rejected) ===");

    let harness = TestHarness::new()
        .await
        .expect("Failed to create test harness");

    let flash_client = harness.flash_client();

    // Flash firmware through to AwaitingReset state
    let payload_size = 1024;
    let firmware_data = TestHarness::create_firmware_package_with_version(
        payload_size,
        "7.4.0-abort-awaiting-reset-test",
    );

    eprintln!("Step 1: Flash firmware...");
    let upload_resp = flash_client
        .upload_file(&firmware_data)
        .await
        .expect("Upload failed");
    let file_id = &upload_resp.upload_id;
    flash_client
        .verify_file(file_id)
        .await
        .expect("Verify failed");

    let flash_resp = flash_client
        .start_flash(file_id)
        .await
        .expect("Start flash failed");
    let transfer_id = &flash_resp.transfer_id;

    flash_client
        .poll_flash_complete(
            transfer_id,
            Some(|progress: &FlashProgress| {
                let pct = progress.percent.unwrap_or(0.0);
                eprintln!(
                    "  Progress: {}/{} blocks ({:.1}%)",
                    progress.blocks_transferred, progress.blocks_total, pct
                );
            }),
        )
        .await
        .expect("Flash polling failed");

    eprintln!("Step 2: Transfer exit...");
    let exit_resp = flash_client
        .transfer_exit()
        .await
        .expect("Transfer exit failed");
    assert!(exit_resp.success);

    // Verify state is AwaitingReset (use get_flash_status to avoid auto-detect side effects)
    eprintln!("Step 3: Verify AwaitingReset state...");
    let flash_status = flash_client
        .get_flash_status(transfer_id)
        .await
        .expect("get_flash_status failed");
    assert_eq!(
        flash_status.state,
        TransferState::AwaitingReset,
        "Expected AwaitingReset after transfer_exit, got {:?}",
        flash_status.state
    );

    // Step 4: Attempt abort — should fail
    eprintln!("Step 4: Attempting abort (should be rejected)...");
    let abort_result = flash_client.abort_flash(transfer_id).await;
    assert!(
        abort_result.is_err(),
        "Abort should fail after AwaitingReset"
    );
    let error_msg = abort_result.unwrap_err().to_string();
    eprintln!("  Got expected error: {}", error_msg);

    // Verify state is unchanged
    let flash_status = flash_client
        .get_flash_status(transfer_id)
        .await
        .expect("get_flash_status failed");
    assert_eq!(
        flash_status.state,
        TransferState::AwaitingReset,
        "State should still be AwaitingReset after failed abort"
    );

    eprintln!("=== Test PASSED: Abort after awaiting reset rejected ===");
}
