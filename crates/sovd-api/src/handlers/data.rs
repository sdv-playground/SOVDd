//! Data parameter handlers
//!
//! All data access is now DID-based. Parameters are identified by their
//! UDS Data Identifier (DID) in hex format (e.g., "F405", "0xF405").
//!
//! Conversions are managed via the DidStore from sovd-conv.
//! Definitions can be loaded from YAML files or registered dynamically.

use axum::extract::{Path, Query, RawQuery, State};
use axum::Json;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sovd_conv::format_did;
use sovd_core::error::BackendError;
use sovd_core::DataCategory;

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
    /// Spec §5.7: sibling i18n key for the `name` field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub translation_id: Option<String>,
    /// Data type
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_type: Option<String>,
    /// Unit (if set)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// ISO 17978-3 §7.9 data category (Table 70 / `ValueMetaData.category`).
    /// Resolved from the DID definition (explicit `category:` or DID-number
    /// default) or carried up from a backend `ParameterInfo`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<sovd_core::DataCategory>,
    /// Whether this DID supports writing
    pub writable: bool,
    /// API endpoint (uses semantic id when available)
    pub href: String,
}

/// ISO 17978-3 §7.9.2.1 Table 72 — body of `GET /data-categories`.
#[derive(Serialize)]
pub struct DataCategoryListResponse {
    /// Supported categories (Table 73 `DataCategoryInformation`).
    pub items: Vec<DataCategoryInformation>,
}

/// ISO 17978-3 §7.9.2.1 Table 73 — `DataCategoryInformation` type.
#[derive(Serialize)]
pub struct DataCategoryInformation {
    /// Category name (Table 70 `DataCategory`).
    pub item: DataCategory,
    /// Optional identifier for translating the category name (§5.7).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category_translation_id: Option<String>,
}

impl DataCategoryInformation {
    /// Build a `DataCategoryInformation` with a derived `category_translation_id`
    /// (`TID_<wire>`, matching the spec example), so clients have a stable i18n
    /// key per category.
    fn new(category: DataCategory) -> Self {
        Self {
            item: category,
            category_translation_id: Some(format!("TID_{}", category.as_wire())),
        }
    }
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
    /// Server-side read time, RFC 3339 (ISO 17978-3 C-050).
    pub timestamp: String,
}

/// Request for a DID write — spec `{value}` body (ISO 17978-2 ≈line 489:
/// "the value(s) to be written").
///
/// Whether `value` is a physical/converted value or a raw byte
/// representation is inferred from the DID definition (a registered
/// conversion → physical; none → raw), not from a body hint. The
/// previous non-spec `format` field was removed for C-131 — no consumer
/// sent it, and SOVDd stays spec-pure. Any extra keys in the body (e.g.
/// a stray `format`) are ignored by serde, so an old client doesn't 500.
#[derive(Deserialize)]
pub struct WriteDidRequest {
    /// Value to write (physical value for a converted DID; hex string or
    /// byte array for a raw DID).
    pub value: serde_json::Value,
}

// =============================================================================
// Handlers
// =============================================================================

/// Parse the ISO 17978-3 §7.9 `?categories=` filter (Table 78) from the raw
/// query string.
///
/// C-064 `explode=true`: each value is its own repeated `categories=` key,
/// OR-combined.  Absent (or only unknown/custom tokens) → `None`, meaning no
/// category filter is applied.  Unknown tokens are ignored rather than
/// rejected so a client probing a custom (`x-<ext>-…`) category degrades to
/// "no match" instead of a 400.
fn parse_category_filter(raw_query: &Option<String>) -> Option<Vec<DataCategory>> {
    let raw = raw_query.as_deref()?;
    let mut cats: Vec<DataCategory> = Vec::new();
    let mut present = false;
    for pair in raw.split('&').filter(|s| !s.is_empty()) {
        let (key, val) = pair.split_once('=').unwrap_or((pair, ""));
        if key == "categories" {
            present = true;
            // DataCategory tokens are alnum camelCase — no percent-decoding
            // needed (mirrors the `?parameters=` handling in streams.rs).
            for v in val.split(',').map(str::trim).filter(|v| !v.is_empty()) {
                if let Some(c) = DataCategory::from_wire(v) {
                    if !cats.contains(&c) {
                        cats.push(c);
                    }
                }
            }
        }
    }
    // The key was present (filter requested) → always return Some, even if it
    // resolved to an empty set (all-unknown tokens), so the result is the
    // empty list rather than the unfiltered list.
    present.then_some(cats)
}

