//! Data parameter handlers
//!
//! All data access is now DID-based. Parameters are identified by their
//! UDS Data Identifier (DID) in hex format (e.g., "F405", "0xF405").
//!
//! Conversions are managed via the DidStore from sovd-conv.
//! Definitions can be loaded from YAML files or registered dynamically.

use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sovd_conv::format_did;
use sovd_core::error::BackendError;

use crate::error::ApiError;
use crate::state::AppState;

// =============================================================================
// Query Parameters
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct ReadQuery {
    /// If true, return raw bytes without conversion
    #[serde(default)]
    pub raw: bool,
}

// =============================================================================
// Response Types
// =============================================================================

/// Response for listing registered DIDs
#[derive(Serialize)]
pub struct DidListResponse {
    /// Number of registered DIDs
    pub count: usize,
    /// List of registered DIDs with their conversions
    pub items: Vec<DidInfoResponse>,
}

/// Info about a registered DID
#[derive(Serialize)]
pub struct DidInfoResponse {
    /// SOVD-compliant parameter identifier (semantic name)
    /// Use this in API calls: /data/{id}
    pub id: String,
    /// DID in hex format (for UDS debugging)
    pub did: String,
    /// Display name (if set)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Data type
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_type: Option<String>,
    /// Unit (if set)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// Whether this DID supports writing
    pub writable: bool,
    /// API endpoint (uses semantic id when available)
    pub href: String,
}

/// Response for DID read operations
#[derive(Serialize)]
pub struct DidResponse {
    /// Semantic parameter ID (e.g., "vin", "coolant_temp")
    pub id: String,
    /// DID (uppercase hex, no prefix)
    pub did: String,
    /// Decoded value (if conversion registered) or raw hex string
    pub value: serde_json::Value,
    /// Unit (only if conversion registered)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// Raw hex bytes (always included)
    pub raw: String,
    /// Byte length
    pub length: usize,
    /// Whether a conversion was applied
    pub converted: bool,
    /// Timestamp
    pub timestamp: i64,
}

/// Request for DID write operations
#[derive(Deserialize)]
pub struct WriteDidRequest {
    /// Value to write (number for converted, hex string/array for raw)
    pub value: serde_json::Value,
    /// Format hint: "hex", "raw", or "auto"
    #[serde(default = "default_format")]
    pub format: String,
}

fn default_format() -> String {
    "auto".to_string()
}

// =============================================================================
// Handlers
// =============================================================================

