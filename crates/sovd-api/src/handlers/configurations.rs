//! Configuration handlers — ISO 17978-3 §7.12.
//!
//! A configuration is a single resource, always read and written *as a whole*
//! (§7.12.3.1 / §7.12.4.1). Its representation is decided by the
//! `ConfigurationType` in the metadata (Table 145): `parameter` is a JSON
//! object of settings, `bulk` is an opaque payload with its own MIME type.
//!
//! Routes:
//!
//! * `GET    /{entity}/configurations`      — list metadata (§7.12.2); `include-schema`
//! * `DELETE /{entity}/configurations`      — reset all to default (§7.12.5.2) — 204
//! * `GET    /{entity}/configurations/{id}` — read (§7.12.3) — JSON or bulk, 406 on mismatch
//! * `PUT    /{entity}/configurations/{id}` — write (§7.12.4) — 204
//! * `DELETE /{entity}/configurations/{id}` — reset one to default (§7.12.5.3) — 204
//!
//! There is no `POST`: the spec defines no way to create a configuration
//! resource. Every route is gated on `capabilities().configurations`.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use sovd_core::{ConfigurationMetaData, ConfigurationType, ConfigurationWrite, DiagnosticBackend};

use crate::error::ApiError;
use crate::state::AppState;

/// The JSON media type — the `parameter` representation (§7.12.3.3/§7.12.4.3)
/// and the default when a write carries no `Content-Type`.
const APPLICATION_JSON: &str = "application/json";

/// `GET /{entity}/configurations` response — §7.12.2 Table 143.
#[derive(Debug, Serialize)]
pub struct ConfigurationsResponse {
    pub items: Vec<ConfigurationMetaData>,
    /// C: only provided if the query parameter `include-schema` is `true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<serde_json::Value>,
}

/// `include-schema` (Table 141 / Table 148), default `false`.
#[derive(Debug, Default, Deserialize)]
pub struct SchemaQuery {
    #[serde(rename = "include-schema", default)]
    pub include_schema: bool,
}

/// Resolve a component backend and reject it unless it advertises §7.12
/// support — mirrors `bulk_data::require_bulk_data` (a clean 501 rather than a
/// confusing empty list from the default trait methods).
fn require_configurations(
    state: &AppState,
    component_id: &str,
) -> Result<Arc<dyn DiagnosticBackend>, ApiError> {
    gate(state.get_backend(component_id)?.clone())
}

/// Same gate for an already-resolved (sub-)entity backend.
fn gate(backend: Arc<dyn DiagnosticBackend>) -> Result<Arc<dyn DiagnosticBackend>, ApiError> {
    if !backend.capabilities().configurations {
        return Err(ApiError::NotImplemented(
            "This component does not support configurations".to_string(),
        ));
    }
    Ok(backend)
}

/// Schema of the `items` array, emitted when `include-schema=true`
/// (§7.12.2 Table 143 — an OpenAPI Schema Object describing the response).
///
/// The `type` property is spelled as a plain string enum here; the spec's
/// example nests it under a `ConfigurationType` key, which is not a valid
/// Schema Object — the enum itself is Table 145.
fn items_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["items"],
        "properties": {
            "items": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["id", "name", "type"],
                    "properties": {
                        "id": { "type": "string" },
                        "name": { "type": "string" },
                        "translation_id": { "type": "string" },
                        "type": { "type": "string", "enum": ["bulk", "parameter"] },
                        "version": { "type": "string" },
                        "content_type": { "type": "string" }
                    }
                }
            }
        }
    })
}

/// Does `accept` admit `mime`?  Server-driven content negotiation per
/// §7.12.3.1: an absent or empty `Accept` means "no preference", and the
/// server returns its default representation.
fn accepts(accept: Option<&str>, mime: &str) -> bool {
    let Some(accept) = accept.map(str::trim).filter(|a| !a.is_empty()) else {
        return true;
    };
    let mime = mime.split(';').next().unwrap_or_default().trim();
    let Some((want_type, want_sub)) = mime.split_once('/') else {
        return false;
    };
    accept.split(',').any(|range| {
        let range = range.split(';').next().unwrap_or_default().trim();
        match range.split_once('/') {
            Some((t, s)) => {
                (t == "*" || t.eq_ignore_ascii_case(want_type))
                    && (s == "*" || s.eq_ignore_ascii_case(want_sub))
            }
            None => false,
        }
    })
}

