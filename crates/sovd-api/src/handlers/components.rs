//! Component discovery handlers

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::{Extension, Json};
use serde::Serialize;

use sovd_core::Capabilities;

use crate::auth::ClientContext;
use crate::error::ApiError;
use crate::handlers::updates::{x_update_verdict_scope, XUpdateVerdictScope};
use crate::state::AppState;
use crate::topology::NodeTopology;
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
}

/// The node's document (`/v1/components/{node}`, see [`crate::topology`]): its
/// node-level resources and its subcomponents. It has no resources of a backend.
#[derive(Serialize)]
pub struct NodeDetailResponse {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub entity_type: String,
    /// The unit's own name ([`NodeTopology::identification`]), distinct from
    /// the node's `id`. Vendor property: the `x-sovd-` prefix is reserved for
    /// spec-defined names (§5.4.5). Absent when not configured.
    #[serde(rename = "x-identification", skip_serializing_if = "Option::is_none")]
    pub identification: Option<String>,
    pub status: String,
    /// Present when a backend supports software update, as on the root document.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updates: Option<String>,
    pub operations: String,
    pub data: String,
    pub subcomponents: String,
}

/// The node's entity type.
const NODE_TYPE: &str = "node";

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
///
/// On `/v1` of a server configured with a node, the node alone: the backends
/// are its subcomponents (see [`crate::topology`]).
pub async fn list_components(
    State(state): State<AppState>,
    Extension(tree): Extension<Tree>,
    client: Option<Extension<ClientContext>>,
) -> Json<ComponentsResponse> {
    let client = client.map(|Extension(c)| c);
    let items = match state.node().filter(|_| tree == Tree::V1) {
        Some(node) => node_visible(&state, node, client.as_ref())
            .then(|| ComponentInfo {
                id: node.id().to_owned(),
                name: node.name().to_owned(),
                description: None,
                entity_type: NODE_TYPE.to_owned(),
                href: node.href(),
            })
            .into_iter()
            .collect(),
        None => backend_items(&state, client.as_ref(), |id| tree.component(id)).collect(),
    };

    Json(ComponentsResponse { items })
}

/// GET /v1/components/{node} — the node's document (node topology only).
///
/// Scope-checked here rather than as a component: the node is open to the
/// clients the components listing shows it to.
pub async fn get_node(
    State(state): State<AppState>,
    Extension(node): Extension<Arc<NodeTopology>>,
    client: Option<Extension<ClientContext>>,
) -> Result<Json<NodeDetailResponse>, ApiError> {
    if !node_visible(&state, &node, client.as_ref().map(|Extension(c)| c)) {
        return Err(ApiError::Forbidden(format!(
            "no scope for node {} or any of its subcomponents",
            node.id()
        )));
    }
    let href = node.href();
    let updates = state
        .backends()
        .values()
        .any(|backend| backend.capabilities().software_update)
        .then(|| format!("{href}/updates"));
    Ok(Json(NodeDetailResponse {
        id: node.id().to_owned(),
        name: node.name().to_owned(),
        entity_type: NODE_TYPE.to_owned(),
        identification: node.identification().map(str::to_owned),
        status: format!("{href}/status"),
        updates,
        operations: format!("{href}/operations"),
        data: format!("{href}/data"),
        subcomponents: node.subcomponents_href(),
    }))
}

/// GET /v1/components/{node}/subcomponents — the backends, as the node's
/// subcomponents: the items of `/v1/components` with their node-form hrefs,
/// scope-filtered alike (C-031 non-leakage).
pub async fn list_subcomponents(
    State(state): State<AppState>,
    Extension(node): Extension<Arc<NodeTopology>>,
    client: Option<Extension<ClientContext>>,
) -> Json<ComponentsResponse> {
    let client = client.map(|Extension(c)| c);
    let items = backend_items(&state, client.as_ref(), |id| node.subcomponent_href(id)).collect();
    Json(ComponentsResponse { items })
}

/// A node-level `data` or `operations` collection ([`node_collection`]).
#[derive(Serialize)]
pub struct NodeItemsResponse {
    pub items: Vec<NodeItem>,
}

/// An item of a [`NodeItemsResponse`].
#[derive(Serialize)]
pub struct NodeItem {
    pub id: String,
    pub name: String,
    pub href: String,
}

