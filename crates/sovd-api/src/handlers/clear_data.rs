//! `clear-data` collection — ISO 17978-3 §7.13.
//!
//! Spec actions per Table 9 / §7.13:
//!   * `cached-data`              — wipe cached/computed values
//!   * `learned-data`             — wipe adaptive/learned values
//!   * `client-defined-resources` — wipe what the client previously
//!     created on the entity (e.g. dynamic DIDs, subscriptions)
//!
//! Today the action endpoint returns spec-mandated 202 Accepted and
//! records `running`→`completed` in the in-memory status store; the
//! actual data wipe is a TODO — backend trait wiring lands per-
//! backend later.  Clients that care can poll
//! `GET /clear-data/status` to see the most recent state.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Serialize;

use crate::error::ApiError;
use crate::state::AppState;

const SUPPORTED_TYPES: &[&str] = &["cached-data", "learned-data", "client-defined-resources"];

#[derive(Debug, Serialize)]
pub struct ClearDataTypesResponse {
    /// Supported clear-data action ids per spec §7.13.
    pub items: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ClearDataStatusResponse {
    /// Most recent action status — `idle` when no action has run.
    pub status: String,
}

/// GET /vehicle/v1/components/:component_id/clear-data
pub async fn list_clear_data_types(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<Json<ClearDataTypesResponse>, ApiError> {
    let _ = state.get_backend(&component_id)?;
    Ok(Json(ClearDataTypesResponse {
        items: SUPPORTED_TYPES.iter().map(|s| s.to_string()).collect(),
    }))
}

/// GET /vehicle/v1/components/:component_id/clear-data/status
pub async fn clear_data_status(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<Json<ClearDataStatusResponse>, ApiError> {
    let _ = state.get_backend(&component_id)?;
    let status = state
        .clear_data_status
        .0
        .lock()
        .get(&component_id)
        .cloned()
        .unwrap_or_else(|| "idle".to_string());
    Ok(Json(ClearDataStatusResponse { status }))
}

/// PUT /vehicle/v1/components/:component_id/clear-data/:action
///
/// Spec wants 202 Accepted with the wipe executing asynchronously.
/// We record state transitions in the status store; real backend
/// dispatch is TODO.
pub async fn clear_data_action(
    State(state): State<AppState>,
    Path((component_id, action)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let _ = state.get_backend(&component_id)?;

    if !SUPPORTED_TYPES.iter().any(|t| *t == action) {
        return Err(ApiError::BadRequest(format!(
            "Unsupported clear-data action: {} (supported: {})",
            action,
            SUPPORTED_TYPES.join(", ")
        )));
    }

    // No backend trait method dispatches a real clear-data wipe yet.
    // Recording a fabricated `completed` would make a conformance
    // poller believe the wipe happened.  Be honest: 501.  The action
    // is validated above so unknown actions still 400, and an unknown
    // component still 404s via get_backend.
    tracing::info!(
        component = %component_id,
        action = %action,
        "clear-data: no backend dispatch — returning 501"
    );
    Err(ApiError::NotImplemented(
        "clear-data actions are not yet wired to a backend".into(),
    ))
}
