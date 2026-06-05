//! ISO 17978-3 §8.4 data-write conformance (C-131) — in-process router tests.
//!
//! Two concerns:
//!   * NRC→HTTP mapping: a `0x2E` WriteDataByIdentifier that the ECU rejects
//!     with a UDS Negative Response Code surfaces as the mapped HTTP status
//!     (400/403/409/502/503), and the `error-response` body carries
//!     `service` + `nrc` + `http_code` (yaml:156).
//!   * Spec `{value}` body: the write body is `{value}` only — converted vs
//!     raw is inferred from the DID definition, not a body hint; a stray
//!     `format` key is ignored (no 500).
//!
//! Mirrors the `TestServer` in-process pattern from `data_categories.rs`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use sovd_client::testing::TestServer;
use sovd_conv::types::DataType;
use sovd_conv::{DidDefinition, DidStore};
use sovd_core::{
    BackendError, BackendResult, Capabilities, DataValue, DiagnosticBackend, EntityInfo,
    FaultFilter, FaultsResult, OperationExecution, OperationInfo, ParameterInfo,
};

use sovd_api::{create_router, AppState};

// ---------------------------------------------------------------------------
// Mock backend
// ---------------------------------------------------------------------------

/// ECU mock for write tests:
///   * `write_raw_did` records the bytes last written (so round-trip tests can
///     assert the encoding), unless `nrc` is non-zero, in which case it
///     rejects with `EcuError { nrc, sid: 0x2E }` (UDS WriteDataByIdentifier).
///   * `read_raw_did` serves the DID's current bytes.
struct WriteBackend {
    info: EntityInfo,
    capabilities: Capabilities,
    /// When non-zero, `write_raw_did` rejects with this NRC.
    nrc: AtomicU8,
    /// Bytes last written via `write_raw_did` (for round-trip assertions).
    last_written: Mutex<Option<Vec<u8>>>,
}

impl WriteBackend {
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
            nrc: AtomicU8::new(0),
            last_written: Mutex::new(None),
        }
    }

    fn with_nrc(self, nrc: u8) -> Self {
        self.nrc.store(nrc, Ordering::SeqCst);
        self
    }
}

