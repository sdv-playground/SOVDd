//! ECU Reset handlers (UDS 0x11 ECUReset) — ISO 17978-3 §7.19.
//!
//! `PUT {entity}/status/restart` is async per spec line 552: returns
//! 202 + `Location` header to a status sub-resource.  Reset is fire-
//! and-forget — once the ECU is rebooting there's no observable
//! progress — so the status sub-resource is a stateless stub that
//! always reads `completed`.

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sovd_core::EntityStatusBody;

use crate::error::ApiError;
use crate::state::AppState;

/// Request for ECU reset
#[derive(Debug, Deserialize)]
pub struct EcuResetRequest {
    /// Reset type: "hard", "soft", "key_off_on", or hex value like "0x01"
    pub reset_type: String,
}

/// Execution-resource body returned alongside the 202.
#[derive(Debug, Serialize)]
pub struct EcuResetExecution {
    pub status: String,
    pub exec_id: String,
    pub reset_type: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub power_down_time: Option<u8>,
    pub href: String,
}

/// Status returned from `GET .../status/restart/{exec_id}`.
#[derive(Debug, Serialize)]
pub struct EcuResetExecutionStatus {
    pub status: String,
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

/// PUT /vehicle/v1/components/:component_id/status/restart — spec §7.19.
///
/// Returns **202 Accepted** with a `Location` header to
/// `/vehicle/v1/components/{id}/status/restart/{exec_id}` for status
/// polling.  409 on conflict (flash in progress) is produced by the
/// backend layer.
pub async fn status_restart(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
    Json(request): Json<EcuResetRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let backend = state.get_backend(&component_id)?;
    let (reset_type_byte, reset_type_name) = parse_reset_type(&request.reset_type)?;
    let power_down_time = backend.ecu_reset(reset_type_byte).await?;

    let exec_id = Uuid::new_v4().to_string();
    let href = format!(
        "/vehicle/v1/components/{}/status/restart/{}",
        component_id, exec_id
    );

    let mut headers = HeaderMap::new();
    headers.insert(
        header::LOCATION,
        HeaderValue::from_str(&href)
            .map_err(|e| ApiError::Internal(format!("bad Location header: {e}")))?,
    );

    let body = EcuResetExecution {
        status: "completed".to_string(),
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

    Ok((StatusCode::ACCEPTED, headers, Json(body)))
}

/// GET /vehicle/v1/components/:component_id/status/restart/:exec_id
///
/// Stub status — reset is fire-and-forget; once the kernel takes
/// over we have no observable progress to report.  Any well-formed
/// `exec_id` returns `completed`.
pub async fn status_restart_execution(
    Path((_component_id, exec_id)): Path<(String, String)>,
) -> Json<EcuResetExecutionStatus> {
    Json(EcuResetExecutionStatus {
        status: "completed".to_string(),
        exec_id,
    })
}

/// GET {entity}/status — read an entity's runtime status (ISO 17978-3 §7.19.2).
/// Returns the standard `EntityStatus` (`ready`/`notReady`) plus whatever vendor
/// `x-sumo-*` runtime fields the backend supplies (e.g. a monotonic boot/restart
/// counter + uptime, which an orchestrator uses to verify a reset took effect).
/// The `restart` control link is filled in here since the route always exists.
pub async fn status_read(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<Json<EntityStatusBody>, ApiError> {
    let backend = state.get_backend(&component_id)?;
    let mut status = backend.read_entity_status().await?;
    if status.restart.is_empty() {
        status.restart = vec![format!(
            "/vehicle/v1/components/{component_id}/status/restart"
        )];
    }
    Ok(Json(status))
}
