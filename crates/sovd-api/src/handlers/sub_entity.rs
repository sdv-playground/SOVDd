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

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::Utc;
use sovd_core::DiagnosticBackend;

use crate::error::ApiError;
use crate::state::AppState;

// Re-use response types from sibling handler modules.
use super::data::{DidInfoResponse, DidListResponse, DidResponse, ReadQuery};
use super::faults::{ClearFaultsResponse, FaultFilterQuery, FaultInfoResponse, FaultsResponse};
use super::files::{FileInfo, ListFilesResponse, UploadFileResponse};
use super::flash::{
    ActivationStateResponse, CommitRollbackResponse, ListTransfersResponse, StartFlashRequest,
    StartFlashResponse, TransferExitResponse, TransferInfo,
};
use super::modes::{
    SecurityModeGetResponse, SecurityModeRequest, SessionModeRequest, SessionModeResponse,
};
use super::operations::{
    ExecuteOperationRequest, OperationInfoResponse, OperationResultResponse, OperationsResponse,
};
use sovd_core::{FaultFilter, FaultSeverity, OperationStatus, SecurityState, VerifyResult};

/// Resolve a sub-entity backend from a `(component_id, app_id)` path.
///
/// The `app_id` may contain `/` separators for multi-level nesting
/// (e.g. `"uds_gw/transmission_ecu"` when the ECU sits behind a nested
/// gateway).  We walk each segment via `get_sub_entity()` so that the
/// resolution mirrors `resolve_target` in `modes.rs`.
async fn resolve(
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
        current = current.get_sub_entity(segment).await.map_err(|_| {
            ApiError::NotFound(format!(
                "Sub-entity '{}' not found on '{}'",
                segment, component_id
            ))
        })?;
    }
    Ok(current)
}

// =========================================================================
// Files
// =========================================================================

/// POST /vehicle/v1/components/:component_id/apps/:app_id/files
pub async fn upload_file(
    State(state): State<AppState>,
    Path((component_id, app_id)): Path<(String, String)>,
    body: Bytes,
) -> Result<(StatusCode, Json<UploadFileResponse>), ApiError> {
    let backend = resolve(&state, &component_id, &app_id).await?;
    let file_id = backend
        .receive_package(&body)
        .await
        .map_err(ApiError::from)?;
    let size = body.len();

    tracing::info!(app_id = %app_id, file_id = %file_id, size, "File uploaded to sub-entity");

    let base = format!(
        "/vehicle/v1/components/{}/apps/{}/files",
        component_id, app_id
    );
    Ok((
        StatusCode::CREATED,
        Json(UploadFileResponse {
            file_id: file_id.clone(),
            size,
            verify_url: format!("{}/{}/verify", base, file_id),
            href: format!("{}/{}", base, file_id),
        }),
    ))
}

/// GET /vehicle/v1/components/:component_id/apps/:app_id/files
pub async fn list_files(
    State(state): State<AppState>,
    Path((component_id, app_id)): Path<(String, String)>,
) -> Result<Json<ListFilesResponse>, ApiError> {
    let backend = resolve(&state, &component_id, &app_id).await?;
    let packages = backend.list_packages().await.map_err(ApiError::from)?;
    let base = format!(
        "/vehicle/v1/components/{}/apps/{}/files",
        component_id, app_id
    );
    let files = packages
        .into_iter()
        .map(|info| {
            let fid = info.id.clone();
            FileInfo {
                info,
                href: format!("{}/{}", base, fid),
                verify_url: format!("{}/{}/verify", base, fid),
            }
        })
        .collect();
    Ok(Json(ListFilesResponse { files }))
}

/// GET /vehicle/v1/components/:component_id/apps/:app_id/files/:file_id
pub async fn get_file(
    State(state): State<AppState>,
    Path((component_id, app_id, file_id)): Path<(String, String, String)>,
) -> Result<Json<FileInfo>, ApiError> {
    let backend = resolve(&state, &component_id, &app_id).await?;
    let info = backend
        .get_package(&file_id)
        .await
        .map_err(ApiError::from)?;
    let base = format!(
        "/vehicle/v1/components/{}/apps/{}/files",
        component_id, app_id
    );
    Ok(Json(FileInfo {
        info,
        href: format!("{}/{}", base, file_id),
        verify_url: format!("{}/{}/verify", base, file_id),
    }))
}

