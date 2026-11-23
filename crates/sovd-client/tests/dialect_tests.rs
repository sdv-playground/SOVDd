//! Path dialects against the real server: `sovd_api::create_router` in-process, on the three
//! shapes a client can meet — legacy (`/vehicle/v1`), `/v1` flat (no node) and `/v1` node form
//! (`AppState::with_node` + `with_node_topology`). Every request the server receives is recorded
//! before any rewrite, so the tests pin the exact paths each shape produces.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::extract::Request;
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::IntoResponse;
use axum::routing::put;
use axum::{Json, Router};
use sovd_api::{create_router, with_node_topology, AppState, NodeTopology};
use sovd_client::testing::TestServer;
use sovd_client::{Dialect, FlashClient, FlashConfig, Paths, SovdClient};
use sovd_core::{
    ActivationState, BackendError, BackendResult, Capabilities, DataValue, DiagnosticBackend,
    EntityInfo, FaultFilter, FaultsResult, FlashProgress, FlashState, FlashStatus,
    OperationExecution, OperationInfo, PackageStream, ParameterInfo,
};

// =============================================================================
// Mock backend: one data item and a singleshot update executor
// =============================================================================

struct Mock {
    info: EntityInfo,
    capabilities: Capabilities,
    flash_state: Mutex<FlashState>,
}

impl Mock {
    fn new(id: &str) -> Arc<Self> {
        Arc::new(Self {
            info: EntityInfo {
                id: id.into(),
                name: format!("{id} mock"),
                entity_type: "ecu".into(),
                description: None,
                href: format!("/vehicle/v1/components/{id}"),
                status: Some("online".into()),
            },
            capabilities: Capabilities {
                software_update: true,
                ..Default::default()
            },
            flash_state: Mutex::new(FlashState::Transferring),
        })
    }

    fn set(&self, state: FlashState) {
        *self.flash_state.lock().unwrap() = state;
    }
}

#[async_trait]
impl DiagnosticBackend for Mock {
    fn entity_info(&self) -> &EntityInfo {
        &self.info
    }
    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }
    fn update_shape(&self) -> &'static str {
        "singleshot"
    }
    async fn list_parameters(&self) -> BackendResult<Vec<ParameterInfo>> {
        Ok(vec![ParameterInfo {
            id: "coolant_temp".into(),
            name: "Coolant temperature".into(),
            description: None,
            unit: Some("degC".into()),
            data_type: Some("uint8".into()),
            read_only: true,
            href: format!("/vehicle/v1/components/{}/data/coolant_temp", self.info.id),
            did: None,
            category: Some(sovd_core::DataCategory::CurrentData),
        }])
    }
    async fn read_data(&self, ids: &[String]) -> BackendResult<Vec<DataValue>> {
        Ok(ids
            .iter()
            .filter(|id| id.as_str() == "coolant_temp")
            .map(|id| DataValue::new(id.clone(), "Coolant temperature", serde_json::json!(42)))
            .collect())
    }
    async fn get_faults(&self, _: Option<&FaultFilter>) -> BackendResult<FaultsResult> {
        Ok(FaultsResult {
            faults: Vec::new(),
            status_availability_mask: None,
        })
    }
    async fn list_operations(&self) -> BackendResult<Vec<OperationInfo>> {
        Ok(Vec::new())
    }
    async fn start_operation(&self, op_id: &str, _: &[u8]) -> BackendResult<OperationExecution> {
        Err(BackendError::OperationNotFound(op_id.into()))
    }
    async fn ecu_reset(&self, _reset_type: u8) -> BackendResult<Option<u8>> {
        Ok(None)
    }
    async fn receive_package_stream(
        &self,
        mut stream: PackageStream,
        _content_length: Option<u64>,
    ) -> BackendResult<String> {
        use futures::StreamExt;
        while let Some(chunk) = stream.next().await {
            chunk.map_err(|e| BackendError::Internal(format!("stream: {e}")))?;
        }
        self.set(FlashState::AwaitingActivation);
        Ok("pkg-1".into())
    }
    async fn verify_part(&self, _file_id: &str, _expected_sha256: &str) -> BackendResult<()> {
        Ok(())
    }
    async fn start_flash(&self) -> BackendResult<String> {
        self.set(FlashState::Transferring);
        Ok("xfer-1".into())
    }
    async fn get_flash_status(&self, transfer_id: &str) -> BackendResult<FlashStatus> {
        Ok(FlashStatus {
            transfer_id: transfer_id.into(),
            package_id: "pkg".into(),
            state: *self.flash_state.lock().unwrap(),
            progress: Some(FlashProgress {
                bytes_transferred: 1,
                bytes_total: 1,
                blocks_transferred: 1,
                blocks_total: 1,
                percent: 100.0,
            }),
            error: None,
        })
    }
    async fn get_activation_state(&self) -> BackendResult<ActivationState> {
        Ok(ActivationState {
            supports_rollback: false,
            state: *self.flash_state.lock().unwrap(),
            active_version: None,
            previous_version: None,
            reset_kind: sovd_core::ResetKind::Local,
        })
    }
    async fn finalize_flash(&self) -> BackendResult<()> {
        self.set(FlashState::Activated);
        Ok(())
    }
    async fn commit_flash(&self) -> BackendResult<()> {
        self.set(FlashState::Committed);
        Ok(())
    }
}

