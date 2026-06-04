//! ISO 17978-3 §7.9 data-category model — in-process router tests.
//!
//! Covers the `DataCategory` tagging + filtering wired in this slice:
//!   * `GET /{entity}/data-categories` enumerates the distinct categories
//!     present (Table 72/73).
//!   * `GET /{entity}/data?categories=identData` filters the list to the
//!     identification DIDs (Table 78, explode=true / OR-combined).
//!   * a standard identification DID (`0xF180..=0xF19E`) defaults to
//!     `identData` with no explicit definition; an explicit `category:` on a
//!     definition overrides the DID-number default.
//!   * the single read (`GET /data/{id}`) does NOT carry `category`
//!     (`ReadValue`, Table 85, has no category attribute).
//!
//! These mirror the `TestServer` in-process pattern from
//! `definitions_e2e.rs` / `spec_update_flow.rs`.

use std::collections::HashMap;
use std::sync::Arc;

use sovd_client::testing::TestServer;
use sovd_conv::types::DataType;
use sovd_conv::{DidDefinition, DidStore};
use sovd_core::{
    BackendError, BackendResult, Capabilities, DataCategory, DataValue, DiagnosticBackend,
    EntityInfo, FaultFilter, FaultsResult, OperationExecution, OperationInfo, ParameterInfo,
};

use sovd_api::{create_router, AppState};

// ---------------------------------------------------------------------------
// Mock backends
// ---------------------------------------------------------------------------

/// DidStore-backed ECU: `list_parameters` is empty (categories come from the
/// DID definitions), `read_raw_did` serves raw bytes for the registered DIDs.
struct StoreBackend {
    info: EntityInfo,
    capabilities: Capabilities,
    did_values: HashMap<u16, Vec<u8>>,
}

impl StoreBackend {
    fn new(id: &str) -> Self {
        let mut did_values = HashMap::new();
        did_values.insert(0xF190, b"WF0XXXGCDX1234567".to_vec()); // VIN
        did_values.insert(0xF189, b"SW-1.2.3".to_vec()); // ECU SW version
        did_values.insert(0xF40C, vec![0x1C, 0x20]); // engine rpm (measurement)
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
            did_values,
        }
    }
}

