//! Mode handlers (UDS session, security, and link control)
//!
//! Implements SOVD standard mode endpoints as defined in ASAM SOVD specification.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use sovd_core::{DiagnosticBackend, LinkControlResult, SecurityState};

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

/// SOVD standard seed response (for Request_Seed)
#[derive(Debug, Serialize)]
pub struct SovdSeed {
    /// Seed bytes as space-separated hex (e.g., "0xaa 0xbb 0xcc 0xdd")
    #[serde(rename = "Request_Seed")]
    pub request_seed: String,
}

/// SOVD standard response for security seed request
#[derive(Debug, Serialize)]
pub struct SecuritySeedResponse {
    /// Mode identifier (always "security")
    pub id: String,
    /// The seed value
    pub seed: SovdSeed,
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

#[derive(Debug, Deserialize)]
pub struct LinkControlRequest {
    /// Action: "verify_fixed", "verify_specific", or "transition"
    pub action: String,
    /// Baud rate identifier for verify_fixed (e.g., "500k", "0x12")
    #[serde(default)]
    pub baud_rate_id: Option<String>,
    /// Baud rate in bps for verify_specific
    #[serde(default)]
    pub baud_rate: Option<u32>,
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
/// - Request seed: returns `{"id": "security", "seed": {"Request_Seed": "0xaa 0xbb..."}}`
/// - Send key: returns `{"id": "security", "value": "level1"}`
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
        // Return seed response
        let seed_hex = mode.seed.unwrap_or_default();
        // Convert hex string to space-separated 0xNN format
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

// =============================================================================
// Link Control Handlers
// =============================================================================

/// SOVD standard GET response for link mode
#[derive(Debug, Serialize)]
pub struct LinkModeGetResponse {
    /// Mode identifier (always "link")
    pub id: String,
    /// Current baud rate in bps
    pub current_baud_rate: u32,
    /// Pending baud rate (verified but not transitioned)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_baud_rate: Option<u32>,
    /// Link state description
    pub link_state: String,
}

/// GET /vehicle/v1/components/:component_id/modes/link
/// Get current link status
pub async fn get_link_mode(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<Json<LinkModeGetResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;
    let mode = backend.get_link_mode().await?;
    Ok(Json(LinkModeGetResponse {
        id: "link".to_string(),
        current_baud_rate: mode.current_baud_rate,
        pending_baud_rate: mode.pending_baud_rate,
        link_state: mode.link_state,
    }))
}

/// PUT /vehicle/v1/components/:component_id/modes/link
/// Control link baud rate
pub async fn put_link_mode(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
    Json(request): Json<LinkControlRequest>,
) -> Result<Json<LinkControlResult>, ApiError> {
    let backend = state.get_backend(&component_id)?;

    let result = backend
        .set_link_mode(
            &request.action,
            request.baud_rate_id.as_deref(),
            request.baud_rate,
        )
        .await?;

    Ok(Json(result))
}
