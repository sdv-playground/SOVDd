//! Fault/DTC handlers — ISO 17978-3 §7.8

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use sovd_core::{Fault, FaultFilter, FaultSeverity};

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Serialize)]
pub struct FaultsResponse {
    pub items: Vec<FaultInfoResponse>,
    pub total_count: usize,
}

/// Spec §7.8 Table 61 (`Fault`).  Wire fields:
///
///   code (M), fault_name (M), severity 1..4 (O), status (C),
///   scope (O), display_code (O), symptom (C),
///   fault_translation_id (O), symptom_translation_id (O).
///
/// The non-spec extras `id`, `category`, `active` are gone — clients
/// derive "active" from `status.testFailed`, the URL path carries the
/// id, and `category` was a UDS-internal helper that doesn't belong
/// on the wire.
#[derive(Serialize)]
pub struct FaultInfoResponse {
    pub code: String,
    pub fault_name: String,
    pub severity: FaultSeverity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symptom: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fault_translation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symptom_translation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<serde_json::Value>,
    pub href: String,
}

#[derive(Serialize)]
pub struct ClearFaultsResponse {
    pub success: bool,
    pub cleared_count: u32,
    pub message: String,
}

/// Query: spec uses integer severity (1..4).  Filter is exact-match.
#[derive(Deserialize, Default)]
pub struct FaultFilterQuery {
    pub severity: Option<u8>,
    pub category: Option<String>,
    pub active_only: Option<bool>,
    pub limit: Option<usize>,
}

impl From<&Fault> for FaultInfoResponse {
    fn from(fault: &Fault) -> Self {
        Self {
            code: fault.code.clone(),
            fault_name: fault.message.clone(),
            severity: fault.severity,
            scope: None,
            display_code: None,
            symptom: None,
            fault_translation_id: None,
            symptom_translation_id: None,
            status: fault.status.clone(),
            href: fault.href.clone(),
        }
    }
}

/// GET /vehicle/v1/components/:component_id/faults
/// List all faults
pub async fn list_faults(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
    Query(query): Query<FaultFilterQuery>,
) -> Result<Json<FaultsResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;

    let filter = if query.severity.is_some()
        || query.category.is_some()
        || query.active_only.is_some()
        || query.limit.is_some()
    {
        Some(FaultFilter {
            severity: query.severity.map(FaultSeverity::from),
            category: query.category,
            active_only: query.active_only,
            limit: query.limit,
            ..Default::default()
        })
    } else {
        None
    };

    let result = backend.get_faults(filter.as_ref()).await?;
    let total_count = result.faults.len();

    let items: Vec<FaultInfoResponse> = result.faults.iter().map(FaultInfoResponse::from).collect();

    Ok(Json(FaultsResponse { items, total_count }))
}

/// GET /vehicle/v1/components/:component_id/faults/:fault_id
/// Get detailed fault information
pub async fn get_fault(
    State(state): State<AppState>,
    Path((component_id, fault_id)): Path<(String, String)>,
) -> Result<Json<FaultInfoResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;
    let fault = backend.get_fault_detail(&fault_id).await?;

    Ok(Json(FaultInfoResponse::from(&fault)))
}

/// DELETE /vehicle/v1/components/:component_id/faults
///
/// Spec mandates 204 No Content for DELETE on a collection (no body).
/// `ClearFaultsResponse` kept in the codebase for the typed-client
/// shape but no longer serialized to the wire.
pub async fn clear_faults(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let backend = state.get_backend(&component_id)?;
    let _ = backend.clear_faults(None).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// DELETE /vehicle/v1/components/:component_id/faults/:fault_id
///
/// Spec §7.8 fault.delete — clear a single DTC.  UDS 0x14
/// (ClearDiagnosticInformation) takes a 3-byte groupOfDTC; we parse
/// the path's `fault_id` as hex and pass it through.  We deliberately
/// refuse the clear-all sentinel (`0xFFFFFF`) here — that belongs to
/// `DELETE /faults`, not the per-fault path — and unparseable ids get
/// 501 rather than silently wiping the whole DTC store.
pub async fn delete_fault(
    State(state): State<AppState>,
    Path((component_id, fault_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let backend = state.get_backend(&component_id)?;
    let code = u32::from_str_radix(fault_id.trim_start_matches("0x"), 16).map_err(|_| {
        ApiError::NotImplemented(format!(
            "single-DTC delete requires a hex fault id (got {fault_id:?})"
        ))
    })?;
    if code == 0xFF_FFFF {
        return Err(ApiError::BadRequest(
            "clear-all not allowed on single-fault path; use DELETE /faults".into(),
        ));
    }
    let _ = backend.clear_faults(Some(code)).await?;
    Ok(StatusCode::NO_CONTENT)
}
