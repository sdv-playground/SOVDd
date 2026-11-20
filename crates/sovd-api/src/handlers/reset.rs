//! ECU Reset handlers (UDS 0x11 ECUReset) — ISO 17978-3 §7.19.
//!
//! `PUT {entity}/status/restart` answers per tree:
//!   * `/v1` (ISO 17978-3 §7.19.4): 202 with no body and
//!     `Location: {entity}/status`; readiness is read there (`ready` /
//!     `notReady`, Table 281).  An entity whose backend does not control its
//!     own restart ([`DiagnosticBackend::supports_restart`] is `false`) gets
//!     405 `precondition-not-fulfilled`.
//!   * legacy (`/vehicle/v1`, frozen): 202 with a vendor legacy body
//!     `{status, exec_id, href, …}` and `Location` to an untracked
//!     `status/restart/{exec_id}` stub.  Its `status` reflects the backend's
//!     [`DiagnosticBackend::ecu_reset_outcome`]: `completed` when the reset
//!     has taken effect, `running` when it was only triggered.
//!
//! [`DiagnosticBackend::supports_restart`]: sovd_core::DiagnosticBackend::supports_restart
//! [`DiagnosticBackend::ecu_reset_outcome`]: sovd_core::DiagnosticBackend::ecu_reset_outcome

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sovd_core::{
    error_code, DiagnosticBackend, EcuResetOutcome, EntityStatusBody, GenericError, OperationStatus,
};

use crate::error::ApiError;
use crate::state::AppState;
use crate::Tree;

/// Request for ECU reset
#[derive(Debug, Deserialize)]
pub struct EcuResetRequest {
    /// Reset type: "hard", "soft", "key_off_on", or hex value like "0x01"
    pub reset_type: String,
}

/// Vendor legacy body returned alongside the 202 on the legacy tree.
#[derive(Debug, Serialize)]
pub struct EcuResetExecution {
    pub status: OperationStatus,
    pub exec_id: String,
    pub reset_type: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub power_down_time: Option<u8>,
    pub href: String,
}

/// Status returned from the legacy `GET .../status/restart/{exec_id}` stub.
#[derive(Debug, Serialize)]
pub struct EcuResetExecutionStatus {
    pub status: OperationStatus,
    pub exec_id: String,
}

/// Parse reset type string to UDS reset type byte
fn parse_reset_type(s: &str) -> Result<(u8, &'static str), ApiError> {
    match s.to_lowercase().as_str() {
        "hard" | "hardreset" => Ok((0x01, "hard")),
        "key_off_on" | "keyoffonreset" => Ok((0x02, "key_off_on")),
        "soft" | "softreset" => Ok((0x03, "soft")),
        "0x01" | "0x1" | "1" => Ok((0x01, "hard")),
        "0x02" | "0x2" | "2" => Ok((0x02, "key_off_on")),
        "0x03" | "0x3" | "3" => Ok((0x03, "soft")),
        _ => {
            // Try parsing as hex
            let cleaned = s.trim_start_matches("0x").trim_start_matches("0X");
            u8::from_str_radix(cleaned, 16)
                .map(|v| {
                    (
                        v,
                        match v {
                            0x01 => "hard",
                            0x02 => "key_off_on",
                            0x03 => "soft",
                            _ => "custom",
                        },
                    )
                })
                .map_err(|_| {
                    ApiError::BadRequest(format!(
                        "Invalid reset type: {}. Use 'hard', 'soft', 'key_off_on', or hex value",
                        s
                    ))
                })
        }
    }
}

/// PUT {base}/components/:component_id/status/restart — see the module docs
/// for the per-tree response.  409 on conflict (flash in progress) is produced
/// by the backend layer.
pub async fn status_restart(
    State(state): State<AppState>,
    Extension(tree): Extension<Tree>,
    Path(component_id): Path<String>,
    Json(request): Json<EcuResetRequest>,
) -> Result<Response, ApiError> {
    let backend = state.get_backend(&component_id)?;
    let (reset_type_byte, reset_type_name) = parse_reset_type(&request.reset_type)?;
    restart_entity(
        backend.as_ref(),
        tree,
        &tree.component(&component_id),
        reset_type_byte,
        reset_type_name,
    )
    .await
}

