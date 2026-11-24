//! A backend's data items served on `/v1` only (`DiagnosticBackend::list_v1_parameters`,
//! `read_v1_data`): `/v1` lists and reads them like any other item; the frozen
//! `/vehicle/v1` never asks for them, so it answers exactly as the same backend
//! without them does, whatever that backend answers for an id it does not know.
//! Same in-process `TestServer` pattern as `v1_read_value.rs`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use chrono::Utc;
use sovd_client::testing::TestServer;
use sovd_core::{
    BackendError, BackendResult, Capabilities, DataCategory, DataValue, DiagnosticBackend,
    EntityInfo, FaultFilter, FaultsResult, OperationExecution, OperationInfo, ParameterInfo,
};

use sovd_api::{create_router, with_node_topology, AppState, NodeTopology};

/// The `/v1`-only item.
const V1_ONLY: &str = "x-state";

/// How a backend's `read_data` answers an id it does not know.
#[derive(Clone, Copy, Debug)]
enum Unknown {
    /// An empty result.
    Empty,
    /// `BackendError::ParameterNotFound`.
    NotFound,
}

/// How many times the `/v1`-only surface and `read_data` were asked.
#[derive(Default)]
struct Calls {
    list_v1: AtomicUsize,
    read_v1: AtomicUsize,
    read_data_v1_only: AtomicUsize,
}

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

fn param(id: &str, name: &str, category: DataCategory) -> ParameterInfo {
    ParameterInfo {
        id: id.to_string(),
        name: name.to_string(),
        description: None,
        unit: None,
        data_type: Some("string".to_string()),
        read_only: true,
        href: String::new(),
        did: None,
        category: Some(category),
    }
}

fn value(id: &str) -> DataValue {
    DataValue {
        id: id.to_string(),
        name: id.to_string(),
        value: serde_json::json!(format!("{id} value")),
        unit: None,
        timestamp: Utc::now(),
        raw: None,
        did: None,
        length: None,
    }
}

/// Serves `rpm` through `list_parameters`/`read_data`, and, when
/// `with_v1_only`, [`V1_ONLY`] through the `/v1`-only surface, which also
/// lists `rpm` again (under another name). Its `read_v1_data` fails when
/// `fail_v1`. Its app `vm` is the same backend.
struct ItemBackend {
    info: EntityInfo,
    capabilities: Capabilities,
    with_v1_only: bool,
    fail_v1: bool,
    unknown: Unknown,
    calls: Arc<Calls>,
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
        Ok(vec![param(
            "rpm",
            "Engine speed",
            DataCategory::CurrentData,
        )])
    }
    async fn read_data(&self, ids: &[String]) -> BackendResult<Vec<DataValue>> {
        let id = ids.first().cloned().unwrap_or_default();
        if id == V1_ONLY {
            self.calls.read_data_v1_only.fetch_add(1, Ordering::SeqCst);
        }
        match (id.as_str(), self.unknown) {
            ("rpm", _) => Ok(vec![value("rpm")]),
            (_, Unknown::Empty) => Ok(vec![]),
            (other, Unknown::NotFound) => Err(BackendError::ParameterNotFound(other.to_string())),
        }
    }
    async fn list_v1_parameters(&self) -> BackendResult<Vec<ParameterInfo>> {
        self.calls.list_v1.fetch_add(1, Ordering::SeqCst);
        if !self.with_v1_only {
            return Ok(vec![]);
        }
        Ok(vec![
            param(V1_ONLY, "State", DataCategory::IdentData),
            param("rpm", "Not this one", DataCategory::IdentData),
        ])
    }
    async fn read_v1_data(&self, id: &str) -> BackendResult<Option<DataValue>> {
        self.calls.read_v1.fetch_add(1, Ordering::SeqCst);
        match id {
            V1_ONLY if self.with_v1_only && self.fail_v1 => Err(BackendError::Transport(
                "state service unreachable".to_string(),
            )),
            V1_ONLY if self.with_v1_only => Ok(Some(value(V1_ONLY))),
            _ => Ok(None),
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
            "vm" => Ok(Arc::new(ItemBackend {
                info: entity_info("vm"),
                capabilities: Capabilities::default(),
                with_v1_only: self.with_v1_only,
                fail_v1: self.fail_v1,
                unknown: self.unknown,
                calls: self.calls.clone(),
            })),
            other => Err(BackendError::EntityNotFound(other.to_string())),
        }
    }
}

/// A state with component `ecu`, an [`ItemBackend`], and its call counters.
fn state(with_v1_only: bool, fail_v1: bool, unknown: Unknown) -> (AppState, Arc<Calls>) {
    let calls = Arc::new(Calls::default());
    let mut backends: HashMap<String, Arc<dyn DiagnosticBackend>> = HashMap::new();
    backends.insert(
        "ecu".to_string(),
        Arc::new(ItemBackend {
            info: entity_info("ecu"),
            capabilities: Capabilities::default(),
            with_v1_only,
            fail_v1,
            unknown,
            calls: calls.clone(),
        }),
    );
    (AppState::new(backends), calls)
}

