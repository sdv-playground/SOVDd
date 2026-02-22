//! Application state for the SOVD API

use std::collections::HashMap;
use std::sync::Arc;

use sovd_conv::DidStore;
use sovd_core::DiagnosticBackend;
use sovd_uds::config::OutputConfig;

use crate::error::ApiError;
use crate::handlers::subscriptions::SubscriptionManager;

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
}

impl AppState {
    /// Create a new AppState with the given backends
    pub fn new(backends: HashMap<String, Arc<dyn DiagnosticBackend>>) -> Self {
        Self {
            backends: Arc::new(backends),
            did_store: Arc::new(DidStore::new()),
            subscription_manager: Arc::new(SubscriptionManager::new()),
            output_configs: Arc::new(HashMap::new()),
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
