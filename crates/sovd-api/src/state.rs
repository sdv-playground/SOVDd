//! Application state for the SOVD API

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;

use parking_lot::Mutex;
use sovd_conv::DidStore;
use sovd_core::{DiagnosticBackend, OperationExecution};
use sovd_uds::config::OutputConfig;

use crate::auth::{AuthContext, Authorizer};
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
    /// Legacy `state` field for the /executions wire (kept for the
    /// deprecation window).  New callers should consult `phase` +
    /// `status` (ISO 17978-3 §7.18.7 / Table 270).
    pub state: UpdateState,
    /// ISO 17978-3 §7.18.7 lifecycle phase. `prepare` and `execute`
    /// run in sequence; status transitions within each phase.
    pub phase: Phase,
    /// ISO 17978-3 §7.18.7 status within the current phase.
    pub status: Status,
    /// Optional progress 0..100, populated by long-running tasks
    /// (prepare's bulk-data verify loop, execute's bank installation).
    pub progress: Option<u8>,
    /// Optional free-form description of the current step; intended
    /// for UI ("validating manifest", "writing bank_b/kernel", ...).
    pub step: Option<String>,
    /// Populated only when `status == Failed` per Table 270.  Carries
    /// the GenericError the originating task hit.
    pub error: Option<UpdateError>,
    /// Vendor-extension fine-grained execute-phase substate, used
    /// when control mode is orchestrated.  See
    /// `tasks/spec-aligned-updates-wire.md` §2.2.
    pub substate: Option<&'static str>,
    /// Component's declared `ResetKind`, captured from
    /// `get_activation_state()` once at register time (the component is
    /// idle then, so the call is cheap — never re-read per status-poll).
    /// Surfaced on the wire as `x-sumo-reset-kind` so the campaign
    /// orchestrator can coalesce RT/host-os ECU resets. `None` when the
    /// backend doesn't report activation state at register.
    pub reset_kind: Option<sovd_core::ResetKind>,
    /// Backend's transfer_id, populated once `start_flash` runs.
    pub transfer_id: Option<String>,
    /// Abort handle for the in-flight prepare/execute task, so
    /// `DELETE /updates/{id}` can cancel it.  `None` when no task
    /// is running; cleared when a task completes.
    pub task_handle: Option<tokio::task::AbortHandle>,
    /// Watch channel for the orchestrator's trial verdict.  Sender
    /// is held here so `PUT /x-sumo-commit` / `/x-sumo-rollback` can
    /// post to it.  `None` in standard mode (Phase A behaviour);
    /// `Some` when `PUT /execute?x-sumo-control=orchestrated` runs
    /// and the task pauses at `substate=awaiting-verdict`.
    pub verdict_tx: Option<tokio::sync::watch::Sender<Verdict>>,
}

/// Subset of `GenericError` (sovd-core) carried in `UpdatesEntry.error`.
/// Wire shape matches Table 16's `GenericError`.
#[derive(Clone, Debug, serde::Serialize)]
pub struct UpdateError {
    pub error_code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
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

/// ISO 17978-3 §7.18.7 Table 271 `Phase` enum.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// Default — no phase started yet. Wire emits this until the
    /// first `PUT /prepare` or `PUT /automated`.
    #[default]
    Prepare,
    Execute,
}

impl Phase {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Prepare => "prepare",
            Self::Execute => "execute",
        }
    }
}

/// ISO 17978-3 §7.18.7 Table 273 `Status` enum.  Same four values for
/// both phases; semantics shift per phase per Table 273.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    /// Phase hasn't started yet (initial state after register or
    /// transition into a new phase).
    #[default]
    Pending,
    InProgress,
    Failed,
    Completed,
}

/// Orchestrator's trial-verdict signal.  Used only when the execute
/// phase runs in `x-sumo-control=orchestrated` mode (Phase B).  Sent
/// over the per-entry watch channel; the paused execute task wakes
/// when the verdict transitions from `Pending`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Verdict {
    /// No verdict yet — the task remains paused.
    #[default]
    Pending,
    /// Orchestrator confirmed the trial succeeded; commit_flash.
    Commit,
    /// Orchestrator rejected the trial; rollback_flash.
    Rollback,
}

impl Status {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "inProgress",
            Self::Failed => "failed",
            Self::Completed => "completed",
        }
    }
}

/// Per-server tuning for the `/updates` collection.  The orchestrated
/// watchdog gates how long the execute task will pause at
/// `substate=awaiting-verdict` before auto-rolling-back; the default
/// matches the upper bound a real OEM workshop tester typically needs
/// to complete a multi-ECU health check.
#[derive(Clone, Debug)]
pub struct UpdatesConfig {
    /// Time the execute task will wait for an `x-sumo-commit` or
    /// `x-sumo-rollback` verdict before timing out.
    pub orchestrated_watchdog: std::time::Duration,
}

impl Default for UpdatesConfig {
    fn default() -> Self {
        Self {
            orchestrated_watchdog: std::time::Duration::from_secs(600),
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
    /// Tunable knobs for the `/updates` lifecycle.
    pub updates_config: Arc<UpdatesConfig>,
    /// Client→SOVDd authentication context (JWT-bearer slice). Defaults to
    /// disabled (open surface); set via [`AppState::with_auth`].
    auth: Arc<dyn Authorizer>,
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
            updates_config: Arc::new(UpdatesConfig::default()),
            auth: Arc::new(AuthContext::default()),
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
            updates_config: Arc::new(UpdatesConfig::default()),
            auth: Arc::new(AuthContext::default()),
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
            updates_config: Arc::new(UpdatesConfig::default()),
            auth: Arc::new(AuthContext::default()),
        }
    }

    /// Override the `/updates` collection's tuning knobs (watchdog,
    /// future limits).  Builder-style consume + return.
    pub fn with_updates_config(mut self, config: UpdatesConfig) -> Self {
        self.updates_config = Arc::new(config);
        self
    }

    /// Attach the client-authentication context (JWT-bearer slice).
    /// Builder-style consume + return.
    pub fn with_auth(mut self, auth: Arc<AuthContext>) -> Self {
        self.auth = auth;
        self
    }

    /// Attach a custom authorizer — the injection seam. An embedder (the
    /// machine-manager layer) provides an HSM-backed / capability-tiered
    /// implementation; standalone SOVDd uses the built-in [`AuthContext`] modes
    /// via [`AppState::with_auth`].
    pub fn with_authorizer(mut self, auth: Arc<dyn Authorizer>) -> Self {
        self.auth = auth;
        self
    }

    /// The authorizer, read by the auth middleware.
    pub fn auth(&self) -> &dyn Authorizer {
        self.auth.as_ref()
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