// =============================================================================
// Server: the real router, the embedder's root restart stubs, a request recorder
// =============================================================================

/// The three shapes a client meets.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Shape {
    /// Legacy client (on a node server: `/vehicle/v1` stays flat there).
    Legacy,
    /// `/v1` client on a server without a node.
    V1Flat,
    /// `/v1` client on a node server (node `hpc`).
    V1Node,
}

const SHAPES: [Shape; 3] = [Shape::Legacy, Shape::V1Flat, Shape::V1Node];

struct Server {
    server: TestServer,
    log: Arc<Mutex<Vec<String>>>,
}

impl Server {
    /// The real router over `ids`, with node `hpc` when `node`. The root `status/restart` is
    /// the embedder's (not SOVDd's): stubbed here as an embedder would mount it.
    async fn start(ids: &[&str], node: bool) -> Self {
        let backends = ids
            .iter()
            .map(|id| (id.to_string(), Mock::new(id) as Arc<dyn DiagnosticBackend>))
            .collect();
        let mut state = AppState::new(backends);
        if node {
            state = state.with_node(NodeTopology::new("hpc", "HPC")).unwrap();
        }
        let embedder = Router::new()
            .route(
                "/vehicle/v1/status/restart",
                put(|| async {
                    (
                        StatusCode::ACCEPTED,
                        Json(serde_json::json!({
                            "status": "running", "exec_id": "e1", "reset_type": "hard",
                            "message": "hard reset initiated",
                            "href": "/vehicle/v1/status/restart/e1",
                        })),
                    )
                }),
            )
            .route(
                "/v1/status/restart",
                put(|| async { (StatusCode::ACCEPTED, [(header::LOCATION, "/v1/status")]) }),
            );
        let router = with_node_topology(create_router(state.clone()).merge(embedder), &state);
        let log = Arc::new(Mutex::new(Vec::new()));
        let recorder = log.clone();
        let router = Router::new()
            .fallback_service(router)
            .layer(middleware::from_fn(move |req: Request, next: Next| {
                let recorder = recorder.clone();
                async move {
                    let path = req.uri().path_and_query().map(|p| p.as_str().to_string());
                    let line = format!("{} {}", req.method(), path.unwrap_or_default());
                    {
                        let mut log = recorder.lock().unwrap();
                        // Status polls repeat a variable number of times: keep one per run.
                        let poll = line.starts_with("GET ") && line.ends_with("/status");
                        if !(poll && log.last() == Some(&line)) {
                            log.push(line);
                        }
                    }
                    next.run(req).await.into_response()
                }
            }));
        let server = TestServer::start(router).await.expect("test server");
        Self { server, log }
    }

    /// The server a shape is met on.
    async fn for_shape(shape: Shape) -> Self {
        Self::start(&["vm1", "vm2"], shape == Shape::V1Node).await
    }

    fn base(&self) -> String {
        self.server.base_url()
    }

    /// The requests recorded since the last call.
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.log.lock().unwrap())
    }

    async fn client(&self, shape: Shape) -> SovdClient {
        let client = SovdClient::new(&self.base()).unwrap();
        let client = match shape {
            Shape::Legacy => client,
            _ => client.with_dialect(Dialect::V1).await.unwrap(),
        };
        self.take();
        client
    }

    async fn flash_client(&self, shape: Shape, component: &str) -> FlashClient {
        let client = FlashClient::for_sovd(&self.base(), component).unwrap();
        let client = match shape {
            Shape::Legacy => client,
            _ => client.with_dialect(Dialect::V1).await.unwrap(),
        };
        self.take();
        client
    }
}

/// `legacy`, `v1 flat` or `v1 node` per shape.
fn pick(shape: Shape, [legacy, flat, node]: [&str; 3]) -> &str {
    match shape {
        Shape::Legacy => legacy,
        Shape::V1Flat => flat,
        Shape::V1Node => node,
    }
}

