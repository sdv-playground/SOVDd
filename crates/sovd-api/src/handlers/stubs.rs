//! P1 stub handlers for spec collections that don't yet have a
//! backend.  Each module here exposes a spec-correct route surface
//! so 3rd-party gateways can navigate the resource tree without
//! 404s, while making it obvious that nothing dispatches downstream
//! yet.
//!
//! Common shape:
//!   * `GET /…` on a collection → empty `{items: []}`
//!   * `GET /…/{id}` → 404 (collection is empty)
//!   * `POST /…` → 501 `sovd-server-misconfigured` (no backend wired)
//!   * `PUT /…/{id}` → 204 (idempotent accept — no state to store)
//!   * `DELETE /…/{id}` → 204
//!
//! Spec sections covered: §7.11 triggers, §7.12 configurations,
//! §7.15 scripts, §7.17 locks, §7.22 communication-logs, plus the
//! Table 9 `data-categories` / `data-groups` enumeration collections.
//! Sub-paths inside `modes/` and `faults/` are added to those existing
//! handlers rather than here.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::state::AppState;

/// Empty-collection response used by most stubs.
#[derive(Debug, Serialize)]
pub struct EmptyListResponse<T: Serialize> {
    pub items: Vec<T>,
}

impl<T: Serialize> Default for EmptyListResponse<T> {
    fn default() -> Self {
        Self { items: Vec::new() }
    }
}

fn require_component(state: &AppState, component_id: &str) -> Result<(), ApiError> {
    state.get_backend(component_id).map(|_| ())
}

// =============================================================================
// configurations — §7.12
// =============================================================================

#[derive(Debug, Serialize)]
pub struct ConfigurationSummary {
    pub id: String,
    pub href: String,
}