/// GET /vehicle/v1/components/:component_id/data
/// List DIDs available for the specified component (from DidStore)
///
/// Only returns DIDs that are either:
/// - Explicitly associated with this component via the `components` field
/// - Available to all components (no `components` field specified)
///
/// For gateways/app entities, returns only the entity's own parameters.
/// Child ECU parameters are accessed via sub-entity paths per SOVD §6.5.
pub async fn list_parameters(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<Json<DidListResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;

    // For entities with sub-entities (gateways, app entities), return only the
    // entity's own parameters via backend.list_parameters(). Don't use DidStore
    // global definitions — gateways can't read ECU-level DIDs.
    // Child ECU parameters are accessed via sub-entity paths per SOVD §6.5.
    let sub_entities = backend.list_sub_entities().await.unwrap_or_default();
    if !sub_entities.is_empty() {
        let params = backend.list_parameters().await.unwrap_or_default();
        let items: Vec<DidInfoResponse> = params
            .into_iter()
            .map(|p| DidInfoResponse {
                id: p.id.clone(),
                did: p.did.unwrap_or_default(),
                name: Some(p.name),
                data_type: p.data_type,
                unit: p.unit,
                writable: !p.read_only,
                href: format!("/vehicle/v1/components/{}/data/{}", component_id, p.id),
            })
            .collect();

        return Ok(Json(DidListResponse {
            count: items.len(),
            items,
        }));
    }

    // Regular component: list DIDs filtered by component from DidStore
    let definitions = state.did_store().list_for_component(&component_id);

    // If no local DID definitions, fall back to backend.list_parameters()
    // (handles proxy backends that get parameters from a remote server)
    if definitions.is_empty() {
        if let Ok(params) = backend.list_parameters().await {
            if !params.is_empty() {
                let items: Vec<DidInfoResponse> = params
                    .into_iter()
                    .map(|p| DidInfoResponse {
                        id: p.id.clone(),
                        did: p.did.unwrap_or_default(),
                        name: Some(p.name),
                        data_type: p.data_type,
                        unit: p.unit,
                        writable: !p.read_only,
                        href: format!("/vehicle/v1/components/{}/data/{}", component_id, p.id),
                    })
                    .collect();

                return Ok(Json(DidListResponse {
                    count: items.len(),
                    items,
                }));
            }
        }
    }

    let mut items: Vec<DidInfoResponse> = definitions
        .into_iter()
        .map(|(did, def)| {
            // Use semantic id if available, otherwise fall back to DID hex
            let did_hex = format_did(did);
            let id = def.id.clone().unwrap_or_else(|| did_hex.clone());
            DidInfoResponse {
                id: id.clone(),
                did: did_hex,
                name: def.name,
                data_type: Some(def.data_type.to_string()),
                unit: def.unit,
                writable: def.writable,
                href: format!("/vehicle/v1/components/{}/data/{}", component_id, id),
            }
        })
        .collect();

    // Sort by id for consistent ordering
    items.sort_by(|a, b| a.id.cmp(&b.id));

    Ok(Json(DidListResponse {
        count: items.len(),
        items,
    }))
}

/// GET /vehicle/v1/components/:component_id/data/:did
/// Read a DID value (applies conversion if registered)
pub async fn read_parameter(
    State(state): State<AppState>,
    Path((component_id, did)): Path<(String, String)>,
    Query(query): Query<ReadQuery>,
) -> Result<Json<DidResponse>, ApiError> {
    read_did_internal(&state, &component_id, &did, query.raw).await
}

/// PUT /vehicle/v1/components/:component_id/data/:did
/// Write to a DID (applies conversion if registered)
pub async fn write_parameter(
    State(state): State<AppState>,
    Path((component_id, did)): Path<(String, String)>,
    Json(request): Json<WriteDidRequest>,
) -> Result<Json<DidResponse>, ApiError> {
    write_did_internal(&state, &component_id, &did, request).await
}

/// GET /vehicle/v1/components/:component_id/data/:child_id/:child_param_id
/// Read a parameter through a gateway (handles nested path)
pub async fn read_gateway_parameter(
    State(state): State<AppState>,
    Path((component_id, child_id, child_param_id)): Path<(String, String, String)>,
    Query(query): Query<ReadQuery>,
) -> Result<Json<DidResponse>, ApiError> {
    // Combine into prefixed format and delegate to internal handler
    let prefixed_param = format!("{}/{}", child_id, child_param_id);
    read_did_internal(&state, &component_id, &prefixed_param, query.raw).await
}

/// PUT /vehicle/v1/components/:component_id/data/:child_id/:child_param_id
/// Write a parameter through a gateway (handles nested path)
pub async fn write_gateway_parameter(
    State(state): State<AppState>,
    Path((component_id, child_id, child_param_id)): Path<(String, String, String)>,
    Json(request): Json<WriteDidRequest>,
) -> Result<Json<DidResponse>, ApiError> {
    // Combine into prefixed format and delegate to internal handler
    let prefixed_param = format!("{}/{}", child_id, child_param_id);
    write_did_internal(&state, &component_id, &prefixed_param, request).await
}

/// GET /vehicle/v1/components/:component_id/data/:gw_id/:child_id/:param_id
/// Read a parameter through a deeply nested gateway (e.g., vehicle_gw → uds_gw → engine_ecu → param)
pub async fn read_deep_gateway_parameter(
    State(state): State<AppState>,
    Path((component_id, gw_id, child_id, child_param_id)): Path<(String, String, String, String)>,
    Query(query): Query<ReadQuery>,
) -> Result<Json<DidResponse>, ApiError> {
    let prefixed_param = format!("{}/{}/{}", gw_id, child_id, child_param_id);
    read_did_internal(&state, &component_id, &prefixed_param, query.raw).await
}