// =============================================================================
// Discovery
// =============================================================================

#[tokio::test]
async fn discovery_finds_the_node() {
    let s = Server::start(&["vm1", "vm2"], true).await;
    let client = SovdClient::new(&s.base()).unwrap();
    assert_eq!(
        client.discover_node().await.unwrap().as_deref(),
        Some("hpc")
    );
    assert_eq!(
        s.take(),
        ["GET /v1/components", "GET /v1/components/hpc/subcomponents"]
    );
    let client = client.with_dialect(Dialect::V1).await.unwrap();
    assert_eq!(client.paths(), &Paths::v1(Some("hpc".into())));

    let flash = FlashClient::for_sovd(&s.base(), "vm1")
        .unwrap()
        .with_dialect(Dialect::V1)
        .await
        .unwrap();
    assert_eq!(flash.config().paths, Paths::v1(Some("hpc".into())));
}

#[tokio::test]
async fn discovery_without_node_lists_several_components() {
    let s = Server::start(&["vm1", "vm2"], false).await;
    let client = SovdClient::new(&s.base()).unwrap();
    assert_eq!(client.discover_node().await.unwrap(), None);
    assert_eq!(s.take(), ["GET /v1/components"]);
}

#[tokio::test]
async fn discovery_single_component_is_not_mistaken_for_a_node() {
    let s = Server::start(&["vm1"], false).await;
    let client = SovdClient::new(&s.base()).unwrap();
    assert_eq!(client.discover_node().await.unwrap(), None);
    assert_eq!(
        s.take(),
        ["GET /v1/components", "GET /v1/components/vm1/subcomponents"]
    );
    let client = client.with_dialect(Dialect::V1).await.unwrap();
    assert_eq!(client.paths(), &Paths::v1(None));
}

#[tokio::test]
async fn legacy_dialect_requests_nothing() {
    let s = Server::start(&["vm1"], true).await;
    let client = SovdClient::new(&s.base())
        .unwrap()
        .with_dialect(Dialect::Legacy)
        .await
        .unwrap();
    assert_eq!(client.paths(), &Paths::legacy());
    FlashClient::for_sovd(&s.base(), "vm1")
        .unwrap()
        .with_dialect(Dialect::Legacy)
        .await
        .unwrap();
    assert!(s.take().is_empty());
}

// =============================================================================
// The client's requests on each shape
// =============================================================================

#[tokio::test]
async fn component_listing_on_every_shape() {
    for shape in SHAPES {
        let s = Server::for_shape(shape).await;
        let client = s.client(shape).await;
        let mut ids: Vec<_> = client
            .list_components()
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect();
        ids.sort();
        assert_eq!(ids, ["vm1", "vm2"], "{shape:?}");
        let want = pick(
            shape,
            [
                "GET /vehicle/v1/components",
                "GET /v1/components",
                "GET /v1/components/hpc/subcomponents",
            ],
        );
        assert_eq!(s.take(), [want], "{shape:?}");
    }
}

#[tokio::test]
async fn data_read_on_every_shape() {
    for shape in SHAPES {
        let s = Server::for_shape(shape).await;
        let client = s.client(shape).await;
        let value = client.read_data("vm1", "coolant_temp").await.unwrap();
        assert_eq!(value.value, serde_json::json!(42), "{shape:?}");
        let status = client.read_status("vm1").await.unwrap();
        assert!(
            serde_json::to_value(&status).unwrap()["status"].is_string(),
            "{shape:?}"
        );
        let m = pick(
            shape,
            [
                "/vehicle/v1/components/vm1",
                "/v1/components/vm1",
                "/v1/components/hpc/subcomponents/vm1",
            ],
        );
        assert_eq!(
            s.take(),
            [
                format!("GET {m}/data/coolant_temp"),
                format!("GET {m}/status")
            ],
            "{shape:?}"
        );
    }
}

