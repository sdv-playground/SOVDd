//! Mode handlers (UDS session + security)
//!
//! Implements SOVD standard mode endpoints as defined in ASAM SOVD
//! specification.
//!
//! C-025 / C-130: only the standardized mode names are served here.
//! UDS LinkControl (0x87) has no entry in the Table 343 service→mode
//! mapping ("not represented"), so there is no `modes/link` resource —
//! the former link-control handlers + request/response types were
//! removed.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use sovd_core::{DiagnosticBackend, SecurityState};

use crate::error::ApiError;
use crate::state::AppState;

/// Optional query parameters for mode endpoints
#[derive(Debug, Deserialize)]
pub struct ModeQuery {
    /// Target sub-entity path (e.g., "uds_gw/engine_ecu") for routing
    /// session/security changes to a specific child backend through a gateway.
    #[serde(default)]
    pub target: Option<String>,
}

/// Resolve a target sub-entity path through the gateway hierarchy.
/// E.g., "uds_gw/engine_ecu" navigates gateway → uds_gw → engine_ecu.
async fn resolve_target(
    backend: &Arc<dyn DiagnosticBackend>,
    target: &str,
) -> Result<Arc<dyn DiagnosticBackend>, ApiError> {
    let mut current = backend.clone();
    for segment in target.split('/') {
        if segment.is_empty() {
            continue;
        }
        current = current
            .get_sub_entity(segment)
            .await
            .map_err(|_| ApiError::NotFound(format!("Sub-entity not found: {}", segment)))?;
    }
    Ok(current)
}

// =============================================================================
// Request/Response Types (SOVD Standard)
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct SessionModeRequest {
    /// Session name: "default", "extended", "programming", "engineering", "telematics", or hex
    pub value: String,
}

/// SOVD standard session mode response
#[derive(Debug, Serialize)]
pub struct SessionModeResponse {
    /// Mode identifier (always "session")
    pub id: String,
    /// Current session value
    pub value: String,
}

#[derive(Debug, Deserialize)]
pub struct SecurityModeRequest {
    /// Either "levelN_requestseed" to request seed, or "levelN" to send key
    pub value: String,
    /// Key in hex format (required when sending key)
    #[serde(default)]
    pub key: Option<String>,
}

/// Response for a security seed request.
///
/// Spec primitive `string:hex` (sovd_iso17978_spec.yaml line 192):
/// concatenated lowercase hex, no `0x` per byte, no spacing.
#[derive(Debug, Serialize)]
pub struct SecuritySeedResponse {
    /// Mode identifier (always "security")
    pub id: String,
    /// Seed bytes, concatenated lowercase hex.
    pub seed: String,
}

/// SOVD standard response for security send key (success)
#[derive(Debug, Serialize)]
pub struct SecurityKeyResponse {
    /// Mode identifier (always "security")
    pub id: String,
    /// The security level that was unlocked
    pub value: String,
}

/// SOVD standard GET response for security mode
#[derive(Debug, Serialize)]
pub struct SecurityModeGetResponse {
    /// Mode identifier (always "security")
    pub id: String,
    /// Human-readable name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Current security state as value (e.g., "locked", "level1")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

// =============================================================================
// Session Mode Handlers
// =============================================================================

/// GET /vehicle/v1/components/:component_id/modes/session?target=child/path
/// Get current diagnostic session (SOVD standard format)
pub async fn get_session_mode(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
    Query(query): Query<ModeQuery>,
) -> Result<Json<SessionModeResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;
    let target_backend: Arc<dyn DiagnosticBackend> = if let Some(ref target) = query.target {
        resolve_target(backend, target).await?
    } else {
        backend.clone()
    };
    let mode = target_backend.get_session_mode().await?;
    Ok(Json(SessionModeResponse {
        id: "session".to_string(),
        value: mode.session,
    }))
}

/// PUT /vehicle/v1/components/:component_id/modes/session?target=child/path
/// Change diagnostic session (SOVD standard format)
pub async fn put_session_mode(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
    Query(query): Query<ModeQuery>,
    Json(request): Json<SessionModeRequest>,
) -> Result<Json<SessionModeResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;
    let target_backend: Arc<dyn DiagnosticBackend> = if let Some(ref target) = query.target {
        resolve_target(backend, target).await?
    } else {
        backend.clone()
    };
    let mode = target_backend.set_session_mode(&request.value).await?;
    Ok(Json(SessionModeResponse {
        id: "session".to_string(),
        value: mode.session,
    }))
}

// =============================================================================
// Communication-Control Mode Handlers (UDS CommunicationControl 0x28)
// =============================================================================

/// PUT body for `modes/comm-ctrl` — ECU-specific subfunction enum value
/// (ISO 17978-3 §8.3.4 / Table 343), e.g. `"disable-rx-tx"`.
#[derive(Debug, Deserialize)]
pub struct CommControlModeRequest {
    /// Subfunction value (kebab-case) from the ECU-specific enum.
    pub value: String,
}

/// Response for `modes/comm-ctrl` (GET + PUT). Mirrors the session shape
/// (`id`/`value`) and adds the ECU-specific `supported` enumeration.
#[derive(Debug, Serialize)]
pub struct CommControlModeResponse {
    /// Mode identifier (always "comm-ctrl").
    pub id: String,
    /// Current subfunction value.
    pub value: String,
    /// ECU-specific enumeration of accepted subfunction values.
    pub supported: Vec<String>,
}

