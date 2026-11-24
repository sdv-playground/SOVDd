//! Sub-entity resource handlers (SOVD spec §6.5)
//!
//! The SOVD spec says every sub-entity (app/ECU behind a gateway) inherits
//! the full set of SOVD resources at its own URL path:
//!
//! ```text
//! /vehicle/v1/components/{gateway}/apps/{ecu}/files
//! /vehicle/v1/components/{gateway}/apps/{ecu}/flash/transfer
//! /vehicle/v1/components/{gateway}/apps/{ecu}/modes/session
//! ```
//!
//! These handlers resolve the sub-entity via `get_sub_entity()` and call
//! its `DiagnosticBackend` methods directly — no prefix routing needed.

use std::sync::Arc;

use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use chrono::Utc;
use sovd_core::DiagnosticBackend;

use crate::error::ApiError;
use crate::state::AppState;
use crate::Tree;

// Re-use response types from sibling handler modules.
use super::configurations::SchemaQuery;
use super::data::{Bounds, DidInfoResponse, DidListResponse, DidResponse, ReadQuery};
use super::faults::{FaultFilterQuery, FaultInfoResponse, FaultsResponse};
// F.D8b: handlers::files + handlers::flash deleted along with the
// /flash and /files wires; the legacy sub-entity handlers below
// referenced their response types and are themselves retired now.
use super::modes::{
    SecurityModeGetResponse, SecurityModeRequest, SessionModeRequest, SessionModeResponse,
};
use super::operations::{OperationInfoResponse, OperationsResponse};
use axum::response::IntoResponse;
use sovd_core::{FaultFilter, FaultSeverity, OperationStatus, SecurityState};

/// Resolve a sub-entity backend from a `(component_id, app_id)` path.
///
/// The `app_id` may contain `/` separators for multi-level nesting
/// (e.g. `"uds_gw/transmission_ecu"` when the ECU sits behind a nested
/// gateway).  We walk each segment via `get_sub_entity()` so that the
/// resolution mirrors `resolve_target` in `modes.rs`.
pub(super) async fn resolve(
    state: &AppState,
    component_id: &str,
    app_id: &str,
) -> Result<Arc<dyn DiagnosticBackend>, ApiError> {
    let parent = state.get_backend(component_id)?;
    let mut current: Arc<dyn DiagnosticBackend> = parent.clone();
    for segment in app_id.split('/') {
        if segment.is_empty() {
            continue;
        }
        current = current.get_sub_entity(segment).await.map_err(|e| match e {
            sovd_core::BackendError::EntityNotFound(_) => ApiError::NotFound(format!(
                "Sub-entity '{}' not found on '{}'",
                segment, component_id
            )),
            other => ApiError::from(other),
        })?;
    }
    Ok(current)
}

// =========================================================================
// ECU Reset — PUT status/restart (see `reset` for the per-tree response)
// =========================================================================

/// PUT .../apps/:app_id/status/restart — sub-entity reset; same per-tree
/// response as the component route ([`super::reset`]).
pub async fn status_restart(
    State(state): State<AppState>,
    Extension(tree): Extension<Tree>,
    Path((component_id, app_id)): Path<(String, String)>,
    Json(request): Json<super::reset::EcuResetRequest>,
) -> Result<axum::response::Response, ApiError> {
    let backend = resolve(&state, &component_id, &app_id).await?;

    let reset_type_str = request.reset_type.to_lowercase();
    let (reset_type_byte, reset_type_name) = match reset_type_str.as_str() {
        "hard" | "hardreset" => (0x01u8, "hard"),
        "key_off_on" | "keyoffonreset" => (0x02, "key_off_on"),
        "soft" | "softreset" => (0x03, "soft"),
        _ => {
            let cleaned = request
                .reset_type
                .trim_start_matches("0x")
                .trim_start_matches("0X");
            let v = u8::from_str_radix(cleaned, 16).map_err(|_| {
                ApiError::BadRequest(format!(
                    "Invalid reset type: {}. Use 'hard', 'soft', 'key_off_on', or hex value",
                    request.reset_type
                ))
            })?;
            (
                v,
                match v {
                    0x01 => "hard",
                    0x02 => "key_off_on",
                    0x03 => "soft",
                    _ => "custom",
                },
            )
        }
    };

    tracing::info!(app_id = %app_id, reset_type = %reset_type_name, "Sub-entity ECU reset");

    super::reset::restart_entity(
        backend.as_ref(),
        tree,
        &tree.app(&component_id, &app_id),
        reset_type_byte,
        reset_type_name,
    )
    .await
}