#[tokio::test]
async fn update_flow_on_every_shape() {
    for shape in SHAPES {
        let s = Server::for_shape(shape).await;
        let fc = s.flash_client(shape, "vm1").await;
        let id = fc.open_update_with("Kernel", "1.0").await.unwrap();
        assert_eq!(id, "kernel-1.0");
        fc.upload_part("manifest", b"bytes").await.unwrap();
        let prepared = fc.prepare().await.unwrap();
        assert_eq!(prepared.status, "completed", "{shape:?}");
        // `false`: on `/v1` it is orchestrated anyway (the server refuses it otherwise); a
        // singleshot update settles on its own either way.
        let executed = fc.execute(false).await.unwrap();
        assert_eq!(
            (executed.phase.as_str(), executed.status.as_str()),
            ("execute", "completed"),
            "{shape:?}"
        );
        let status = fc.spec_status().await.unwrap();
        assert_eq!(status.status, "completed", "{shape:?}");

        let m = pick(
            shape,
            [
                "/vehicle/v1/components/vm1",
                "/v1/components/vm1",
                "/v1/components/hpc/subcomponents/vm1",
            ],
        );
        let u = format!("{m}/updates/kernel-1.0");
        let execute = match shape {
            Shape::Legacy => format!("PUT {u}/execute"),
            _ => format!("PUT {u}/execute?x-ota-control=orchestrated"),
        };
        assert_eq!(
            s.take(),
            [
                format!("POST {m}/updates"),
                format!("PUT {u}/bulk-data/manifest"),
                format!("PUT {u}/prepare"),
                format!("GET {u}/status"),
                execute,
                format!("GET {u}/status"),
            ],
            "{shape:?}"
        );
    }
}

#[tokio::test]
async fn restart_on_every_shape() {
    for shape in SHAPES {
        let s = Server::for_shape(shape).await;
        let client = s.client(shape).await;
        let fc = s.flash_client(shape, "vm1").await;
        client.system_restart(None, "hard").await.unwrap();
        client.system_restart(Some("vm1"), "hard").await.unwrap();
        let reset = client.ecu_reset("vm1", "hard").await.unwrap();
        let flash_reset = fc.ecu_reset("hard").await.unwrap();

        let m = pick(
            shape,
            [
                "/vehicle/v1/components/vm1",
                "/v1/components/vm1",
                "/v1/components/hpc/subcomponents/vm1",
            ],
        );
        let root = pick(
            shape,
            [
                "/vehicle/v1/status/restart",
                "/v1/status/restart",
                "/v1/components/hpc/status/restart",
            ],
        );
        assert_eq!(
            s.take(),
            [
                format!("PUT {root}"),
                format!("PUT {m}/status/restart"),
                format!("PUT {m}/status/restart"),
                format!("PUT {m}/status/restart"),
            ],
            "{shape:?}"
        );
        if shape == Shape::Legacy {
            // The legacy body, as before.
            assert_eq!(reset.message, "hard reset initiated");
            assert!(!reset.exec_id.is_empty());
            assert!(reset.href.starts_with(&format!("{m}/status/restart/")));
            assert!(!flash_reset.exec_id.is_empty());
        } else {
            // `/v1`: 202, no body; the Location (in the form the client speaks) is reported.
            assert_eq!(reset.status, "running");
            assert_eq!(reset.reset_type, "hard");
            assert_eq!(reset.href, format!("{m}/status"));
            assert_eq!(flash_reset.status, "running");
            assert_eq!(flash_reset.href, format!("{m}/status"));
            assert!(flash_reset.exec_id.is_empty());
        }
    }
}

#[tokio::test]
async fn resolved_paths_are_reused_without_discovery() {
    let s = Server::start(&["vm1", "vm2"], true).await;
    let paths = SovdClient::new(&s.base())
        .unwrap()
        .with_dialect(Dialect::V1)
        .await
        .unwrap()
        .paths()
        .clone();
    s.take();
    let client = SovdClient::new(&s.base())
        .unwrap()
        .with_paths(paths.clone());
    let fc = FlashClient::new(
        FlashConfig::builder(s.base())
            .component_id("vm2")
            .paths(paths)
            .build(),
    )
    .unwrap();
    client.read_status("vm2").await.unwrap();
    fc.list_updates().await.unwrap();
    assert_eq!(
        s.take(),
        [
            "GET /v1/components/hpc/subcomponents/vm2/status",
            "GET /v1/components/hpc/subcomponents/vm2/updates",
        ]
    );
}

// =============================================================================
// Legacy golden: a default client's requests are exactly the paths it sent before
// dialects existed (results ignored: only the requests matter).
// =============================================================================

