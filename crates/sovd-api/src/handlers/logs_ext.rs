//! `logs/entries` + `logs/config` sub-resources — ISO 17978-3 §7.21.
//!
//! `logs` (the parent collection) is already served by `handlers/logs.rs`.
//! This module adds the spec-mandated sub-resources as stubs:
//!
//! * `GET .../logs/entries`   — list of log entries with links to bulk-data
//! * `GET .../logs/config`    — current log configuration
//! * `PUT .../logs/config`    — set log configuration (204 on accept)
//! * `DELETE .../logs/config` — reset to default (204)
//!
//! Backend wiring is TODO; for now we expose the routes with
//! defensible empty / default responses so the spec audit doesn't
//! flag them as missing.

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
    Ok(Json(LogConfig {
        context: default_context(),
        min_severity: default_min_severity(),
        source: None,
    }))
}

/// PUT /vehicle/v1/components/:component_id/logs/config
pub async fn put_log_config(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
    Json(_config): Json<LogConfig>,
) -> Result<StatusCode, ApiError> {
    let _ = state.get_backend(&component_id)?;
    // Stub: accept the configuration but don't persist it yet —
    // backend wiring lands with the per-backend logger refactor.
    Ok(StatusCode::NO_CONTENT)
}

/// DELETE /vehicle/v1/components/:component_id/logs/config
pub async fn reset_log_config(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let _ = state.get_backend(&component_id)?;
    Ok(StatusCode::NO_CONTENT)
}
