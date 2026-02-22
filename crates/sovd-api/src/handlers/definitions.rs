//! DID Definition management handlers
//!
//! These endpoints allow dynamic loading and management of DID definitions.
//! Definitions can be uploaded as YAML or JSON.

use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use sovd_conv::{format_did, parse_did, DidDefinition, DidStore};

use crate::error::ApiError;
use crate::state::AppState;

// =============================================================================
// Query Parameters
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    /// Output format: "json" (default) or "yaml"
    #[serde(default)]
    pub format: Option<String>,
}

// =============================================================================
// Request/Response Types
// =============================================================================

/// Response for definition upload
#[derive(Serialize)]
pub struct UploadResponse {
    pub status: String,
    pub loaded: usize,
    pub dids: Vec<String>,
}

/// Response for listing definitions
#[derive(Serialize)]
pub struct DefinitionsListResponse {
    pub count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<sovd_conv::StoreMeta>,
    pub dids: Vec<DefinitionSummary>,
}

/// Summary of a single definition
#[derive(Serialize)]
pub struct DefinitionSummary {
    /// SOVD-compliant semantic identifier
    pub id: String,
    /// Raw DID in hex format
    pub did: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub data_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

/// Response for delete operation
#[derive(Serialize)]
pub struct DeleteResponse {
    pub status: String,
    pub deleted: String,
}

// =============================================================================
// Handlers
// =============================================================================

/// POST /admin/definitions
/// Upload definitions from YAML or JSON
pub async fn upload_definitions(
    State(state): State<AppState>,
    body: String,
) -> Result<Json<UploadResponse>, ApiError> {
    let did_store = state.did_store();

    // Try to parse as YAML (which is a superset of JSON)
    let loaded_store: DidStore = DidStore::from_yaml(&body)
        .map_err(|e| ApiError::BadRequest(format!("Invalid YAML/JSON: {}", e)))?;

    // Get the DIDs that were loaded
    let loaded_dids: Vec<String> = loaded_store
        .list()
        .iter()
        .map(|&did| format_did(did))
        .collect();

    let count = loaded_dids.len();

    // Merge into the existing store
    for did in loaded_store.list() {
        if let Some(def) = loaded_store.get(did) {
            did_store.register(did, def);
        }
    }

    // Update metadata if present
    let loaded_meta = loaded_store.meta();
    if loaded_meta.name.is_some() || loaded_meta.version.is_some() {
        did_store.set_meta(loaded_meta);
    }

    Ok(Json(UploadResponse {
        status: "ok".to_string(),
        loaded: count,
        dids: loaded_dids,
    }))
}

/// GET /admin/definitions
/// List all registered definitions
pub async fn list_definitions(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let did_store = state.did_store();

    // Check if YAML export requested
    if query.format.as_deref() == Some("yaml") {
        let yaml = did_store
            .to_yaml()
            .map_err(|e| ApiError::Internal(format!("Failed to export YAML: {}", e)))?;

        return Ok((
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/yaml")],
            yaml,
        )
            .into_response());
    }

    // Default: JSON response
    let definitions = did_store.list_all();
    let meta = did_store.meta();

    let mut dids: Vec<DefinitionSummary> = definitions
        .into_iter()
        .map(|(did, def)| {
            let did_hex = format_did(did);
            DefinitionSummary {
                id: def.id.clone().unwrap_or_else(|| did_hex.clone()),
                did: did_hex,
                name: def.name,
                data_type: def.data_type.to_string(),
                unit: def.unit,
            }
        })
        .collect();

    // Sort by id
    dids.sort_by(|a, b| a.id.cmp(&b.id));

    let response = DefinitionsListResponse {
        count: dids.len(),
        meta: if meta.name.is_some() || meta.version.is_some() {
            Some(meta)
        } else {
            None
        },
        dids,
    };

    Ok(Json(response).into_response())
}

/// GET /admin/definitions/:did
/// Get a single definition
pub async fn get_definition(
    State(state): State<AppState>,
    Path(did): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let did_store = state.did_store();

    let did_u16 = parse_did(&did)
        .map_err(|_| ApiError::BadRequest(format!("Invalid DID format: {}", did)))?;

    let def = did_store
        .get(did_u16)
        .ok_or_else(|| ApiError::NotFound(format!("Definition not found: {}", did)))?;

    // Serialize the definition
    let mut value = serde_json::to_value(&def)
        .map_err(|e| ApiError::Internal(format!("Serialization error: {}", e)))?;

    let did_hex = format_did(did_u16);

    // Add the DID and id to the response
    if let serde_json::Value::Object(ref mut map) = value {
        map.insert("did".to_string(), serde_json::json!(&did_hex));
        // Ensure id is always present (use semantic id if available, else DID)
        if !map.contains_key("id") || map.get("id") == Some(&serde_json::Value::Null) {
            map.insert("id".to_string(), serde_json::json!(&did_hex));
        }
    }

    Ok(Json(value))
}

/// DELETE /admin/definitions/:did
/// Delete a definition
pub async fn delete_definition(
    State(state): State<AppState>,
    Path(did): Path<String>,
) -> Result<Json<DeleteResponse>, ApiError> {
    let did_store = state.did_store();

    let did_u16 = parse_did(&did)
        .map_err(|_| ApiError::BadRequest(format!("Invalid DID format: {}", did)))?;

    did_store
        .remove(did_u16)
        .ok_or_else(|| ApiError::NotFound(format!("Definition not found: {}", did)))?;

    Ok(Json(DeleteResponse {
        status: "ok".to_string(),
        deleted: format_did(did_u16),
    }))
}

/// DELETE /admin/definitions
/// Clear all definitions
pub async fn clear_definitions(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let did_store = state.did_store();
    let count = did_store.len();

    did_store.clear();

    Ok(Json(serde_json::json!({
        "status": "ok",
        "cleared": count
    })))
}

/// PUT /admin/definitions/:did
/// Register or update a single definition
pub async fn put_definition(
    State(state): State<AppState>,
    Path(did): Path<String>,
    Json(def): Json<DidDefinition>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let did_store = state.did_store();

    let did_u16 = parse_did(&did)
        .map_err(|_| ApiError::BadRequest(format!("Invalid DID format: {}", did)))?;

    did_store.register(did_u16, def);

    Ok(Json(serde_json::json!({
        "status": "ok",
        "did": format_did(did_u16)
    })))
}
