//! Reading an in-store DID on a non-ECU entity (no `read_raw_did`, and the DID
//! isn't an identification DID that can be synthesized from entity metadata)
//! must return a truthful **404** that points at the owning child — NOT the
//! old misleading `501 sovd-server-misconfigured`.
//!
//! Mirrors the in-process `TestServer` pattern from `data_write_nrc.rs`.

use std::collections::HashMap;
use std::sync::Arc;

use sovd_api::{create_router, AppState};
use sovd_client::testing::TestServer;
use sovd_conv::types::DataType;
use sovd_conv::{DidDefinition, DidStore};
use sovd_core::{
    BackendError, BackendResult, Capabilities, DataValue, DiagnosticBackend, EntityInfo,
    FaultFilter, FaultsResult, OperationExecution, OperationInfo, ParameterInfo,
};

/// A non-ECU entity (e.g. an aggregating gateway, which owns no DIDs and
/// forwards to children). It does NOT override `read_raw_did`, so the trait
/// default `NotSupported` applies — exactly the gateway condition.
struct NonEcuBackend {
    info: EntityInfo,
    capabilities: Capabilities,
}

impl NonEcuBackend {
    fn new(id: &str) -> Self {
        Self {
            info: EntityInfo {
                id: id.to_string(),
                name: format!("{id} gateway"),
                entity_type: "gateway".to_string(),
                description: None,
                href: format!("/vehicle/v1/components/{id}"),
                status: Some("online".to_string()),
            },
            capabilities: Capabilities::default(),
        }
    }
}

#[async_trait::async_trait]
impl DiagnosticBackend for NonEcuBackend {
    fn entity_info(&self) -> &EntityInfo {
        &self.info
    }
    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }
    // The crux: NO `read_raw_did` override → the trait default `NotSupported`
    // applies (the gateway condition). The rest are trivial required stubs.
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
}

/// A store with one plainly non-identification DID, so `synthesize_entity_did`
/// can't fabricate it and the read falls through to the 404 path.
fn store_with_custom_did() -> Arc<DidStore> {
    let store = DidStore::new();
    let def = DidDefinition::scalar(DataType::Uint16)
        .with_id("custom_counter")
        .with_name("Custom counter");
    store.register(0x1234, def);
    Arc::new(store)
}

#[tokio::test]
async fn read_on_non_ecu_entity_is_404_not_501() {
    let mut backends = HashMap::new();
    backends.insert(
        "gw".to_string(),
        Arc::new(NonEcuBackend::new("gw")) as Arc<dyn DiagnosticBackend>,
    );
    let state = AppState::with_did_store(backends, store_with_custom_did());
    let server = TestServer::start(create_router(state))
        .await
        .expect("test server");

    let url = format!(
        "{}/vehicle/v1/components/gw/data/custom_counter",
        server.base_url()
    );
    let resp = reqwest::Client::new().get(url).send().await.expect("get");

    // The regression we're guarding: this used to be 501 sovd-server-misconfigured.
    assert_eq!(
        resp.status().as_u16(),
        404,
        "a DID read on a raw-less (non-ECU) entity must be 404, not 501"
    );

    let body: serde_json::Value = resp.json().await.expect("error body json");
    assert_ne!(
        body["error_code"], "sovd-server-misconfigured",
        "must not surface as a server-misconfigured error: {body}"
    );
    let msg = body["message"].as_str().unwrap_or_default();
    assert!(
        msg.contains("sub-entity"),
        "the 404 should point at the owning child sub-entity: {body}"
    );
}