/// GET /vehicle/v1/components/:component_id/modes/comm-ctrl?target=child/path
pub async fn get_comm_control_mode(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
    Query(query): Query<ModeQuery>,
) -> Result<Json<CommControlModeResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;
    let target_backend: Arc<dyn DiagnosticBackend> = if let Some(ref target) = query.target {
        resolve_target(backend, target).await?
    } else {
        backend.clone()
    };
    let mode = target_backend.get_communication_control().await?;
    Ok(Json(CommControlModeResponse {
        id: "comm-ctrl".to_string(),
        value: mode.value,
        supported: mode.supported,
    }))
}

/// PUT /vehicle/v1/components/:component_id/modes/comm-ctrl?target=child/path
/// Sends UDS CommunicationControl (0x28) with the selected subfunction.
pub async fn put_comm_control_mode(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
    Query(query): Query<ModeQuery>,
    Json(request): Json<CommControlModeRequest>,
) -> Result<Json<CommControlModeResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;
    let target_backend: Arc<dyn DiagnosticBackend> = if let Some(ref target) = query.target {
        resolve_target(backend, target).await?
    } else {
        backend.clone()
    };
    let mode = target_backend
        .set_communication_control(&request.value)
        .await?;
    Ok(Json(CommControlModeResponse {
        id: "comm-ctrl".to_string(),
        value: mode.value,
        supported: mode.supported,
    }))
}

// =============================================================================
// DTC-Setting Mode Handlers (UDS ControlDTCSetting 0x85)
// =============================================================================

/// PUT body for `modes/dtcsetting` — `"on"`/`"off"` enum (ISO 17978-3
/// §8.3.5 / Table 343).
#[derive(Debug, Deserialize)]
pub struct DtcSettingModeRequest {
    /// DTC-setting state: "on" or "off".
    pub value: String,
}

/// Response for `modes/dtcsetting` (GET + PUT). Mirrors the session shape.
#[derive(Debug, Serialize)]
pub struct DtcSettingModeResponse {
    /// Mode identifier (always "dtcsetting").
    pub id: String,
    /// Current DTC-setting state ("on"/"off").
    pub value: String,
}

/// GET /vehicle/v1/components/:component_id/modes/dtcsetting?target=child/path
pub async fn get_dtc_setting_mode(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
    Query(query): Query<ModeQuery>,
) -> Result<Json<DtcSettingModeResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;
    let target_backend: Arc<dyn DiagnosticBackend> = if let Some(ref target) = query.target {
        resolve_target(backend, target).await?
    } else {
        backend.clone()
    };
    let mode = target_backend.get_dtc_setting().await?;
    Ok(Json(DtcSettingModeResponse {
        id: "dtcsetting".to_string(),
        value: mode.value,
    }))
}

/// PUT /vehicle/v1/components/:component_id/modes/dtcsetting?target=child/path
/// Sends UDS ControlDTCSetting (0x85) on (0x01) / off (0x02).
pub async fn put_dtc_setting_mode(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
    Query(query): Query<ModeQuery>,
    Json(request): Json<DtcSettingModeRequest>,
) -> Result<Json<DtcSettingModeResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;
    let target_backend: Arc<dyn DiagnosticBackend> = if let Some(ref target) = query.target {
        resolve_target(backend, target).await?
    } else {
        backend.clone()
    };
    let mode = target_backend.set_dtc_setting(&request.value).await?;
    Ok(Json(DtcSettingModeResponse {
        id: "dtcsetting".to_string(),
        value: mode.value,
    }))
}

// =============================================================================
// Security Mode Handlers
// =============================================================================

/// GET /vehicle/v1/components/:component_id/modes/security?target=child/path
/// Get current security access state (SOVD standard format)
pub async fn get_security_mode(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
    Query(query): Query<ModeQuery>,
) -> Result<Json<SecurityModeGetResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;
    let target_backend: Arc<dyn DiagnosticBackend> = if let Some(ref target) = query.target {
        resolve_target(backend, target).await?
    } else {
        backend.clone()
    };
    let mode = target_backend.get_security_mode().await?;

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

/// PUT /vehicle/v1/components/:component_id/modes/security?target=child/path
/// Request seed or send key for security access (SOVD standard format)
///
/// Returns different response types based on the operation:
/// - Request seed: `{"id": "security", "seed": "aabbccdd"}` — seed is
///   concatenated lowercase hex per spec `string:hex` primitive.
/// - Send key: `{"id": "security", "value": "level1"}`
pub async fn put_security_mode(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
    Query(query): Query<ModeQuery>,
    Json(request): Json<SecurityModeRequest>,
) -> Result<axum::response::Response, ApiError> {
    use axum::response::IntoResponse;

    let backend = state.get_backend(&component_id)?;
    let target_backend: Arc<dyn DiagnosticBackend> = if let Some(ref target) = query.target {
        resolve_target(backend, target).await?
    } else {
        backend.clone()
    };

    let key_bytes = request
        .key
        .as_ref()
        .map(hex::decode)
        .transpose()
        .map_err(|e| ApiError::BadRequest(format!("Invalid hex key: {}", e)))?;

    let is_seed_request = request.value.to_lowercase().ends_with("_requestseed");

    let mode = target_backend
        .set_security_mode(&request.value, key_bytes.as_deref())
        .await?;

    if is_seed_request {
        // Concatenated lowercase hex per spec `string:hex` primitive
        // (sovd_iso17978_spec.yaml line 192).
        let seed = mode.seed.unwrap_or_default().to_lowercase();
        Ok(Json(SecuritySeedResponse {
            id: "security".to_string(),
            seed,
        })
        .into_response())
    } else {
        // Return key response (success = unlocked)
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
