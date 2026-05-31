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
pub async fn list_log_entries(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<Json<LogEntriesResponse>, ApiError> {
    let _ = state.get_backend(&component_id)?;
    Ok(Json(LogEntriesResponse { items: Vec::new() }))
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
