//! ISO 17978-3 §7.12 configurations — in-process router tests.
//!
//! Covers the wire shape of the five spec routes:
//!   * `GET /{entity}/configurations` — `ConfigurationMetaData[]` (Tables 143/144)
//!     with the `schema` member only when `include-schema=true`.
//!   * `GET /{entity}/configurations/{id}` — `ReadValue` for a `parameter`
//!     configuration (Table 85), the raw payload + its `Content-Type` for a
//!     `bulk` one, 406 with no body when `Accept` can't be honoured.
//!   * `PUT /{entity}/configurations/{id}` — 204; 400 on missing required
//!     values (Table 153), 409 on an unsatisfied precondition.
//!   * `DELETE /{entity}/configurations[/{id}]` — 204; 409 for a
//!     non-resettable configuration (§7.12.5.1 NOTE 3).
//!   * no `POST` — the spec defines no way to create a configuration.
//!
//! Same in-process `TestServer` pattern as `data_categories.rs`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use sovd_client::testing::TestServer;
use sovd_core::{
    BackendError, BackendResult, BulkConfiguration, Capabilities, ConfigurationMetaData,
    ConfigurationType, ConfigurationValue, ConfigurationWrite, DataValue, DiagnosticBackend,
    EntityInfo, FaultFilter, FaultsResult, OperationExecution, OperationInfo, ParameterInfo,
};

use sovd_api::{create_router, AppState};

/// The bulk configuration's payload, echoed back by `read_bulk_configuration`.
const MODEL_BYTES: &[u8] = b"\x4D\x61\x79\x20\x74\x68\x65\x20\x66\x6F\x72\x63\x65";

// ---------------------------------------------------------------------------
// Mock backend
// ---------------------------------------------------------------------------

/// An entity with four configurations exercising every §7.12 branch:
/// a plain `parameter` one, a `bulk` one, one whose write hits an unsatisfied
/// precondition, and one that is non-resettable.
struct ConfigBackend {
    info: EntityInfo,
    capabilities: Capabilities,
    /// Everything the handlers pushed down, in order — lets a test assert the
    /// backend actually received the write/reset.
    calls: Mutex<Vec<String>>,
}

impl ConfigBackend {
    fn new(id: &str, configurations: bool) -> Self {
        Self {
            info: EntityInfo {
                id: id.to_string(),
                name: format!("{id} app"),
                entity_type: "application".to_string(),
                description: None,
                href: format!("/vehicle/v1/components/{id}"),
                status: Some("online".to_string()),
            },
            capabilities: Capabilities {
                configurations,
                ..Capabilities::default()
            },
            calls: Mutex::new(Vec::new()),
        }
    }

    fn record(&self, call: String) {
        self.calls.lock().expect("calls lock").push(call);
    }
}

#[async_trait::async_trait]
impl DiagnosticBackend for ConfigBackend {
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

    async fn get_sub_entity(&self, id: &str) -> BackendResult<Arc<dyn DiagnosticBackend>> {
        match id {
            "child" => Ok(Arc::new(ConfigBackend::new("child", true))),
            other => Err(BackendError::EntityNotFound(other.to_string())),
        }
    }

    async fn list_configurations(&self) -> BackendResult<Vec<ConfigurationMetaData>> {
        Ok(vec![
            ConfigurationMetaData {
                id: "ObjectRecognitionModel".to_string(),
                name: "Model of objects to be recognized".to_string(),
                translation_id: None,
                configuration_type: ConfigurationType::Bulk,
                version: Some("1.45.2107".to_string()),
                content_type: Some("application/octet-stream".to_string()),
            },
            ConfigurationMetaData {
                id: "ALKConfig".to_string(),
                name: "Configuration for the Advanced Lane Keeping System".to_string(),
                translation_id: None,
                configuration_type: ConfigurationType::Parameter,
                version: None,
                content_type: None,
            },
            ConfigurationMetaData {
                id: "LockedConfig".to_string(),
                name: "Writable only while the vehicle is not in motion".to_string(),
                translation_id: None,
                configuration_type: ConfigurationType::Parameter,
                version: None,
                content_type: None,
            },
            ConfigurationMetaData {
                id: "FixedConfig".to_string(),
                name: "Non-resettable configuration".to_string(),
                translation_id: None,
                configuration_type: ConfigurationType::Parameter,
                version: None,
                content_type: None,
            },
        ])
    }

