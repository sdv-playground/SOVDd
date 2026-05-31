//! Dynamic data list handlers — ISO 17978-3 §5.3.6 (`data-lists`) + §7.14
//! (`operations.executions` for defining new lists).
//!
//! Wire shape:
//!   POST /vehicle/v1/components/:id/operations/define-data/executions
//!     body: { "ddid": "0xF200", "source_dids": [...] }
//!     → defines a UDS dynamic DID via 0x2C 0x02; returns the resulting
//!       `data-lists/{list_id}` reference.
//!   GET  /vehicle/v1/components/:id/data-lists
//!     → list defined DDIDs.
//!   GET  /vehicle/v1/components/:id/data-lists/{list_id}
//!     → read the current value of the dynamic DID via UDS 0x22.
//!   DELETE /vehicle/v1/components/:id/data-lists/{list_id}
//!     → clear via UDS 0x2C 0x03.
//!
//! `{list_id}` is the dynamic DID in hex form, e.g. `F200` or `0xF200`.
//! UDS reserves 0xF200-0xF3FF for dynamic DIDs.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sovd_conv::format_did;

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct DefineDataRequest {
    /// Dynamic DID to create (hex string, e.g. "0xF200")
    pub ddid: String,
    /// Source DID slices
    pub source_dids: Vec<SourceDidDefinition>,
}

#[derive(Debug, Deserialize)]
pub struct SourceDidDefinition {
    /// Source DID (hex string)
    pub did: String,
    /// Byte position within source data (1-based)
    pub position: u8,
    /// Number of bytes to extract
    pub size: u8,
}

/// Execution result for `operations/define-data/executions`.
///
/// Synchronous completion — the dynamic DID is defined and addressable via
/// `data-lists/{list_id}` before this response returns.
#[derive(Debug, Serialize)]
pub struct DefineDataExecution {
    /// SOVD execution status (always `completed` here — sync).
    pub status: String,
    /// DDID in canonical form, e.g. "0xF200".
    pub ddid: String,
    /// Resolved data-list identifier (DDID in uppercase hex without prefix).
    pub list_id: String,
    /// HATEOAS link to the resulting data-list.
    pub href: String,
}

#[derive(Debug, Serialize)]
pub struct DataListsResponse {
    pub items: Vec<DataListInfo>,
    pub total_count: usize,
}

#[derive(Debug, Serialize)]
pub struct DataListInfo {
    pub id: String,
    pub ddid: String,
    pub href: String,
}

#[derive(Debug, Serialize)]
pub struct DataListReadResponse {
    pub id: String,
    pub ddid: String,
    pub raw: String,
    pub length: usize,
    /// RFC 3339 read time (ISO 17978-3 C-050).
    pub timestamp: String,
}

fn parse_did(s: &str) -> Result<u16, ApiError> {
    let cleaned = s.trim_start_matches("0x").trim_start_matches("0X");
    u16::from_str_radix(cleaned, 16)
        .map_err(|_| ApiError::BadRequest(format!("Invalid DID format: {}", s)))
}

fn list_id_for(ddid: u16) -> String {
    format!("{:04X}", ddid)
}

/// POST /vehicle/v1/components/:component_id/operations/define-data/executions
pub async fn define_data(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
    Json(request): Json<DefineDataRequest>,
) -> Result<(StatusCode, Json<DefineDataExecution>), ApiError> {
    let backend = state.get_backend(&component_id)?;

    let ddid = parse_did(&request.ddid)?;

    if !(0xF200..=0xF3FF).contains(&ddid) {
        return Err(ApiError::BadRequest(format!(
            "DDID 0x{:04X} out of valid range (0xF200-0xF3FF)",
            ddid
        )));
    }

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

    backend.define_data_identifier(ddid, &sources).await?;

    let list_id = list_id_for(ddid);
    let href = format!(
        "/vehicle/v1/components/{}/data-lists/{}",
        component_id, list_id
    );

    Ok((
        StatusCode::CREATED,
        Json(DefineDataExecution {
            status: "completed".to_string(),
            ddid: format!("0x{:04X}", ddid),
            list_id,
            href,
        }),
    ))
}

/// GET /vehicle/v1/components/:component_id/data-lists
///
/// Return the empty list when the backend doesn't track dynamic DIDs (default
/// trait behaviour). UDS backends that hold define state can expose it by
/// implementing a richer enumerator later; for now this satisfies the spec
/// surface without inventing a fake catalogue.
pub async fn list_data_lists(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<Json<DataListsResponse>, ApiError> {
    // Resolving the backend ensures the component exists; ignore the handle.
    let _backend = state.get_backend(&component_id)?;

    Ok(Json(DataListsResponse {
        items: Vec::new(),
        total_count: 0,
    }))
}

/// GET /vehicle/v1/components/:component_id/data-lists/:list_id
pub async fn read_data_list(
    State(state): State<AppState>,
    Path((component_id, list_id)): Path<(String, String)>,
) -> Result<Json<DataListReadResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;
    let ddid = parse_did(&list_id)?;
    let raw_bytes = backend.read_raw_did(ddid).await?;

    Ok(Json(DataListReadResponse {
        id: list_id_for(ddid),
        ddid: format_did(ddid),
        raw: hex::encode(&raw_bytes),
        length: raw_bytes.len(),
        timestamp: Utc::now().to_rfc3339(),
    }))
}

/// DELETE /vehicle/v1/components/:component_id/data-lists/:list_id
pub async fn clear_data_list(
    State(state): State<AppState>,
    Path((component_id, list_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    let backend = state.get_backend(&component_id)?;
    let ddid = parse_did(&list_id)?;
    backend.clear_data_identifier(ddid).await?;
    Ok(StatusCode::NO_CONTENT)
}
