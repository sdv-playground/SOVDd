//! Operation hrefs on `/v1` — backends carry their operation hrefs in the
//! legacy form (`/vehicle/v1/...` literals in `OperationInfo.href` /
//! `OutputInfo.href`); no `/v1` response may render one.
//!
//! Covers every `/v1` path that lists or returns operations — the component's
//! operations list, a single operation (routine and IO control), an
//! execution's `Location` and body, a sub-entity's operations — in the flat
//! form without and with a node and in the nested member form (where every
//! URL is a nested one), and the `apps` listing beside them (its backend
//! `EntityInfo.href` is a legacy literal too). The legacy tree still renders
//! its own URLs, byte-identical with and without a node. SOVDd serves no
//! node-level operation itself (`/v1/operations/...` is an embedder's).

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use common::*;
use sovd_api::{create_router, with_node_topology, AppState, NodeTopology};
use sovd_client::testing::TestServer;
use sovd_core::{
    BackendResult, Capabilities, DataValue, DiagnosticBackend, EntityInfo, FaultFilter,
    FaultsResult, OperationExecution, OperationInfo, OutputDetail, OutputInfo, ParameterInfo,
};

/// A backend whose every href is the legacy-form literal a real one carries.
struct LegacyHrefBackend {
    info: EntityInfo,
    capabilities: Capabilities,
}

impl LegacyHrefBackend {
    fn new(id: &str) -> Self {
        Self {
            info: EntityInfo {
                id: id.to_string(),
                name: format!("{id} ecu"),
                entity_type: "ecu".to_string(),
                description: None,
                href: format!("/vehicle/v1/components/{id}"),
                status: Some("online".to_string()),
            },
            capabilities: Capabilities {
                operations: true,
                sub_entities: true,
                ..Capabilities::default()
            },
        }
    }
}

#[async_trait::async_trait]
impl DiagnosticBackend for LegacyHrefBackend {
    fn entity_info(&self) -> &EntityInfo {
        &self.info
    }
    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }
    async fn list_parameters(&self) -> BackendResult<Vec<ParameterInfo>> {
        Ok(vec![])
    }
    async fn read_data(&self, _: &[String]) -> BackendResult<Vec<DataValue>> {
        Ok(vec![])
    }
    async fn get_faults(&self, _: Option<&FaultFilter>) -> BackendResult<FaultsResult> {
        Ok(FaultsResult {
            faults: vec![],
            status_availability_mask: None,
        })
    }
    async fn list_operations(&self) -> BackendResult<Vec<OperationInfo>> {
        Ok(vec![OperationInfo {
            id: "selftest".to_string(),
            name: "Self test".to_string(),
            description: None,
            parameters: vec![],
            requires_security: false,
            security_level: 0,
            href: format!(
                "/vehicle/v1/components/{}/operations/selftest",
                self.info.id
            ),
        }])
    }
    async fn start_operation(&self, op: &str, _: &[u8]) -> BackendResult<OperationExecution> {
        Ok(OperationExecution::running("backend-exec", op))
    }
    async fn list_outputs(&self) -> BackendResult<Vec<OutputInfo>> {
        Ok(vec![OutputInfo {
            id: "fan".to_string(),
            name: "Fan".to_string(),
            output_id: "F206".to_string(),
            requires_security: false,
            security_level: 0,
            href: format!("/vehicle/v1/components/{}/outputs/fan", self.info.id),
            data_type: None,
            unit: None,
        }])
    }
    async fn get_output(&self, id: &str) -> BackendResult<OutputDetail> {
        Ok(OutputDetail {
            id: id.to_string(),
            name: "Fan".to_string(),
            output_id: "F206".to_string(),
            current_value: "00".to_string(),
            default_value: "00".to_string(),
            controlled_by_tester: false,
            frozen: false,
            requires_security: false,
            security_level: 0,
            value: None,
            default: None,
            data_type: None,
            unit: None,
            min: None,
            max: None,
            allowed: vec![],
        })
    }
    async fn list_sub_entities(&self) -> BackendResult<Vec<EntityInfo>> {
        Ok(vec![LegacyHrefBackend::new("app_a").info])
    }
    async fn get_sub_entity(&self, id: &str) -> BackendResult<Arc<dyn DiagnosticBackend>> {
        Ok(Arc::new(LegacyHrefBackend::new(id)))
    }
}

