//! A data read per tree: `/v1` answers an ISO 17978-3 `ReadValue` (§7.9.3,
//! `{"id", "data"}`), the frozen `/vehicle/v1` its `DidResponse`
//! (`{"id", "did", "value", ...}`) as before. Same in-process `TestServer`
//! pattern as `reset_outcome.rs`.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use sovd_client::testing::TestServer;
use sovd_core::{
    BackendError, BackendResult, Capabilities, DataValue, DiagnosticBackend, EntityInfo,
    FaultFilter, FaultsResult, OperationExecution, OperationInfo, ParameterInfo,
};

use sovd_api::{create_router, AppState};

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
    async fn read_data(&self, ids: &[String]) -> BackendResult<Vec<DataValue>> {
        match ids.first().map(String::as_str) {
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

async fn server() -> TestServer {
    let mut backends: HashMap<String, Arc<dyn DiagnosticBackend>> = HashMap::new();
    backends.insert(
        "bank".to_string(),
        Arc::new(VersionedBackend {
            info: entity_info("bank"),
            capabilities: Capabilities::default(),
            version: "1.2.0-rc.1",
        }),
    );
    TestServer::start(create_router(AppState::new(backends)))
        .await
        .expect("test server")
}

async fn read(server: &TestServer, path: &str) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = reqwest::Client::new()
        .get(format!("{}{path}", server.base_url()))
        .send()
        .await
        .expect("get");
    let status = resp.status();
    (status, resp.json().await.expect("json body"))
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