/// PUT /vehicle/v1/components/:component_id/data/:gw_id/:child_id/:param_id
/// Write a parameter through a deeply nested gateway
pub async fn write_deep_gateway_parameter(
    State(state): State<AppState>,
    Path((component_id, gw_id, child_id, child_param_id)): Path<(String, String, String, String)>,
    Json(request): Json<WriteDidRequest>,
) -> Result<Json<DidResponse>, ApiError> {
    let prefixed_param = format!("{}/{}/{}", gw_id, child_id, child_param_id);
    write_did_internal(&state, &component_id, &prefixed_param, request).await
}

/// GET /vehicle/v1/components/:component_id/did/:did
/// Read a DID value (alias for /data/:did)
pub async fn read_did(
    State(state): State<AppState>,
    Path((component_id, did)): Path<(String, String)>,
    Query(query): Query<ReadQuery>,
) -> Result<Json<DidResponse>, ApiError> {
    read_did_internal(&state, &component_id, &did, query.raw).await
}

/// PUT /vehicle/v1/components/:component_id/did/:did
/// Write to a DID (alias for /data/:did)
pub async fn write_did(
    State(state): State<AppState>,
    Path((component_id, did)): Path<(String, String)>,
    Json(request): Json<WriteDidRequest>,
) -> Result<Json<DidResponse>, ApiError> {
    write_did_internal(&state, &component_id, &did, request).await
}

// =============================================================================
// Internal Implementation
// =============================================================================