    async fn read_configuration(
        &self,
        configuration_id: &str,
    ) -> BackendResult<ConfigurationValue> {
        Ok(ConfigurationValue {
            id: configuration_id.to_string(),
            data: serde_json::json!({
                "MaximumSpeed": 150,
                "MinimumDistanceToLine": 15.0,
            }),
            schema: Some(serde_json::json!({
                "type": "object",
                "properties": { "MaximumSpeed": { "type": "integer" } },
            })),
        })
    }

    async fn write_configuration(
        &self,
        configuration_id: &str,
        write: &ConfigurationWrite,
    ) -> BackendResult<()> {
        if configuration_id == "LockedConfig" {
            return Err(BackendError::PreconditionFailed(
                "vehicle is in motion".to_string(),
            ));
        }
        // §7.12.4.1: a configuration is written as a WHOLE — a body missing a
        // required parameter is a 400 (Table 153).
        if write.data.get("MaximumSpeed").is_none() {
            return Err(BackendError::InvalidRequest(
                "missing required parameter: MaximumSpeed".to_string(),
            ));
        }
        self.record(format!("write {configuration_id} {}", write.data));
        Ok(())
    }

    async fn read_bulk_configuration(
        &self,
        configuration_id: &str,
        _accept: Option<&str>,
    ) -> BackendResult<BulkConfiguration> {
        self.record(format!("read-bulk {configuration_id}"));
        Ok(BulkConfiguration {
            content_type: "application/octet-stream".to_string(),
            body: MODEL_BYTES.to_vec(),
        })
    }

    async fn write_bulk_configuration(
        &self,
        configuration_id: &str,
        content_type: &str,
        body: &[u8],
    ) -> BackendResult<()> {
        self.record(format!(
            "write-bulk {configuration_id} {content_type} {}",
            body.len()
        ));
        Ok(())
    }

    async fn reset_configuration(&self, configuration_id: &str) -> BackendResult<()> {
        if configuration_id == "FixedConfig" {
            return Err(BackendError::PreconditionFailed(
                "configuration is not resettable".to_string(),
            ));
        }
        self.record(format!("reset {configuration_id}"));
        Ok(())
    }

    async fn reset_all_configurations(&self) -> BackendResult<()> {
        self.record("reset-all".to_string());
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Start a server with one `ConfigBackend` and hand back the backend so tests
/// can assert what the handlers pushed down.
async fn server(configurations: bool) -> (TestServer, Arc<ConfigBackend>) {
    let backend = Arc::new(ConfigBackend::new("alk", configurations));
    let mut backends: HashMap<String, Arc<dyn DiagnosticBackend>> = HashMap::new();
    backends.insert("alk".to_string(), backend.clone());
    let server = TestServer::start(create_router(AppState::new(backends)))
        .await
        .expect("test server");
    (server, backend)
}

fn http() -> reqwest::Client {
    reqwest::Client::new()
}

fn url(server: &TestServer, path: &str) -> String {
    format!("{}/vehicle/v1/components/alk{}", server.base_url(), path)
}

async fn get(server: &TestServer, path: &str) -> reqwest::Response {
    http().get(url(server, path)).send().await.expect("get")
}

async fn get_json(server: &TestServer, path: &str) -> serde_json::Value {
    let resp = get(server, path).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK, "GET {path}");
    resp.json().await.expect("json")
}

fn calls(backend: &ConfigBackend) -> Vec<String> {
    backend.calls.lock().expect("calls lock").clone()
}

// ---------------------------------------------------------------------------
// §7.12.2 — query for configurations
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_omits_schema_by_default() {
    let (server, _backend) = server(true).await;
    let body = get_json(&server, "/configurations").await;

    // Table 143: `schema` is conditional on include-schema=true.
    assert!(body.get("schema").is_none(), "unexpected schema: {body}");

    let items = body["items"].as_array().expect("items array");
    assert_eq!(items.len(), 4);

    // Table 144: version/content_type are present for bulk, absent for parameter.
    let bulk = &items[0];
    assert_eq!(bulk["id"], "ObjectRecognitionModel");
    assert_eq!(bulk["type"], "bulk");
    assert_eq!(bulk["version"], "1.45.2107");
    assert_eq!(bulk["content_type"], "application/octet-stream");

    let parameter = &items[1];
    assert_eq!(parameter["id"], "ALKConfig");
    assert_eq!(parameter["type"], "parameter");
    assert!(parameter.get("version").is_none());
    assert!(parameter.get("content_type").is_none());
    assert!(parameter.get("translation_id").is_none());
}

#[tokio::test]
async fn list_includes_schema_on_request() {
    let (server, _backend) = server(true).await;
    let body = get_json(&server, "/configurations?include-schema=true").await;

    let schema = &body["schema"];
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["required"][0], "items");
    assert_eq!(schema["properties"]["items"]["type"], "array");
    let item_props = &schema["properties"]["items"]["items"]["properties"];
    assert_eq!(item_props["type"]["enum"][0], "bulk");
    assert_eq!(item_props["type"]["enum"][1], "parameter");
    // The items themselves are still there alongside the schema.
    assert_eq!(body["items"].as_array().expect("items").len(), 4);
}

