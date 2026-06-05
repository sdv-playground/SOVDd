//! ISO 17978-3 §8.3.4/§8.3.5 + Table 343 (C-130/C-135) — in-process router
//! tests for `modes/comm-ctrl` (UDS CommunicationControl 0x28) and
//! `modes/dtcsetting` (UDS ControlDTCSetting 0x85).
//!
//! Concerns:
//!   * PUT a valid enum value → 200 with the `{id, value, ...}` body.
//!   * GET reflects the last-set value (write-only services); comm-ctrl GET
//!     also carries the ECU-specific `supported` enumeration.
//!   * An unknown enum value → 400 (BackendError::InvalidRequest).
//!   * The OLD route names (`communication-control`, `dtc-setting`) are gone
//!     (404) — they were renamed to `comm-ctrl` / `dtcsetting` for C-130.
//!
//! Mirrors the `TestServer` in-process pattern from `data_write_nrc.rs`.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use sovd_client::testing::TestServer;
use sovd_core::{
    BackendError, BackendResult, Capabilities, CommControlMode, DataValue, DiagnosticBackend,
    DtcSettingMode, EntityInfo, FaultFilter, FaultsResult, OperationExecution, OperationInfo,
    ParameterInfo,
};

use sovd_api::{create_router, AppState};

// ---------------------------------------------------------------------------
// Mock backend: tracks last-set comm-ctrl / dtcsetting state in-process,
// mirroring the UdsBackend semantics (enum→subfunction validated here, GET
// returns last-set). Unknown values map to InvalidRequest (→400).
// ---------------------------------------------------------------------------

const COMM_VALUES: &[&str] = &[
    "enable-rx-tx",
    "enable-rx-disable-tx",
    "disable-rx-enable-tx",
    "disable-rx-tx",
];

struct ModesBackend {
    info: EntityInfo,
    capabilities: Capabilities,
    comm: Mutex<String>,
    dtc: Mutex<String>,
}

impl ModesBackend {
    fn new(id: &str) -> Self {
        Self {
            info: EntityInfo {
                id: id.to_string(),
                name: format!("{id} ECU"),
                entity_type: "ecu".to_string(),
                description: None,
                href: format!("/vehicle/v1/components/{id}"),
                status: Some("online".to_string()),
            },
            capabilities: Capabilities::default(),
            comm: Mutex::new("enable-rx-tx".to_string()),
            dtc: Mutex::new("on".to_string()),
        }
    }
}

#[async_trait::async_trait]
impl DiagnosticBackend for ModesBackend {
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

