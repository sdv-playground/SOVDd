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
use sovd_core::{IoControlAction, OperationExecution, OperationInfo, OperationStatus};
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
    /// Spec §5.7: sibling i18n key for the `name` field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub translation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Spec §5.7: `<attr>_translation_id` for the `description` field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description_translation_id: Option<String>,
    pub requires_security: bool,
    pub security_level: u8,
    pub href: String,

    // ----------------------------------------------------------------
    // UDS 0x2F IO control extras — populated only for operations whose
    // backend representation is an output, not a RoutineControl.
    // These are vendor-shaped fields (spec is permissive about
    // additional attributes per §5.10).
    // ----------------------------------------------------------------
    /// UDS DID in hex (e.g. `"F206"`); present for IO control ops.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_id: Option<String>,
    /// Decoded data type (`"uint8"`, `"float32"`, …) when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_type: Option<String>,
    /// Allowed values for enum-typed outputs.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub allowed: Vec<String>,
    /// Supported IO control actions: `return_to_ecu`, `reset_to_default`,
    /// `freeze`, `short_term_adjust`.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub control_types: Vec<String>,
    /// Current raw value as hex string (populated on `op.read` for
    /// outputs).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_value: Option<String>,
    /// Current typed value (decoded via output config) when `op.read`
    /// is called on an output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    /// Default value (after `reset_to_default`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    /// `true` when the tester currently owns the output (UDS 0x2F).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub controlled_by_tester: Option<bool>,
    /// `true` when the value is frozen via `freeze_current_state`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frozen: Option<bool>,
}

fn default_operation_response() -> OperationInfoResponse {
    OperationInfoResponse {
        id: String::new(),
        name: String::new(),
        translation_id: None,
        description: None,
        description_translation_id: None,
        requires_security: false,
        security_level: 0,
        href: String::new(),
        output_id: None,
        data_type: None,
        allowed: Vec::new(),
        control_types: Vec::new(),
        current_value: None,
        value: None,
        default: None,
        controlled_by_tester: None,
        frozen: None,
    }
}

impl From<&OperationInfo> for OperationInfoResponse {
    fn from(op: &OperationInfo) -> Self {
        OperationInfoResponse {
            id: op.id.clone(),
            name: op.name.clone(),
            description: op.description.clone(),
            requires_security: op.requires_security,
            security_level: op.security_level,
            href: op.href.clone(),
            ..default_operation_response()
        }
    }
}

/// Body for `POST .../operations/{op_id}/executions`.
///
/// `parameters` is polymorphic:
///   - String — hex-encoded RoutineControl bytes (UDS 0x31 path).
///   - Object — structured IO control request (UDS 0x2F path),
///     `{"action": "freeze" | "reset_to_default" | "return_to_ecu"
///     | "short_term_adjust", "value": <optional>}`.
#[derive(Debug, Deserialize, Default)]
pub struct StartExecutionRequest {
    #[serde(default)]
    pub parameters: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ExecutionQuery {
    /// When set, the handler delegates a fresh `result` poll to the
    /// backend (UDS 0x31 0x03) before serializing the response.
    #[serde(default)]
    pub refresh: bool,
}

/// GET /vehicle/v1/components/:component_id/operations
///
/// Spec C-133: UDS InputOutputControl (0x2F) folds into the
/// operations collection alongside UDS RoutineControl (0x31).
/// We merge backend.list_operations() with backend.list_outputs()
/// here so a single GET enumerates both classes.
pub async fn list_operations(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<Json<OperationsResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;
    let routines = backend.list_operations().await?;
    let outputs = backend.list_outputs().await.unwrap_or_default();

    let base = format!("/vehicle/v1/components/{}/operations", component_id);
    let mut items: Vec<OperationInfoResponse> = routines
        .iter()
        .map(|op| OperationInfoResponse {
            id: op.id.clone(),
            name: op.name.clone(),
            description: op.description.clone(),
            requires_security: op.requires_security,
            security_level: op.security_level,
            href: format!("{}/{}/executions", base, op.id),
            ..default_operation_response()
        })
        .collect();

    items.extend(outputs.iter().map(|out| OperationInfoResponse {
        id: out.id.clone(),
        name: out.name.clone(),
        description: Some(format!("IO control (UDS 0x2F DID {})", out.output_id)),
        requires_security: out.requires_security,
        security_level: out.security_level,
        href: format!("{}/{}/executions", base, out.id),
        output_id: Some(out.output_id.clone()),
        ..default_operation_response()
    }));

    Ok(Json(OperationsResponse { items }))
}

/// GET /vehicle/v1/components/:component_id/operations/:operation_id
///
/// Spec §7.14 `op.read` — capability description for a single
/// operation.  For IO control (UDS 0x2F) operations we attach the
/// rich state (current value, default, allowed, controlled_by_tester,
/// frozen) so clients don't need a second round-trip.
pub async fn get_operation(
    State(state): State<AppState>,
    Path((component_id, operation_id)): Path<(String, String)>,
) -> Result<Json<OperationInfoResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;
    let base = format!("/vehicle/v1/components/{}/operations", component_id);
    let href = format!("{}/{}/executions", base, operation_id);