async fn server(with_v1_only: bool, fail_v1: bool, unknown: Unknown) -> (TestServer, Arc<Calls>) {
    let (state, calls) = state(with_v1_only, fail_v1, unknown);
    let server = TestServer::start(create_router(state))
        .await
        .expect("test server");
    (server, calls)
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

/// The entity paths, of the component and of its app.
const ENTITIES: [&str; 2] = ["/components/ecu", "/components/ecu/apps/vm"];

#[tokio::test]
async fn v1_lists_and_reads_the_v1_only_item() {
    let (server, calls) = server(true, false, Unknown::Empty).await;
    for entity in ENTITIES {
        let (status, listing) = get_json(&server, &format!("/v1{entity}/data")).await;
        assert_eq!(status, reqwest::StatusCode::OK, "{entity}");
        let mut items: Vec<(&str, &str)> = listing["items"]
            .as_array()
            .expect("items")
            .iter()
            .map(|i| (i["id"].as_str().unwrap(), i["name"].as_str().unwrap()))
            .collect();
        items.sort();
        // An id the listing already has keeps its entry.
        assert_eq!(
            items,
            [("rpm", "Engine speed"), (V1_ONLY, "State")],
            "{entity}: {listing}"
        );
        assert_eq!(listing["count"], 2, "{entity}: {listing}");

        let (status, body) = get_json(&server, &format!("/v1{entity}/data/{V1_ONLY}")).await;
        assert_eq!(status, reqwest::StatusCode::OK, "{entity}");
        assert_eq!(
            body,
            serde_json::json!({ "id": V1_ONLY, "data": "x-state value" }),
            "{entity}"
        );
    }
    // The data categories include the item's.
    let (status, body) = get_json(&server, "/v1/components/ecu/data-categories").await;
    assert_eq!(status, reqwest::StatusCode::OK);
    let categories: Vec<&str> = body["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|i| i["item"].as_str().unwrap())
        .collect();
    assert_eq!(categories, ["currentData", "identData"], "{body}");
    // The `/v1` read of the item never reaches `read_data`.
    assert_eq!(calls.read_data_v1_only.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_node_form_reads_the_v1_only_item() {
    let (state, _) = state(true, false, Unknown::Empty);
    let state = state
        .with_node(NodeTopology::new("hpc", "Node"))
        .expect("valid node");
    let server = TestServer::start(with_node_topology(create_router(state.clone()), &state))
        .await
        .expect("test server");
    let (status, body) = get_json(
        &server,
        &format!("/v1/components/hpc/subcomponents/ecu/data/{V1_ONLY}"),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(
        body,
        serde_json::json!({ "id": V1_ONLY, "data": "x-state value" })
    );
    let (status, listing) = get_json(&server, "/v1/components/hpc/subcomponents/ecu/data").await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert!(
        listing["items"]
            .as_array()
            .expect("items")
            .iter()
            .any(|i| i["id"] == V1_ONLY
                && i["href"] == format!("/v1/components/hpc/subcomponents/ecu/data/{V1_ONLY}")),
        "{listing}"
    );
}

/// For a backend whose `read_data` answers an unknown id with an empty result
/// and for one that answers `ParameterNotFound`, the legacy tree answers with
/// the `/v1`-only item exactly as without it, and never asks the `/v1`-only
/// surface.
#[tokio::test]
async fn the_legacy_tree_answers_as_the_backend_without_the_item() {
    for unknown in [Unknown::Empty, Unknown::NotFound] {
        let (with, calls) = server(true, false, unknown).await;
        let (without, _) = server(false, false, unknown).await;
        let mut paths = vec!["/vehicle/v1/components/ecu/data-categories".to_string()];
        for entity in ENTITIES {
            paths.push(format!("/vehicle/v1{entity}/data"));
            paths.push(format!("/vehicle/v1{entity}/data/{V1_ONLY}"));
            paths.push(format!("/vehicle/v1{entity}/data/{V1_ONLY}?raw=true"));
        }
        for path in &paths {
            let answer = get(&with, path).await;
            assert_eq!(answer, get(&without, path).await, "{unknown:?} {path}");
            if path.contains(V1_ONLY) {
                assert!(!answer.0.is_success(), "{unknown:?} {path}: {answer:?}");
            }
        }
        assert_eq!(calls.list_v1.load(Ordering::SeqCst), 0, "{unknown:?}");
        assert_eq!(calls.read_v1.load(Ordering::SeqCst), 0, "{unknown:?}");
    }
}

#[tokio::test]
async fn a_failing_v1_only_read_is_an_error_not_a_fallback() {
    let (server, calls) = server(true, true, Unknown::Empty).await;
    for entity in ENTITIES {
        let (status, body) = get_json(&server, &format!("/v1{entity}/data/{V1_ONLY}")).await;
        assert_eq!(
            status,
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            "{entity}: {body}"
        );
        assert!(body["error_code"].is_string(), "{entity}: {body}");
    }
    assert_eq!(calls.read_data_v1_only.load(Ordering::SeqCst), 0);
}