/// GET /v1/components/{node}/data and /v1/components/{node}/operations (node
/// topology, `collection` being `"data"` or `"operations"`), and GET /v1/data
/// on a server with a node-level data provider ([`AppState::with_node_data`]),
/// when no embedder route serves the root form: the node-level items the
/// embedder contributes to the capability description ([`AppState::with_capability_extensions`],
/// `paths` and `v1_paths` alike),
/// a `GET {base}/data/{item}` path an item of `data`, a
/// `{base}/operations/{op}[/executions]` path one of `operations` (either
/// base). An item's `name` is its path's summary (the `{op}` path's `get`
/// before the `executions` path's `post`), else its id. The provider's items
/// are items of `data` too, named by the provider; an id both name is listed
/// once, as the provider's, since the provider serves its read.
///
/// Scope-checked as the node's document is ([`check_node_visible`]).
pub(crate) async fn node_collection(
    state: &AppState,
    client: Option<&ClientContext>,
    collection: &str,
) -> Result<Json<NodeItemsResponse>, ApiError> {
    check_node_visible(state, client)?;
    let mut names = std::collections::BTreeMap::<String, Option<String>>::new();
    let extensions = state.capability_extensions();
    for (template, item) in extensions.paths.iter().chain(&extensions.v1_paths) {
        let Some(rest) = Tree::split_path(template)
            .and_then(|(_, rest)| rest.strip_prefix(&format!("/{collection}/")))
        else {
            continue;
        };
        let (id, tail) = rest.find('/').map_or((rest, ""), |i| rest.split_at(i));
        let method = match (collection, tail) {
            ("data", "") if item.get("get").is_some() => "get",
            ("operations", "") => "get",
            ("operations", "/executions") => "post",
            _ => continue,
        };
        if id.is_empty() || id.starts_with('{') {
            continue;
        }
        let name = names.entry(id.to_owned()).or_default();
        if let Some(summary) = item[method]["summary"].as_str() {
            if tail.is_empty() || name.is_none() {
                *name = Some(summary.to_owned());
            }
        }
    }
    if let Some(node_data) = state.node_data().filter(|_| collection == "data") {
        for item in node_data.list_data().await? {
            names.insert(item.id, Some(item.name));
        }
    }
    let base = state
        .node()
        .map_or_else(|| Tree::V1.base().to_owned(), |node| node.href());
    let items = names
        .into_iter()
        .map(|(id, name)| NodeItem {
            href: format!("{base}/{collection}/{id}"),
            name: name.unwrap_or_else(|| id.clone()),
            id,
        })
        .collect();
    Ok(Json(NodeItemsResponse { items }))
}

/// The backends `client` may access (no `ClientContext` ⇒ auth disabled ⇒
/// every one), as listing items with the hrefs `href` renders.
fn backend_items<'a>(
    state: &'a AppState,
    client: Option<&'a ClientContext>,
    href: impl Fn(&str) -> String + 'a,
) -> impl Iterator<Item = ComponentInfo> + 'a {
    state
        .backends()
        .iter()
        .filter(move |(id, _)| client.is_none_or(|c| c.can_access_component(id)))
        .map(move |(id, backend)| {
            let info = backend.entity_info();
            ComponentInfo {
                id: id.clone(),
                name: info.name.clone(),
                description: info.description.clone(),
                entity_type: info.entity_type.clone(),
                href: href(id),
            }
        })
}

/// Forbidden unless `client` sees the server's node-level resources: on a
/// server with a node when it sees the node ([`node_visible`]), on one without
/// when it may access any component (no `ClientContext` ⇒ auth disabled ⇒ it
/// sees them).
pub(crate) fn check_node_visible(
    state: &AppState,
    client: Option<&ClientContext>,
) -> Result<(), ApiError> {
    match state.node() {
        Some(node) if !node_visible(state, node, client) => Err(ApiError::Forbidden(format!(
            "no scope for node {} or any of its subcomponents",
            node.id()
        ))),
        None if !client
            .is_none_or(|c| state.backends().keys().any(|id| c.can_access_component(id))) =>
        {
            Err(ApiError::Forbidden("no scope for any component".to_owned()))
        }
        _ => Ok(()),
    }
}

/// Whether `client` sees the node: when it may access the node or any of its
/// subcomponents (no `ClientContext` ⇒ auth disabled ⇒ yes).
fn node_visible(state: &AppState, node: &NodeTopology, client: Option<&ClientContext>) -> bool {
    client.is_none_or(|c| {
        c.can_access_component(node.id())
            || state.backends().keys().any(|id| c.can_access_component(id))
    })
}

/// GET /vehicle/v1/components/:component_id
/// Get detailed information about a component
pub async fn get_component(
    State(state): State<AppState>,
    Extension(tree): Extension<Tree>,
    Path(component_id): Path<String>,
) -> Result<Json<ComponentDetailResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;
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

    let base = tree.component(&component_id);
    let response = ComponentDetailResponse {
        id: component_id.clone(),
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
    };

    Ok(Json(response))
}
