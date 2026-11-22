//! Root capability document — ISO 17978-3 §7.6.3 Table 53 (SOVDServer entity).

use axum::extract::State;
use axum::{Extension, Json};
use serde::Serialize;

use crate::auth::ClientContext;
use crate::state::AppState;
use crate::Tree;

/// Response body for `GET {base}/` — the SOVDServer entity's capabilities.
///
/// C1: only collections the server itself serves at root level are
/// referenced; entity collections underneath are never aggregated here.
#[derive(Serialize)]
pub struct RootCapabilityResponse {
    pub id: String,
    pub name: String,
    /// C4: present only when the components collection is not empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub components: Option<String>,
    /// §7.18.1.1: the server-root `updates` collection, present when at
    /// least one component accepts software updates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updates: Option<String>,
    /// Vendor extension: the components that accept software updates.
    #[serde(rename = "x-update")]
    pub x_update: XUpdateRoot,
}

/// `x-update` member of the root capability document.
#[derive(Serialize)]
pub struct XUpdateRoot {
    /// Ids of the components whose backend supports software update, sorted.
    #[serde(rename = "update-targets")]
    pub update_targets: Vec<String>,
}

/// GET {base}/ — the SOVD server's capability document (§7.6.3 Table 53).
///
/// C-031 non-leakage: `update-targets` is filtered to the components the
/// client's scopes permit, like the components listing.
pub async fn get_root(
    State(state): State<AppState>,
    Extension(tree): Extension<Tree>,
    client: Option<Extension<ClientContext>>,
) -> Json<RootCapabilityResponse> {
    let client = client.map(|Extension(c)| c);
    let backends = state.backends();
    let updates = backends
        .values()
        .any(|backend| backend.capabilities().software_update)
        .then(|| format!("{}/updates", tree.server()));
    let mut update_targets: Vec<String> = backends
        .iter()
        .filter(|(id, backend)| {
            backend.capabilities().software_update
                && client
                    .as_ref()
                    .is_none_or(|c| c.can_access_component(id.as_str()))
        })
        .map(|(id, _)| id.clone())
        .collect();
    update_targets.sort();

    Json(RootCapabilityResponse {
        id: "sovd-server".to_string(),
        name: "SOVDd".to_string(),
        components: (!backends.is_empty()).then(|| format!("{}/components", tree.base())),
        updates,
        x_update: XUpdateRoot { update_targets },
    })
}