/// Retain only items whose category is in the requested set (if any).
fn apply_category_filter(items: &mut Vec<DidInfoResponse>, filter: &Option<Vec<DataCategory>>) {
    if let Some(wanted) = filter {
        items.retain(|item| item.category.is_some_and(|c| wanted.contains(&c)));
    }
}

/// GET /vehicle/v1/components/:component_id/data
/// List DIDs available for the specified component (from DidStore)
///
/// Only returns DIDs that are either:
/// - Explicitly associated with this component via the `components` field
/// - Available to all components (no `components` field specified)
///
/// For gateways/app entities, returns only the entity's own parameters.
/// Child ECU parameters are accessed via sub-entity paths per SOVD §6.5.
///
/// ISO 17978-3 §7.9: `?categories=` (Table 78, explode=true / OR-combined)
/// filters the returned `ValueMetaData` by their `category`. Absent → no
/// filter.
pub async fn list_parameters(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<DidListResponse>, ApiError> {
    let category_filter = parse_category_filter(&raw_query);

    let mut items = resolve_data_items(&state, &component_id).await?;
    apply_category_filter(&mut items, &category_filter);

    // Sort by id for consistent ordering
    items.sort_by(|a, b| a.id.cmp(&b.id));

    Ok(Json(DidListResponse {
        count: items.len(),
        items,
    }))
}

/// Resolve the component's data parameters as category-bearing
/// [`DidInfoResponse`] items (unfiltered, unsorted).
///
/// Shared by `list_parameters` (§7.9 `GET /data`) and `list_data_categories`
/// (`GET /data-categories`) so both see the identical set + category tagging.
/// Resolution order mirrors the spec data model:
///   1. entities with sub-entities (gateway/app) → backend `list_parameters`;
///   2. regular ECU components → DidStore definitions for the component;
///   3. proxy backends with no local DIDs → backend `list_parameters` fallback.
async fn resolve_data_items(
    state: &AppState,
    component_id: &str,
) -> Result<Vec<DidInfoResponse>, ApiError> {
    let backend = state.get_backend(component_id)?;

    // For entities with sub-entities (gateways, app entities), return only the
    // entity's own parameters via backend.list_parameters(). Don't use DidStore
    // global definitions — gateways can't read ECU-level DIDs.
    // Child ECU parameters are accessed via sub-entity paths per SOVD §6.5.
    let sub_entities = backend.list_sub_entities().await.unwrap_or_default();
    if !sub_entities.is_empty() {
        let params = backend.list_parameters().await.unwrap_or_default();
        return Ok(params
            .into_iter()
            .map(|p| param_info_to_did_info(p, component_id))
            .collect());
    }

    // Regular component: list DIDs filtered by component from DidStore
    let definitions = state.did_store().list_for_component(component_id);

    // If no local DID definitions, fall back to backend.list_parameters()
    // (handles proxy backends that get parameters from a remote server)
    if definitions.is_empty() {
        if let Ok(params) = backend.list_parameters().await {
            if !params.is_empty() {
                return Ok(params
                    .into_iter()
                    .map(|p| param_info_to_did_info(p, component_id))
                    .collect());
            }
        }
    }

    Ok(definitions
        .into_iter()
        .map(|(did, def)| {
            // Use semantic id if available, otherwise fall back to DID hex
            let did_hex = format_did(did);
            let id = def.id.clone().unwrap_or_else(|| did_hex.clone());
            // §7.9 category: explicit definition category wins, else default
            // by DID number (identification range vs measurement).
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
                href: format!("/vehicle/v1/components/{}/data/{}", component_id, id),
            }
        })
        .collect())
}