/// GET .../apps/:app_id/status — the app's runtime status (§7.19.2), `/v1`
/// only; the `Location` target of a `/v1` app restart.
pub async fn status_read(
    State(state): State<AppState>,
    Extension(tree): Extension<Tree>,
    Path((component_id, app_id)): Path<(String, String)>,
) -> Result<Json<sovd_core::EntityStatusBody>, ApiError> {
    let backend = resolve(&state, &component_id, &app_id).await?;
    super::reset::entity_status(backend.as_ref(), &tree.app(&component_id, &app_id))
        .await
        .map(Json)
}

/// GET .../apps/:app_id/status/restart/:exec_id — legacy tree only; stub
/// status, see [`super::reset::status_restart_execution`].
pub async fn status_restart_execution(
    Path((_component_id, _app_id, exec_id)): Path<(String, String, String)>,
) -> Json<super::reset::EcuResetExecutionStatus> {
    Json(super::reset::EcuResetExecutionStatus {
        status: OperationStatus::Completed,
        exec_id,
    })
}

// =========================================================================
// Modes (session, security)
// =========================================================================

/// GET .../apps/:app_id/modes/session
pub async fn get_session_mode(
    State(state): State<AppState>,
    Path((component_id, app_id)): Path<(String, String)>,
) -> Result<Json<SessionModeResponse>, ApiError> {
    let backend = resolve(&state, &component_id, &app_id).await?;
    let mode = backend.get_session_mode().await?;
    Ok(Json(SessionModeResponse {
        id: "session".to_string(),
        value: mode.session,
    }))
}

/// PUT .../apps/:app_id/modes/session
pub async fn put_session_mode(
    State(state): State<AppState>,
    Path((component_id, app_id)): Path<(String, String)>,
    Json(request): Json<SessionModeRequest>,
) -> Result<Json<SessionModeResponse>, ApiError> {
    let backend = resolve(&state, &component_id, &app_id).await?;
    let mode = backend.set_session_mode(&request.value).await?;
    Ok(Json(SessionModeResponse {
        id: "session".to_string(),
        value: mode.session,
    }))
}

/// GET .../apps/:app_id/modes/security
pub async fn get_security_mode(
    State(state): State<AppState>,
    Path((component_id, app_id)): Path<(String, String)>,
) -> Result<Json<SecurityModeGetResponse>, ApiError> {
    let backend = resolve(&state, &component_id, &app_id).await?;
    let mode = backend.get_security_mode().await?;
    let value = match mode.state {
        SecurityState::Locked => Some("locked".to_string()),
        SecurityState::Unlocked => mode.level.map(|l| format!("level{}", l)),
        SecurityState::SeedAvailable => mode.level.map(|l| format!("level{}_seedavailable", l)),
    };
    Ok(Json(SecurityModeGetResponse {
        id: "security".to_string(),
        name: Some("Security access".to_string()),
        value,
    }))
}

/// PUT .../apps/:app_id/modes/security
pub async fn put_security_mode(
    State(state): State<AppState>,
    Path((component_id, app_id)): Path<(String, String)>,
    Json(request): Json<SecurityModeRequest>,
) -> Result<axum::response::Response, ApiError> {
    use super::modes::{SecurityKeyResponse, SecuritySeedResponse};
    use axum::response::IntoResponse;

    let backend = resolve(&state, &component_id, &app_id).await?;
    let key_bytes = request
        .key
        .as_ref()
        .map(hex::decode)
        .transpose()
        .map_err(|e| ApiError::BadRequest(format!("Invalid hex key: {}", e)))?;
    let is_seed_request = request.value.to_lowercase().ends_with("_requestseed");
    let mode = backend
        .set_security_mode(&request.value, key_bytes.as_deref())
        .await?;

    if is_seed_request {
        let seed = mode.seed.unwrap_or_default().to_lowercase();
        Ok(Json(SecuritySeedResponse {
            id: "security".to_string(),
            seed,
        })
        .into_response())
    } else {
        let level_value = mode
            .level
            .map(|l| format!("level{}", l))
            .unwrap_or_default();
        Ok(Json(SecurityKeyResponse {
            id: "security".to_string(),
            value: level_value,
        })
        .into_response())
    }
}