/// Header value as `&str`, or `None` when absent/non-ASCII.
fn header_str(headers: &HeaderMap, name: header::HeaderName) -> Option<&str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// Look up one configuration's metadata — the type decides which
/// representation the read/reset path uses (§7.12.3.1).
async fn find_metadata(
    backend: &Arc<dyn DiagnosticBackend>,
    configuration_id: &str,
) -> Result<ConfigurationMetaData, ApiError> {
    backend
        .list_configurations()
        .await?
        .into_iter()
        .find(|c| c.id == configuration_id)
        .ok_or_else(|| {
            ApiError::from(sovd_core::BackendError::ConfigurationNotFound(
                configuration_id.to_string(),
            ))
        })
}

// =============================================================================
// Shared implementations (component root and sub-entity paths differ only in
// how the backend is resolved).
// =============================================================================

async fn list_impl(
    backend: Arc<dyn DiagnosticBackend>,
    include_schema: bool,
) -> Result<Json<ConfigurationsResponse>, ApiError> {
    let items = backend.list_configurations().await?;
    Ok(Json(ConfigurationsResponse {
        items,
        schema: include_schema.then(items_schema),
    }))
}

async fn read_impl(
    backend: Arc<dyn DiagnosticBackend>,
    configuration_id: &str,
    accept: Option<&str>,
    include_schema: bool,
) -> Result<Response, ApiError> {
    let meta = find_metadata(&backend, configuration_id).await?;
    match meta.configuration_type {
        // §7.12.3.3 — the parameter representation is `application/json`;
        // 406 (no body) when the client won't take it (Table 149).
        ConfigurationType::Parameter => {
            if !accepts(accept, APPLICATION_JSON) {
                return Err(ApiError::NotAcceptable(format!(
                    "Configuration '{configuration_id}' is only available as {APPLICATION_JSON}"
                )));
            }
            let mut value = backend.read_configuration(configuration_id).await?;
            if !include_schema {
                value.schema = None;
            }
            Ok(Json(value).into_response())
        }
        // §7.12.3.2 — bulk data in its own MIME type (Table 147). The
        // metadata's `content_type` is what the entity can produce, so the
        // negotiation happens here; the backend still gets the `Accept` header
        // so it may pick something more specific (and answer `NotAcceptable`
        // if it cannot).
        ConfigurationType::Bulk => {
            let advertised = meta.content_type.as_deref().unwrap_or(
                // Table 144 marks content_type mandatory for bulk; fall back to
                // the §7.12.3.2 default rather than refusing the read.
                "application/octet-stream",
            );
            if !accepts(accept, advertised) {
                return Err(ApiError::NotAcceptable(format!(
                    "Configuration '{configuration_id}' is only available as {advertised}"
                )));
            }
            let bulk = backend
                .read_bulk_configuration(configuration_id, accept)
                .await?;
            Ok(([(header::CONTENT_TYPE, bulk.content_type)], bulk.body).into_response())
        }
    }
}