// ---------------------------------------------------------------------------
// §7.12.3 — read a configuration resource
// ---------------------------------------------------------------------------

#[tokio::test]
async fn read_parameter_configuration() {
    let (server, _backend) = server(true).await;
    let body = get_json(&server, "/configurations/ALKConfig").await;

    // Table 85 ReadValue: id + data; schema only on include-schema=true.
    assert_eq!(body["id"], "ALKConfig");
    assert_eq!(body["data"]["MaximumSpeed"], 150);
    assert!(body.get("schema").is_none(), "unexpected schema: {body}");

    let with_schema = get_json(&server, "/configurations/ALKConfig?include-schema=true").await;
    assert_eq!(with_schema["schema"]["type"], "object");
}

#[tokio::test]
async fn read_unknown_configuration_is_404() {
    let (server, _backend) = server(true).await;
    let resp = get(&server, "/configurations/NoSuchConfig").await;
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn read_bulk_configuration_with_matching_accept() {
    let (server, backend) = server(true).await;
    let resp = http()
        .get(url(&server, "/configurations/ObjectRecognitionModel"))
        .header(reqwest::header::ACCEPT, "application/octet-stream")
        .send()
        .await
        .expect("get");

    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/octet-stream")
    );
    assert_eq!(resp.bytes().await.expect("body").as_ref(), MODEL_BYTES);
    assert_eq!(calls(&backend), vec!["read-bulk ObjectRecognitionModel"]);
}

#[tokio::test]
async fn read_bulk_configuration_with_mismatching_accept_is_406() {
    let (server, backend) = server(true).await;
    let resp = http()
        .get(url(&server, "/configurations/ObjectRecognitionModel"))
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .expect("get");

    // Table 147: 406 with no response body.
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_ACCEPTABLE);
    assert!(resp.bytes().await.expect("body").is_empty());
    assert!(calls(&backend).is_empty(), "backend should not be reached");
}

#[tokio::test]
async fn read_parameter_configuration_with_mismatching_accept_is_406() {
    let (server, _backend) = server(true).await;
    let resp = http()
        .get(url(&server, "/configurations/ALKConfig"))
        .header(reqwest::header::ACCEPT, "application/octet-stream")
        .send()
        .await
        .expect("get");

    // Table 149: the parameter form is JSON-only.
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_ACCEPTABLE);
    assert!(resp.bytes().await.expect("body").is_empty());
}

// ---------------------------------------------------------------------------
// §7.12.4 — write a configuration resource
// ---------------------------------------------------------------------------

#[tokio::test]
async fn write_parameter_configuration_is_204() {
    let (server, backend) = server(true).await;
    let resp = http()
        .put(url(&server, "/configurations/ALKConfig"))
        .json(&serde_json::json!({
            "data": { "MaximumSpeed": 100 },
            "signature": "cz71a129fez26sw8",
        }))
        .send()
        .await
        .expect("put");

    assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);
    assert!(resp.bytes().await.expect("body").is_empty());
    assert_eq!(
        calls(&backend),
        vec![r#"write ALKConfig {"MaximumSpeed":100}"#]
    );
}