async fn read_did_internal(
    state: &AppState,
    component_id: &str,
    param_id: &str,
    raw_only: bool,
) -> Result<Json<DidResponse>, ApiError> {
    let backend = state.get_backend(component_id)?;
    let did_store = state.did_store();

    // Check if this is a gateway with a prefixed parameter (e.g., "vtx_ecm/vin")
    // Gateway parameters use format: "{backend_id}/{param_id}"
    if let Some((child_backend_id, child_param_id)) = param_id.split_once('/') {
        // Get the child backend from the gateway's sub-entities
        if let Ok(child_backend) = backend.get_sub_entity(child_backend_id).await {
            // Route to the child backend via DidStore
            let child_did_u16 = match did_store.resolve_did(child_param_id) {
                Some(did) => did,
                None => {
                    // DID not in local store — fall back to read_data() on child
                    let values = child_backend
                        .read_data(&[child_param_id.to_string()])
                        .await?;

                    if let Some(dv) = values.into_iter().next() {
                        let raw = dv.raw.clone().unwrap_or_default();
                        let length = dv.length.unwrap_or(0);
                        let has_raw = !raw.is_empty();
                        return Ok(Json(DidResponse {
                            id: format!("{}/{}", child_backend_id, child_param_id),
                            did: dv.did.unwrap_or_default(),
                            value: if raw_only && has_raw {
                                serde_json::json!(raw)
                            } else {
                                dv.value
                            },
                            unit: if raw_only { None } else { dv.unit },
                            raw,
                            length,
                            converted: !raw_only && has_raw,
                            timestamp: Utc::now().timestamp_millis(),
                        }));
                    }

                    return Err(ApiError::BadRequest(format!(
                        "Unknown parameter: {}",
                        child_param_id
                    )));
                }
            };

            // Get the definition for this specific child component
            let child_def = did_store.get_for_component(child_did_u16, child_backend_id);
            let semantic_id = child_def
                .as_ref()
                .and_then(|def| def.id.clone())
                .unwrap_or_else(|| child_param_id.to_string());

            if let Some(def) = child_def {
                // Local DidStore has a definition — read raw bytes and decode locally
                let raw_bytes = child_backend.read_raw_did(child_did_u16).await?;

                if raw_only {
                    return Ok(Json(DidResponse {
                        id: format!("{}/{}", child_backend_id, semantic_id),
                        did: format_did(child_did_u16),
                        value: serde_json::json!(hex::encode(&raw_bytes)),
                        unit: None,
                        raw: hex::encode(&raw_bytes),
                        length: raw_bytes.len(),
                        converted: false,
                        timestamp: Utc::now().timestamp_millis(),
                    }));
                }

                let (value, unit, converted) = match did_store.decode(child_did_u16, &raw_bytes) {
                    Ok(decoded) => (decoded, def.unit, true),
                    Err(_) => (serde_json::json!(hex::encode(&raw_bytes)), None, false),
                };

                return Ok(Json(DidResponse {
                    id: format!("{}/{}", child_backend_id, semantic_id),
                    did: format_did(child_did_u16),
                    value,
                    unit,
                    raw: hex::encode(&raw_bytes),
                    length: raw_bytes.len(),
                    converted,
                    timestamp: Utc::now().timestamp_millis(),
                }));
            } else {
                // No local definition — fall back to read_data() on the child backend.
                // For proxy backends this returns already-decoded values from upstream.
                let values = child_backend
                    .read_data(&[child_param_id.to_string()])
                    .await?;

                if let Some(dv) = values.into_iter().next() {
                    let raw = dv.raw.clone().unwrap_or_default();
                    let length = dv.length.unwrap_or(0);
                    let has_raw = !raw.is_empty();
                    return Ok(Json(DidResponse {
                        id: format!("{}/{}", child_backend_id, semantic_id),
                        did: dv.did.unwrap_or_else(|| format_did(child_did_u16)),
                        value: if raw_only && has_raw {
                            serde_json::json!(raw)
                        } else {
                            dv.value
                        },
                        unit: if raw_only { None } else { dv.unit },
                        raw,
                        length,
                        converted: !raw_only && has_raw,
                        timestamp: Utc::now().timestamp_millis(),
                    }));
                }

                return Err(ApiError::NotFound(format!(
                    "Parameter not found: {}",
                    child_param_id
                )));
            }
        }

        // Child not in top-level backends -- route through the gateway backend.
        // This handles proxy backends where children are inside a GatewayBackend.
        let values = backend.read_data(&[param_id.to_string()]).await?;

        if let Some(dv) = values.into_iter().next() {
            let raw = dv.raw.clone().unwrap_or_default();
            let length = dv.length.unwrap_or(0);
            let has_raw = !raw.is_empty();
            return Ok(Json(DidResponse {
                id: param_id.to_string(),
                did: dv.did.unwrap_or_default(),
                value: if raw_only && has_raw {
                    serde_json::json!(raw)
                } else {
                    dv.value
                },
                unit: if raw_only { None } else { dv.unit },
                raw,
                length,
                converted: !raw_only && has_raw,
                timestamp: Utc::now().timestamp_millis(),
            }));
        }

        return Err(ApiError::NotFound(format!(
            "Parameter not found: {}",
            param_id
        )));
    }

    // Resolve parameter: try semantic name first, then DID hex format
    // This allows SOVD-compliant names like "coolant_temperature" while
    // also supporting raw DID access like "F405" for private data
    let did_u16 = match did_store.resolve_did(param_id) {
        Some(did) => did,
        None => {
            // DID not in local store — fall back to backend.read_data() for
            // proxy/app backends that resolve parameters via upstream HTTP.
            let values = backend.read_data(&[param_id.to_string()]).await?;

            if let Some(dv) = values.into_iter().next() {
                let raw = dv.raw.clone().unwrap_or_default();
                let length = dv.length.unwrap_or(0);
                let has_raw = !raw.is_empty();
                return Ok(Json(DidResponse {
                    id: param_id.to_string(),
                    did: dv.did.unwrap_or_default(),
                    value: if raw_only && has_raw {
                        serde_json::json!(raw)
                    } else {
                        dv.value
                    },
                    unit: if raw_only { None } else { dv.unit },
                    raw,
                    length,
                    converted: !raw_only && has_raw,
                    timestamp: Utc::now().timestamp_millis(),
                }));
            }

            return Err(ApiError::BadRequest(format!(
                "Unknown parameter: {}",
                param_id
            )));
        }
    };

    // Get the definition for this specific component
    let component_def = did_store.get_for_component(did_u16, component_id);

    // Get the semantic ID (from definition or fall back to param_id)
    let semantic_id = component_def
        .as_ref()
        .and_then(|def| def.id.clone())
        .unwrap_or_else(|| param_id.to_string());

    // Read raw bytes via the backend.
    // For non-ECU entities (gateways, app entities), read_raw_did is not supported.
    // Fall back to synthesizing identification data from entity_info.
    let raw_bytes = match backend.read_raw_did(did_u16).await {
        Ok(bytes) => bytes,
        Err(BackendError::NotSupported(_)) => {
            // Synthesize identification data from entity metadata
            if let Some(value) = synthesize_entity_did(did_u16, backend.entity_info()) {
                let raw = hex::encode(value.as_bytes());
                return Ok(Json(DidResponse {
                    id: semantic_id,
                    did: format_did(did_u16),
                    value: serde_json::json!(value),
                    unit: None,
                    raw,
                    length: value.len(),
                    converted: true,
                    timestamp: Utc::now().timestamp_millis(),
                }));
            }
            return Err(ApiError::NotImplemented("read_raw_did".to_string()));
        }
        Err(e) => return Err(e.into()),
    };

    // If raw_only requested, skip conversion
    if raw_only {
        return Ok(Json(DidResponse {
            id: semantic_id,
            did: format_did(did_u16),
            value: serde_json::json!(hex::encode(&raw_bytes)),
            unit: None,
            raw: hex::encode(&raw_bytes),
            length: raw_bytes.len(),
            converted: false,
            timestamp: Utc::now().timestamp_millis(),
        }));
    }

    // Try to decode using DidStore
    let (value, unit, converted) = if let Some(def) = component_def {
        match did_store.decode(did_u16, &raw_bytes) {
            Ok(decoded) => (decoded, def.unit, true),
            Err(_) => (serde_json::json!(hex::encode(&raw_bytes)), None, false),
        }
    } else {
        // No definition - return raw hex
        (serde_json::json!(hex::encode(&raw_bytes)), None, false)
    };

    Ok(Json(DidResponse {
        id: semantic_id,
        did: format_did(did_u16),
        value,
        unit,
        raw: hex::encode(&raw_bytes),
        length: raw_bytes.len(),
        converted,
        timestamp: Utc::now().timestamp_millis(),
    }))
}