#[async_trait::async_trait]
impl DiagnosticBackend for StoreBackend {
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
        self.did_values
            .get(&did)
            .cloned()
            .ok_or_else(|| BackendError::ParameterNotFound(format!("DID 0x{did:04X} not found")))
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

/// Backend-driven entity: no DidStore definitions; categories arrive on the
/// `ParameterInfo` from `list_parameters` (exercises the backend fallback path
/// + `param_info_to_did_info`).
struct ParamBackend {
    info: EntityInfo,
    capabilities: Capabilities,
}

impl ParamBackend {
    fn new(id: &str) -> Self {
        Self {
            info: EntityInfo {
                id: id.to_string(),
                name: format!("{id} app"),
                entity_type: "application".to_string(),
                description: None,
                href: format!("/vehicle/v1/components/{id}"),
                status: Some("online".to_string()),
            },
            capabilities: Capabilities::default(),
        }
    }
}

#[async_trait::async_trait]
impl DiagnosticBackend for ParamBackend {
    fn entity_info(&self) -> &EntityInfo {
        &self.info
    }
    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }
    async fn list_parameters(&self) -> BackendResult<Vec<ParameterInfo>> {
        Ok(vec![
            ParameterInfo {
                id: "sw_version".to_string(),
                name: "SW version".to_string(),
                description: None,
                unit: None,
                data_type: Some("string".to_string()),
                read_only: true,
                href: String::new(),
                did: None,
                category: Some(DataCategory::IdentData),
            },
            ParameterInfo {
                id: "battery_voltage".to_string(),
                name: "Battery voltage".to_string(),
                description: None,
                unit: Some("V".to_string()),
                data_type: Some("float64".to_string()),
                read_only: true,
                href: String::new(),
                did: None,
                category: Some(DataCategory::CurrentData),
            },
        ])
    }
    async fn read_data(&self, ids: &[String]) -> BackendResult<Vec<DataValue>> {
        Ok(ids
            .iter()
            .map(|id| DataValue::new(id.clone(), id.clone(), serde_json::json!("x")))
            .collect())
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

/// DidStore with: VIN (F190, identification — no explicit category),
/// engine_rpm (F40C, measurement), ecu_sw_version (F189, identification), and
/// an explicit override: a non-identification DID (F201) tagged `identData`.
fn ecu_store() -> Arc<DidStore> {
    let store = DidStore::new();
    store.register(
        0xF190,
        DidDefinition::scalar(DataType::String)
            .with_id("vin")
            .with_name("VIN"),
    );
    store.register(
        0xF189,
        DidDefinition::scalar(DataType::String)
            .with_id("ecu_sw_version")
            .with_name("ECU Software Version"),
    );
    store.register(
        0xF40C,
        DidDefinition::scaled(DataType::Uint16, 0.25, 0.0)
            .with_id("engine_rpm")
            .with_name("Engine RPM")
            .with_unit("rpm"),
    );
    // Explicit override: F201 is OUTSIDE the identification range (would
    // default to currentData) but is explicitly tagged identData.
    let mut explicit = DidDefinition::scalar(DataType::Bytes)
        .with_id("explicit_ident")
        .with_name("Explicit ident");
    explicit.category = Some(DataCategory::IdentData);
    store.register(0xF201, explicit);
    Arc::new(store)
}

async fn server_with<B: DiagnosticBackend + 'static>(
    id: &str,
    backend: B,
    store: Option<Arc<DidStore>>,
) -> TestServer {
    let mut backends = HashMap::new();
    backends.insert(
        id.to_string(),
        Arc::new(backend) as Arc<dyn DiagnosticBackend>,
    );
    let state = match store {
        Some(s) => AppState::with_did_store(backends, s),
        None => AppState::new(backends),
    };
    TestServer::start(create_router(state))
        .await
        .expect("test server")
}

fn http() -> reqwest::Client {
    reqwest::Client::new()
}

async fn get_json(server: &TestServer, path: &str) -> serde_json::Value {
    let url = format!("{}{}", server.base_url(), path);
    let resp = http().get(url).send().await.expect("get");
    assert_eq!(resp.status(), reqwest::StatusCode::OK, "GET {path}");
    resp.json().await.expect("json")
}

/// Collect the `category` wire token of every item in a `GET /data` response.
fn categories_in_list(body: &serde_json::Value) -> Vec<String> {
    body["items"]
        .as_array()
        .expect("items array")
        .iter()
        .filter_map(|it| it["category"].as_str().map(str::to_string))
        .collect()
}

/// Collect the `id` of every item in a `GET /data` response.
fn ids_in_list(body: &serde_json::Value) -> Vec<String> {
    body["items"]
        .as_array()
        .expect("items array")
        .iter()
        .filter_map(|it| it["id"].as_str().map(str::to_string))
        .collect()
}

// ---------------------------------------------------------------------------
// Tests — DidStore-backed (primary) path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn data_categories_lists_distinct_categories() {
    let server = server_with("ecu1", StoreBackend::new("ecu1"), Some(ecu_store())).await;
    let body = get_json(&server, "/vehicle/v1/components/ecu1/data-categories").await;

    let items = body["items"].as_array().expect("items");
    let tokens: Vec<&str> = items.iter().filter_map(|i| i["item"].as_str()).collect();
    // VIN/ecu_sw_version/explicit → identData; engine_rpm → currentData.
    assert!(
        tokens.contains(&"identData"),
        "expected identData in {tokens:?}"
    );
    assert!(
        tokens.contains(&"currentData"),
        "expected currentData in {tokens:?}"
    );
    // Distinct: each category appears exactly once.
    assert_eq!(tokens.len(), 2, "categories must be distinct: {tokens:?}");
    // Table 73: each entry carries a translation id.
    assert!(items
        .iter()
        .all(|i| i["category_translation_id"].is_string()));
}

#[tokio::test]
async fn list_data_includes_category_on_every_item() {
    let server = server_with("ecu1", StoreBackend::new("ecu1"), Some(ecu_store())).await;
    let body = get_json(&server, "/vehicle/v1/components/ecu1/data").await;

    // ValueMetaData.category is M — every listed item carries a category.
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 4);
    assert!(
        items.iter().all(|i| i["category"].is_string()),
        "every item must carry category: {body}"
    );
}

