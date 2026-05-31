//! Application state for the SOVD API

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;

use parking_lot::Mutex;
use sovd_conv::DidStore;
use sovd_core::{DiagnosticBackend, OperationExecution};
use sovd_uds::config::OutputConfig;

use crate::error::ApiError;
use crate::handlers::subscriptions::SubscriptionManager;

/// Bounded recent-executions cache keyed by `(component_id, op_id, exec_id)`.
///
/// UDS RoutineControl is synchronous: `start_operation` returns the
/// final state immediately; subsequent backend polls have nothing new
/// to report.  Storing the start result here lets `GET .../executions/
/// {exec_id}` return the captured `OperationExecution` for ~64 recent
/// executions per component, after which entries roll off.
#[derive(Default)]
pub struct OperationExecutionCache {
    inner: Mutex<HashMap<(String, String, String), OperationExecution>>,
    // FIFO order of keys for bounded eviction.
    order: Mutex<VecDeque<(String, String, String)>>,
}

const OPERATION_EXEC_CACHE_CAP: usize = 64;

impl OperationExecutionCache {
    pub fn record(&self, component_id: &str, op_id: &str, execution: OperationExecution) {
        let key = (
            component_id.to_string(),
            op_id.to_string(),
            execution.execution_id.clone(),
        );
        let mut order = self.order.lock();
        let mut inner = self.inner.lock();
        if inner.len() >= OPERATION_EXEC_CACHE_CAP {
            if let Some(evict) = order.pop_front() {
                inner.remove(&evict);
            }
        }
        order.push_back(key.clone());
        inner.insert(key, execution);
    }

    pub fn get(
        &self,
        component_id: &str,
        op_id: &str,
        exec_id: &str,
    ) -> Option<OperationExecution> {
        self.inner
            .lock()
            .get(&(
                component_id.to_string(),
                op_id.to_string(),
                exec_id.to_string(),
            ))
            .cloned()
    }
}

/// Per-component log configuration — Spec §7.21 `logs/config`.
///
/// Initialised on first read with server defaults; mutated by
/// `PUT .../logs/config`; cleared by `DELETE .../logs/config`.  Held
/// in-memory only — restart drops it back to defaults.
#[derive(Clone, Debug, Default)]
pub struct LogConfigStore(pub Arc<Mutex<HashMap<String, serde_json::Value>>>);

/// Per-component clear-data activity tracker.  Each entry is the
/// most-recently issued clear-data action's status (`idle`, `running`,
/// `completed`, `failed`).
#[derive(Clone, Debug, Default)]
pub struct ClearDataStatusStore(pub Arc<Mutex<HashMap<String, String>>>);

/// Per-update tracking for the spec-compliant `/updates` collection.
///
/// F.D2 adds a thin wire alias over the existing flash backend; the
/// backend already manages session state (transfer_id is the truth)
/// but the new wire surfaces `/bulk-data/{part_id}` PUTs that the
/// backend has no first-class notion of.  This store remembers which
/// part ids the SOVD layer has accepted for each update so:
///
/// - `GET /updates/{id}/bulk-data` can enumerate them.
/// - `POST /updates/{id}/executions {verify}` can refuse if zero parts
///   have been uploaded (otherwise the backend would just succeed on an
///   empty session).
///
/// Held in memory only — survives no restart.  Entries roll off when
/// the update reaches a terminal state.
#[derive(Clone, Debug, Default)]
pub struct UpdatesStore(pub Arc<Mutex<HashMap<String, UpdatesEntry>>>);

#[derive(Clone, Debug, Default)]
pub struct UpdatesEntry {
    pub component_id: String,
    pub parts: Vec<UpdatePart>,
    pub manifest: Option<serde_json::Value>,
    /// Phase the SOVD wire has reached.
    pub state: UpdateState,
    /// Backend's transfer_id, populated once `verify` calls `start_flash`.
    pub transfer_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct UpdatePart {
    pub part_id: String,
    pub size: u64,
    pub sha256: String,
    /// Backend's file_id from `receive_package_stream`; needed by the
    /// `verify` step which calls `verify_package(file_id)` per part.
    pub file_id: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UpdateState {
    #[default]
    Registered,
    Uploading,
    Verified,
    Finalized,
    Committed,
    RolledBack,
    Aborted,
}

impl UpdateState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Registered => "registered",
            Self::Uploading => "uploading",
            Self::Verified => "verified",
            Self::Finalized => "finalized",
            Self::Committed => "committed",
            Self::RolledBack => "rolledback",
            Self::Aborted => "aborted",
        }
    }
}

