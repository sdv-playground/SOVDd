//! Component discovery handlers

use axum::extract::{Path, State};
use axum::{Extension, Json};
use serde::Serialize;

use sovd_core::Capabilities;

use crate::auth::ClientContext;
use crate::error::ApiError;
use crate::handlers::updates::{x_update_verdict_scope, XUpdateVerdictScope};
use crate::state::AppState;
use crate::Tree;

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
    /// §7.6.3 Table 53 (C1): present only when the component supports it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updates: Option<String>,
    #[serde(rename = "bulk-data", skip_serializing_if = "Option::is_none")]
    pub bulk_data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configurations: Option<String>,
    /// A node's subcomponents collection (node topology only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subcomponents: Option<String>,
}

/// The document of a node without a host backend: it has no resources of its own,
/// only its subcomponents.
#[derive(Serialize)]
pub struct NodeDetailResponse {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub entity_type: String,
    pub subcomponents: String,
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
    /// Whether that verdict is given per update or by the node operation
    /// (`/v1` only, see `updates::x_update_verdict_scope`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict_scope: Option<XUpdateVerdictScope>,
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
    Extension(tree): Extension<Tree>,
    client: Option<Extension<ClientContext>>,
) -> Json<ComponentsResponse> {
    let client = client.map(|Extension(c)| c);
    if let Some(node) = state.node() {
        // A node is the server's one component. A client sees it when it may access
        // the node or any of its subcomponents.
        let visible = match &client {
            Some(c) => {
                c.can_access_component(node.id())
                    || state.backends().keys().any(|id| c.can_access_component(id))
            }
            None => true,
        };
        let items = visible
            .then(|| ComponentInfo {
                id: node.id().to_owned(),
                name: node.name().to_owned(),
                description: None,
                entity_type: node_entity_type(&state, node),
                href: node.href(tree),
            })
            .into_iter()
            .collect();
        return Json(ComponentsResponse { items });
    }
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
                href: tree.component(id),
            }
        })
        .collect();

    Json(ComponentsResponse { items })
}

/// GET /vehicle/v1/components/:component_id
/// Get detailed information about a component
pub async fn get_component(
    State(state): State<AppState>,
    Extension(tree): Extension<Tree>,
    Path(component_id): Path<String>,
) -> Result<axum::response::Response, ApiError> {
    use axum::response::IntoResponse;
    if let Some(node) = state.node().filter(|node| node.id() == component_id) {
        // The node: its host backend's document under the node's own URL, or, without
        // a host, just the way to its subcomponents.
        let subcomponents = node.subcomponents_href(tree);
        return Ok(match node.host() {
            Some(host) => {
                let mut detail = component_detail(&state, tree, host, node.href(tree))?;
                detail.id = node.id().to_owned();
                detail.name = node.name().to_owned();
                detail.subcomponents = Some(subcomponents);
                Json(detail).into_response()
            }
            None => Json(NodeDetailResponse {
                id: node.id().to_owned(),
                name: node.name().to_owned(),
                entity_type: node_entity_type(&state, node),
                subcomponents,
            })
            .into_response(),
        });
    }
    let base = tree.component(&component_id);
    Ok(Json(component_detail(&state, tree, &component_id, base)?).into_response())
}

/// GET {base}/components/{node}/subcomponents — the backends, as the node's subcomponents.
/// Scope-filtered like the components collection (C-031 non-leakage). 404 for any other
/// component, and on a server that is not a node.
pub async fn list_subcomponents(
    State(state): State<AppState>,
    Extension(tree): Extension<Tree>,
    Path(component_id): Path<String>,
    client: Option<Extension<ClientContext>>,
) -> Result<Json<ComponentsResponse>, ApiError> {
    let node = state
        .node()
        .filter(|node| node.id() == component_id)
        .ok_or_else(|| {
            ApiError::NotFound(format!("Component has no subcomponents: {component_id}"))
        })?;
    let client = client.map(|Extension(c)| c);
    let mut items: Vec<ComponentInfo> = state
        .backends()
        .iter()
        .filter(|(id, _)| {
            client
                .as_ref()
                .is_none_or(|c| c.can_access_component(id.as_str()))
        })
        .map(|(id, backend)| {
            let info = backend.entity_info();
            ComponentInfo {
                id: id.clone(),
                name: info.name.clone(),
                description: info.description.clone(),
                entity_type: info.entity_type.clone(),
                href: node.subcomponent_href(tree, id),
            }
        })
        .collect();
    items.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(Json(ComponentsResponse { items }))
}

/// A node's entity type: its host backend's, or `node` without one.
fn node_entity_type(state: &AppState, node: &crate::NodeTopology) -> String {
    node.host()
        .and_then(|host| state.backends().get(host))
        .map_or_else(
            || "node".to_owned(),
            |backend| backend.entity_info().entity_type.clone(),
        )
}

/// The document of backend `component_id`, its resource URLs under `base`.
fn component_detail(
    state: &AppState,
    tree: Tree,
    component_id: &str,
    base: String,
) -> Result<ComponentDetailResponse, ApiError> {
    let backend = state.get_backend(component_id)?;
    let info = backend.entity_info();
    let caps = backend.capabilities();
    let mut capabilities = CapabilitiesResponse::from(caps);
    let verdict_scope = x_update_verdict_scope(tree, backend.as_ref());
    capabilities.x_update = match backend.update_shape() {
        "banked" => Some(XUpdateCapability {
            verdict: true,
            verdict_scope,
        }),
        "singleshot" => Some(XUpdateCapability {
            verdict: false,
            verdict_scope,
        }),
        _ => None,
    };

    let response = ComponentDetailResponse {
        id: component_id.to_owned(),
        name: info.name.clone(),
        description: info.description.clone(),
        entity_type: info.entity_type.clone(),
        capabilities,
        data: format!("{base}/data"),
        faults: format!("{base}/faults"),
        operations: format!("{base}/operations"),
        logs: if caps.logs {
            Some(format!("{base}/logs"))
        } else {
            None
        },
        apps: if caps.sub_entities {
            Some(format!("{base}/apps"))
        } else {
            None
        },
        updates: caps.software_update.then(|| format!("{base}/updates")),
        bulk_data: caps.bulk_data.then(|| format!("{base}/bulk-data")),
        configurations: caps
            .configurations
            .then(|| format!("{base}/configurations")),
        subcomponents: None,
    };

    Ok(response)
}