// =========================================================================
// Data (parameters)
// =========================================================================

/// GET .../apps/:app_id/data
pub async fn list_sub_entity_parameters(
    State(state): State<AppState>,
    Extension(tree): Extension<Tree>,
    Path((component_id, app_id)): Path<(String, String)>,
) -> Result<Json<DidListResponse>, ApiError> {
    let backend = resolve(&state, &component_id, &app_id).await?;
    let sub_entity_id = backend.entity_info().id.clone();
    let base = format!("{}/data", tree.app(&component_id, &app_id));

    // Use DidStore only when there are component-specific definitions (not just globals).
    // Proxy backends should fall through to list_parameters() for upstream resolution.
    let has_local_dids = state
        .did_store()
        .has_component_specific_dids(&sub_entity_id);
    if has_local_dids {
        let definitions = state.did_store().list_for_component(&sub_entity_id);
        let mut items: Vec<DidInfoResponse> = definitions
            .into_iter()
            .map(|(did, def)| {
                let did_hex = sovd_conv::format_did(did);
                let id = def.id.clone().unwrap_or_else(|| did_hex.clone());
                // §7.9 category (explicit def category or DID-number default).
                let category = Some(def.resolve_category(did));
                DidInfoResponse {
                    id: id.clone(),
                    did: did_hex,
                    name: def.name,
                    translation_id: None,
                    data_type: Some(def.data_type.to_string()),
                    unit: def.unit,
                    category,
                    writable: def.writable,
                    href: format!("{}/{}", base, id),
                }
            })
            .collect();
        items.sort_by(|a, b| a.id.cmp(&b.id));
        let count = items.len();
        return Ok(Json(DidListResponse { count, items }));
    }

    // Fall back to backend.list_parameters() (proxy backends that get params from upstream)
    let params = backend.list_parameters().await.map_err(ApiError::from)?;
    let items: Vec<DidInfoResponse> = params
        .into_iter()
        .map(|p| {
            let did = p.did.unwrap_or_default();
            // §7.9 category from the backend, or DID-number default.
            let category = p.category.or_else(|| {
                if did.is_empty() {
                    None
                } else {
                    Some(sovd_core::DataCategory::from_did_str(&did))
                }
            });
            DidInfoResponse {
                id: p.id.clone(),
                did,
                name: Some(p.name),
                translation_id: None,
                data_type: p.data_type,
                unit: p.unit,
                category,
                writable: !p.read_only,
                href: format!("{}/{}", base, p.id),
            }
        })
        .collect();
    let count = items.len();
    Ok(Json(DidListResponse { count, items }))
}

/// GET .../apps/:app_id/data/:param_id: a `ReadValue` under `/v1`, a
/// `DidResponse` under `/vehicle/v1` (see `data::data_read_body`).
pub async fn read_sub_entity_parameter(
    State(state): State<AppState>,
    Extension(tree): Extension<Tree>,
    Path((component_id, app_id, param_id)): Path<(String, String, String)>,
    Query(query): Query<ReadQuery>,
    schema: Result<Query<SchemaQuery>, QueryRejection>,
) -> Result<axum::response::Response, ApiError> {
    let include_schema = super::data::include_schema(tree, schema)?;
    let (read, bounds) =
        read_sub_entity_did(&state, &component_id, &app_id, &param_id, query.raw).await?;
    Ok(super::data::data_read_body(
        tree,
        read,
        bounds,
        include_schema,
    ))
}

