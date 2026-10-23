//! Component discovery handlers

use axum::extract::{Path, State};
use axum::{Extension, Json};
use serde::Serialize;

use sovd_core::Capabilities;

use crate::auth::ClientContext;
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
    /// Vendor extension: the executor's x-update state. `IDLE` once it holds
    /// no update (every update deleted and cleaned up), else `BUSY`.
    #[serde(rename = "x-update")]
    pub x_update: XUpdateExecutor,
}

/// `x-update` member of the component body.
#[derive(Serialize)]
pub struct XUpdateExecutor {
    pub state: XUpdateExecutorState,
}

/// Whether the executor holds an update.
#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum XUpdateExecutorState {
    Idle,
    Busy,
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
    /// §7.20 bulk-data collection (log-file download / large payloads).
    pub bulk_data: bool,
    /// §7.9 diagnostics collection (read-only system probes — mem/df/du/…).
    pub diagnostics: bool,
    /// §7.12 configurations collection (read/write/reset configuration resources).
    pub configurations: bool,
    /// Vendor extension: x-update executor properties, derived from the
    /// backend's `update_shape()`. Absent when the shape is not advertised.
    #[serde(rename = "x-update", skip_serializing_if = "Option::is_none")]
    pub x_update: Option<XUpdateCapability>,
}

/// `capabilities.x-update` member.
#[derive(Serialize)]
pub struct XUpdateCapability {
    /// Whether an update on this component pauses in trial for a verdict
    /// (`banked`) or completes without one (`singleshot`).
    pub verdict: bool,
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
            bulk_data: caps.bulk_data,
            diagnostics: caps.diagnostics,
            configurations: caps.configurations,
            x_update: None,
        }
    }
}

/// GET /vehicle/v1/components
/// List all available components.
///
/// C-031 non-leakage: when authentication is enabled, the listing is filtered
/// to the components the client's scopes permit (no `ClientContext` ⇒ auth
/// disabled ⇒ list everything).
pub async fn list_components(
    State(state): State<AppState>,
    client: Option<Extension<ClientContext>>,
) -> Json<ComponentsResponse> {
    let client = client.map(|Extension(c)| c);
    let items: Vec<ComponentInfo> = state
        .backends()
        .iter()
        .filter(|(id, _)| match &client {
            Some(c) => c.can_access_component(id.as_str()),
            None => true, // auth disabled ⇒ no filtering
        })
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
    let mut capabilities = CapabilitiesResponse::from(caps);
    capabilities.x_update = match backend.update_shape() {
        "banked" => Some(XUpdateCapability { verdict: true }),
        "singleshot" => Some(XUpdateCapability { verdict: false }),
        _ => None,
    };

    let response = ComponentDetailResponse {
        id: component_id.clone(),
        name: info.name.clone(),
        description: info.description.clone(),
        entity_type: info.entity_type.clone(),
        capabilities,
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
        x_update: XUpdateExecutor {
            state: if state
                .updates
                .0
                .lock()
                .keys()
                .any(|key| key.component_id() == component_id)
            {
                XUpdateExecutorState::Busy
            } else {
                XUpdateExecutorState::Idle
            },
        },
    };

    Ok(Json(response))
}
