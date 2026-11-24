//! A backend's data items served on `/v1` only (`DiagnosticBackend::v1_only_data`):
//! `/v1` lists and reads them like any other item; the frozen `/vehicle/v1` neither
//! lists them nor reads them, and answers exactly as a backend without the item
//! makes it answer. Same in-process `TestServer` pattern as `v1_read_value.rs`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use chrono::Utc;
use sovd_client::testing::TestServer;
use sovd_core::{
    BackendError, BackendResult, Capabilities, DataValue, DiagnosticBackend, EntityInfo,
    FaultFilter, FaultsResult, OperationExecution, OperationInfo, ParameterInfo,
};

use sovd_api::{create_router, AppState};

/// The `/v1`-only item.
const V1_ONLY: &str = "x-state";

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

fn param(id: &str) -> ParameterInfo {
    ParameterInfo {
        id: id.to_string(),
        name: format!("{id} name"),
        description: None,
        unit: None,
        data_type: Some("string".to_string()),
        read_only: true,
        href: String::new(),
        did: None,
        category: Some(sovd_core::DataCategory::CurrentData),
    }
}

/// Serves `rpm` and, when `with_v1_only`, the `/v1`-only [`V1_ONLY`], through
/// `list_parameters`/`read_data`; an id it does not know reads as an empty
/// result. Counts the `read_data` calls for [`V1_ONLY`]. Its app `vm` is the
/// same backend.
struct ItemBackend {
    info: EntityInfo,
    capabilities: Capabilities,
    with_v1_only: bool,
    v1_only_reads: Arc<AtomicUsize>,
}

impl ItemBackend {
    fn new(id: &str, with_v1_only: bool, v1_only_reads: Arc<AtomicUsize>) -> Self {
        Self {
            info: entity_info(id),
            capabilities: Capabilities::default(),
            with_v1_only,
            v1_only_reads,
        }
    }
}

#[async_trait::async_trait]
impl DiagnosticBackend for ItemBackend {
    fn entity_info(&self) -> &EntityInfo {
        &self.info
    }
    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }
    async fn list_parameters(&self) -> BackendResult<Vec<ParameterInfo>> {
        let mut params = vec![param("rpm")];
        if self.with_v1_only {
            params.push(param(V1_ONLY));
        }
        Ok(params)
    }
    async fn read_data(&self, ids: &[String]) -> BackendResult<Vec<DataValue>> {
        let id = ids.first().cloned().unwrap_or_default();
        if id == V1_ONLY {
            self.v1_only_reads.fetch_add(1, Ordering::SeqCst);
        }
        if id != "rpm" && !(self.with_v1_only && id == V1_ONLY) {
            return Ok(vec![]);
        }
        Ok(vec![DataValue {
            id: id.clone(),
            name: format!("{id} name"),
            value: serde_json::json!(format!("{id} value")),
            unit: None,
            timestamp: Utc::now(),
            raw: None,
            did: None,
            length: None,
        }])
    }
    fn v1_only_data(&self) -> Vec<String> {
        if self.with_v1_only {
            vec![V1_ONLY.to_string()]
        } else {
            Vec::new()
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
            "vm" => Ok(Arc::new(ItemBackend::new(
                "vm",
                self.with_v1_only,
                self.v1_only_reads.clone(),
            ))),
            other => Err(BackendError::EntityNotFound(other.to_string())),
        }
    }
}

/// A server with component `ecu`, an [`ItemBackend`] with or without the
/// `/v1`-only item, and the counter of its `read_data` calls for that item.
async fn server(with_v1_only: bool) -> (TestServer, Arc<AtomicUsize>) {
    let reads = Arc::new(AtomicUsize::new(0));
    let mut backends: HashMap<String, Arc<dyn DiagnosticBackend>> = HashMap::new();
    backends.insert(
        "ecu".to_string(),
        Arc::new(ItemBackend::new("ecu", with_v1_only, reads.clone())),
    );
    let server = TestServer::start(create_router(AppState::new(backends)))
        .await
        .expect("test server");
    (server, reads)
}

async fn get(server: &TestServer, path: &str) -> (reqwest::StatusCode, String) {
    let resp = reqwest::Client::new()
        .get(format!("{}{path}", server.base_url()))
        .send()
        .await
        .expect("get");
    let status = resp.status();
    (status, resp.text().await.expect("text body"))
}

async fn get_json(server: &TestServer, path: &str) -> (reqwest::StatusCode, serde_json::Value) {
    let (status, body) = get(server, path).await;
    (status, serde_json::from_str(&body).expect("json body"))
}

/// The entity `/data` paths, of the component and of its app.
const ENTITIES: [&str; 2] = ["/components/ecu", "/components/ecu/apps/vm"];

#[tokio::test]
async fn v1_lists_and_reads_the_v1_only_item_like_any_other() {
    let (server, reads) = server(true).await;
    for entity in ENTITIES {
        let (status, listing) = get_json(&server, &format!("/v1{entity}/data")).await;
        assert_eq!(status, reqwest::StatusCode::OK, "{entity}");
        let mut ids: Vec<&str> = listing["items"]
            .as_array()
            .expect("items")
            .iter()
            .map(|item| item["id"].as_str().expect("id"))
            .collect();
        ids.sort();
        assert_eq!(ids, ["rpm", V1_ONLY], "{entity}: {listing}");

        for id in ["rpm", V1_ONLY] {
            let (status, body) = get_json(&server, &format!("/v1{entity}/data/{id}")).await;
            assert_eq!(status, reqwest::StatusCode::OK, "{entity} {id}");
            assert_eq!(
                body,
                serde_json::json!({ "id": id, "data": format!("{id} value") }),
                "{entity} {id}"
            );
        }
    }
    assert_eq!(reads.load(Ordering::SeqCst), ENTITIES.len());
}

#[tokio::test]
async fn the_legacy_listing_is_that_of_a_backend_without_the_item() {
    let (with, _) = server(true).await;
    let (without, _) = server(false).await;
    for entity in ENTITIES {
        let path = format!("/vehicle/v1{entity}/data");
        let (status, listing) = get(&with, &path).await;
        assert_eq!(status, reqwest::StatusCode::OK, "{path}");
        assert_eq!(
            (status, listing.clone()),
            get(&without, &path).await,
            "{path}"
        );
        let listing: serde_json::Value = serde_json::from_str(&listing).expect("json");
        assert_eq!(listing["count"], 1, "{path}: {listing}");
        assert_eq!(listing["items"][0]["id"], "rpm", "{path}: {listing}");
    }
}

#[tokio::test]
async fn the_legacy_read_is_that_of_an_id_the_backend_does_not_know() {
    let (with, reads) = server(true).await;
    let (without, _) = server(false).await;
    for entity in ENTITIES {
        let path = format!("/vehicle/v1{entity}/data/{V1_ONLY}");
        let (status, body) = get(&with, &path).await;
        assert!(!status.is_success(), "{path}: {status}");
        assert_eq!((status, body), get(&without, &path).await, "{path}");
    }
    assert_eq!(
        reads.load(Ordering::SeqCst),
        0,
        "the legacy tree does not read the item from the backend"
    );
    // The ordinary item still reads on the legacy tree.
    for entity in ENTITIES {
        let (status, body) = get_json(&with, &format!("/vehicle/v1{entity}/data/rpm")).await;
        assert_eq!(status, reqwest::StatusCode::OK, "{entity}");
        assert_eq!(body["value"], "rpm value", "{entity}: {body}");
    }
}