#[async_trait::async_trait]
impl DiagnosticBackend for WriteBackend {
    fn entity_info(&self) -> &EntityInfo {
        &self.info
    }
    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }
    async fn list_parameters(&self) -> BackendResult<Vec<ParameterInfo>> {
        Ok(vec![])
    }
    async fn read_raw_did(&self, _did: u16) -> BackendResult<Vec<u8>> {
        Ok(self
            .last_written
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| vec![0x00, 0x00]))
    }
    async fn write_raw_did(&self, _did: u16, data: &[u8]) -> BackendResult<()> {
        let nrc = self.nrc.load(Ordering::SeqCst);
        if nrc != 0 {
            return Err(BackendError::EcuError {
                nrc,
                sid: 0x2E,
                message: format!("ECU rejected write with NRC 0x{nrc:02X}"),
            });
        }
        *self.last_written.lock().unwrap() = Some(data.to_vec());
        Ok(())
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

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// DidStore with a writable converted DID (`engine_rpm`, F40C, scale 0.25) and
/// a writable raw DID (`raw_blob`, F1A0, Bytes — no conversion).
fn write_store() -> Arc<DidStore> {
    let store = DidStore::new();

    let mut rpm = DidDefinition::scaled(DataType::Uint16, 0.25, 0.0)
        .with_id("engine_rpm")
        .with_name("Engine RPM")
        .with_unit("rpm");
    rpm.writable = true;
    store.register(0xF40C, rpm);

    let mut blob = DidDefinition::scalar(DataType::Bytes)
        .with_id("raw_blob")
        .with_name("Raw blob");
    blob.writable = true;
    store.register(0xF1A0, blob);

    Arc::new(store)
}

async fn server_with(backend: WriteBackend) -> TestServer {
    let mut backends = HashMap::new();
    backends.insert(
        "ecu1".to_string(),
        Arc::new(backend) as Arc<dyn DiagnosticBackend>,
    );
    let state = AppState::with_did_store(backends, write_store());
    TestServer::start(create_router(state))
        .await
        .expect("test server")
}

fn http() -> reqwest::Client {
    reqwest::Client::new()
}

/// PUT a write body to `engine_rpm` and return the raw response.
async fn put_write(server: &TestServer, param: &str, body: serde_json::Value) -> reqwest::Response {
    let url = format!(
        "{}/vehicle/v1/components/ecu1/data/{}",
        server.base_url(),
        param
    );
    http().put(url).json(&body).send().await.expect("put")
}

// ---------------------------------------------------------------------------
// Part B — NRC→HTTP mapping
// ---------------------------------------------------------------------------

/// Drive a write that the ECU rejects with `nrc`; assert the mapped HTTP
/// status and that the `error-response` body carries service/nrc/http_code.
async fn assert_nrc_maps_to(nrc: u8, expected_status: u16) {
    let server = server_with(WriteBackend::new("ecu1").with_nrc(nrc)).await;
    let resp = put_write(&server, "engine_rpm", serde_json::json!({"value": 1000})).await;

    assert_eq!(
        resp.status().as_u16(),
        expected_status,
        "NRC 0x{nrc:02X} should map to HTTP {expected_status}"
    );

    let body: serde_json::Value = resp.json().await.expect("error body json");
    assert_eq!(
        body["error_code"], "error-response",
        "error_code must be error-response: {body}"
    );
    // service + nrc + http_code are all present (parameters are string[]).
    assert_eq!(
        body["parameters"]["service"][0], "0x2E",
        "service param: {body}"
    );
    assert_eq!(
        body["parameters"]["nrc"][0],
        format!("0x{nrc:02X}"),
        "nrc param: {body}"
    );
    assert_eq!(
        body["parameters"]["http_code"][0],
        expected_status.to_string(),
        "http_code param must match the mapped status: {body}"
    );
}

#[tokio::test]
async fn nrc_request_out_of_range_maps_to_400() {
    assert_nrc_maps_to(0x31, 400).await;
}

#[tokio::test]
async fn nrc_security_access_denied_maps_to_403() {
    assert_nrc_maps_to(0x33, 403).await;
}

#[tokio::test]
async fn nrc_conditions_not_correct_maps_to_409() {
    assert_nrc_maps_to(0x22, 409).await;
}

#[tokio::test]
async fn nrc_general_reject_maps_to_502() {
    assert_nrc_maps_to(0x10, 502).await;
}

#[tokio::test]
async fn nrc_general_programming_failure_maps_to_502() {
    assert_nrc_maps_to(0x72, 502).await;
}

#[tokio::test]
async fn nrc_busy_repeat_request_maps_to_503() {
    assert_nrc_maps_to(0x21, 503).await;
}

#[tokio::test]
async fn unlisted_nrc_defaults_to_409() {
    // 0x70 is in the explicit 409 set; 0x99 is unlisted → also 409 (default).
    assert_nrc_maps_to(0x70, 409).await;
    assert_nrc_maps_to(0x99, 409).await;
}

// ---------------------------------------------------------------------------
// Part C — REAL UDS converter into the api error body
//
// Part B's mock backend fabricates `EcuError` directly, so it never exercises
// `sovd_uds::error::convert_uds_error` (the function the live `UdsBackend`
// write path runs to turn a UDS negative response into a `BackendError`). That
// converter used to short-circuit specific NRCs (0x33→SecurityRequired→401,
// 0x13→InvalidRequest, …) *before* the single-source `nrc_to_status` ran,
// bypassing the NRC→HTTP table and the Table-18 body. These tests drive the
// **real** converter end-to-end into `ApiError`'s `IntoResponse` and assert the
// mapped status + `error-response` body — the path the mock could not catch.
// ---------------------------------------------------------------------------

/// Drive `UdsError::NegativeResponse` through the real `convert_uds_error`
/// (live `UdsBackend` write path) → `ApiError` → HTTP response, and return the
/// status + parsed body.
async fn ecu_nrc_response(
    service_id: u8,
    nrc: sovd_uds::NegativeResponseCode,
) -> (u16, serde_json::Value) {
    use axum::response::IntoResponse;

    let backend_err = sovd_uds::error::convert_uds_error(sovd_uds::UdsError::NegativeResponse {
        service_id,
        nrc,
    });
    let api_err: sovd_api::ApiError = backend_err.into();
    let resp = api_err.into_response();

    let status = resp.status().as_u16();
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .expect("read body");
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("body json");
    (status, body)
}

/// 0x33 securityAccessDenied — the old converter made this `SecurityRequired`
/// → 401 with no `service`/`nrc`. The real path must now yield 403 + the
/// Table-18 `error-response` body.
#[tokio::test]
async fn real_path_security_denied_maps_to_403_with_body() {
    let (status, body) =
        ecu_nrc_response(0x2E, sovd_uds::NegativeResponseCode::SecurityAccessDenied).await;

    assert_eq!(status, 403, "0x33 → 403 through the real converter: {body}");
    assert_eq!(body["error_code"], "error-response", "{body}");
    assert_eq!(body["parameters"]["service"][0], "0x2E", "{body}");
    assert_eq!(body["parameters"]["nrc"][0], "0x33", "{body}");
    assert_eq!(body["parameters"]["http_code"][0], "403", "{body}");
}

/// 0x13 incorrectMessageLengthOrInvalidFormat — old converter made this
/// `InvalidRequest` (400 but no service/nrc body). Real path must be 400 + the
/// full Table-18 body.
#[tokio::test]
async fn real_path_incorrect_length_maps_to_400_with_body() {
    let (status, body) = ecu_nrc_response(
        0x2E,
        sovd_uds::NegativeResponseCode::IncorrectMessageLengthOrFormat,
    )
    .await;

    assert_eq!(status, 400, "0x13 → 400 through the real converter: {body}");
    assert_eq!(body["error_code"], "error-response", "{body}");
    assert_eq!(body["parameters"]["service"][0], "0x2E", "{body}");
    assert_eq!(body["parameters"]["nrc"][0], "0x13", "{body}");
    assert_eq!(body["parameters"]["http_code"][0], "400", "{body}");
}

/// 0x36 exceededNumberOfAttempts — old converter made this `RateLimited` →
/// 429 (not an RFC-9110 §15 status). Real path must be 403 (security gate) +
/// the Table-18 body.
#[tokio::test]
async fn real_path_exceeded_attempts_maps_to_403_not_429() {
    let (status, body) = ecu_nrc_response(
        0x2E,
        sovd_uds::NegativeResponseCode::ExceededNumberOfAttempts,
    )
    .await;

    assert_eq!(
        status, 403,
        "0x36 → 403 (was 429) through the real converter: {body}"
    );
    assert_eq!(body["error_code"], "error-response", "{body}");
    assert_eq!(body["parameters"]["nrc"][0], "0x36", "{body}");
}

// ---------------------------------------------------------------------------
// Part A — spec `{value}` body, raw-vs-converted inference
// ---------------------------------------------------------------------------

#[tokio::test]
async fn converted_did_writes_physical_value() {
    // engine_rpm has a conversion (scale 0.25): physical 1000 rpm → raw 4000
    // → big-endian [0x0F, 0xA0]. The body is `{value: 1000}` (physical), and
    // the server infers "converted" from the DID definition.
    let server = server_with(WriteBackend::new("ecu1")).await;
    let resp = put_write(&server, "engine_rpm", serde_json::json!({"value": 1000})).await;
    assert_eq!(resp.status().as_u16(), 204, "converted write → 204");

    // Read it back; the decoded physical value round-trips to 1000.
    let url = format!(
        "{}/vehicle/v1/components/ecu1/data/engine_rpm",
        server.base_url()
    );
    let read: serde_json::Value = http().get(url).send().await.unwrap().json().await.unwrap();
    assert_eq!(read["raw"], "0fa0", "raw bytes = 4000 BE: {read}");
    assert_eq!(read["value"], 1000.0, "decoded physical value: {read}");
}

#[tokio::test]
async fn raw_did_writes_hex_string_verbatim() {
    // raw_blob has NO conversion → `value` is a raw byte representation; a hex
    // string is decoded to those bytes verbatim.
    let server = server_with(WriteBackend::new("ecu1")).await;
    let resp = put_write(
        &server,
        "raw_blob",
        serde_json::json!({"value": "deadbeef"}),
    )
    .await;
    assert_eq!(resp.status().as_u16(), 204, "raw write → 204");

    let url = format!(
        "{}/vehicle/v1/components/ecu1/data/raw_blob?raw=true",
        server.base_url()
    );
    let read: serde_json::Value = http().get(url).send().await.unwrap().json().await.unwrap();
    assert_eq!(read["raw"], "deadbeef", "raw bytes verbatim: {read}");
}

#[tokio::test]
async fn raw_did_writes_byte_array() {
    // A byte array is also a valid raw representation.
    let server = server_with(WriteBackend::new("ecu1")).await;
    let resp = put_write(
        &server,
        "raw_blob",
        serde_json::json!({"value": [1, 2, 3, 4]}),
    )
    .await;
    assert_eq!(resp.status().as_u16(), 204);

    let url = format!(
        "{}/vehicle/v1/components/ecu1/data/raw_blob?raw=true",
        server.base_url()
    );
    let read: serde_json::Value = http().get(url).send().await.unwrap().json().await.unwrap();
    assert_eq!(read["raw"], "01020304", "byte array → bytes: {read}");
}

#[tokio::test]
async fn stray_format_key_is_ignored_not_500() {
    // The non-spec `format` field was removed; an old client that still sends
    // it must not 500 — serde ignores the extra key and the write succeeds on
    // `value` alone.
    let server = server_with(WriteBackend::new("ecu1")).await;
    let resp = put_write(
        &server,
        "raw_blob",
        serde_json::json!({"value": "deadbeef", "format": "hex"}),
    )
    .await;
    assert_eq!(
        resp.status().as_u16(),
        204,
        "stray format key must be ignored, not 500: {}",
        resp.status()
    );

    let url = format!(
        "{}/vehicle/v1/components/ecu1/data/raw_blob?raw=true",
        server.base_url()
    );
    let read: serde_json::Value = http().get(url).send().await.unwrap().json().await.unwrap();
    assert_eq!(read["raw"], "deadbeef", "value still honored: {read}");
}

#[tokio::test]
async fn missing_value_key_is_400_not_500() {
    // A body with no `value` is a malformed request (Json rejects the missing
    // required field) — must be a 4xx, never a 500.
    let server = server_with(WriteBackend::new("ecu1")).await;
    let resp = put_write(&server, "raw_blob", serde_json::json!({"nope": 1})).await;
    assert!(
        resp.status().is_client_error(),
        "missing value → client error, got {}",
        resp.status()
    );
}