/// Restart `backend`, the entity at `entity_path` (its full path under `tree`),
/// and build the tree's 202 response.  Shared by the component and app routes.
pub(super) async fn restart_entity(
    backend: &dyn DiagnosticBackend,
    tree: Tree,
    entity_path: &str,
    reset_type_byte: u8,
    reset_type_name: &str,
) -> Result<Response, ApiError> {
    if tree == Tree::V1 {
        if !backend.supports_restart() {
            let err = GenericError::new(
                error_code::PRECONDITION_NOT_FULFILLED,
                "this entity is restarted by the node-level status/restart",
            );
            return Ok((StatusCode::METHOD_NOT_ALLOWED, Json(err)).into_response());
        }
        match backend.ecu_reset_outcome(reset_type_byte).await? {
            EcuResetOutcome::Completed { .. } => {
                tracing::info!(entity = %entity_path, reset_type = %reset_type_name, "restart completed")
            }
            EcuResetOutcome::Initiated { .. } => {
                tracing::info!(entity = %entity_path, reset_type = %reset_type_name, "restart initiated")
            }
        }
        let location = location_header(&format!("{entity_path}/status"))?;
        return Ok((StatusCode::ACCEPTED, [(header::LOCATION, location)]).into_response());
    }

    let (status, power_down_time) = match backend.ecu_reset_outcome(reset_type_byte).await? {
        EcuResetOutcome::Completed { power_down_time } => {
            (OperationStatus::Completed, power_down_time)
        }
        EcuResetOutcome::Initiated { power_down_time } => {
            (OperationStatus::Running, power_down_time)
        }
    };

    let exec_id = Uuid::new_v4().to_string();
    let href = format!("{entity_path}/status/restart/{exec_id}");

    let mut headers = HeaderMap::new();
    headers.insert(header::LOCATION, location_header(&href)?);

    let body = EcuResetExecution {
        status,
        exec_id,
        reset_type: reset_type_name.to_string(),
        message: match reset_type_name {
            "hard" => "hard reset initiated".to_string(),
            "soft" => "soft reset initiated".to_string(),
            "key_off_on" => "key_off_on reset initiated".to_string(),
            _ => format!("Reset type 0x{:02X} initiated", reset_type_byte),
        },
        power_down_time,
        href,
    };

    Ok((StatusCode::ACCEPTED, headers, Json(body)).into_response())
}

fn location_header(href: &str) -> Result<HeaderValue, ApiError> {
    HeaderValue::from_str(href).map_err(|e| ApiError::Internal(format!("bad Location header: {e}")))
}

/// GET /vehicle/v1/components/:component_id/status/restart/:exec_id —
/// legacy tree only.
///
/// Stub: restart executions are not tracked, so this does not reflect
/// the outcome reported by the PUT — any well-formed `exec_id` reads
/// `completed`, even for a reset that was only `running` at PUT time.
/// Use `GET {entity}/status` to verify a reset took effect.
pub async fn status_restart_execution(
    Path((_component_id, exec_id)): Path<(String, String)>,
) -> Json<EcuResetExecutionStatus> {
    Json(EcuResetExecutionStatus {
        status: OperationStatus::Completed,
        exec_id,
    })
}

/// GET {entity}/status — read an entity's runtime status (ISO 17978-3 §7.19.2).
/// Returns the standard `EntityStatus` (`ready`/`notReady`) plus whatever vendor
/// `x-<vendor>-*` runtime fields the backend supplies (e.g. a monotonic boot/restart
/// counter + uptime, which an orchestrator uses to verify a reset took effect).
pub async fn status_read(
    State(state): State<AppState>,
    Extension(tree): Extension<Tree>,
    Path(component_id): Path<String>,
) -> Result<Json<EntityStatusBody>, ApiError> {
    let backend = state.get_backend(&component_id)?;
    entity_status(backend.as_ref(), &tree.component(&component_id))
        .await
        .map(Json)
}

/// The `GET {entity}/status` body for `backend`, the entity at `entity_path`.
/// Table 280 C2: the `restart` link is present only if the entity controls its
/// own restart, so it is filled in (the route always exists) or dropped per
/// [`DiagnosticBackend::supports_restart`].
pub(super) async fn entity_status(
    backend: &dyn DiagnosticBackend,
    entity_path: &str,
) -> Result<EntityStatusBody, ApiError> {
    let mut status = backend.read_entity_status().await?;
    if !backend.supports_restart() {
        status.restart.clear();
    } else if status.restart.is_empty() {
        status.restart = vec![format!("{entity_path}/status/restart")];
    }
    Ok(status)
}