fn legacy_href_state() -> AppState {
    let backends: HashMap<String, Arc<dyn DiagnosticBackend>> = HashMap::from([(
        "ecu".to_string(),
        Arc::new(LegacyHrefBackend::new("ecu")) as Arc<dyn DiagnosticBackend>,
    )]);
    AppState::new(backends)
}

async fn server(node: bool) -> TestServer {
    let mut state = legacy_href_state();
    if node {
        state = state
            .with_node(NodeTopology::new("hpc", "HPC"))
            .expect("valid node");
    }
    TestServer::start(with_node_topology(create_router(state.clone()), &state))
        .await
        .expect("test server")
}

/// The component's resources that list or return operations (and its apps),
/// relative to its URL.
const READS: [&str; 6] = [
    "/operations",
    "/operations/selftest",
    "/operations/fan",
    "/apps",
    "/apps/app_a",
    "/apps/app_a/operations",
];

/// Every URL of `body` and `location`, checked: none in the legacy form, all
/// under `base`.
fn assert_urls_under(base: &str, what: &str, body: &str, location: Option<&str>) {
    assert!(!body.contains("/vehicle/v1"), "{what}: {body}");
    let mut found: Vec<String> = location.map(str::to_owned).into_iter().collect();
    urls(&serde_json::from_str(body).expect("JSON body"), &mut found);
    for url in found {
        assert!(url.starts_with(base), "{what}: {url} is not under {base}");
    }
}

/// `GET`s [`READS`] and starts and reads executions under `base`, checking
/// every URL of every response.
async fn check_operations_under(server: &TestServer, base: &str) {
    for rest in READS {
        let (status, body) = get(server, &format!("{base}{rest}"), None).await;
        let body = String::from_utf8(body).unwrap();
        assert_eq!(status, 200, "{base}{rest}: {body}");
        assert_urls_under(base, &format!("{base}{rest}"), &body, None);
    }
    for op in [
        "operations/selftest",
        "operations/fan",
        "apps/app_a/operations/selftest",
    ] {
        let parameters = if op.ends_with("fan") {
            serde_json::json!({ "action": "freeze" })
        } else {
            serde_json::Value::Null
        };
        let resp = request(
            server,
            reqwest::Method::POST,
            &format!("{base}/{op}/executions"),
            Some(serde_json::json!({ "parameters": parameters })),
        )
        .await;
        let status = resp.status().as_u16();
        let execution = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .map(|l| l.to_str().unwrap().to_owned())
            .expect("Location");
        let body = resp.text().await.unwrap();
        assert!((200..300).contains(&status), "{base}/{op}: {status} {body}");
        assert!(
            execution.starts_with(&format!("{base}/{op}/executions/")),
            "{base}/{op}: {execution}"
        );
        assert_urls_under(base, &format!("POST {base}/{op}"), &body, Some(&execution));

        let (status, body) = get(server, &execution, None).await;
        let body = String::from_utf8(body).unwrap();
        assert_eq!(status, 200, "{execution}: {body}");
        assert_urls_under(base, &execution, &body, None);
    }
}

#[tokio::test]
async fn v1_operations_render_no_legacy_href() {
    check_operations_under(&server(false).await, "/v1/components/ecu").await;
}

#[tokio::test]
async fn v1_operations_render_no_legacy_href_with_a_node() {
    let server = server(true).await;
    check_operations_under(&server, "/v1/components/hpc/subcomponents/ecu").await;
    // The flat form is not served on a node server.
    let (status, _) = get(&server, "/v1/components/ecu/operations", None).await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn legacy_operations_are_unchanged_with_a_node() {
    let node = server(true).await;
    let plain = server(false).await;
    for rest in READS {
        let path = format!("/vehicle/v1/components/ecu{rest}");
        let (status, body) = get(&node, &path, None).await;
        assert_eq!(status, 200, "{path}");
        assert_eq!(
            (status, body.clone()),
            get(&plain, &path, None).await,
            "{path}"
        );
        // The legacy tree renders its own URLs, rebuilt as on `/v1`.
        let mut found = Vec::new();
        urls(&json(&body), &mut found);
        for url in found {
            assert!(
                url.starts_with("/vehicle/v1/components/ecu"),
                "{path}: {url}"
            );
        }
    }
}