#[tokio::test]
async fn filter_categories_ident_returns_only_identification_dids() {
    let server = server_with("ecu1", StoreBackend::new("ecu1"), Some(ecu_store())).await;
    let body = get_json(
        &server,
        "/vehicle/v1/components/ecu1/data?categories=identData",
    )
    .await;

    let ids = ids_in_list(&body);
    // VIN (F190), ECU SW version (F189), and the explicit override (F201).
    assert!(ids.contains(&"vin".to_string()), "ids: {ids:?}");
    assert!(ids.contains(&"ecu_sw_version".to_string()), "ids: {ids:?}");
    assert!(ids.contains(&"explicit_ident".to_string()), "ids: {ids:?}");
    // engine_rpm is a measurement → excluded.
    assert!(!ids.contains(&"engine_rpm".to_string()), "ids: {ids:?}");
    // Every returned item is identData.
    assert!(categories_in_list(&body).iter().all(|c| c == "identData"));
}

#[tokio::test]
async fn filter_categories_current_returns_only_measurements() {
    let server = server_with("ecu1", StoreBackend::new("ecu1"), Some(ecu_store())).await;
    let body = get_json(
        &server,
        "/vehicle/v1/components/ecu1/data?categories=currentData",
    )
    .await;

    let ids = ids_in_list(&body);
    assert_eq!(ids, vec!["engine_rpm".to_string()], "ids: {ids:?}");
}

#[tokio::test]
async fn filter_categories_explode_or_combines_repeated_keys() {
    let server = server_with("ecu1", StoreBackend::new("ecu1"), Some(ecu_store())).await;
    // explode=true: two repeated keys, OR-combined → both categories present.
    let body = get_json(
        &server,
        "/vehicle/v1/components/ecu1/data?categories=identData&categories=currentData",
    )
    .await;
    // All four DIDs come back (3 ident + 1 current).
    assert_eq!(ids_in_list(&body).len(), 4);
}

#[tokio::test]
async fn no_filter_returns_all() {
    let server = server_with("ecu1", StoreBackend::new("ecu1"), Some(ecu_store())).await;
    let body = get_json(&server, "/vehicle/v1/components/ecu1/data").await;
    assert_eq!(ids_in_list(&body).len(), 4);
}

#[tokio::test]
async fn unknown_category_filters_to_empty() {
    let server = server_with("ecu1", StoreBackend::new("ecu1"), Some(ecu_store())).await;
    // A custom / unknown category token resolves to "no match", not the
    // unfiltered list.
    let body = get_json(
        &server,
        "/vehicle/v1/components/ecu1/data?categories=x-acme-foo",
    )
    .await;
    assert!(ids_in_list(&body).is_empty(), "body: {body}");
}

#[tokio::test]
async fn standard_ident_did_defaults_to_ident_data_without_explicit_definition() {
    // F189 (ECU SW version) is in 0xF180..=0xF19E and has NO explicit
    // category — it must resolve to identData by the DID-number default.
    let server = server_with("ecu1", StoreBackend::new("ecu1"), Some(ecu_store())).await;
    let body = get_json(&server, "/vehicle/v1/components/ecu1/data").await;
    let sw = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == "ecu_sw_version")
        .expect("ecu_sw_version present");
    assert_eq!(sw["category"], "identData");
}

#[tokio::test]
async fn single_read_does_not_carry_category() {
    // ReadValue (Table 85) has no `category` attribute — the per-resource
    // read must not invent one.
    let server = server_with("ecu1", StoreBackend::new("ecu1"), Some(ecu_store())).await;
    let body = get_json(&server, "/vehicle/v1/components/ecu1/data/vin").await;
    assert!(
        body.get("category").is_none(),
        "single read must not carry category: {body}"
    );
}

// ---------------------------------------------------------------------------
// Tests — backend-driven (ParameterInfo.category) path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn backend_param_categories_flow_through_list_and_categories() {
    let server = server_with("app1", ParamBackend::new("app1"), None).await;

    // data-categories enumerates both categories the backend produced.
    let cats = get_json(&server, "/vehicle/v1/components/app1/data-categories").await;
    let tokens: Vec<&str> = cats["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["item"].as_str())
        .collect();
    assert!(tokens.contains(&"identData"), "{tokens:?}");
    assert!(tokens.contains(&"currentData"), "{tokens:?}");

    // ?categories=identData keeps only the identification param.
    let body = get_json(
        &server,
        "/vehicle/v1/components/app1/data?categories=identData",
    )
    .await;
    assert_eq!(ids_in_list(&body), vec!["sw_version".to_string()]);
}
