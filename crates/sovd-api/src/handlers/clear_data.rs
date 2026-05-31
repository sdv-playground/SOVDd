//! `clear-data` collection — ISO 17978-3 §7.13.
//!
//! Lets the client wipe temporary data buckets on an entity:
//! cached-data, learned-data (adaptive values), or client-defined
//! resources.  Stub implementation today — returns an empty list of
//! supported types and 501 on any actual wipe.  Real wiring per
//! backend is tracked in the migration plan.

use axum::extract::{Path, State};
use axum::Json;
use serde::Serialize;

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct ClearDataTypesResponse {
    /// Supported clear-data action ids (e.g. `cached-data`,
    /// `learned-data`, `client-defined-resources`).  Empty until a
    /// backend wires it.
    pub items: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ClearDataStatusResponse {
    /// Most recently issued clear-data status — `idle` when no
    /// action is in flight.
    pub status: String,
}

/// GET /vehicle/v1/components/:component_id/clear-data — list supported types.
pub async fn list_clear_data_types(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<Json<ClearDataTypesResponse>, ApiError> {
    let _ = state.get_backend(&component_id)?;
    Ok(Json(ClearDataTypesResponse { items: Vec::new() }))
}

/// GET /vehicle/v1/components/:component_id/clear-data/status — current status.
pub async fn clear_data_status(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<Json<ClearDataStatusResponse>, ApiError> {
    let _ = state.get_backend(&component_id)?;
    Ok(Json(ClearDataStatusResponse {
        status: "idle".to_string(),
    }))
}

/// PUT /vehicle/v1/components/:component_id/clear-data/:action — not yet wired.
pub async fn clear_data_action(
    Path((_component_id, action)): Path<(String, String)>,
) -> Result<(), ApiError> {
    Err(ApiError::NotImplemented(format!(
        "clear-data action '{}' not yet supported by this server",
        action
    )))
}
