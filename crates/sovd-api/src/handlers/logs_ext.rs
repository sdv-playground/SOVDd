//! `logs/entries` + `logs/config` sub-resources — ISO 17978-3 §7.21.
//!
//! `logs` (the parent collection) is served by `handlers/logs.rs`.
//! This module adds the spec-mandated sub-resources:
//!
//! * `GET .../logs/entries`   — list of log entries with links to bulk-data
//! * `GET .../logs/config`    — current log configuration
//! * `PUT .../logs/config`    — set log configuration (204 on accept)
//! * `DELETE .../logs/config` — reset to default (204)
//!
//! Config persists in-memory in `AppState::log_config` (lost on
//! restart).  Backend wiring (per-component logger reconfigure) is
//! a TODO — the config is stored and returned faithfully, but no
//! downstream component reads it yet.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct LogEntriesResponse {
    /// Log entries — empty until a backend implements the entry list.
    pub items: Vec<LogEntryRef>,
}

#[derive(Debug, Serialize)]
pub struct LogEntryRef {
    pub id: String,
    /// `bulk-data` URL where the log file can be downloaded.
    pub href: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogConfig {
    /// Log context — e.g. `"rfc5424"` or `"autosar-dlt"`.
    #[serde(default = "default_context")]
    pub context: String,
    /// Minimum severity threshold (1..4 per spec §7.8 fault severity).
    #[serde(default = "default_min_severity")]
    pub min_severity: u8,
    /// Optional source filter (e.g. component or app id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

impl LogConfig {
    fn defaults() -> Self {
        Self {
            context: default_context(),
            min_severity: default_min_severity(),
            source: None,
        }
    }
}

fn default_context() -> String {
    "rfc5424".to_string()
}

fn default_min_severity() -> u8 {
    3 // WARN
}

/// GET /vehicle/v1/components/:component_id/logs/entries
///
/// §7.21: each entry LINKS TO its log file, which is retrieved via bulk-data
/// (C-121). So we surface the backend's `logs` bulk-data category as entries
/// whose `href` points at the `/bulk-data/logs/{id}` download. Empty (not an
/// error) when the backend exposes no `logs` category.
pub async fn list_log_entries(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<Json<LogEntriesResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;
    let base = format!("/vehicle/v1/components/{component_id}/bulk-data/logs");
    // list_bulk_data defaults to EntityNotFound for an unknown category; a
    // backend without a `logs` category (or without bulk-data at all) simply has
    // no entries — degrade to empty rather than surfacing a 404 on /logs/entries.
    let items = match backend
        .list_bulk_data("logs", &sovd_core::BulkDataFilter::default())
        .await
    {
        Ok(list) => list
            .into_iter()
            .map(|it| LogEntryRef {
                href: format!("{base}/{}", it.id),
                id: it.id,
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    Ok(Json(LogEntriesResponse { items }))
}

/// GET /vehicle/v1/components/:component_id/logs/config
pub async fn get_log_config(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<Json<LogConfig>, ApiError> {
    let _ = state.get_backend(&component_id)?;
    let stored = state.log_config.0.lock().get(&component_id).cloned();
    match stored {
        Some(v) => {
            let cfg: LogConfig = serde_json::from_value(v)
                .map_err(|e| ApiError::Internal(format!("corrupted log_config state: {e}")))?;
            Ok(Json(cfg))
        }
        None => Ok(Json(LogConfig::defaults())),
    }
}

/// PUT /vehicle/v1/components/:component_id/logs/config
pub async fn put_log_config(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
    Json(config): Json<LogConfig>,
) -> Result<StatusCode, ApiError> {
    // C-121 (ISO 17978-3 §7.21): the log `context` names the entry format and
    // must be a known type. Reject anything else with 400 before touching state.
    // (Wire values stay lowercase to match `default_context()`; the spec's
    // canonical ContextType enum spells these RFC5424 / AUTOSAR_DLT.)
    if !matches!(config.context.as_str(), "rfc5424" | "autosar-dlt") {
        return Err(ApiError::BadRequest(format!(
            "unsupported log context {:?}; allowed: \"rfc5424\", \"autosar-dlt\"",
            config.context
        )));
    }
    let _ = state.get_backend(&component_id)?;
    let value = serde_json::to_value(&config)
        .map_err(|e| ApiError::Internal(format!("log_config serde: {e}")))?;
    state.log_config.0.lock().insert(component_id, value);
    Ok(StatusCode::NO_CONTENT)
}

/// DELETE /vehicle/v1/components/:component_id/logs/config
pub async fn reset_log_config(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let _ = state.get_backend(&component_id)?;
    state.log_config.0.lock().remove(&component_id);
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovd_core::{
        BackendError, BackendResult, Capabilities, DataValue, DiagnosticBackend, EntityInfo,
        FaultFilter, FaultsResult, OperationExecution, OperationInfo, ParameterInfo,
    };
    use std::collections::HashMap;
    use std::sync::Arc;

    /// Minimal backend so `get_backend` resolves on the happy path; the methods
    /// here are the trait's only required stubs (the rest default to
    /// `NotSupported`).
    struct StubBackend {
        info: EntityInfo,
        caps: Capabilities,
    }

    impl StubBackend {
        fn new(id: &str) -> Self {
            Self {
                info: EntityInfo {
                    id: id.to_string(),
                    name: id.to_string(),
                    entity_type: "ecu".to_string(),
                    description: None,
                    href: format!("/vehicle/v1/components/{id}"),
                    status: Some("online".to_string()),
                },
                caps: Capabilities::default(),
            }
        }
    }

    #[async_trait::async_trait]
    impl DiagnosticBackend for StubBackend {
        fn entity_info(&self) -> &EntityInfo {
            &self.info
        }
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
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
        async fn start_operation(
            &self,
            op: &str,
            _params: &[u8],
        ) -> BackendResult<OperationExecution> {
            Err(BackendError::OperationNotFound(op.to_string()))
        }
    }

    fn state_with_ecu() -> AppState {
        let mut backends: HashMap<String, Arc<dyn DiagnosticBackend>> = HashMap::new();
        backends.insert("ecu".to_string(), Arc::new(StubBackend::new("ecu")));
        AppState::new(backends)
    }

    fn config_with_context(context: &str) -> LogConfig {
        LogConfig {
            context: context.to_string(),
            min_severity: default_min_severity(),
            source: None,
        }
    }

    #[tokio::test]
    async fn put_log_config_accepts_known_contexts() {
        // Both spec-defined context types (C-121) are accepted → 204.
        for ctx in ["rfc5424", "autosar-dlt"] {
            let status = put_log_config(
                State(state_with_ecu()),
                Path("ecu".to_string()),
                Json(config_with_context(ctx)),
            )
            .await
            .expect("known context is accepted");
            assert_eq!(status, StatusCode::NO_CONTENT, "context {ctx:?}");
        }
    }

    #[tokio::test]
    async fn put_log_config_rejects_unknown_context() {
        // An out-of-enum context (C-121) is a 400 that names the bad value.
        let err = put_log_config(
            State(state_with_ecu()),
            Path("ecu".to_string()),
            Json(config_with_context("syslog")),
        )
        .await
        .expect_err("unknown context is rejected");
        match err {
            ApiError::BadRequest(msg) => {
                assert!(msg.contains("syslog"), "message names the bad value: {msg}");
            }
            other => panic!("expected 400 BadRequest, got {other:?}"),
        }
    }
}