/// POST /vehicle/v1/components/:component_id/apps/:app_id/files/:file_id/verify
pub async fn verify_file(
    State(state): State<AppState>,
    Path((component_id, app_id, file_id)): Path<(String, String, String)>,
) -> Result<Json<VerifyResult>, ApiError> {
    let backend = resolve(&state, &component_id, &app_id).await?;
    let result = backend
        .verify_package(&file_id)
        .await
        .map_err(ApiError::from)?;
    tracing::info!(app_id = %app_id, file_id = %file_id, valid = result.valid, "Sub-entity file verified");
    Ok(Json(result))
}

/// DELETE /vehicle/v1/components/:component_id/apps/:app_id/files/:file_id
pub async fn delete_file(
    State(state): State<AppState>,
    Path((component_id, app_id, file_id)): Path<(String, String, String)>,
) -> Result<StatusCode, ApiError> {
    let backend = resolve(&state, &component_id, &app_id).await?;
    backend
        .delete_package(&file_id)
        .await
        .map_err(ApiError::from)?;
    Ok(StatusCode::NO_CONTENT)
}

// =========================================================================
// Flash
// =========================================================================

/// POST /vehicle/v1/components/:component_id/apps/:app_id/flash/transfer
pub async fn start_flash(
    State(state): State<AppState>,
    Path((component_id, app_id)): Path<(String, String)>,
    Json(request): Json<StartFlashRequest>,
) -> Result<(StatusCode, Json<StartFlashResponse>), ApiError> {
    let backend = resolve(&state, &component_id, &app_id).await?;
    let transfer_id = backend
        .start_flash(&request.file_id)
        .await
        .map_err(ApiError::from)?;

    tracing::info!(app_id = %app_id, transfer_id = %transfer_id, "Flash started on sub-entity");

    let base = format!(
        "/vehicle/v1/components/{}/apps/{}/flash",
        component_id, app_id
    );
    Ok((
        StatusCode::ACCEPTED,
        Json(StartFlashResponse {
            transfer_id: transfer_id.clone(),
            status_url: format!("{}/transfer/{}", base, transfer_id),
            finalize_url: format!("{}/transferexit", base),
        }),
    ))
}

/// GET /vehicle/v1/components/:component_id/apps/:app_id/flash/transfer
pub async fn list_transfers(
    State(state): State<AppState>,
    Path((component_id, app_id)): Path<(String, String)>,
) -> Result<Json<ListTransfersResponse>, ApiError> {
    let backend = resolve(&state, &component_id, &app_id).await?;
    let transfers = backend
        .list_flash_transfers()
        .await
        .map_err(ApiError::from)?;
    let base = format!(
        "/vehicle/v1/components/{}/apps/{}/flash/transfer",
        component_id, app_id
    );
    let transfers = transfers
        .into_iter()
        .map(|status| {
            let tid = status.transfer_id.clone();
            TransferInfo {
                status,
                href: format!("{}/{}", base, tid),
            }
        })
        .collect();
    Ok(Json(ListTransfersResponse { transfers }))
}

/// GET .../apps/:app_id/flash/transfer/:transfer_id
pub async fn get_transfer(
    State(state): State<AppState>,
    Path((component_id, app_id, transfer_id)): Path<(String, String, String)>,
) -> Result<Json<TransferInfo>, ApiError> {
    let backend = resolve(&state, &component_id, &app_id).await?;
    let status = backend
        .get_flash_status(&transfer_id)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(TransferInfo {
        status,
        href: format!(
            "/vehicle/v1/components/{}/apps/{}/flash/transfer/{}",
            component_id, app_id, transfer_id
        ),
    }))
}

