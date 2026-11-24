//! A data read per tree: `/v1` answers an ISO 17978-3 `ReadValue` (§7.9.3,
//! `{"id", "data"}`, Table 85; `schema` only on `include-schema=true`), the
//! frozen `/vehicle/v1` its `DidResponse` (`{"id", "did", "value", ...}`) as
//! before, whatever the query. Same in-process `TestServer` pattern as
//! `reset_outcome.rs`.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use sovd_client::testing::TestServer;
use sovd_core::{
    BackendError, BackendResult, Capabilities, DataValue, DiagnosticBackend, EntityInfo,
    FaultFilter, FaultsResult, OperationExecution, OperationInfo, ParameterInfo,
};

use sovd_api::{
    create_router, with_node_topology, AppState, DataType, DidDefinition, DidStore, NodeTopology,
};

fn entity_info(id: &str) -> EntityInfo {
    EntityInfo {
        id: id.to_string(),
        name: format!("{id} ecu"),
        entity_type: "ecu".to_string(),
        description: None,
        href: format!("/vehicle/v1/components/{id}"),
        status: Some("online".to_string()),
    }
}

/// Serves `sw-version` through `read_data`, as a component manager's vendor
/// item does (no DID in the store), and has one app, `vm`, that does the same.
/// Also serves `coolant` through `read_data` with a unit and raw bytes, and the
/// raw bytes of DID F40C ([`store`]'s `engine_rpm`) through `read_raw_did`.
struct VersionedBackend {
    info: EntityInfo,
    capabilities: Capabilities,
    version: &'static str,
}

fn sw_version(version: &str) -> serde_json::Value {
    serde_json::json!({ "version": version, "items": [] })
}

#[async_trait::async_trait]
impl DiagnosticBackend for VersionedBackend {
    fn entity_info(&self) -> &EntityInfo {
        &self.info
    }
    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }
    async fn list_parameters(&self) -> BackendResult<Vec<ParameterInfo>> {
        Ok(vec![])
    }
    async fn read_raw_did(&self, did: u16) -> BackendResult<Vec<u8>> {
        match did {
            0xF40C => Ok(vec![0x0D, 0x49]),
            other => Err(BackendError::ParameterNotFound(format!("{other:04X}"))),
        }
    }
    async fn read_data(&self, ids: &[String]) -> BackendResult<Vec<DataValue>> {
        match ids.first().map(String::as_str) {
            Some("coolant") => Ok(vec![DataValue {
                id: "coolant".to_string(),
                name: "Coolant temperature".to_string(),
                value: serde_json::json!(50),
                unit: Some("degC".to_string()),
                timestamp: Utc::now(),
                raw: Some("5a".to_string()),
                did: Some("F405".to_string()),
                length: Some(1),
            }]),
            Some("sw-version") => Ok(vec![DataValue {
                id: "sw-version".to_string(),
                name: "Software version".to_string(),
                value: sw_version(self.version),
                unit: None,
                timestamp: Utc::now(),
                raw: None,
                did: None,
                length: None,
            }]),
            other => Err(BackendError::ParameterNotFound(
                other.unwrap_or_default().to_string(),
            )),
        }
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
    async fn get_sub_entity(&self, id: &str) -> BackendResult<Arc<dyn DiagnosticBackend>> {
        match id {
            "vm" => Ok(Arc::new(VersionedBackend {
                info: entity_info("vm"),
                capabilities: Capabilities::default(),
                version: "2.0.0",
            })),
            other => Err(BackendError::EntityNotFound(other.to_string())),
        }
    }
}

/// `engine_rpm`, DID F40C: uint16 × 0.25 in `rpm`, bounded 0..=16383.75.
fn store() -> Arc<DidStore> {
    let store = DidStore::new();
    store.register(
        0xF40C,
        DidDefinition::scaled(DataType::Uint16, 0.25, 0.0)
            .with_id("engine_rpm")
            .with_unit("rpm")
            .with_bounds(0.0, 16383.75),
    );
    Arc::new(store)
}