async fn read_sub_entity_did(
    state: &AppState,
    component_id: &str,
    app_id: &str,
    param_id: &str,
    raw: bool,
) -> Result<(DidResponse, Bounds), ApiError> {
    let (state, component_id, app_id, param_id) = (
        state.clone(),
        component_id.to_string(),
        app_id.to_string(),
        param_id.to_string(),
    );
    let query = ReadQuery { raw };
    let backend = resolve(&state, &component_id, &app_id).await?;
    let sub_entity_id = backend.entity_info().id.clone();
    let did_store = state.did_store();

    // Use DidStore only when there are component-specific definitions.
    // Proxy backends don't have local DIDs and should fall through to read_data().
    let has_local_dids = did_store.has_component_specific_dids(&sub_entity_id);
    if has_local_dids {
        if let Some(did_u16) = did_store.resolve_did(&param_id) {
            let component_def = did_store.get_for_component(did_u16, &sub_entity_id);
            let semantic_id = component_def
                .as_ref()
                .and_then(|def| def.id.clone())
                .unwrap_or_else(|| param_id.clone());

            let raw_bytes = backend.read_raw_did(did_u16).await?;

            if query.raw {
                return Ok((
                    DidResponse {
                        id: semantic_id,
                        did: sovd_conv::format_did(did_u16),
                        value: serde_json::json!(hex::encode(&raw_bytes)),
                        unit: None,
                        raw: hex::encode(&raw_bytes),
                        length: raw_bytes.len(),
                        converted: false,
                        timestamp: Utc::now().to_rfc3339(),
                    },
                    (None, None),
                ));
            }

            let (value, unit, converted, bounds) = if let Some(def) = component_def {
                match did_store.decode(did_u16, &raw_bytes) {
                    Ok(decoded) => (decoded, def.unit, true, (def.min, def.max)),
                    Err(_) => (
                        serde_json::json!(hex::encode(&raw_bytes)),
                        None,
                        false,
                        (None, None),
                    ),
                }
            } else {
                (
                    serde_json::json!(hex::encode(&raw_bytes)),
                    None,
                    false,
                    (None, None),
                )
            };

            return Ok((
                DidResponse {
                    id: semantic_id,
                    did: sovd_conv::format_did(did_u16),
                    value,
                    unit,
                    raw: hex::encode(&raw_bytes),
                    length: raw_bytes.len(),
                    converted,
                    timestamp: Utc::now().to_rfc3339(),
                },
                bounds,
            ));
        }
    }

    // Fall back to backend.read_data() (proxy backends resolve params via upstream)
    let values = backend
        .read_data(std::slice::from_ref(&param_id))
        .await
        .map_err(ApiError::from)?;

    let dv = values
        .into_iter()
        .next()
        .ok_or_else(|| ApiError::NotFound(format!("Parameter not found: {}", param_id)))?;

    let raw = dv.raw.clone().unwrap_or_default();
    let length = dv.length.unwrap_or(0);
    let has_raw = !raw.is_empty();

    Ok((
        DidResponse {
            id: param_id,
            did: dv.did.unwrap_or_default(),
            value: if query.raw && has_raw {
                serde_json::json!(raw)
            } else {
                dv.value
            },
            unit: if query.raw { None } else { dv.unit },
            raw,
            length,
            converted: !query.raw && has_raw,
            timestamp: Utc::now().to_rfc3339(),
        },
        (None, None),
    ))
}