    // RoutineControl path: pluck from list_operations.
    let routines = backend.list_operations().await.unwrap_or_default();
    if let Some(op) = routines.iter().find(|o| o.id == operation_id) {
        return Ok(Json(OperationInfoResponse {
            id: op.id.clone(),
            name: op.name.clone(),
            description: op.description.clone(),
            requires_security: op.requires_security,
            security_level: op.security_level,
            href,
            ..default_operation_response()
        }));
    }

    // IO control path: enrich with output detail + per-component
    // config (allowed values, decoded current value).
    let outputs = backend.list_outputs().await.unwrap_or_default();
    if let Some(out) = outputs.iter().find(|o| o.id == operation_id) {
        let detail = backend.get_output(&operation_id).await.ok();
        let cfg = state.get_output_config(&component_id, &operation_id);

        // Always-known per-spec control set (the four UDS 0x2F sub-
        // functions); plumbed via the OperationInfoResponse so a
        // single GET answers "what can I do".
        let control_types = vec![
            "return_to_ecu".to_string(),
            "reset_to_default".to_string(),
            "freeze".to_string(),
            "short_term_adjust".to_string(),
        ];

        return Ok(Json(OperationInfoResponse {
            id: out.id.clone(),
            name: out.name.clone(),
            description: Some(format!("IO control (UDS 0x2F DID {})", out.output_id)),
            requires_security: out.requires_security,
            security_level: out.security_level,
            href,
            output_id: Some(out.output_id.clone()),
            data_type: out.data_type.clone(),
            allowed: cfg.map(|c| c.allowed.clone()).unwrap_or_default(),
            control_types,
            current_value: detail.as_ref().map(|d| d.current_value.clone()),
            value: detail.as_ref().and_then(|d| d.value.clone()),
            default: detail.as_ref().and_then(|d| d.default.clone()),
            controlled_by_tester: detail.as_ref().map(|d| d.controlled_by_tester),
            frozen: detail.as_ref().map(|d| d.frozen),
            ..default_operation_response()
        }));
    }

    Err(ApiError::NotFound(format!(
        "Operation not found: {}",
        operation_id
    )))
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

    // Decide between RoutineControl (0x31) and InputOutputControl (0x2F).
    // Heuristic: if the operation_id matches an output, dispatch to
    // control_output; else fall through to start_operation.  Output
    // lookups are cheap (small list) and we avoid an explicit "type"
    // hint in the wire body.
    let outputs = backend.list_outputs().await.unwrap_or_default();
    let is_output = outputs.iter().any(|o| o.id == operation_id);

    let mut execution = if is_output {
        // IO control path — parse the structured parameters body.
        let (action, value) = parse_io_control_params(request.parameters.as_ref())?;
        let result = backend.control_output(&operation_id, action, value).await?;
        sovd_core::OperationExecution::completed(
            String::new(),
            operation_id.clone(),
            serde_json::to_value(result).unwrap_or(serde_json::Value::Null),
        )
    } else {
        let params: Vec<u8> = match request.parameters.as_ref() {
            Some(serde_json::Value::String(hex)) => hex::decode(hex)
                .map_err(|e| ApiError::BadRequest(format!("Invalid hex parameters: {}", e)))?,
            Some(serde_json::Value::Null) | None => Vec::new(),
            Some(other) => {
                return Err(ApiError::BadRequest(format!(
                    "Operation '{}' is a RoutineControl op; parameters must be a hex string, got {}",
                    operation_id, other
                )));
            }
        };
        backend.start_operation(&operation_id, &params).await?
    };
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

/// Parse the structured `parameters` object for an IO control op.
///
/// Accepts shapes:
///   `{"action": "freeze"}`
///   `{"action": "short_term_adjust", "value": <any>}`
fn parse_io_control_params(
    params: Option<&serde_json::Value>,
) -> Result<(IoControlAction, Option<serde_json::Value>), ApiError> {
    let obj = match params {
        Some(serde_json::Value::Object(m)) => m,
        Some(other) => {
            return Err(ApiError::BadRequest(format!(
                "IO control parameters must be an object with `action`, got {}",
                other
            )));
        }
        None => {
            return Err(ApiError::BadRequest(
                "IO control parameters required (need at least `action`)".into(),
            ));
        }
    };
    let action_str = obj
        .get("action")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::BadRequest("IO control parameters missing `action`".into()))?;
    let action = IoControlAction::parse(action_str).ok_or_else(|| {
        ApiError::BadRequest(format!(
            "Invalid IO control action: {} (use return_to_ecu | reset_to_default | freeze | short_term_adjust)",
            action_str
        ))
    })?;
    let value = obj.get("value").cloned();
    Ok((action, value))
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