/// Component `bank` over [`store`].
fn state() -> AppState {
    let mut backends: HashMap<String, Arc<dyn DiagnosticBackend>> = HashMap::new();
    backends.insert(
        "bank".to_string(),
        Arc::new(VersionedBackend {
            info: entity_info("bank"),
            capabilities: Capabilities::default(),
            version: "1.2.0-rc.1",
        }),
    );
    AppState::with_did_store(backends, store())
}

async fn server() -> TestServer {
    TestServer::start(create_router(state()))
        .await
        .expect("test server")
}

/// [`state`] as the subcomponent `bank` of node `hpc`.
async fn node_server() -> TestServer {
    let state = state()
        .with_node(NodeTopology::new("hpc", "Node"))
        .expect("valid node");
    TestServer::start(with_node_topology(create_router(state.clone()), &state))
        .await
        .expect("test server")
}

async fn read(server: &TestServer, path: &str) -> (reqwest::StatusCode, serde_json::Value) {
    let (status, body) = read_text(server, path).await;
    (status, serde_json::from_str(&body).expect("json body"))
}

async fn read_text(server: &TestServer, path: &str) -> (reqwest::StatusCode, String) {
    let resp = reqwest::Client::new()
        .get(format!("{}{path}", server.base_url()))
        .send()
        .await
        .expect("get");
    let status = resp.status();
    (status, resp.text().await.expect("text body"))
}

/// The `/v1` read of `path` with `include-schema=true`, and the same read
/// without the parameter (and with `include-schema=false`), which must be the
/// former without its `schema`.
async fn read_with_schema(server: &TestServer, path: &str) -> serde_json::Value {
    let sep = if path.contains('?') { '&' } else { '?' };
    let (status, mut with) = read(server, &format!("{path}{sep}include-schema=true")).await;
    assert_eq!(status, reqwest::StatusCode::OK, "{path}");
    let schema = with
        .as_object_mut()
        .expect("object")
        .remove("schema")
        .unwrap_or_else(|| panic!("{path}: no schema"));
    for query in ["", "include-schema=false"] {
        let full = match query {
            "" => path.to_string(),
            q => format!("{path}{sep}{q}"),
        };
        let (status, without) = read(server, &full).await;
        assert_eq!(status, reqwest::StatusCode::OK, "{full}");
        assert_eq!(without, with, "{full}: no schema member, the rest the same");
    }
    schema
}

/// The schema of a `ReadValue` whose `data` is described by `data`.
fn read_value_schema(data: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": { "id": { "type": "string" }, "data": data },
    })
}

#[tokio::test]
async fn on_v1_a_data_read_is_a_read_value() {
    let server = server().await;
    let (status, body) = read(&server, "/v1/components/bank/data/sw-version").await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(
        body,
        serde_json::json!({ "id": "sw-version", "data": sw_version("1.2.0-rc.1") })
    );
}

#[tokio::test]
async fn on_v1_a_sub_entity_data_read_is_a_read_value() {
    let server = server().await;
    let (status, body) = read(&server, "/v1/components/bank/apps/vm/data/sw-version").await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(
        body,
        serde_json::json!({ "id": "sw-version", "data": sw_version("2.0.0") })
    );
}

#[tokio::test]
async fn the_legacy_tree_keeps_the_did_response() {
    let server = server().await;
    for (path, version) in [
        ("/vehicle/v1/components/bank/data/sw-version", "1.2.0-rc.1"),
        (
            "/vehicle/v1/components/bank/apps/vm/data/sw-version",
            "2.0.0",
        ),
    ] {
        let (status, body) = read(&server, path).await;
        assert_eq!(status, reqwest::StatusCode::OK, "{path}");
        assert_eq!(body["id"], "sw-version", "{path}: {body}");
        assert_eq!(body["value"], sw_version(version), "{path}: {body}");
        assert!(body.get("data").is_none(), "{path}: {body}");
        assert!(body["timestamp"].is_string(), "{path}: {body}");
    }
}