/// DELETE .../apps/:app_id/flash/transfer/:transfer_id
pub async fn abort_transfer(
    State(state): State<AppState>,
    Path((component_id, app_id, transfer_id)): Path<(String, String, String)>,
) -> Result<StatusCode, ApiError> {
    let backend = resolve(&state, &component_id, &app_id).await?;
    backend
        .abort_flash(&transfer_id)
        .await
        .map_err(ApiError::from)?;
    Ok(StatusCode::NO_CONTENT)
}

/// PUT .../apps/:app_id/flash/transferexit
pub async fn transfer_exit(
    State(state): State<AppState>,
    Path((component_id, app_id)): Path<(String, String)>,
) -> Result<Json<TransferExitResponse>, ApiError> {
    let backend = resolve(&state, &component_id, &app_id).await?;
    backend.finalize_flash().await.map_err(ApiError::from)?;
    tracing::info!(app_id = %app_id, "Sub-entity flash finalized");
    Ok(Json(TransferExitResponse {
        success: true,
        message: "Transfer exit completed successfully".to_string(),
    }))
}

/// POST .../apps/:app_id/flash/commit
pub async fn commit_flash(
    State(state): State<AppState>,
    Path((component_id, app_id)): Path<(String, String)>,
) -> Result<Json<CommitRollbackResponse>, ApiError> {
    let backend = resolve(&state, &component_id, &app_id).await?;
    backend.commit_flash().await.map_err(ApiError::from)?;
    tracing::info!(app_id = %app_id, "Sub-entity firmware committed");
    Ok(Json(CommitRollbackResponse {
        success: true,
        message: "Firmware committed successfully".to_string(),
    }))
}

/// POST .../apps/:app_id/flash/rollback
pub async fn rollback_flash(
    State(state): State<AppState>,
    Path((component_id, app_id)): Path<(String, String)>,
) -> Result<Json<CommitRollbackResponse>, ApiError> {
    let backend = resolve(&state, &component_id, &app_id).await?;
    backend.rollback_flash().await.map_err(ApiError::from)?;
    tracing::info!(app_id = %app_id, "Sub-entity firmware rolled back");
    Ok(Json(CommitRollbackResponse {
        success: true,
        message: "Firmware rolled back successfully".to_string(),
    }))
}

