//! Component discovery handlers

use axum::extract::{Path, State};
use axum::Json;
use serde::Serialize;

use sovd_core::Capabilities;

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Serialize)]
pub struct ComponentsResponse {
    pub items: Vec<ComponentInfo>,
}

#[derive(Serialize)]
pub struct ComponentInfo {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "type")]
    pub entity_type: String,
    pub href: String,
}

#[derive(Serialize)]
pub struct ComponentDetailResponse {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "type")]
    pub entity_type: String,
    pub capabilities: CapabilitiesResponse,
    pub data: String,
    pub faults: String,
    pub operations: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logs: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apps: Option<String>,
}

#[derive(Serialize)]
pub struct CapabilitiesResponse {
    pub read_data: bool,
    pub write_data: bool,
    pub faults: bool,
    pub clear_faults: bool,
    pub logs: bool,
    pub operations: bool,
    pub software_update: bool,
    pub io_control: bool,
    pub sessions: bool,
    pub security: bool,
    pub sub_entities: bool,
    pub subscriptions: bool,
}

impl From<&Capabilities> for CapabilitiesResponse {
    fn from(caps: &Capabilities) -> Self {
        Self {
            read_data: caps.read_data,
            write_data: caps.write_data,
            faults: caps.faults,
            clear_faults: caps.clear_faults,
            logs: caps.logs,
            operations: caps.operations,
            software_update: caps.software_update,
            io_control: caps.io_control,
            sessions: caps.sessions,
            security: caps.security,
            sub_entities: caps.sub_entities,
            subscriptions: caps.subscriptions,
        }
    }
}

/// GET /vehicle/v1/components
/// List all available components
pub async fn list_components(State(state): State<AppState>) -> Json<ComponentsResponse> {
    let items: Vec<ComponentInfo> = state
        .backends()
        .iter()
        .map(|(id, backend)| {
            let info = backend.entity_info();
            ComponentInfo {
                id: id.clone(),
                name: info.name.clone(),
                description: info.description.clone(),
                entity_type: info.entity_type.clone(),
                href: format!("/vehicle/v1/components/{}", id),
            }
        })
        .collect();

    Json(ComponentsResponse { items })
}

/// GET /vehicle/v1/components/:component_id
/// Get detailed information about a component
pub async fn get_component(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<Json<ComponentDetailResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;
    let info = backend.entity_info();
    let caps = backend.capabilities();

    let response = ComponentDetailResponse {
        id: component_id.clone(),
        name: info.name.clone(),
        description: info.description.clone(),
        entity_type: info.entity_type.clone(),
        capabilities: CapabilitiesResponse::from(caps),
        data: format!("/vehicle/v1/components/{}/data", component_id),
        faults: format!("/vehicle/v1/components/{}/faults", component_id),
        operations: format!("/vehicle/v1/components/{}/operations", component_id),
        logs: if caps.logs {
            Some(format!("/vehicle/v1/components/{}/logs", component_id))
        } else {
            None
        },
        apps: if caps.sub_entities {
            Some(format!("/vehicle/v1/components/{}/apps", component_id))
        } else {
            None
        },
    };

    Ok(Json(response))
}
