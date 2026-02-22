//! Fault/DTC handlers

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use sovd_core::{Fault, FaultFilter, FaultSeverity};

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Serialize)]
pub struct FaultsResponse {
    pub items: Vec<FaultInfoResponse>,
    pub total_count: usize,
    /// Status availability mask (UDS-specific)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_availability_mask: Option<u8>,
}

#[derive(Serialize)]
pub struct FaultInfoResponse {
    pub id: String,
    pub dtc_code: String,
    pub severity: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    pub active: bool,
    /// DTC status information
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

#[derive(Deserialize, Default)]
pub struct FaultFilterQuery {
    pub severity: Option<String>,
    pub category: Option<String>,
    pub active_only: Option<bool>,
    pub limit: Option<usize>,
}

impl From<&Fault> for FaultInfoResponse {
    fn from(fault: &Fault) -> Self {
        Self {
            id: fault.id.clone(),
            dtc_code: fault.code.clone(),
            severity: match fault.severity {
                FaultSeverity::Info => "info",
                FaultSeverity::Warning => "warning",
                FaultSeverity::Error => "error",
                FaultSeverity::Critical => "critical",
            }
            .to_string(),
            message: fault.message.clone(),
            category: fault.category.clone(),
            active: fault.active,
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
            severity: query.severity.and_then(|s| match s.as_str() {
                "info" => Some(FaultSeverity::Info),
                "warning" => Some(FaultSeverity::Warning),
                "error" => Some(FaultSeverity::Error),
                "critical" => Some(FaultSeverity::Critical),
                _ => None,
            }),
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

    Ok(Json(FaultsResponse {
        items,
        total_count,
        status_availability_mask: result.status_availability_mask,
    }))
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
/// Clear all faults
pub async fn clear_faults(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<Json<ClearFaultsResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;
    let result = backend.clear_faults(None).await?;

    Ok(Json(ClearFaultsResponse {
        success: result.success,
        cleared_count: result.cleared_count,
        message: result.message,
    }))
}

/// GET /vehicle/v1/components/:component_id/dtcs
/// List active (currently failing) DTCs only
pub async fn list_active_dtcs(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<Json<FaultsResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;

    // Filter for active faults only (test_failed = true, status bit 0x01)
    let filter = Some(FaultFilter {
        active_only: Some(true),
        ..Default::default()
    });

    let result = backend.get_faults(filter.as_ref()).await?;
    let total_count = result.faults.len();

    let items: Vec<FaultInfoResponse> = result.faults.iter().map(FaultInfoResponse::from).collect();

    Ok(Json(FaultsResponse {
        items,
        total_count,
        status_availability_mask: result.status_availability_mask,
    }))
}