/// GET .../apps/:app_id/flash/activation
pub async fn get_activation_state(
    State(state): State<AppState>,
    Path((component_id, app_id)): Path<(String, String)>,
) -> Result<Json<ActivationStateResponse>, ApiError> {
    let backend = resolve(&state, &component_id, &app_id).await?;
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

// =========================================================================
// ECU Reset
// =========================================================================

/// POST .../apps/:app_id/reset
pub async fn ecu_reset(
    State(state): State<AppState>,
    Path((component_id, app_id)): Path<(String, String)>,
    Json(request): Json<super::reset::EcuResetRequest>,
) -> Result<Json<super::reset::EcuResetResponse>, ApiError> {
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

    let power_down_time = backend.ecu_reset(reset_type_byte).await?;

    let message = match reset_type_name {
        "hard" => "hard reset initiated".to_string(),
        "soft" => "soft reset initiated".to_string(),
        "key_off_on" => "key_off_on reset initiated".to_string(),
        _ => format!("Reset type 0x{:02X} initiated", reset_type_byte),
    };

    tracing::info!(app_id = %app_id, reset_type = %reset_type_name, "Sub-entity ECU reset");

    Ok(Json(super::reset::EcuResetResponse {
        success: true,
        reset_type: reset_type_name.to_string(),
        message,
        power_down_time,
    }))
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
    use super::modes::{SecurityKeyResponse, SecuritySeedResponse, SovdSeed};
    use axum::response::IntoResponse;

    let backend = resolve(&state, &component_id, &app_id).await?;
    let key_bytes = request
        .key
        .as_ref()
        .map(|k| hex::decode(k))
        .transpose()
        .map_err(|e| ApiError::BadRequest(format!("Invalid hex key: {}", e)))?;
    let is_seed_request = request.value.to_lowercase().ends_with("_requestseed");
    let mode = backend
        .set_security_mode(&request.value, key_bytes.as_deref())
        .await?;

    if is_seed_request {
        let seed_hex = mode.seed.unwrap_or_default();
        let seed_formatted = seed_hex
            .as_bytes()
            .chunks(2)
            .map(|chunk| format!("0x{}", std::str::from_utf8(chunk).unwrap_or("00")))
            .collect::<Vec<_>>()
            .join(" ");
        Ok(Json(SecuritySeedResponse {
            id: "security".to_string(),
            seed: SovdSeed {
                request_seed: seed_formatted,
            },
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
    Path((component_id, app_id)): Path<(String, String)>,
) -> Result<Json<DidListResponse>, ApiError> {
    let backend = resolve(&state, &component_id, &app_id).await?;
    let sub_entity_id = backend.entity_info().id.clone();
    let base = format!(
        "/vehicle/v1/components/{}/apps/{}/data",
        component_id, app_id
    );

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
                DidInfoResponse {
                    id: id.clone(),
                    did: did_hex,
                    name: def.name,
                    data_type: Some(def.data_type.to_string()),
                    unit: def.unit,
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
        .map(|p| DidInfoResponse {
            id: p.id.clone(),
            did: p.did.unwrap_or_default(),
            name: Some(p.name),
            data_type: p.data_type,
            unit: p.unit,
            writable: !p.read_only,
            href: format!("{}/{}", base, p.id),
        })
        .collect();
    let count = items.len();
    Ok(Json(DidListResponse { count, items }))
}

/// GET .../apps/:app_id/data/:param_id
pub async fn read_sub_entity_parameter(
    State(state): State<AppState>,
    Path((component_id, app_id, param_id)): Path<(String, String, String)>,
    Query(query): Query<ReadQuery>,
) -> Result<Json<DidResponse>, ApiError> {
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
                return Ok(Json(DidResponse {
                    id: semantic_id,
                    did: sovd_conv::format_did(did_u16),
                    value: serde_json::json!(hex::encode(&raw_bytes)),
                    unit: None,
                    raw: hex::encode(&raw_bytes),
                    length: raw_bytes.len(),
                    converted: false,
                    timestamp: Utc::now().timestamp_millis(),
                }));
            }

            let (value, unit, converted) = if let Some(def) = component_def {
                match did_store.decode(did_u16, &raw_bytes) {
                    Ok(decoded) => (decoded, def.unit, true),
                    Err(_) => (serde_json::json!(hex::encode(&raw_bytes)), None, false),
                }
            } else {
                (serde_json::json!(hex::encode(&raw_bytes)), None, false)
            };

            return Ok(Json(DidResponse {
                id: semantic_id,
                did: sovd_conv::format_did(did_u16),
                value,
                unit,
                raw: hex::encode(&raw_bytes),
                length: raw_bytes.len(),
                converted,
                timestamp: Utc::now().timestamp_millis(),
            }));
        }
    }

    // Fall back to backend.read_data() (proxy backends resolve params via upstream)
    let values = backend
        .read_data(&[param_id.clone()])
        .await
        .map_err(ApiError::from)?;

    let dv = values
        .into_iter()
        .next()
        .ok_or_else(|| ApiError::NotFound(format!("Parameter not found: {}", param_id)))?;

    let raw = dv.raw.clone().unwrap_or_default();
    let length = dv.length.unwrap_or(0);
    let has_raw = !raw.is_empty();

    Ok(Json(DidResponse {
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
        timestamp: Utc::now().timestamp_millis(),
    }))
}

/// PUT .../apps/:app_id/data/:param_id
pub async fn write_sub_entity_parameter(
    State(state): State<AppState>,
    Path((component_id, app_id, param_id)): Path<(String, String, String)>,
    Json(request): Json<super::data::WriteDidRequest>,
) -> Result<Json<DidResponse>, ApiError> {
    let backend = resolve(&state, &component_id, &app_id).await?;
    let sub_entity_id = backend.entity_info().id.clone();
    let did_store = state.did_store();

    // Resolve DID and write via DidStore when component-specific definitions exist
    let has_local_dids = did_store.has_component_specific_dids(&sub_entity_id);
    if has_local_dids {
        if let Some(did_u16) = did_store.resolve_did(&param_id) {
            let component_def = did_store.get_for_component(did_u16, &sub_entity_id);
            let semantic_id = component_def
                .as_ref()
                .and_then(|def| def.id.clone())
                .unwrap_or_else(|| param_id.clone());

            let data = if component_def.is_some() {
                match did_store.encode(did_u16, &request.value) {
                    Ok(bytes) => bytes,
                    Err(_) => super::data::convert_value_to_bytes(&request)?,
                }
            } else {
                super::data::convert_value_to_bytes(&request)?
            };

            backend.write_raw_did(did_u16, &data).await?;

            let (value, unit, converted) = if let Some(def) = component_def {
                match did_store.decode(did_u16, &data) {
                    Ok(decoded) => (decoded, def.unit, true),
                    Err(_) => (serde_json::json!(hex::encode(&data)), None, false),
                }
            } else {
                (serde_json::json!(hex::encode(&data)), None, false)
            };

            return Ok(Json(DidResponse {
                id: semantic_id,
                did: sovd_conv::format_did(did_u16),
                value,
                unit,
                raw: hex::encode(&data),
                length: data.len(),
                converted,
                timestamp: Utc::now().timestamp_millis(),
            }));
        }
    }

    // Fall back to backend.write_data() for proxy backends
    let data = super::data::convert_value_to_bytes(&request)?;
    backend.write_data(&param_id, &data).await?;

    Ok(Json(DidResponse {
        id: param_id,
        did: String::new(),
        value: request.value,
        unit: None,
        raw: hex::encode(&data),
        length: data.len(),
        converted: false,
        timestamp: Utc::now().timestamp_millis(),
    }))
}

// =========================================================================
// Faults
// =========================================================================

/// GET .../apps/:app_id/faults
pub async fn list_sub_entity_faults(
    State(state): State<AppState>,
    Path((component_id, app_id)): Path<(String, String)>,
    Query(query): Query<FaultFilterQuery>,
) -> Result<Json<FaultsResponse>, ApiError> {
    let backend = resolve(&state, &component_id, &app_id).await?;

    let filter = FaultFilter {
        severity: query.severity.as_deref().and_then(|s| match s {
            "info" => Some(FaultSeverity::Info),
            "warning" => Some(FaultSeverity::Warning),
            "error" => Some(FaultSeverity::Error),
            "critical" => Some(FaultSeverity::Critical),
            _ => None,
        }),
        category: query.category.clone(),
        active_only: query.active_only,
        since: None,
        limit: query.limit,
    };

    let result = backend
        .get_faults(Some(&filter))
        .await
        .map_err(ApiError::from)?;

    let base = format!(
        "/vehicle/v1/components/{}/apps/{}/faults",
        component_id, app_id
    );
    let items: Vec<FaultInfoResponse> = result
        .faults
        .iter()
        .map(|f| FaultInfoResponse {
            id: f.id.clone(),
            dtc_code: f.code.clone(),
            severity: format!("{:?}", f.severity).to_lowercase(),
            message: f.message.clone(),
            category: f.category.clone(),
            active: f.active,
            status: f.status.clone(),
            href: format!("{}/{}", base, f.id),
        })
        .collect();

    let total_count = items.len();
    Ok(Json(FaultsResponse {
        items,
        total_count,
        status_availability_mask: result.status_availability_mask,
    }))
}

/// GET .../apps/:app_id/faults/:fault_id
pub async fn get_sub_entity_fault(
    State(state): State<AppState>,
    Path((component_id, app_id, fault_id)): Path<(String, String, String)>,
) -> Result<Json<FaultInfoResponse>, ApiError> {
    let backend = resolve(&state, &component_id, &app_id).await?;
    let fault = backend
        .get_fault_detail(&fault_id)
        .await
        .map_err(ApiError::from)?;

    let base = format!(
        "/vehicle/v1/components/{}/apps/{}/faults",
        component_id, app_id
    );
    Ok(Json(FaultInfoResponse {
        id: fault.id.clone(),
        dtc_code: fault.code.clone(),
        severity: format!("{:?}", fault.severity).to_lowercase(),
        message: fault.message.clone(),
        category: fault.category.clone(),
        active: fault.active,
        status: fault.status.clone(),
        href: format!("{}/{}", base, fault.id),
    }))
}

/// DELETE .../apps/:app_id/faults
pub async fn clear_sub_entity_faults(
    State(state): State<AppState>,
    Path((component_id, app_id)): Path<(String, String)>,
) -> Result<Json<ClearFaultsResponse>, ApiError> {
    let backend = resolve(&state, &component_id, &app_id).await?;
    let result = backend.clear_faults(None).await.map_err(ApiError::from)?;
    Ok(Json(ClearFaultsResponse {
        success: result.success,
        cleared_count: result.cleared_count,
        message: result.message,
    }))
}

// =========================================================================
// Operations
// =========================================================================

/// GET .../apps/:app_id/operations
pub async fn list_sub_entity_operations(
    State(state): State<AppState>,
    Path((component_id, app_id)): Path<(String, String)>,
) -> Result<Json<OperationsResponse>, ApiError> {
    let backend = resolve(&state, &component_id, &app_id).await?;
    let operations = backend.list_operations().await.map_err(ApiError::from)?;

    let base = format!(
        "/vehicle/v1/components/{}/apps/{}/operations",
        component_id, app_id
    );
    let items: Vec<OperationInfoResponse> = operations
        .iter()
        .map(|op| OperationInfoResponse {
            id: op.id.clone(),
            name: op.name.clone(),
            description: op.description.clone(),
            requires_security: op.requires_security,
            security_level: op.security_level,
            href: format!("{}/{}", base, op.id),
        })
        .collect();

    Ok(Json(OperationsResponse { items }))
}

/// POST .../apps/:app_id/operations/:operation_id
pub async fn execute_sub_entity_operation(
    State(state): State<AppState>,
    Path((component_id, app_id, operation_id)): Path<(String, String, String)>,
    Json(request): Json<ExecuteOperationRequest>,
) -> Result<Json<OperationResultResponse>, ApiError> {
    let backend = resolve(&state, &component_id, &app_id).await?;

    let action = request.action.to_lowercase();
    match action.as_str() {
        "start" | "" => {
            let params = request
                .parameters
                .as_deref()
                .map(|p| hex::decode(p).unwrap_or_default())
                .unwrap_or_default();

            let execution = backend
                .start_operation(&operation_id, &params)
                .await
                .map_err(ApiError::from)?;

            Ok(Json(OperationResultResponse {
                operation_id: execution.operation_id,
                action: "start".to_string(),
                status: execution.status,
                result_data: execution.result.and_then(|v| {
                    v.get("routine_result")
                        .and_then(|r| r.as_str())
                        .map(|s| s.to_string())
                }),
                error: execution.error,
                timestamp: execution.started_at.timestamp_millis(),
            }))
        }
        "result" | "status" => {
            let execution = backend
                .get_operation_status(&operation_id)
                .await
                .map_err(ApiError::from)?;

            Ok(Json(OperationResultResponse {
                operation_id: execution.operation_id,
                action: "result".to_string(),
                status: execution.status,
                result_data: execution.result.and_then(|v| {
                    v.get("routine_result")
                        .and_then(|r| r.as_str())
                        .map(|s| s.to_string())
                }),
                error: execution.error,
                timestamp: execution.started_at.timestamp_millis(),
            }))
        }
        "stop" => {
            backend
                .stop_operation(&operation_id)
                .await
                .map_err(ApiError::from)?;

            Ok(Json(OperationResultResponse {
                operation_id: operation_id.clone(),
                action: "stop".to_string(),
                status: OperationStatus::Cancelled,
                result_data: None,
                error: None,
                timestamp: Utc::now().timestamp_millis(),
            }))
        }
        _ => Err(ApiError::BadRequest(format!(
            "Invalid action: {}. Use 'start', 'result', or 'stop'",
            action
        ))),
    }
}