#[tokio::test]
async fn legacy_requests_match_the_pre_dialect_paths() {
    let s = Server::start(&["vm1"], true).await;
    let c = SovdClient::new(&s.base()).unwrap();
    let _ = c.list_components().await;
    let _ = c.get_component("vm1").await;
    let _ = c.read_status("vm1").await;
    let _ = c.system_restart(None, "hard").await;
    let _ = c.system_restart(Some("gw"), "hard").await;
    let _ = c.list_parameters("vm1").await;
    let _ = c.list_sub_entity_parameters("gw", "a/b").await;
    let _ = c.read_sub_entity_data("gw", "app", "p/q").await;
    let _ = c.read_data("vm1", "coolant_temp").await;
    let _ = c.read_data_raw("vm1", "F190").await;
    let _ = c.read_data_batch("vm1", &["a", "b"]).await;
    let _ = c.write_data("vm1", "p", serde_json::json!(1)).await;
    let _ = c.get_faults("vm1").await;
    let _ = c.get_fault("vm1", "P0123").await;
    let _ = c.clear_faults("vm1").await;
    let _ = c.get_logs("vm1").await;
    let _ = c.list_log_sources("vm1").await;
    let _ = c.get_log("vm1", "l1").await;
    let _ = c.list_bulk_data_categories("vm1").await;
    let _ = c.list_operations("vm1").await;
    let _ = c.get_operation_execution("vm1", "op", "x1").await;
    let _ = c.list_scripts("vm1", None).await;
    let _ = c.list_apps("vm1").await;
    let _ = c.get_app("gw", "app").await;
    let _ = c.get_session("vm1").await;
    let _ = c.list_cyclic_subscriptions("vm1").await;
    let _ = c.get_cyclic_subscription("vm1", "s1").await;
    let _ = c.delete_cyclic_subscription("vm1", "s1").await;
    let _ = c.ecu_reset("vm1", "hard").await;

    let fc = FlashClient::for_sovd(&s.base(), "vm1").unwrap();
    let _ = fc.open_update_with("Kernel", "1.0").await;
    let _ = fc.upload_part("#kernel", b"bytes").await;
    let _ = fc.prepare().await;
    let _ = fc.execute(false).await;
    let _ = fc.list_updates().await;
    let _ = fc.force_rollback().await;
    let _ = fc.ecu_reset("hard").await;
    let sub = FlashClient::for_sovd_sub_entity(&s.base(), "gw", "a/b").unwrap();
    let _ = sub.list_updates().await;

    let golden = [
        "GET /vehicle/v1/components",
        "GET /vehicle/v1/components/vm1",
        "GET /vehicle/v1/components/vm1/status",
        "PUT /vehicle/v1/status/restart",
        "PUT /vehicle/v1/components/gw/status/restart",
        "GET /vehicle/v1/components/vm1/data",
        "GET /vehicle/v1/components/gw/apps/a%2Fb/data",
        "GET /vehicle/v1/components/gw/apps/app/data/p%2Fq",
        "GET /vehicle/v1/components/vm1/data/coolant_temp",
        "GET /vehicle/v1/components/vm1/data/F190?raw=true",
        "GET /vehicle/v1/components/vm1/data?ids=a,b",
        "PUT /vehicle/v1/components/vm1/data/p",
        "GET /vehicle/v1/components/vm1/faults",
        "GET /vehicle/v1/components/vm1/faults/P0123",
        "DELETE /vehicle/v1/components/vm1/faults",
        "GET /vehicle/v1/components/vm1/logs",
        "GET /vehicle/v1/components/vm1/logs/sources",
        "GET /vehicle/v1/components/vm1/logs/l1",
        "GET /vehicle/v1/components/vm1/bulk-data",
        "GET /vehicle/v1/components/vm1/operations",
        "GET /vehicle/v1/components/vm1/operations/op/executions/x1",
        "GET /vehicle/v1/components/vm1/scripts",
        "GET /vehicle/v1/components/vm1/apps",
        "GET /vehicle/v1/components/gw/apps/app",
        "GET /vehicle/v1/components/vm1/modes/session",
        "GET /vehicle/v1/components/vm1/cyclic-subscriptions",
        "GET /vehicle/v1/components/vm1/cyclic-subscriptions/s1",
        "DELETE /vehicle/v1/components/vm1/cyclic-subscriptions/s1",
        "PUT /vehicle/v1/components/vm1/status/restart",
        "POST /vehicle/v1/components/vm1/updates",
        "PUT /vehicle/v1/components/vm1/updates/kernel-1.0/bulk-data/%23kernel",
        "PUT /vehicle/v1/components/vm1/updates/kernel-1.0/prepare",
        "GET /vehicle/v1/components/vm1/updates/kernel-1.0/status",
        "PUT /vehicle/v1/components/vm1/updates/kernel-1.0/execute",
        "GET /vehicle/v1/components/vm1/updates/kernel-1.0/status",
        "GET /vehicle/v1/components/vm1/updates",
        "PUT /vehicle/v1/components/vm1/x-ota-force-rollback",
        "PUT /vehicle/v1/components/vm1/status/restart",
        "GET /vehicle/v1/components/gw/apps/a%2Fb/updates",
    ];
    assert_eq!(s.take(), golden);
}
