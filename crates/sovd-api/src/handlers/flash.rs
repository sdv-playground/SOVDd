//! Flash transfer handlers for async flash flow
//!
//! Provides endpoints for starting, monitoring, and finalizing flash transfers.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use sovd_core::FlashStatus;

use crate::error::ApiError;
use crate::state::AppState;

/// Request to start a flash transfer
#[derive(Debug, Deserialize)]
pub struct StartFlashRequest {
    /// ID of the file/package to flash
    pub file_id: String,
}

/// Response for starting a flash transfer
#[derive(Debug, Serialize)]
pub struct StartFlashResponse {
    /// Transfer ID for monitoring progress
    pub transfer_id: String,
    /// URL to check transfer status
    pub status_url: String,
    /// URL to finalize the transfer
    pub finalize_url: String,
}

/// Response for listing transfers
#[derive(Debug, Serialize)]
pub struct ListTransfersResponse {
    /// List of flash transfers
    pub transfers: Vec<TransferInfo>,
}

/// Transfer information with HATEOAS links
#[derive(Debug, Serialize)]
pub struct TransferInfo {
    /// Flash status
    #[serde(flatten)]
    pub status: FlashStatus,
    /// URL to this transfer
    pub href: String,
}

/// POST /vehicle/v1/components/:component_id/flash/transfer
/// Start a flash transfer
pub async fn start_flash(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
    Json(request): Json<StartFlashRequest>,
) -> Result<(StatusCode, Json<StartFlashResponse>), ApiError> {
    let backend = state.get_backend(&component_id)?;

    let transfer_id = backend
        .start_flash(&request.file_id)
        .await
        .map_err(ApiError::from)?;

    tracing::info!(
        component_id = %component_id,
        file_id = %request.file_id,
        transfer_id = %transfer_id,
        "Flash transfer started"
    );

    let response = StartFlashResponse {
        transfer_id: transfer_id.clone(),
        status_url: format!(
            "/vehicle/v1/components/{}/flash/transfer/{}",
            component_id, transfer_id
        ),
        finalize_url: format!("/vehicle/v1/components/{}/flash/transferexit", component_id),
    };

    Ok((StatusCode::ACCEPTED, Json(response)))
}

/// GET /vehicle/v1/components/:component_id/flash/transfer
/// List all flash transfers
pub async fn list_transfers(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<Json<ListTransfersResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;

    let transfers = backend
        .list_flash_transfers()
        .await
        .map_err(ApiError::from)?;

    let transfers: Vec<TransferInfo> = transfers
        .into_iter()
        .map(|status| {
            let transfer_id = status.transfer_id.clone();
            TransferInfo {
                status,
                href: format!(
                    "/vehicle/v1/components/{}/flash/transfer/{}",
                    component_id, transfer_id
                ),
            }
        })
        .collect();

    Ok(Json(ListTransfersResponse { transfers }))
}

/// GET /vehicle/v1/components/:component_id/flash/transfer/:transfer_id
/// Get status of a flash transfer
pub async fn get_transfer(
    State(state): State<AppState>,
    Path((component_id, transfer_id)): Path<(String, String)>,
) -> Result<Json<TransferInfo>, ApiError> {
    let backend = state.get_backend(&component_id)?;

    let status = backend
        .get_flash_status(&transfer_id)
        .await
        .map_err(ApiError::from)?;

    Ok(Json(TransferInfo {
        status,
        href: format!(
            "/vehicle/v1/components/{}/flash/transfer/{}",
            component_id, transfer_id
        ),
    }))
}

/// DELETE /vehicle/v1/components/:component_id/flash/transfer/:transfer_id
/// Abort a flash transfer
pub async fn abort_transfer(
    State(state): State<AppState>,
    Path((component_id, transfer_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let backend = state.get_backend(&component_id)?;

    backend
        .abort_flash(&transfer_id)
        .await
        .map_err(ApiError::from)?;

    tracing::warn!(
        component_id = %component_id,
        transfer_id = %transfer_id,
        "Flash transfer aborted"
    );

    Ok(StatusCode::NO_CONTENT)
}

/// PUT /vehicle/v1/components/:component_id/flash/transferexit
/// Finalize a flash transfer (UDS 0x37)
pub async fn transfer_exit(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<Json<TransferExitResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;

    backend.finalize_flash().await.map_err(ApiError::from)?;

    tracing::info!(
        component_id = %component_id,
        "Flash transfer finalized"
    );

    Ok(Json(TransferExitResponse {
        success: true,
        message: "Transfer exit completed successfully".to_string(),
    }))
}

/// Response for transfer exit
#[derive(Debug, Serialize)]
pub struct TransferExitResponse {
    /// Whether the operation succeeded
    pub success: bool,
    /// Status message
    pub message: String,
}

/// Response for commit/rollback operations
#[derive(Debug, Serialize)]
pub struct CommitRollbackResponse {
    /// Whether the operation succeeded
    pub success: bool,
    /// Status message
    pub message: String,
}

/// Response for activation state query
#[derive(Debug, Serialize)]
pub struct ActivationStateResponse {
    /// Whether this ECU supports rollback
    pub supports_rollback: bool,
    /// Current activation state
    pub state: String,
    /// Currently active firmware version
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_version: Option<String>,
    /// Previous firmware version (available for rollback)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_version: Option<String>,
}

/// POST /vehicle/v1/components/:component_id/flash/commit
/// Commit activated firmware (makes it permanent)
pub async fn commit_flash(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<Json<CommitRollbackResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;

    backend.commit_flash().await.map_err(ApiError::from)?;

    tracing::info!(
        component_id = %component_id,
        "Firmware committed"
    );

    Ok(Json(CommitRollbackResponse {
        success: true,
        message: "Firmware committed successfully".to_string(),
    }))
}

/// POST /vehicle/v1/components/:component_id/flash/rollback
/// Rollback activated firmware to previous version
pub async fn rollback_flash(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<Json<CommitRollbackResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;

    backend.rollback_flash().await.map_err(ApiError::from)?;

    tracing::info!(
        component_id = %component_id,
        "Firmware rolled back"
    );

    Ok(Json(CommitRollbackResponse {
        success: true,
        message: "Firmware rolled back successfully".to_string(),
    }))
}

/// GET /vehicle/v1/components/:component_id/flash/activation
/// Get firmware activation state
pub async fn get_activation_state(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<Json<ActivationStateResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;

    let activation = backend
        .get_activation_state()
        .await
        .map_err(ApiError::from)?;

    Ok(Json(ActivationStateResponse {
        supports_rollback: activation.supports_rollback,
        state: activation.state.to_string(),
        active_version: activation.active_version,
        previous_version: activation.previous_version,
    }))
}