async fn write_did_internal(
    state: &AppState,
    component_id: &str,
    param_id: &str,
    request: WriteDidRequest,
) -> Result<Json<DidResponse>, ApiError> {
    let backend = state.get_backend(component_id)?;
    let did_store = state.did_store();

    // Check if this is a gateway with a prefixed parameter
    if param_id.contains('/') {
        // Try gateway routing: write through the backend's write_data
        let data = convert_value_to_bytes(&request)?;
        backend.write_data(param_id, &data).await?;

        return Ok(Json(DidResponse {
            id: param_id.to_string(),
            did: String::new(),
            value: request.value,
            unit: None,
            raw: hex::encode(&data),
            length: data.len(),
            converted: false,
            timestamp: Utc::now().timestamp_millis(),
        }));
    }

    // Resolve parameter: try semantic name first, then DID hex format
    let did_u16 = did_store
        .resolve_did(param_id)
        .ok_or_else(|| ApiError::BadRequest(format!("Unknown parameter: {}", param_id)))?;

    // Get the definition for this specific component
    let component_def = did_store.get_for_component(did_u16, component_id);

    // Get the semantic ID (from definition or fall back to param_id)
    let semantic_id = component_def
        .as_ref()
        .and_then(|def| def.id.clone())
        .unwrap_or_else(|| param_id.to_string());

    // Encode the value
    let data = if component_def.is_some() {
        // If definition exists, try to encode using it
        match did_store.encode(did_u16, &request.value) {
            Ok(bytes) => bytes,
            Err(_) => convert_value_to_bytes(&request)?,
        }
    } else {
        convert_value_to_bytes(&request)?
    };

    // Write via backend
    backend.write_raw_did(did_u16, &data).await?;

    // Return response with decoded value
    let (value, unit, converted) = if let Some(def) = component_def {
        match did_store.decode(did_u16, &data) {
            Ok(decoded) => (decoded, def.unit, true),
            Err(_) => (serde_json::json!(hex::encode(&data)), None, false),
        }
    } else {
        (serde_json::json!(hex::encode(&data)), None, false)
    };

    Ok(Json(DidResponse {
        id: semantic_id,
        did: format_did(did_u16),
        value,
        unit,
        raw: hex::encode(&data),
        length: data.len(),
        converted,
        timestamp: Utc::now().timestamp_millis(),
    }))
}