/// GET /vehicle/v1/components/:component_id/data-categories
///
/// ISO 17978-3 §7.9.2.1 (Table 72/73): enumerate the *distinct* data
/// categories present across the component's data resources. Each entry is a
/// `DataCategoryInformation` (`{item, category_translation_id?}`).
pub async fn list_data_categories(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<Json<DataCategoryListResponse>, ApiError> {
    let items = resolve_data_items(&state, &component_id).await?;

    // Distinct categories, in first-seen order for stable output.
    let mut seen: Vec<DataCategory> = Vec::new();
    for item in &items {
        if let Some(c) = item.category {
            if !seen.contains(&c) {
                seen.push(c);
            }
        }
    }

    let items = seen.into_iter().map(DataCategoryInformation::new).collect();
    Ok(Json(DataCategoryListResponse { items }))
}

/// Convert a backend [`ParameterInfo`] into the wire [`DidInfoResponse`],
/// carrying the §7.9 category through. When a backend leaves `category`
/// unset but exposes a hex DID, fall back to the DID-number default so the
/// list item still satisfies `ValueMetaData.category` (M).
fn param_info_to_did_info(p: sovd_core::ParameterInfo, component_id: &str) -> DidInfoResponse {
    let did = p.did.unwrap_or_default();
    let category = p.category.or_else(|| {
        if did.is_empty() {
            None
        } else {
            Some(DataCategory::from_did_str(&did))
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
        href: format!("/vehicle/v1/components/{}/data/{}", component_id, p.id),
    }
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

/// PUT /vehicle/v1/components/:component_id/data/:did — 204 No Content per spec.
pub async fn write_parameter(
    State(state): State<AppState>,
    Path((component_id, did)): Path<(String, String)>,
    Json(request): Json<WriteDidRequest>,
) -> Result<axum::http::StatusCode, ApiError> {
    let _ = write_did_internal(&state, &component_id, &did, request).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
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

    // Child-ECU parameters behind a gateway are addressed via the
    // sub-entity path (`/apps/{child}/data/{param}` → handlers::sub_entity),
    // not a slashed `param_id` here.  The flat gateway data-routing branch
    // was retired for C-021 (single canonical data-addressing path).

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
                    timestamp: Utc::now().to_rfc3339(),
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
                    timestamp: Utc::now().to_rfc3339(),
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
            timestamp: Utc::now().to_rfc3339(),
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
        timestamp: Utc::now().to_rfc3339(),
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

    // Child-ECU writes behind a gateway are addressed via the sub-entity
    // path (`/apps/{child}/data/{param}` → handlers::sub_entity), not a
    // slashed `param_id` here.  Flat gateway routing retired for C-021.

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

    // Raw-vs-converted inference (C-131): a DID whose definition carries a
    // real conversion → `value` is the physical value, encoded via the
    // definition; a DID with no definition (or a bare raw-`Bytes` passthrough)
    // → `value` is a raw byte representation. This replaces the old `format`
    // body hint.
    let has_conversion = component_def.as_ref().is_some_and(|d| d.has_conversion());
    let data = if has_conversion {
        match did_store.encode(did_u16, &request.value) {
            Ok(bytes) => bytes,
            Err(_) => convert_value_to_bytes(&request.value)?,
        }
    } else {
        convert_value_to_bytes(&request.value)?
    };

    // Write via backend
    backend.write_raw_did(did_u16, &data).await?;

    // Return response with the value as it round-trips: decoded physical for a
    // converted DID, raw hex for a raw/undefined DID.
    let (value, unit, converted) = match component_def {
        Some(def) if def.has_conversion() => match did_store.decode(did_u16, &data) {
            Ok(decoded) => (decoded, def.unit, true),
            Err(_) => (serde_json::json!(hex::encode(&data)), None, false),
        },
        _ => (serde_json::json!(hex::encode(&data)), None, false),
    };

    Ok(Json(DidResponse {
        id: semantic_id,
        did: format_did(did_u16),
        value,
        unit,
        raw: hex::encode(&data),
        length: data.len(),
        converted,
        timestamp: Utc::now().to_rfc3339(),
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

/// Convert a *raw* write `value` to bytes — the path taken when the DID has
/// no conversion definition (so `value` is a raw byte representation, not a
/// physical value).
///
/// The raw-vs-converted decision is made by the caller from the DID
/// definition (`write_did_internal` tries `did_store.encode` first when a
/// definition exists); this function is the raw fallback and infers the byte
/// encoding from the JSON value shape:
///   * string → hex if it parses as hex (even length, all hex digits), else
///     the UTF-8 bytes;
///   * number → minimal big-endian unsigned encoding;
///   * array  → each element a byte (0-255).
pub fn convert_value_to_bytes(value: &serde_json::Value) -> Result<Vec<u8>, ApiError> {
    match value {
        serde_json::Value::String(s) => {
            // Treat as hex if it looks like hex; otherwise raw UTF-8 bytes.
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
                    .ok_or_else(|| ApiError::BadRequest("Array values must be 0-255".to_string()))
            })
            .collect(),
        _ => Err(ApiError::BadRequest(
            "Value must be a string, number, or array".to_string(),
        )),
    }
}