/// PUT .../apps/:app_id/data/:param_id — 204 No Content per spec.
pub async fn write_sub_entity_parameter(
    State(state): State<AppState>,
    Path((component_id, app_id, param_id)): Path<(String, String, String)>,
    Json(request): Json<super::data::WriteDidRequest>,
) -> Result<StatusCode, ApiError> {
    let backend = resolve(&state, &component_id, &app_id).await?;
    let sub_entity_id = backend.entity_info().id.clone();
    let did_store = state.did_store();

    let has_local_dids = did_store.has_component_specific_dids(&sub_entity_id);
    if has_local_dids {
        if let Some(did_u16) = did_store.resolve_did(&param_id) {
            let component_def = did_store.get_for_component(did_u16, &sub_entity_id);
            // Raw-vs-converted inference (C-131): encode physical values only
            // when the DID carries a real conversion; otherwise treat `value`
            // as a raw byte representation.
            let has_conversion = component_def.as_ref().is_some_and(|d| d.has_conversion());
            let data = if has_conversion {
                match did_store.encode(did_u16, &request.value) {
                    Ok(bytes) => bytes,
                    Err(_) => super::data::convert_value_to_bytes(&request.value)?,
                }
            } else {
                super::data::convert_value_to_bytes(&request.value)?
            };
            backend.write_raw_did(did_u16, &data).await?;
            return Ok(StatusCode::NO_CONTENT);
        }
    }

    // Fall back to backend.write_data() for proxy backends
    let data = super::data::convert_value_to_bytes(&request.value)?;
    backend.write_data(&param_id, &data).await?;
    Ok(StatusCode::NO_CONTENT)
}

// =========================================================================
// Faults
// =========================================================================

/// GET .../apps/:app_id/faults
pub async fn list_sub_entity_faults(
    State(state): State<AppState>,
    Extension(tree): Extension<Tree>,
    Path((component_id, app_id)): Path<(String, String)>,
    Query(query): Query<FaultFilterQuery>,
) -> Result<Json<FaultsResponse>, ApiError> {
    let backend = resolve(&state, &component_id, &app_id).await?;

    let filter = FaultFilter {
        severity: query.severity.map(FaultSeverity::from),
        category: query.category.clone(),
        active_only: query.active_only,
        since: None,
        limit: query.limit,
    };

    let result = backend
        .get_faults(Some(&filter))
        .await
        .map_err(ApiError::from)?;

    let base = format!("{}/faults", tree.app(&component_id, &app_id));
    let items: Vec<FaultInfoResponse> = result
        .faults
        .iter()
        .map(|f| FaultInfoResponse {
            code: f.code.clone(),
            fault_name: f.message.clone(),
            severity: f.severity,
            scope: None,
            display_code: None,
            symptom: None,
            fault_translation_id: None,
            symptom_translation_id: None,
            status: f.status.clone(),
            href: format!("{}/{}", base, f.id),
        })
        .collect();

    let total_count = items.len();
    Ok(Json(FaultsResponse { items, total_count }))
}

/// GET .../apps/:app_id/faults/:fault_id
pub async fn get_sub_entity_fault(
    State(state): State<AppState>,
    Extension(tree): Extension<Tree>,
    Path((component_id, app_id, fault_id)): Path<(String, String, String)>,
) -> Result<Json<FaultInfoResponse>, ApiError> {
    let backend = resolve(&state, &component_id, &app_id).await?;
    let fault = backend
        .get_fault_detail(&fault_id)
        .await
        .map_err(ApiError::from)?;

    let base = format!("{}/faults", tree.app(&component_id, &app_id));
    Ok(Json(FaultInfoResponse {
        code: fault.code.clone(),
        fault_name: fault.message.clone(),
        severity: fault.severity,
        scope: None,
        display_code: None,
        symptom: None,
        fault_translation_id: None,
        symptom_translation_id: None,
        status: fault.status.clone(),
        href: format!("{}/{}", base, fault.id),
    }))
}

