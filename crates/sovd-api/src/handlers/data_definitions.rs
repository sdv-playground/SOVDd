//! Data Definition (DDID) handlers
//!
//! Dynamically define data identifiers that combine data from multiple source DIDs.

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::state::AppState;

/// Request to create a data definition
#[derive(Debug, Deserialize)]
pub struct CreateDataDefinitionRequest {
    /// Dynamic DID to create (hex string, e.g., "0xF200")
    pub ddid: String,
    /// Source DID definitions
    pub source_dids: Vec<SourceDidDefinition>,
}

/// Source DID definition
#[derive(Debug, Deserialize)]
pub struct SourceDidDefinition {
    /// Source DID (hex string)
    pub did: String,
    /// Byte position within source data (1-based)
    pub position: u8,
    /// Number of bytes to extract
    pub size: u8,
}

/// Response for data definition creation
#[derive(Debug, Serialize)]
pub struct DataDefinitionResponse {
    pub ddid: String,
    pub message: String,
    pub href: String,
}

/// Response for data definition deletion
#[derive(Debug, Serialize)]
pub struct DeleteDataDefinitionResponse {
    pub success: bool,
    pub message: String,
}

/// Parse hex string to u16
fn parse_did(s: &str) -> Result<u16, ApiError> {
    let cleaned = s.trim_start_matches("0x").trim_start_matches("0X");
    u16::from_str_radix(cleaned, 16)
        .map_err(|_| ApiError::BadRequest(format!("Invalid DID format: {}", s)))
}

/// POST /vehicle/v1/components/:component_id/data-definitions
/// Create a dynamic data identifier (DDID) from source DIDs
pub async fn create_data_definition(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
    Json(request): Json<CreateDataDefinitionRequest>,
) -> Result<Json<DataDefinitionResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;

    // Parse DDID
    let ddid = parse_did(&request.ddid)?;

    // Validate DDID range (typically 0xF200-0xF3FF for dynamic DIDs)
    if !(0xF200..=0xF3FF).contains(&ddid) {
        return Err(ApiError::BadRequest(format!(
            "DDID 0x{:04X} out of valid range (0xF200-0xF3FF)",
            ddid
        )));
    }

    // Parse source definitions
    let mut sources = Vec::new();
    for source in &request.source_dids {
        let source_did = parse_did(&source.did)?;
        sources.push((source_did, source.position, source.size));
    }

    if sources.is_empty() {
        return Err(ApiError::BadRequest(
            "At least one source DID is required".to_string(),
        ));
    }

    // Create DDID via backend
    backend.define_data_identifier(ddid, &sources).await?;

    Ok(Json(DataDefinitionResponse {
        ddid: format!("0x{:04X}", ddid),
        message: "Dynamic data identifier created successfully".to_string(),
        href: format!(
            "/vehicle/v1/components/{}/data-definitions/0x{:04X}",
            component_id, ddid
        ),
    }))
}

/// DELETE /vehicle/v1/components/:component_id/data-definitions/:ddid
/// Clear a dynamically defined data identifier
pub async fn delete_data_definition(
    State(state): State<AppState>,
    Path((component_id, ddid_str)): Path<(String, String)>,
) -> Result<Json<DeleteDataDefinitionResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;

    // Parse DDID from path
    let ddid = parse_did(&ddid_str)?;

    // Clear DDID via backend
    backend.clear_data_identifier(ddid).await?;

    Ok(Json(DeleteDataDefinitionResponse {
        success: true,
        message: format!("DDID 0x{:04X} cleared successfully", ddid),
    }))
}