/// Application state shared across all handlers
#[derive(Clone)]
pub struct AppState {
    /// Map of component ID to backend implementation
    backends: Arc<HashMap<String, Arc<dyn DiagnosticBackend>>>,
    /// DID conversion store (shared across all backends)
    did_store: Arc<DidStore>,
    /// Subscription manager
    pub subscription_manager: Arc<SubscriptionManager>,
    /// Output configs per component: component_id -> Vec<OutputConfig>
    output_configs: Arc<HashMap<String, Vec<OutputConfig>>>,
    /// Bounded cache of recent operation executions for `GET ../executions/{id}`.
    pub operation_executions: Arc<OperationExecutionCache>,
    /// Per-component logs/config persistence.
    pub log_config: LogConfigStore,
    /// Per-component clear-data activity status.
    pub clear_data_status: ClearDataStatusStore,
    /// Per-update part tracking for the `/updates` collection.
    pub updates: UpdatesStore,
}

impl AppState {
    /// Create a new AppState with the given backends
    pub fn new(backends: HashMap<String, Arc<dyn DiagnosticBackend>>) -> Self {
        Self {
            backends: Arc::new(backends),
            did_store: Arc::new(DidStore::new()),
            subscription_manager: Arc::new(SubscriptionManager::new()),
            output_configs: Arc::new(HashMap::new()),
            operation_executions: Arc::new(OperationExecutionCache::default()),
            log_config: LogConfigStore::default(),
            clear_data_status: ClearDataStatusStore::default(),
            updates: UpdatesStore::default(),
        }
    }

    /// Create a new AppState with backends and an existing DidStore
    pub fn with_did_store(
        backends: HashMap<String, Arc<dyn DiagnosticBackend>>,
        did_store: Arc<DidStore>,
    ) -> Self {
        Self {
            backends: Arc::new(backends),
            did_store,
            subscription_manager: Arc::new(SubscriptionManager::new()),
            output_configs: Arc::new(HashMap::new()),
            operation_executions: Arc::new(OperationExecutionCache::default()),
            log_config: LogConfigStore::default(),
            clear_data_status: ClearDataStatusStore::default(),
            updates: UpdatesStore::default(),
        }
    }

    /// Create a new AppState with backends, DidStore, and output configs
    pub fn with_output_configs(
        backends: HashMap<String, Arc<dyn DiagnosticBackend>>,
        did_store: Arc<DidStore>,
        output_configs: HashMap<String, Vec<OutputConfig>>,
    ) -> Self {
        Self {
            backends: Arc::new(backends),
            did_store,
            subscription_manager: Arc::new(SubscriptionManager::new()),
            output_configs: Arc::new(output_configs),
            operation_executions: Arc::new(OperationExecutionCache::default()),
            log_config: LogConfigStore::default(),
            clear_data_status: ClearDataStatusStore::default(),
            updates: UpdatesStore::default(),
        }
    }

    /// Create AppState from a single backend (for simple single-entity servers)
    pub fn single(id: impl Into<String>, backend: Arc<dyn DiagnosticBackend>) -> Self {
        let mut backends = HashMap::new();
        backends.insert(id.into(), backend);
        Self::new(backends)
    }

    /// Get a backend by component ID
    pub fn get_backend(&self, component_id: &str) -> Result<&Arc<dyn DiagnosticBackend>, ApiError> {
        self.backends
            .get(component_id)
            .ok_or_else(|| ApiError::NotFound(format!("Component not found: {}", component_id)))
    }

    /// List all component IDs
    pub fn component_ids(&self) -> Vec<&String> {
        self.backends.keys().collect()
    }

    /// Get all backends
    pub fn backends(&self) -> &HashMap<String, Arc<dyn DiagnosticBackend>> {
        &self.backends
    }

    /// Get the DID store
    pub fn did_store(&self) -> &DidStore {
        &self.did_store
    }

    /// Get the DID store Arc (for sharing)
    pub fn did_store_arc(&self) -> Arc<DidStore> {
        self.did_store.clone()
    }

    /// Get the output config for a specific component and output
    pub fn get_output_config(&self, component_id: &str, output_id: &str) -> Option<&OutputConfig> {
        self.output_configs
            .get(component_id)
            .and_then(|configs| configs.iter().find(|c| c.id == output_id))
    }

    /// Get all output configs for a component
    pub fn get_output_configs(&self, component_id: &str) -> Option<&Vec<OutputConfig>> {
        self.output_configs.get(component_id)
    }
}