/// DELETE .../apps/:app_id/faults — 204 No Content per spec.
pub async fn clear_sub_entity_faults(
    State(state): State<AppState>,
    Path((component_id, app_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let backend = resolve(&state, &component_id, &app_id).await?;
    let _ = backend.clear_faults(None).await.map_err(ApiError::from)?;
    Ok(StatusCode::NO_CONTENT)
}

// =========================================================================
// Operations
// =========================================================================

/// GET .../apps/:app_id/operations
pub async fn list_sub_entity_operations(
    State(state): State<AppState>,
    Extension(tree): Extension<Tree>,
    Path((component_id, app_id)): Path<(String, String)>,
) -> Result<Json<OperationsResponse>, ApiError> {
    let backend = resolve(&state, &component_id, &app_id).await?;
    let operations = backend.list_operations().await.map_err(ApiError::from)?;

    let base = format!("{}/operations", tree.app(&component_id, &app_id));
    let items: Vec<OperationInfoResponse> = operations
        .iter()
        .map(OperationInfoResponse::from)
        .map(|mut info| {
            info.href = format!("{}/{}/executions", base, info.id);
            info
        })
        .collect();

    Ok(Json(OperationsResponse { items }))
}

/// POST .../apps/:app_id/operations/:operation_id/executions
///
/// Spec-conforming start.  Mirrors the entity-root handler in
/// handlers/operations.rs — see the module doc there for the
/// single-op-at-a-time `exec_id` contract.
pub async fn start_sub_entity_operation(
    State(state): State<AppState>,
    Extension(tree): Extension<Tree>,
    Path((component_id, app_id, operation_id)): Path<(String, String, String)>,
    Json(request): Json<super::operations::StartExecutionRequest>,
) -> Result<axum::response::Response, ApiError> {
    let backend = resolve(&state, &component_id, &app_id).await?;

    let params: Vec<u8> = match request.parameters.as_ref() {
        Some(serde_json::Value::String(hex)) => hex::decode(hex)
            .map_err(|e| ApiError::BadRequest(format!("Invalid hex parameters: {}", e)))?,
        Some(serde_json::Value::Null) | None => Vec::new(),
        Some(other) => {
            return Err(ApiError::BadRequest(format!(
                "Operation '{}' parameters must be a hex string, got {}",
                operation_id, other
            )));
        }
    };

    let mut execution = backend
        .start_operation(&operation_id, &params)
        .await
        .map_err(ApiError::from)?;

    let exec_id = uuid::Uuid::new_v4().to_string();
    execution.execution_id = exec_id.clone();

    // Cache the final execution under the (gateway, app/op_id) key so the
    // matching GET .../executions/{exec_id} below can return it without
    // re-querying the (synchronous) backend.  Use the fully-qualified
    // `apps/{app}/operations/{op}` form as the cache key namespace.
    let cache_op_id = format!("apps/{}/{}", app_id, operation_id);
    state
        .operation_executions
        .record(&component_id, &cache_op_id, execution.clone());

    let href = format!(
        "{}/operations/{}/executions/{}",
        tree.app(&component_id, &app_id),
        operation_id,
        exec_id
    );
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::LOCATION,
        axum::http::HeaderValue::from_str(&href)
            .map_err(|e| ApiError::Internal(format!("bad Location header: {e}")))?,
    );

    let status_code = match execution.status {
        OperationStatus::Running => StatusCode::ACCEPTED,
        _ => StatusCode::OK,
    };

    Ok((
        status_code,
        headers,
        Json(super::operations::ExecutionResponse::from(execution)),
    )
        .into_response())
}

/// GET .../apps/:app_id/operations/:operation_id/executions/:exec_id
pub async fn get_sub_entity_operation_execution(
    State(state): State<AppState>,
    Path((component_id, app_id, operation_id, exec_id)): Path<(String, String, String, String)>,
) -> Result<Json<super::operations::ExecutionResponse>, ApiError> {
    let cache_op_id = format!("apps/{}/{}", app_id, operation_id);
    if let Some(cached) = state
        .operation_executions
        .get(&component_id, &cache_op_id, &exec_id)
    {
        return Ok(Json(cached.into()));
    }

    let backend = resolve(&state, &component_id, &app_id).await?;
    let mut execution = backend
        .get_operation_status(&operation_id)
        .await
        .map_err(ApiError::from)?;
    execution.execution_id = exec_id;
    Ok(Json(execution.into()))
}

/// DELETE .../apps/:app_id/operations/:operation_id/executions/:exec_id
pub async fn stop_sub_entity_operation_execution(
    State(state): State<AppState>,
    Path((component_id, app_id, operation_id, _exec_id)): Path<(String, String, String, String)>,
) -> Result<StatusCode, ApiError> {
    let backend = resolve(&state, &component_id, &app_id).await?;
    backend
        .stop_operation(&operation_id)
        .await
        .map_err(ApiError::from)?;
    Ok(StatusCode::NO_CONTENT)
}