async fn write_impl(
    backend: Arc<dyn DiagnosticBackend>,
    configuration_id: &str,
    content_type: Option<&str>,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    // §7.12.4.1: the client tells the server in `Content-Type` which
    // representation the body carries — `application/json` is the parameter
    // form (Table 152), anything else is bulk data.
    let content_type = content_type.unwrap_or(APPLICATION_JSON);
    let media_type = content_type.split(';').next().unwrap_or_default().trim();
    if media_type.eq_ignore_ascii_case(APPLICATION_JSON) {
        let write: ConfigurationWrite = serde_json::from_slice(&body)
            .map_err(|e| ApiError::BadRequest(format!("Invalid configuration write body: {e}")))?;
        backend
            .write_configuration(configuration_id, &write)
            .await?;
    } else {
        backend
            .write_bulk_configuration(configuration_id, content_type, &body)
            .await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

// =============================================================================
// Component-root routes — /vehicle/v1/components/{component_id}/configurations
// =============================================================================

/// GET /vehicle/v1/components/:component_id/configurations
pub async fn list_configurations(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
    Query(query): Query<SchemaQuery>,
) -> Result<Json<ConfigurationsResponse>, ApiError> {
    let backend = require_configurations(&state, &component_id)?;
    list_impl(backend, query.include_schema).await
}

/// GET /vehicle/v1/components/:component_id/configurations/:configuration_id
pub async fn read_configuration(
    State(state): State<AppState>,
    Path((component_id, configuration_id)): Path<(String, String)>,
    Query(query): Query<SchemaQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let backend = require_configurations(&state, &component_id)?;
    let accept = header_str(&headers, header::ACCEPT);
    read_impl(backend, &configuration_id, accept, query.include_schema).await
}

/// PUT /vehicle/v1/components/:component_id/configurations/:configuration_id
pub async fn write_configuration(
    State(state): State<AppState>,
    Path((component_id, configuration_id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    let backend = require_configurations(&state, &component_id)?;
    let content_type = header_str(&headers, header::CONTENT_TYPE);
    write_impl(backend, &configuration_id, content_type, body).await
}

/// DELETE /vehicle/v1/components/:component_id/configurations/:configuration_id
pub async fn reset_configuration(
    State(state): State<AppState>,
    Path((component_id, configuration_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let backend = require_configurations(&state, &component_id)?;
    backend.reset_configuration(&configuration_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// DELETE /vehicle/v1/components/:component_id/configurations
pub async fn reset_configurations(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let backend = require_configurations(&state, &component_id)?;
    backend.reset_all_configurations().await?;
    Ok(StatusCode::NO_CONTENT)
}

// =============================================================================
// Sub-entity routes — .../components/{component_id}/apps/{app_id}/configurations
// Every sub-entity inherits the full resource set (§6.5), same as data/faults.
// =============================================================================

/// GET .../apps/:app_id/configurations
pub async fn list_sub_entity_configurations(
    State(state): State<AppState>,
    Path((component_id, app_id)): Path<(String, String)>,
    Query(query): Query<SchemaQuery>,
) -> Result<Json<ConfigurationsResponse>, ApiError> {
    let backend = gate(super::sub_entity::resolve(&state, &component_id, &app_id).await?)?;
    list_impl(backend, query.include_schema).await
}

/// GET .../apps/:app_id/configurations/:configuration_id
pub async fn read_sub_entity_configuration(
    State(state): State<AppState>,
    Path((component_id, app_id, configuration_id)): Path<(String, String, String)>,
    Query(query): Query<SchemaQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let backend = gate(super::sub_entity::resolve(&state, &component_id, &app_id).await?)?;
    let accept = header_str(&headers, header::ACCEPT);
    read_impl(backend, &configuration_id, accept, query.include_schema).await
}

/// PUT .../apps/:app_id/configurations/:configuration_id
pub async fn write_sub_entity_configuration(
    State(state): State<AppState>,
    Path((component_id, app_id, configuration_id)): Path<(String, String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    let backend = gate(super::sub_entity::resolve(&state, &component_id, &app_id).await?)?;
    let content_type = header_str(&headers, header::CONTENT_TYPE);
    write_impl(backend, &configuration_id, content_type, body).await
}

/// DELETE .../apps/:app_id/configurations/:configuration_id
pub async fn reset_sub_entity_configuration(
    State(state): State<AppState>,
    Path((component_id, app_id, configuration_id)): Path<(String, String, String)>,
) -> Result<StatusCode, ApiError> {
    let backend = gate(super::sub_entity::resolve(&state, &component_id, &app_id).await?)?;
    backend.reset_configuration(&configuration_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// DELETE .../apps/:app_id/configurations
pub async fn reset_sub_entity_configurations(
    State(state): State<AppState>,
    Path((component_id, app_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let backend = gate(super::sub_entity::resolve(&state, &component_id, &app_id).await?)?;
    backend.reset_all_configurations().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::accepts;

    #[test]
    fn accept_negotiation() {
        // Absent / empty / wildcard ⇒ the default representation is fine.
        assert!(accepts(None, "application/json"));
        assert!(accepts(Some("  "), "application/octet-stream"));
        assert!(accepts(Some("*/*"), "application/octet-stream"));
        assert!(accepts(Some("application/*"), "application/json"));
        // Exact and parameterised matches.
        assert!(accepts(Some("application/json"), "application/json"));
        assert!(accepts(
            Some("text/html, application/json;q=0.9"),
            "application/json"
        ));
        assert!(accepts(
            Some("multipart/form-data"),
            "multipart/form-data; boundary=x"
        ));
        // Mismatches ⇒ 406.
        assert!(!accepts(
            Some("application/json"),
            "application/octet-stream"
        ));
        assert!(!accepts(
            Some("application/octet-stream"),
            "application/json"
        ));
        assert!(!accepts(Some("text/*"), "application/json"));
    }
}