#[tokio::test]
async fn the_client_reads_the_value_on_either_tree() {
    let server = server().await;
    for dialect in [sovd_client::Dialect::Legacy, sovd_client::Dialect::V1] {
        let client = sovd_client::SovdClient::new(&server.base_url())
            .expect("client")
            .with_dialect(dialect)
            .await
            .expect("dialect");
        let read = client
            .read_data("bank", "sw-version")
            .await
            .expect("read through the client");
        assert_eq!(read.value, sw_version("1.2.0-rc.1"), "{dialect:?}");
    }
}

#[tokio::test]
async fn on_v1_a_converted_read_is_a_read_value() {
    let server = server().await;
    let (status, body) = read(&server, "/v1/components/bank/data/engine_rpm").await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(
        body,
        serde_json::json!({ "id": "engine_rpm", "data": 850.25 })
    );
}

#[tokio::test]
async fn the_schema_of_a_converted_read_carries_bounds_and_unit() {
    let server = server().await;
    let schema = read_with_schema(&server, "/v1/components/bank/data/engine_rpm").await;
    assert_eq!(
        schema,
        read_value_schema(serde_json::json!({
            "type": "number",
            "minimum": 0.0,
            "maximum": 16383.75,
            "x-sovd-unit": { "display_name": "rpm" },
        }))
    );
    // The bytes on the wire, without and with the schema.
    for (query, want) in [
        ("", r#"{"id":"engine_rpm","data":850.25}"#),
        (
            "?include-schema=true",
            r#"{"id":"engine_rpm","data":850.25,"schema":{"properties":{"data":{"maximum":16383.75,"minimum":0.0,"type":"number","x-sovd-unit":{"display_name":"rpm"}},"id":{"type":"string"}},"type":"object"}}"#,
        ),
    ] {
        let (_, body) = read_text(
            &server,
            &format!("/v1/components/bank/data/engine_rpm{query}"),
        )
        .await;
        assert_eq!(body, want, "{query}");
    }
    // A malformed `include-schema` is a 400 on `/v1`.
    let (status, _) = read_text(
        &server,
        "/v1/components/bank/data/engine_rpm?include-schema=nonsense",
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_schema_of_a_raw_read_is_a_string_without_unit_or_bounds() {
    let server = server().await;
    let (status, body) = read(&server, "/v1/components/bank/data/engine_rpm?raw=true").await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(
        body,
        serde_json::json!({ "id": "engine_rpm", "data": "0d49" })
    );
    let schema = read_with_schema(&server, "/v1/components/bank/data/engine_rpm?raw=true").await;
    assert_eq!(
        schema,
        read_value_schema(serde_json::json!({ "type": "string" }))
    );
    // A backend item read raw: its raw hex, no unit.
    let (_, body) = read(&server, "/v1/components/bank/data/coolant?raw=true").await;
    assert_eq!(body, serde_json::json!({ "id": "coolant", "data": "5a" }));
    let schema = read_with_schema(&server, "/v1/components/bank/data/coolant?raw=true").await;
    assert_eq!(
        schema,
        read_value_schema(serde_json::json!({ "type": "string" }))
    );
}

#[tokio::test]
async fn the_schema_of_a_backend_item_carries_its_unit_if_any() {
    let server = server().await;
    // No unit: the type only, an object's members not described.
    let schema = read_with_schema(&server, "/v1/components/bank/data/sw-version").await;
    assert_eq!(
        schema,
        read_value_schema(serde_json::json!({ "type": "object" }))
    );
    let schema = read_with_schema(&server, "/v1/components/bank/apps/vm/data/sw-version").await;
    assert_eq!(
        schema,
        read_value_schema(serde_json::json!({ "type": "object" }))
    );
    // The backend's unit; a backend item has no bounds.
    let schema = read_with_schema(&server, "/v1/components/bank/data/coolant").await;
    assert_eq!(
        schema,
        read_value_schema(serde_json::json!({
            "type": "integer",
            "x-sovd-unit": { "display_name": "degC" },
        }))
    );
}

#[tokio::test]
async fn the_node_form_answers_the_same_read_value() {
    let flat = server().await;
    let node = node_server().await;
    for (item, query) in [
        ("engine_rpm", ""),
        ("engine_rpm", "?include-schema=true"),
        ("engine_rpm", "?raw=true&include-schema=true"),
        ("coolant", "?include-schema=true"),
        ("sw-version", "?include-schema=true"),
    ] {
        let want = read(&flat, &format!("/v1/components/bank/data/{item}{query}")).await;
        assert_eq!(want.0, reqwest::StatusCode::OK, "{item}{query}");
        let got = read(
            &node,
            &format!("/v1/components/hpc/subcomponents/bank/data/{item}{query}"),
        )
        .await;
        assert_eq!(got, want, "{item}{query}");
    }
}

/// `body` with the value of its `timestamp` member replaced by `<ts>`, after
/// checking that it is an RFC 3339 string.
fn without_timestamp(body: &str) -> String {
    let value: serde_json::Value = serde_json::from_str(body).expect("json body");
    let ts = value["timestamp"].as_str().expect("timestamp string");
    chrono::DateTime::parse_from_rfc3339(ts).expect("RFC 3339 timestamp");
    body.replace(&format!("\"timestamp\":\"{ts}\""), "\"timestamp\":\"<ts>\"")
}

/// The frozen `/vehicle/v1` data-read body, byte for byte but the read time:
/// every member, in order, for a converted DID, a raw DID read and a backend
/// item; `include-schema` changes nothing there.
#[tokio::test]
async fn the_legacy_tree_keeps_the_complete_did_response() {
    let server = server().await;
    for (path, want) in [
        (
            "/vehicle/v1/components/bank/data/engine_rpm",
            r#"{"id":"engine_rpm","did":"F40C","value":850.25,"unit":"rpm","raw":"0d49","length":2,"converted":true,"timestamp":"<ts>"}"#,
        ),
        (
            "/vehicle/v1/components/bank/data/engine_rpm?raw=true",
            r#"{"id":"engine_rpm","did":"F40C","value":"0d49","raw":"0d49","length":2,"converted":false,"timestamp":"<ts>"}"#,
        ),
        (
            "/vehicle/v1/components/bank/data/coolant",
            r#"{"id":"coolant","did":"F405","value":50,"unit":"degC","raw":"5a","length":1,"converted":true,"timestamp":"<ts>"}"#,
        ),
        (
            "/vehicle/v1/components/bank/data/coolant?raw=true",
            r#"{"id":"coolant","did":"F405","value":"5a","raw":"5a","length":1,"converted":false,"timestamp":"<ts>"}"#,
        ),
        (
            "/vehicle/v1/components/bank/data/sw-version",
            r#"{"id":"sw-version","did":"","value":{"items":[],"version":"1.2.0-rc.1"},"raw":"","length":0,"converted":false,"timestamp":"<ts>"}"#,
        ),
        (
            "/vehicle/v1/components/bank/apps/vm/data/sw-version",
            r#"{"id":"sw-version","did":"","value":{"items":[],"version":"2.0.0"},"raw":"","length":0,"converted":false,"timestamp":"<ts>"}"#,
        ),
    ] {
        let sep = if path.contains('?') { '&' } else { '?' };
        for query in [
            "",
            "include-schema=true",
            "include-schema=false",
            "include-schema=nonsense",
        ] {
            let full = match query {
                "" => path.to_string(),
                q => format!("{path}{sep}{q}"),
            };
            let (status, body) = read_text(&server, &full).await;
            assert_eq!(status, reqwest::StatusCode::OK, "{full}");
            assert_eq!(without_timestamp(&body), want, "{full}");
        }
    }
}