    async fn get_communication_control(&self) -> BackendResult<CommControlMode> {
        Ok(CommControlMode {
            value: self.comm.lock().unwrap().clone(),
            supported: COMM_VALUES.iter().map(|s| s.to_string()).collect(),
        })
    }
    async fn set_communication_control(&self, value: &str) -> BackendResult<CommControlMode> {
        if !COMM_VALUES.contains(&value) {
            return Err(BackendError::InvalidRequest(format!(
                "Unknown comm-ctrl value '{value}'"
            )));
        }
        *self.comm.lock().unwrap() = value.to_string();
        self.get_communication_control().await
    }
    async fn get_dtc_setting(&self) -> BackendResult<DtcSettingMode> {
        Ok(DtcSettingMode {
            value: self.dtc.lock().unwrap().clone(),
        })
    }
    async fn set_dtc_setting(&self, value: &str) -> BackendResult<DtcSettingMode> {
        if value != "on" && value != "off" {
            return Err(BackendError::InvalidRequest(format!(
                "Unknown dtcsetting value '{value}'"
            )));
        }
        *self.dtc.lock().unwrap() = value.to_string();
        self.get_dtc_setting().await
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn server() -> TestServer {
    let mut backends = HashMap::new();
    backends.insert(
        "ecu1".to_string(),
        Arc::new(ModesBackend::new("ecu1")) as Arc<dyn DiagnosticBackend>,
    );
    TestServer::start(create_router(AppState::new(backends)))
        .await
        .expect("test server")
}

fn http() -> reqwest::Client {
    reqwest::Client::new()
}

async fn put(server: &TestServer, path: &str, body: serde_json::Value) -> reqwest::Response {
    let url = format!("{}{}", server.base_url(), path);
    http().put(url).json(&body).send().await.expect("put")
}

async fn get(server: &TestServer, path: &str) -> reqwest::Response {
    let url = format!("{}{}", server.base_url(), path);
    http().get(url).send().await.expect("get")
}

// ---------------------------------------------------------------------------
// modes/comm-ctrl
// ---------------------------------------------------------------------------

#[tokio::test]
async fn comm_ctrl_get_returns_default_and_supported() {
    let server = server().await;
    let resp = get(&server, "/vehicle/v1/components/ecu1/modes/comm-ctrl").await;
    assert_eq!(resp.status().as_u16(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["id"], "comm-ctrl", "id: {body}");
    assert_eq!(body["value"], "enable-rx-tx", "default value: {body}");
    let supported = body["supported"].as_array().expect("supported array");
    assert_eq!(
        supported.len(),
        4,
        "ECU-specific enum has 4 members: {body}"
    );
    assert!(
        supported.iter().any(|v| v == "disable-rx-tx"),
        "supported includes disable-rx-tx: {body}"
    );
}

#[tokio::test]
async fn comm_ctrl_put_valid_then_get_reflects() {
    let server = server().await;
    let resp = put(
        &server,
        "/vehicle/v1/components/ecu1/modes/comm-ctrl",
        serde_json::json!({"value": "disable-rx-tx"}),
    )
    .await;
    assert_eq!(resp.status().as_u16(), 200, "valid PUT → 200");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["id"], "comm-ctrl");
    assert_eq!(
        body["value"], "disable-rx-tx",
        "PUT echoes new value: {body}"
    );

    // GET reflects the last-set value.
    let got: serde_json::Value = get(&server, "/vehicle/v1/components/ecu1/modes/comm-ctrl")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(
        got["value"], "disable-rx-tx",
        "GET reflects last set: {got}"
    );
}

#[tokio::test]
async fn comm_ctrl_put_unknown_value_is_400() {
    let server = server().await;
    let resp = put(
        &server,
        "/vehicle/v1/components/ecu1/modes/comm-ctrl",
        serde_json::json!({"value": "turbo"}),
    )
    .await;
    assert_eq!(
        resp.status().as_u16(),
        400,
        "unknown comm-ctrl value → 400, got {}",
        resp.status()
    );
}

// ---------------------------------------------------------------------------
// modes/dtcsetting
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dtcsetting_get_returns_default() {
    let server = server().await;
    let resp = get(&server, "/vehicle/v1/components/ecu1/modes/dtcsetting").await;
    assert_eq!(resp.status().as_u16(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["id"], "dtcsetting", "id: {body}");
    assert_eq!(body["value"], "on", "default value: {body}");
}

#[tokio::test]
async fn dtcsetting_put_off_then_on() {
    let server = server().await;
    let resp = put(
        &server,
        "/vehicle/v1/components/ecu1/modes/dtcsetting",
        serde_json::json!({"value": "off"}),
    )
    .await;
    assert_eq!(resp.status().as_u16(), 200, "PUT off → 200");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["value"], "off", "PUT off echoes: {body}");

    let got: serde_json::Value = get(&server, "/vehicle/v1/components/ecu1/modes/dtcsetting")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(got["value"], "off", "GET reflects off: {got}");

    // Flip back on.
    let resp = put(
        &server,
        "/vehicle/v1/components/ecu1/modes/dtcsetting",
        serde_json::json!({"value": "on"}),
    )
    .await;
    assert_eq!(resp.status().as_u16(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["value"], "on", "PUT on echoes: {body}");
}

#[tokio::test]
async fn dtcsetting_put_unknown_value_is_400() {
    let server = server().await;
    let resp = put(
        &server,
        "/vehicle/v1/components/ecu1/modes/dtcsetting",
        serde_json::json!({"value": "disabled"}),
    )
    .await;
    assert_eq!(
        resp.status().as_u16(),
        400,
        "unknown dtcsetting value → 400, got {}",
        resp.status()
    );
}

// ---------------------------------------------------------------------------
// C-130: the OLD route names are gone (404)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn old_route_names_are_404() {
    let server = server().await;

    for path in [
        "/vehicle/v1/components/ecu1/modes/communication-control",
        "/vehicle/v1/components/ecu1/modes/dtc-setting",
    ] {
        let resp = get(&server, path).await;
        assert_eq!(
            resp.status().as_u16(),
            404,
            "old route {path} must be gone (404), got {}",
            resp.status()
        );
    }
}
