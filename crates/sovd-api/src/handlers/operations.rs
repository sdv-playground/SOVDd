//! Operation handlers — ISO 17978-3 §7.14 executions sub-resource.
//!
//! Wire shape:
//!
//!   `POST /vehicle/v1/components/{id}/operations/{op_id}/executions`
//!     body: `{parameters?: "<hex>"}`
//!     → 200 (or 202 if still running) + `Location` header to the
//!       newly-created execution sub-resource + `OperationExecution`
//!       body.
//!
//!   `GET /vehicle/v1/components/{id}/operations/{op_id}/executions/{exec_id}`
//!     → `OperationExecution` (current backend state — UDS RoutineControl
//!       0x31 0x03 result).
//!
//!   `DELETE /vehicle/v1/components/{id}/operations/{op_id}/executions/{exec_id}`
//!     → 204 No Content (UDS RoutineControl 0x31 0x02 stop).
//!
//! `exec_id` is a server-allocated UUID returned on POST.  The handler
//! does not persist per-`exec_id` state — UDS RoutineControl is
//! single-operation-at-a-time, so polling any well-formed `exec_id`
//! returns the *current* backend state for the operation.

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use sovd_core::{OperationExecution, OperationInfo, OperationStatus};
use uuid::Uuid;

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Serialize)]
pub struct OperationsResponse {
    pub items: Vec<OperationInfoResponse>,
}

#[derive(Serialize)]
pub struct OperationInfoResponse {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub requires_security: bool,
    pub security_level: u8,
    pub href: String,
}

impl From<&OperationInfo> for OperationInfoResponse {
    fn from(op: &OperationInfo) -> Self {
        Self {
            id: op.id.clone(),
            name: op.name.clone(),
            description: op.description.clone(),
            requires_security: op.requires_security,
            security_level: op.security_level,
            href: op.href.clone(),
        }
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct StartExecutionRequest {
    /// Hex-encoded RoutineControl parameters (UDS 0x31 sub-function payload).
    #[serde(default)]
    pub parameters: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ExecutionQuery {
    /// When set, the handler delegates a fresh `result` poll to the
    /// backend (UDS 0x31 0x03) before serializing the response.
    #[serde(default)]
    pub refresh: bool,
}

/// GET /vehicle/v1/components/:component_id/operations
pub async fn list_operations(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<Json<OperationsResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;
    let operations = backend.list_operations().await?;

    let base = format!("/vehicle/v1/components/{}/operations", component_id);
    let items: Vec<OperationInfoResponse> = operations
        .iter()
        .map(|op| OperationInfoResponse {
            id: op.id.clone(),
            name: op.name.clone(),
            description: op.description.clone(),
            requires_security: op.requires_security,
            security_level: op.security_level,
            href: format!("{}/{}/executions", base, op.id),
        })
        .collect();

    Ok(Json(OperationsResponse { items }))
}

/// POST /vehicle/v1/components/:component_id/operations/:operation_id/executions
///
/// Start a fresh execution of the operation.
/// Returns 200 if the backend already has a terminal state, otherwise
/// 202 with a `Location` header pointing at the executions sub-resource.
pub async fn start_operation_execution(
    State(state): State<AppState>,
    Path((component_id, operation_id)): Path<(String, String)>,
    Json(request): Json<StartExecutionRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let backend = state.get_backend(&component_id)?;

    let params: Vec<u8> = match request.parameters.as_deref() {
        Some(hex_str) => hex::decode(hex_str)
            .map_err(|e| ApiError::BadRequest(format!("Invalid hex parameters: {}", e)))?,
        None => Vec::new(),
    };

    let mut execution = backend.start_operation(&operation_id, &params).await?;
    let exec_id = Uuid::new_v4().to_string();
    execution.execution_id = exec_id.clone();

    // Cache the final execution so GET .../executions/{exec_id} can serve
    // the captured state without re-querying the backend (UDS
    // RoutineControl is synchronous; the backend has nothing else to say).
    state
        .operation_executions
        .record(&component_id, &operation_id, execution.clone());

    let href = format!(
        "/vehicle/v1/components/{}/operations/{}/executions/{}",
        component_id, operation_id, exec_id
    );
    let mut headers = HeaderMap::new();
    headers.insert(
        header::LOCATION,
        HeaderValue::from_str(&href)
            .map_err(|e| ApiError::Internal(format!("bad Location header: {e}")))?,
    );

    let status_code = match execution.status {
        OperationStatus::Running => StatusCode::ACCEPTED,
        _ => StatusCode::OK,
    };

    Ok((status_code, headers, Json(execution)))
}

/// GET /vehicle/v1/components/:component_id/operations/:operation_id/executions/:exec_id
///
/// Polls the backend's current operation state.  `exec_id` is accepted
/// transparently — see the module doc for the single-op-at-a-time
/// limitation.
pub async fn get_operation_execution(
    State(state): State<AppState>,
    Path((component_id, operation_id, exec_id)): Path<(String, String, String)>,
    Query(query): Query<ExecutionQuery>,
) -> Result<Json<OperationExecution>, ApiError> {
    // Validate the component exists before checking the cache.
    let backend = state.get_backend(&component_id)?;

    // Fast path: cached final state from the original POST.
    if !query.refresh {
        if let Some(cached) = state
            .operation_executions
            .get(&component_id, &operation_id, &exec_id)
        {
            return Ok(Json(cached));
        }
    }

    // Either `?refresh=true` or no cache hit — re-poll the backend.
    let mut execution = backend.get_operation_status(&operation_id).await?;
    execution.execution_id = exec_id;
    Ok(Json(execution))
}

/// DELETE /vehicle/v1/components/:component_id/operations/:operation_id/executions/:exec_id
///
/// Stops the operation (UDS RoutineControl 0x31 0x02).
pub async fn stop_operation_execution(
    State(state): State<AppState>,
    Path((component_id, operation_id, _exec_id)): Path<(String, String, String)>,
) -> Result<StatusCode, ApiError> {
    let backend = state.get_backend(&component_id)?;
    backend.stop_operation(&operation_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
