//! Sub-entity (apps/containers) handlers

use axum::extract::{Path, State};
use axum::Json;
use serde::Serialize;

use crate::error::ApiError;
use crate::handlers::components::CapabilitiesResponse;
use crate::state::AppState;

#[derive(Serialize)]
pub struct AppsResponse {
    pub items: Vec<AppInfoResponse>,
}

#[derive(Serialize)]
pub struct AppInfoResponse {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub entity_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    pub href: String,
}

#[derive(Serialize)]
pub struct AppDetailResponse {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub entity_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    pub capabilities: CapabilitiesResponse,
    pub data: String,
    pub faults: String,
    pub logs: String,
    pub operations: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apps: Option<String>,
}

/// GET /vehicle/v1/components/:component_id/apps
/// List sub-entities (containers/apps)
pub async fn list_apps(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<Json<AppsResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;

    // Check if this backend supports sub-entities
    if !backend.capabilities().sub_entities {
        return Err(ApiError::NotImplemented(
            "This component does not have sub-entities".to_string(),
        ));
    }

    let entities = backend.list_sub_entities().await?;

    let items: Vec<AppInfoResponse> = entities
        .into_iter()
        .map(|e| AppInfoResponse {
            id: e.id.clone(),
            name: e.name.clone(),
            entity_type: e.entity_type.clone(),
            status: e.status.clone(),
            href: format!("/vehicle/v1/components/{}/apps/{}", component_id, e.id),
        })
        .collect();

    Ok(Json(AppsResponse { items }))
}

/// GET /vehicle/v1/components/:component_id/apps/:app_id/apps
/// List sub-entities of a sub-entity (e.g., ECUs behind a nested gateway)
pub async fn list_sub_entity_apps(
    State(state): State<AppState>,
    Path((component_id, app_id)): Path<(String, String)>,
) -> Result<Json<AppsResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;

    let sub_backend = backend.get_sub_entity(&app_id).await?;

    if !sub_backend.capabilities().sub_entities {
        return Err(ApiError::NotImplemented(
            "This sub-entity does not have sub-entities".to_string(),
        ));
    }

    let entities = sub_backend.list_sub_entities().await?;

    let items: Vec<AppInfoResponse> = entities
        .into_iter()
        .map(|e| AppInfoResponse {
            id: e.id.clone(),
            name: e.name.clone(),
            entity_type: e.entity_type.clone(),
            status: e.status.clone(),
            href: format!(
                "/vehicle/v1/components/{}/apps/{}/apps/{}",
                component_id, app_id, e.id
            ),
        })
        .collect();

    Ok(Json(AppsResponse { items }))
}

/// GET /vehicle/v1/components/:component_id/apps/:app_id
/// Get sub-entity details
pub async fn get_app(
    State(state): State<AppState>,
    Path((component_id, app_id)): Path<(String, String)>,
) -> Result<Json<AppDetailResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;

    // Check if this backend supports sub-entities
    if !backend.capabilities().sub_entities {
        return Err(ApiError::NotImplemented(
            "This component does not have sub-entities".to_string(),
        ));
    }

    let sub_backend = backend.get_sub_entity(&app_id).await?;
    let info = sub_backend.entity_info();
    let caps = sub_backend.capabilities();

    let base_path = format!("/vehicle/v1/components/{}/apps/{}", component_id, app_id);

    Ok(Json(AppDetailResponse {
        id: info.id.clone(),
        name: info.name.clone(),
        entity_type: info.entity_type.clone(),
        description: info.description.clone(),
        status: info.status.clone(),
        capabilities: CapabilitiesResponse::from(caps),
        data: format!("{}/data", base_path),
        faults: format!("{}/faults", base_path),
        logs: format!("{}/logs", base_path),
        operations: format!("{}/operations", base_path),
        apps: if caps.sub_entities {
            Some(format!("{}/apps", base_path))
        } else {
            None
        },
    }))
}