pub async fn list_configurations(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<Json<EmptyListResponse<ConfigurationSummary>>, ApiError> {
    require_component(&state, &component_id)?;
    Ok(Json(EmptyListResponse::default()))
}

pub async fn read_configuration(
    State(_state): State<AppState>,
    Path((_component_id, configuration_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Err(ApiError::NotFound(format!(
        "Configuration not found: {}",
        configuration_id
    )))
}

pub async fn create_configuration(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
    Json(_body): Json<serde_json::Value>,
) -> Result<StatusCode, ApiError> {
    require_component(&state, &component_id)?;
    Err(ApiError::NotImplemented(
        "configurations.create not yet wired to a backend".into(),
    ))
}

pub async fn write_configuration(
    State(state): State<AppState>,
    Path((component_id, _configuration_id)): Path<(String, String)>,
    Json(_body): Json<serde_json::Value>,
) -> Result<StatusCode, ApiError> {
    require_component(&state, &component_id)?;
    Err(ApiError::NotImplemented(
        "configurations.update not yet wired to a backend".into(),
    ))
}

pub async fn delete_configuration_one(
    State(state): State<AppState>,
    Path((component_id, _configuration_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    require_component(&state, &component_id)?;
    Err(ApiError::NotImplemented(
        "configurations.reset-one not yet wired to a backend".into(),
    ))
}

pub async fn reset_configurations(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_component(&state, &component_id)?;
    Err(ApiError::NotImplemented(
        "configurations.reset not yet wired to a backend".into(),
    ))
}

// =============================================================================
// locks — §7.17
// =============================================================================

#[derive(Debug, Serialize)]
pub struct LockSummary {
    pub id: String,
    pub href: String,
}

#[derive(Debug, Deserialize)]
pub struct AcquireLockRequest {
    #[serde(default)]
    pub break_lock: bool,
    #[serde(default)]
    pub scopes: Vec<String>,
}

pub async fn list_locks(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<Json<EmptyListResponse<LockSummary>>, ApiError> {
    require_component(&state, &component_id)?;
    Ok(Json(EmptyListResponse::default()))
}

pub async fn acquire_lock(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
    Json(_req): Json<AcquireLockRequest>,
) -> Result<StatusCode, ApiError> {
    require_component(&state, &component_id)?;
    // Spec 201 on create when a lock subsystem exists.  Until then,
    // be honest: 501 sovd-server-misconfigured.
    Err(ApiError::NotImplemented(
        "locks.acquire not yet wired to a lock subsystem".into(),
    ))
}

pub async fn read_lock(
    Path((_component_id, lock_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Err(ApiError::NotFound(format!("Lock not found: {}", lock_id)))
}

pub async fn extend_or_break_lock(
    State(state): State<AppState>,
    Path((component_id, _lock_id)): Path<(String, String)>,
    Json(_req): Json<AcquireLockRequest>,
) -> Result<StatusCode, ApiError> {
    require_component(&state, &component_id)?;
    Err(ApiError::NotImplemented(
        "locks.extend/break not yet wired to a lock subsystem".into(),
    ))
}

pub async fn release_lock(
    State(state): State<AppState>,
    Path((component_id, _lock_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    require_component(&state, &component_id)?;
    Ok(StatusCode::NO_CONTENT)
}

// =============================================================================
// triggers — §7.11
// =============================================================================

#[derive(Debug, Serialize)]
pub struct TriggerSummary {
    pub id: String,
    pub href: String,
}

pub async fn list_triggers(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<Json<EmptyListResponse<TriggerSummary>>, ApiError> {
    require_component(&state, &component_id)?;
    Ok(Json(EmptyListResponse::default()))
}

pub async fn create_trigger(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
    Json(_req): Json<serde_json::Value>,
) -> Result<StatusCode, ApiError> {
    require_component(&state, &component_id)?;
    Err(ApiError::NotImplemented(
        "triggers.create not yet wired to a trigger subsystem".into(),
    ))
}

pub async fn read_trigger(
    Path((_component_id, trigger_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Err(ApiError::NotFound(format!(
        "Trigger not found: {}",
        trigger_id
    )))
}

pub async fn delete_trigger(
    State(state): State<AppState>,
    Path((component_id, _trigger_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    require_component(&state, &component_id)?;
    Ok(StatusCode::NO_CONTENT)
}

// =============================================================================
// communication-logs — §7.22
// =============================================================================

#[derive(Debug, Serialize)]
pub struct CommunicationLogSummary {
    pub id: String,
    pub href: String,
}

pub async fn list_communication_logs(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<Json<EmptyListResponse<CommunicationLogSummary>>, ApiError> {
    require_component(&state, &component_id)?;
    Ok(Json(EmptyListResponse::default()))
}

pub async fn create_communication_log(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
    Json(_req): Json<serde_json::Value>,
) -> Result<StatusCode, ApiError> {
    require_component(&state, &component_id)?;
    Err(ApiError::NotImplemented(
        "communication-logs not yet wired (dev/QA only per spec)".into(),
    ))
}

pub async fn read_communication_log(
    Path((_component_id, log_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Err(ApiError::NotFound(format!(
        "Communication log not found: {}",
        log_id
    )))
}

pub async fn control_communication_log(
    State(state): State<AppState>,
    Path((component_id, _log_id)): Path<(String, String)>,
    Json(_req): Json<serde_json::Value>,
) -> Result<StatusCode, ApiError> {
    require_component(&state, &component_id)?;
    Err(ApiError::NotImplemented(
        "communication-logs.control not yet wired".into(),
    ))
}

pub async fn delete_communication_log(
    State(state): State<AppState>,
    Path((component_id, _log_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    require_component(&state, &component_id)?;
    Ok(StatusCode::NO_CONTENT)
}

// =============================================================================
// scripts — §7.15
// =============================================================================

#[derive(Debug, Serialize)]
pub struct ScriptSummary {
    pub id: String,
    pub href: String,
}

pub async fn list_scripts(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<Json<EmptyListResponse<ScriptSummary>>, ApiError> {
    require_component(&state, &component_id)?;
    Ok(Json(EmptyListResponse::default()))
}

pub async fn read_script(
    Path((_component_id, script_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Err(ApiError::NotFound(format!(
        "Script not found: {}",
        script_id
    )))
}

pub async fn execute_script(
    State(state): State<AppState>,
    Path((component_id, _script_id)): Path<(String, String)>,
    Json(_req): Json<serde_json::Value>,
) -> Result<StatusCode, ApiError> {
    require_component(&state, &component_id)?;
    Err(ApiError::NotImplemented(
        "scripts.execute not yet wired".into(),
    ))
}

// =============================================================================
// data-categories + data-groups — Table 9
// =============================================================================

#[derive(Debug, Serialize)]
pub struct DataCategory {
    pub id: String,
}

#[derive(Debug, Serialize)]
pub struct DataGroup {
    pub id: String,
}

pub async fn list_data_categories(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<Json<EmptyListResponse<DataCategory>>, ApiError> {
    require_component(&state, &component_id)?;
    Ok(Json(EmptyListResponse::default()))
}

pub async fn list_data_groups(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<Json<EmptyListResponse<DataGroup>>, ApiError> {
    require_component(&state, &component_id)?;
    Ok(Json(EmptyListResponse::default()))
}

// =============================================================================
// modes/communication-control + modes/dtc-setting — §7.16
// =============================================================================

// modes/communication-control and modes/dtc-setting return 501 on both
// verbs until the backend exposes UDS 0x28 / 0x85.  Returning a
// fabricated GET value would be worse than 501 — conformance checkers
// would treat the cached "normal"/"on" as a real ECU read.

pub async fn get_comm_control_mode(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_component(&state, &component_id)?;
    Err(ApiError::NotImplemented(
        "modes/communication-control not yet wired (UDS 0x28)".into(),
    ))
}

pub async fn put_comm_control_mode(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
    Json(_req): Json<serde_json::Value>,
) -> Result<StatusCode, ApiError> {
    require_component(&state, &component_id)?;
    Err(ApiError::NotImplemented(
        "modes/communication-control not yet wired (UDS 0x28)".into(),
    ))
}

pub async fn get_dtc_setting_mode(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_component(&state, &component_id)?;
    Err(ApiError::NotImplemented(
        "modes/dtc-setting not yet wired (UDS 0x85)".into(),
    ))
}

pub async fn put_dtc_setting_mode(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
    Json(_req): Json<serde_json::Value>,
) -> Result<StatusCode, ApiError> {
    require_component(&state, &component_id)?;
    Err(ApiError::NotImplemented(
        "modes/dtc-setting not yet wired (UDS 0x85)".into(),
    ))
}