/// Synthesize identification DID values for non-ECU entities (gateways, app entities)
/// that don't support raw DID reads. Returns the string value for known standard DIDs
/// using the entity's own metadata.
fn synthesize_entity_did(did: u16, info: &sovd_core::models::EntityInfo) -> Option<String> {
    use sovd_uds::uds::standard_did;
    match did {
        standard_did::SYSTEM_NAME => Some(info.name.clone()),
        standard_did::SYSTEM_SUPPLIER_ID => Some(format!("SOVDd ({})", info.entity_type)),
        standard_did::ECU_SOFTWARE_VERSION => Some(env!("CARGO_PKG_VERSION").to_string()),
        _ => None,
    }
}

/// Convert request value to bytes based on format
pub fn convert_value_to_bytes(request: &WriteDidRequest) -> Result<Vec<u8>, ApiError> {
    match request.format.as_str() {
        "hex" => {
            let hex_str = request.value.as_str().ok_or_else(|| {
                ApiError::BadRequest("hex format requires a string value".to_string())
            })?;
            hex::decode(hex_str)
                .map_err(|e| ApiError::BadRequest(format!("Invalid hex string: {}", e)))
        }
        "raw" => {
            let arr = request.value.as_array().ok_or_else(|| {
                ApiError::BadRequest("raw format requires an array of byte values".to_string())
            })?;
            arr.iter()
                .map(|v| {
                    v.as_u64()
                        .and_then(|n| if n <= 255 { Some(n as u8) } else { None })
                        .ok_or_else(|| {
                            ApiError::BadRequest("raw array values must be 0-255".to_string())
                        })
                })
                .collect()
        }
        _ => match &request.value {
            serde_json::Value::String(s) => {
                // Try hex first if it looks like hex
                if s.len() % 2 == 0 && s.chars().all(|c| c.is_ascii_hexdigit()) && s.len() >= 2 {
                    hex::decode(s).or_else(|_| Ok(s.as_bytes().to_vec()))
                } else {
                    Ok(s.as_bytes().to_vec())
                }
            }
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_u64() {
                    if i <= 0xFF {
                        Ok(vec![i as u8])
                    } else if i <= 0xFFFF {
                        Ok((i as u16).to_be_bytes().to_vec())
                    } else if i <= 0xFFFFFFFF {
                        Ok((i as u32).to_be_bytes().to_vec())
                    } else {
                        Ok(i.to_be_bytes().to_vec())
                    }
                } else {
                    Err(ApiError::BadRequest(
                        "Numeric value out of range".to_string(),
                    ))
                }
            }
            serde_json::Value::Array(arr) => arr
                .iter()
                .map(|v| {
                    v.as_u64()
                        .and_then(|n| if n <= 255 { Some(n as u8) } else { None })
                        .ok_or_else(|| {
                            ApiError::BadRequest("Array values must be 0-255".to_string())
                        })
                })
                .collect(),
            _ => Err(ApiError::BadRequest(
                "Value must be a string, number, or array".to_string(),
            )),
        },
    }
}
