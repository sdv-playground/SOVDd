//! Per-source log resource model (x-sumo) — in-process router tests.
//!
//! Covers:
//!   * `GET /logs/sources` enumerates the source catalog (name/kind/cursor/href).
//!   * `GET /logs/sources/{name}` resolves to a distinct route (NOT captured by
//!     the `/logs/{log_id}` catch-all) and reads only that source.
//!   * bare `GET /logs` picks the PRIMARY source (first Journal) when the
//!     component has >1 source — never a merge.
//!
//! Mirrors the `TestServer` in-process pattern from `data_categories.rs`.

use std::collections::HashMap;
use std::sync::Arc;

use sovd_client::testing::TestServer;
use sovd_core::{
    Capabilities, DiagnosticBackend, EntityInfo, LogEntry, LogFilter, LogPage, LogPriority,
    LogSourceInfo, LogSourceKind,
};

use sovd_api::{create_router, AppState};

/// A backend that models THREE sources: two journals (`slog2`, `guest`) and a
/// file (`host-boot`). `get_logs` echoes the requested `filter.source` back as
/// the single entry's message, so a test can assert WHICH source was read.
struct MultiSourceBackend {
    info: EntityInfo,
    capabilities: Capabilities,
}

impl MultiSourceBackend {
    fn new(id: &str) -> Self {
        let mut capabilities = Capabilities::default();
        capabilities.logs = true;
        Self {
            info: EntityInfo {
                id: id.to_string(),
                name: format!("{id} host"),
                entity_type: "host_os".to_string(),
                description: None,
                href: format!("/vehicle/v1/components/{id}"),
                status: Some("online".to_string()),
            },
            capabilities,
        }
    }
}

#[async_trait::async_trait]
impl DiagnosticBackend for MultiSourceBackend {
    fn entity_info(&self) -> &EntityInfo {
        &self.info
    }
    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    async fn list_log_sources(&self) -> sovd_core::BackendResult<Vec<LogSourceInfo>> {
        Ok(vec![
            LogSourceInfo {
                name: "slog2".into(),
                kind: LogSourceKind::Journal,
                cursor: false,
                emitters: vec![],
            },
            LogSourceInfo {
                name: "host-boot".into(),
                kind: LogSourceKind::File,
                cursor: true,
                emitters: vec![],
            },
            LogSourceInfo {
                name: "guest".into(),
                kind: LogSourceKind::Journal,
                cursor: true,
                emitters: vec![],
            },
        ])
    }

    /// Echo the requested source into a single entry's message (or `<none>`),
    /// so a test can assert which source the route selected.
    async fn get_logs(&self, filter: &LogFilter) -> sovd_core::BackendResult<Vec<LogEntry>> {
        let src = filter.source.clone();
        Ok(vec![LogEntry {
            id: "e1".into(),
            timestamp: chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
            priority: LogPriority::Info,
            message: format!("source={}", src.as_deref().unwrap_or("<none>")),
            source: src,
            pid: None,
            fields: None,
            log_type: None,
            size: None,
            status: None,
            href: None,
            metadata: None,
        }])
    }

    async fn get_logs_paged(&self, filter: &LogFilter) -> sovd_core::BackendResult<LogPage> {
        Ok(LogPage {
            items: self.get_logs(filter).await?,
            next_cursor: None,
            oldest_cursor: None,
            tip_cursor: None,
        })
    }

    // --- required (non-defaulted) trait methods, minimal stubs ---
    async fn list_parameters(&self) -> sovd_core::BackendResult<Vec<sovd_core::ParameterInfo>> {
        Ok(vec![])
    }
    async fn read_data(&self, _ids: &[String]) -> sovd_core::BackendResult<Vec<sovd_core::DataValue>> {
        Ok(vec![])
    }
    async fn get_faults(
        &self,
        _filter: Option<&sovd_core::FaultFilter>,
    ) -> sovd_core::BackendResult<sovd_core::FaultsResult> {
        Ok(sovd_core::FaultsResult {
            faults: vec![],
            status_availability_mask: None,
        })
    }
    async fn list_operations(&self) -> sovd_core::BackendResult<Vec<sovd_core::OperationInfo>> {
        Ok(vec![])
    }
    async fn start_operation(
        &self,
        op: &str,
        _params: &[u8],
    ) -> sovd_core::BackendResult<sovd_core::OperationExecution> {
        Err(sovd_core::BackendError::OperationNotFound(op.to_string()))
    }
}

async fn server(id: &str) -> TestServer {
    let mut backends = HashMap::new();
    backends.insert(
        id.to_string(),
        Arc::new(MultiSourceBackend::new(id)) as Arc<dyn DiagnosticBackend>,
    );
    TestServer::start(create_router(AppState::new(backends)))
        .await
        .expect("test server")
}

async fn get_json(server: &TestServer, path: &str) -> serde_json::Value {
    let url = format!("{}{}", server.base_url(), path);
    let resp = reqwest::Client::new().get(url).send().await.expect("get");
    assert_eq!(resp.status(), reqwest::StatusCode::OK, "GET {path}");
    resp.json().await.expect("json")
}

#[tokio::test]
async fn logs_sources_lists_the_catalog() {
    let server = server("host").await;
    let body = get_json(&server, "/vehicle/v1/components/host/logs/sources").await;
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 3, "three sources: {body}");

    let slog2 = items.iter().find(|i| i["name"] == "slog2").expect("slog2");
    assert_eq!(slog2["kind"], "journal");
    assert_eq!(slog2["cursor"], false);
    assert_eq!(
        slog2["href"], "/vehicle/v1/components/host/logs/sources/slog2",
        "href addresses the source: {slog2}"
    );
    // Empty emitters must be OMITTED from the wire.
    assert!(slog2.get("emitters").is_none(), "empty emitters skipped: {slog2}");

    let hb = items.iter().find(|i| i["name"] == "host-boot").expect("host-boot");
    assert_eq!(hb["kind"], "file");
    assert_eq!(hb["cursor"], true);
}

#[tokio::test]
async fn logs_sources_name_route_is_not_captured_by_log_id() {
    // The 3-segment /logs/sources/{name} must reach get_source_logs and set
    // filter.source = the path name — NOT be swallowed by /logs/{log_id}.
    let server = server("host").await;
    let body = get_json(
        &server,
        "/vehicle/v1/components/host/logs/sources/host-boot",
    )
    .await;
    let msg = body["items"][0]["message"].as_str().unwrap_or("");
    assert_eq!(
        msg, "source=host-boot",
        "per-source route must select the path source: {body}"
    );
}

#[tokio::test]
async fn bare_logs_picks_primary_journal_when_multi_source() {
    // >1 source + no explicit ?source= → the PRIMARY (first Journal = slog2),
    // never a cross-source merge.
    let server = server("host").await;
    let body = get_json(&server, "/vehicle/v1/components/host/logs").await;
    let msg = body["items"][0]["message"].as_str().unwrap_or("");
    assert_eq!(msg, "source=slog2", "bare /logs → primary journal: {body}");
}

#[tokio::test]
async fn explicit_source_query_still_honoured_on_bare_logs() {
    // An explicit ?source= is authoritative — the primary rule only fills a
    // MISSING source.
    let server = server("host").await;
    let body = get_json(&server, "/vehicle/v1/components/host/logs?source=guest").await;
    let msg = body["items"][0]["message"].as_str().unwrap_or("");
    assert_eq!(msg, "source=guest", "explicit source wins: {body}");
}
