//! Operation (routine) handlers

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use sovd_core::{OperationInfo, OperationStatus};

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

#[derive(Serialize)]
pub struct OperationResultResponse {
    pub operation_id: String,
    pub action: String,
    pub status: OperationStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub timestamp: i64,
}

#[derive(Deserialize, Default)]
pub struct ExecuteOperationRequest {
    /// Action: "start", "stop", or "result"
    #[serde(default = "default_action")]
    pub action: String,
    /// Optional parameters (hex string)
    #[serde(default)]
    pub parameters: Option<String>,
}

fn default_action() -> String {
    "start".to_string()
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

/// GET /vehicle/v1/components/:component_id/operations
/// List available operations
pub async fn list_operations(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<Json<OperationsResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;
    let operations = backend.list_operations().await?;

    let items: Vec<OperationInfoResponse> =
        operations.iter().map(OperationInfoResponse::from).collect();

    Ok(Json(OperationsResponse { items }))
}

/// POST /vehicle/v1/components/:component_id/operations/:operation_id
/// Execute an operation
///
/// Actions:
/// - "start" (0x01) - Start the routine
/// - "stop" (0x02) - Stop a running routine
/// - "result" (0x03) - Request routine results
pub async fn execute_operation(
    State(state): State<AppState>,
    Path((component_id, operation_id)): Path<(String, String)>,
    Json(request): Json<ExecuteOperationRequest>,
) -> Result<Json<OperationResultResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;

    // Parse hex parameters if provided
    let params = if let Some(hex_str) = &request.parameters {
        hex::decode(hex_str)
            .map_err(|e| ApiError::BadRequest(format!("Invalid hex parameters: {}", e)))?
    } else {
        Vec::new()
    };

    // Encode action as first byte (UDS sub-function)
    let sub_function = match request.action.as_str() {
        "start" => 0x01u8,
        "stop" => 0x02u8,
        "result" => 0x03u8,
        _ => {
            return Err(ApiError::BadRequest(format!(
                "Invalid action: {}. Use 'start', 'stop', or 'result'",
                request.action
            )))
        }
    };

    // Combine sub-function with parameters
    let mut full_params = vec![sub_function];
    full_params.extend(params);

    let execution = backend.start_operation(&operation_id, &full_params).await?;

    // Extract result_data as hex string
    let result_data = execution.result.and_then(|v| {
        v.get("routine_result")
            .and_then(|r| r.as_str())
            .map(|s| s.to_string())
    });

    Ok(Json(OperationResultResponse {
        operation_id: execution.operation_id,
        action: request.action,
        status: execution.status,
        result_data,
        error: execution.error,
        timestamp: execution.started_at.timestamp_millis(),
    }))
}