#[tokio::test]
async fn write_bulk_configuration_is_204() {
    let (server, backend) = server(true).await;
    let resp = http()
        .put(url(&server, "/configurations/ObjectRecognitionModel"))
        .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
        .body(MODEL_BYTES.to_vec())
        .send()
        .await
        .expect("put");

    assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(
        calls(&backend),
        vec![format!(
            "write-bulk ObjectRecognitionModel application/octet-stream {}",
            MODEL_BYTES.len()
        )]
    );
}

#[tokio::test]
async fn write_without_required_parameter_is_400() {
    let (server, _backend) = server(true).await;
    let resp = http()
        .put(url(&server, "/configurations/ALKConfig"))
        .json(&serde_json::json!({ "data": { "MinimumDistanceToLine": 10.0 } }))
        .send()
        .await
        .expect("put");

    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["error_code"], "incomplete-request");
}

#[tokio::test]
async fn write_with_unsatisfied_precondition_is_409() {
    let (server, _backend) = server(true).await;
    let resp = http()
        .put(url(&server, "/configurations/LockedConfig"))
        .json(&serde_json::json!({ "data": { "MaximumSpeed": 100 } }))
        .send()
        .await
        .expect("put");

    // Table 153: GenericError with the Table 18 precondition code.
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["error_code"], "precondition-not-fulfilled");
}

// ---------------------------------------------------------------------------
// §7.12.5 — reset configurations
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reset_one_and_all_are_204() {
    let (server, backend) = server(true).await;

    let resp = http()
        .delete(url(&server, "/configurations/ALKConfig"))
        .send()
        .await
        .expect("delete");
    assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);

    let resp = http()
        .delete(url(&server, "/configurations"))
        .send()
        .await
        .expect("delete");
    assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);

    assert_eq!(calls(&backend), vec!["reset ALKConfig", "reset-all"]);
}

#[tokio::test]
async fn reset_non_resettable_configuration_is_409() {
    let (server, _backend) = server(true).await;
    let resp = http()
        .delete(url(&server, "/configurations/FixedConfig"))
        .send()
        .await
        .expect("delete");

    // §7.12.5.1 NOTE 3 — the spec defines no dedicated code, so a
    // non-resettable configuration is an unsatisfied precondition (Table 156).
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["error_code"], "precondition-not-fulfilled");
}

// ---------------------------------------------------------------------------
// Route surface + capability gate
// ---------------------------------------------------------------------------

#[tokio::test]
async fn post_is_not_a_configurations_verb() {
    let (server, _backend) = server(true).await;
    let resp = http()
        .post(url(&server, "/configurations"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("post");

    // The spec has no create verb; whatever axum answers, it must not succeed.
    assert!(
        !resp.status().is_success(),
        "POST /configurations answered {}",
        resp.status()
    );
    assert_eq!(resp.status(), reqwest::StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn routes_are_gated_on_the_capability() {
    let (server, _backend) = server(false).await;
    // Same gate as `bulk_data` / `logs`: 501 sovd-server-misconfigured.
    for path in [
        "/configurations",
        "/configurations/ALKConfig",
        "/configurations?include-schema=true",
    ] {
        let resp = get(&server, path).await;
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::NOT_IMPLEMENTED,
            "GET {path}"
        );
    }
    let resp = http()
        .delete(url(&server, "/configurations"))
        .send()
        .await
        .expect("delete");
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_IMPLEMENTED);
}

// ---------------------------------------------------------------------------
// §6.5 — the same collection under a sub-entity path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sub_entity_inherits_the_configurations_collection() {
    let (server, _backend) = server(true).await;

    let body = get_json(&server, "/apps/child/configurations").await;
    assert_eq!(body["items"].as_array().expect("items").len(), 4);

    let body = get_json(&server, "/apps/child/configurations/ALKConfig").await;
    assert_eq!(body["id"], "ALKConfig");
    assert_eq!(body["data"]["MaximumSpeed"], 150);

    let resp = http()
        .delete(url(&server, "/apps/child/configurations"))
        .send()
        .await
        .expect("delete");
    assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);
}
