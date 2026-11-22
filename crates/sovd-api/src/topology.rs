//! Node topology: one node component on `/v1` whose subcomponents are the backends.
//!
//! By default every backend is a top-level component (`{base}/components/{id}`). A server
//! configured with a [`NodeTopology`] ([`crate::AppState::with_node`]) is one vehicle node on
//! `/v1`, and `/v1` serves it hierarchically only (ISO 17978-3's entity model: a node's members
//! are its subcomponents; §5.4.1: one path per resource):
//!
//! - `/v1/components` lists the node alone;
//! - `/v1/components/{node}` is the node's document, linking its `subcomponents` and the
//!   node-level resources below, and carrying the unit's `x-identification` when set;
//! - `/v1/components/{node}/subcomponents` lists the backends;
//! - `/v1/components/{node}/subcomponents/{id}/...` is backend `id`'s entity API;
//! - `/v1/components/{node}/status`, `.../status/restart`, `.../updates[/...]`,
//!   `.../operations[/...]` and `.../data[/...]` are the node-level resources;
//! - any other `/v1/components/{node}/...` answers 404;
//! - the flat and root forms those paths map onto (`/v1/components/{id}/...` of any other id,
//!   `/v1/status`, `/v1/updates/...`, `/v1/data/...`, ...) answer 404 naming the node form.
//!   The rest of `/v1` (`/v1/`, `/v1/components`, `/v1/docs`) is served as without a node.
//!
//! `/vehicle/v1` is frozen: it has no node, and its responses are those of a server without one.
//!
//! The routes stay the flat ones: [`with_node_topology`] rewrites a node-form path onto its flat
//! (or root-form) route before routing, so handlers, authorization (a subcomponent's scope is
//! still `component:{id}`) and every merged-in embedder route serve the node form unchanged; a
//! request arriving in a flat or root form is answered 404 before routing. It has to wrap the
//! embedder's *final* router, after its own routes are merged in. The `/v1` URLs a response
//! renders through [`crate::Tree::component`] and [`crate::Tree::server`] take the node form, and
//! so does a flat or root-form `Location`.

use std::sync::Arc;

use axum::extract::Request;
use axum::http::header::{HeaderValue, LOCATION};
use axum::http::Uri;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::Router;

use crate::error::ApiError;
use crate::state::AppState;
use crate::Tree;

/// The node a server is on `/v1`: its id and name in `/v1/components`, and optionally the unit's
/// identification on its document. Its id must be a path segment no backend also has as its id
/// (see [`crate::AppState::with_node`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeTopology {
    id: String,
    name: String,
    identification: Option<String>,
}

/// What [`with_node_topology`] does with a request path.
#[derive(Debug, PartialEq, Eq)]
enum Route {
    /// Route it as it is: not a node-form path, or the node's document or subcomponents listing.
    AsIs,
    /// A node-form path: route it as this flat or root-form path.
    Rewrite(String),
    /// A path not served: a flat or root form, or a node-form path the node does not serve.
    NotFound(String),
}

impl NodeTopology {
    /// A node named `name` with the id `id`.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            identification: None,
        }
    }

    /// The node with the identification `identification`: the unit's own name (its mDNS/TLS
    /// identity, e.g. `cvc-host-f88a13a329a72db1`), distinct from the node's logical id (`cvc`).
    /// The node's document carries it as `x-identification`.
    pub fn with_identification(mut self, identification: impl Into<String>) -> Self {
        self.identification = Some(identification.into());
        self
    }

    /// The node's component id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The node's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The unit's identification, if set (see [`Self::with_identification`]).
    pub fn identification(&self) -> Option<&str> {
        self.identification.as_deref()
    }

    /// `/v1/components/{node}`.
    pub(crate) fn href(&self) -> String {
        format!("{}/components/{}", Tree::V1.base(), self.id)
    }

    /// `/v1/components/{node}/subcomponents`.
    pub(crate) fn subcomponents_href(&self) -> String {
        format!("{}/subcomponents", self.href())
    }

    /// The node-form URL of backend `id`: `/v1/components/{node}/subcomponents/{id}`.
    pub(crate) fn subcomponent_href(&self, id: &str) -> String {
        format!("{}/{id}", self.subcomponents_href())
    }

    /// Whether `path` is the node's document or its subcomponents listing, which are
    /// scope-checked in their handlers rather than as a component.
    pub(crate) fn is_own_path(&self, path: &str) -> bool {
        path == self.href() || path == self.subcomponents_href()
    }

    /// The node form of a flat or root-form `/v1` path: `/v1/components/{id}/...` of any other
    /// component id as `/v1/components/{node}/subcomponents/{id}/...`, a node-level resource
    /// `/v1/{status|updates|operations|data}...` as `/v1/components/{node}/...`. `None` for any
    /// other path, the node-form ones included.
    pub(crate) fn node_form(&self, path: &str) -> Option<String> {
        let (Tree::V1, rest) = Tree::split_path(path)? else {
            return None;
        };
        if is_node_level(rest) {
            return Some(format!("{}{rest}", self.href()));
        }
        let (id, tail) = split_segment(rest.strip_prefix("/components/")?);
        (!id.is_empty() && id != self.id).then(|| format!("{}{tail}", self.subcomponent_href(id)))
    }

    /// How `path` is routed.
    fn route(&self, path: &str) -> Route {
        if let Some(node_form) = self.node_form(path) {
            return Route::NotFound(format!(
                "No resource at {path}: on /v1 this server is node '{}', whose path for it is \
                 {node_form}",
                self.id
            ));
        }
        let Some((Tree::V1, rest)) = Tree::split_path(path) else {
            return Route::AsIs;
        };
        let Some(entity) = rest.strip_prefix("/components/") else {
            return Route::AsIs;
        };
        // Not a flat form: the id is empty or the node's.
        let (id, tail) = split_segment(entity);
        if id != self.id || tail.is_empty() || tail == "/subcomponents" {
            return Route::AsIs;
        }
        if let Some(member) = tail.strip_prefix("/subcomponents/") {
            let (member, rest) = split_segment(member);
            return match member {
                // An empty id is no resource, and the node is not a subcomponent of its own:
                // both answer what an unknown component path does.
                "" => Route::NotFound(format!("No resource at {path}")),
                m if m == self.id => Route::NotFound(format!("Component not found: {m}")),
                m => Route::Rewrite(format!("{}/components/{m}{rest}", Tree::V1.base())),
            };
        }
        if is_node_level(tail) {
            return Route::Rewrite(format!("{}{tail}", Tree::V1.base()));
        }
        Route::NotFound(format!("No resource at {path}"))
    }
}

/// Whether `rest` (a path relative to `/v1` or to the node) is a node-level resource: `/status`,
/// `/status/restart`, `/updates[/...]`, `/operations[/...]`, `/data[/...]`.
fn is_node_level(rest: &str) -> bool {
    matches!(rest, "/status" | "/status/restart")
        || ["/updates", "/operations", "/data"].iter().any(|c| {
            rest.strip_prefix(c)
                .is_some_and(|r| r.is_empty() || r.starts_with('/'))
        })
}

/// `"id/rest"` as `("id", "/rest")`, `"id"` as `("id", "")`.
fn split_segment(path: &str) -> (&str, &str) {
    path.find('/').map_or((path, ""), |i| path.split_at(i))
}

tokio::task_local! {
    static NODE: Arc<NodeTopology>;
}

/// The node of the server, while [`with_node_topology`] serves a request on one.
pub(crate) fn current() -> Option<Arc<NodeTopology>> {
    NODE.try_with(Clone::clone).ok()
}

/// `path` (a flat or root-form `/v1` path) in the form it is served in: its
/// [node form](NodeTopology::node_form) on a server with a node, else as it is.
pub(crate) fn served_form(path: String) -> String {
    current()
        .and_then(|node| node.node_form(&path))
        .unwrap_or(path)
}

/// `router` serving `state`'s node topology (see the [module docs](self)): node-form paths are
/// routed onto their flat or root-form routes, the flat and root forms themselves and other
/// `/v1/components/{node}/...` paths answer 404, and responses render node-form URLs. A no-op
/// for a state without a node. Apply it last, around the router with every route merged in.
pub fn with_node_topology(router: Router, state: &AppState) -> Router {
    let Some(node) = state.node().cloned() else {
        return router;
    };
    // A layer on `router` itself would run after routing; wrapping it as the fallback of an empty
    // router runs the rewrite first.
    Router::new()
        .fallback_service(router)
        .layer(middleware::from_fn(move |req: Request, next: Next| {
            serve(node.clone(), req, next)
        }))
}

async fn serve(node: Arc<NodeTopology>, mut req: Request, next: Next) -> Response {
    match node.route(req.uri().path()) {
        Route::AsIs => {}
        Route::NotFound(message) => return ApiError::NotFound(message).into_response(),
        Route::Rewrite(flat) => {
            let path_and_query = match req.uri().query() {
                Some(query) => format!("{flat}?{query}"),
                None => flat,
            };
            let uri = path_and_query.parse().ok().and_then(|path_and_query| {
                let mut parts = req.uri().clone().into_parts();
                parts.path_and_query = Some(path_and_query);
                Uri::from_parts(parts).ok()
            });
            let Some(uri) = uri else {
                return ApiError::BadRequest(format!("invalid path {}", req.uri().path()))
                    .into_response();
            };
            *req.uri_mut() = uri;
        }
    }
    let mut response = NODE.scope(node.clone(), next.run(req)).await;
    // A `Location` an (embedder) handler rendered itself in the flat or root form.
    let location = response
        .headers()
        .get(LOCATION)
        .and_then(|l| l.to_str().ok());
    if let Some(nested) = location.and_then(|l| node.node_form(l)) {
        if let Ok(nested) = HeaderValue::try_from(nested) {
            response.headers_mut().insert(LOCATION, nested);
        }
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(path: &str) -> Route {
        NodeTopology::new("cvc", "CVC").route(path)
    }

    fn rewrite(path: &str) -> Route {
        Route::Rewrite(path.into())
    }

    #[test]
    fn a_subcomponent_path_is_routed_as_its_flat_path() {
        assert_eq!(
            route("/v1/components/cvc/subcomponents/vm1/data/sw-version"),
            rewrite("/v1/components/vm1/data/sw-version")
        );
        assert_eq!(
            route("/v1/components/cvc/subcomponents/vm1"),
            rewrite("/v1/components/vm1")
        );
        // An unknown member is routed too, to the answer an unknown component gets.
        assert_eq!(
            route("/v1/components/cvc/subcomponents/nope/faults"),
            rewrite("/v1/components/nope/faults")
        );
    }

    #[test]
    fn node_level_resources_are_routed_as_their_root_forms() {
        for rest in [
            "/status",
            "/status/restart",
            "/updates",
            "/updates/u1/bulk-data/p1",
            "/operations",
            "/operations/x-update-commit/executions",
            "/data",
            "/data/x-update-state",
        ] {
            assert_eq!(
                route(&format!("/v1/components/cvc{rest}")),
                rewrite(&format!("/v1{rest}"))
            );
        }
    }

    #[test]
    fn the_node_document_and_listing_are_routes_of_their_own() {
        for path in [
            "/v1/components/cvc",
            "/v1/components/cvc/subcomponents",
            // The rest of `/v1` and the server root.
            "/v1/",
            "/v1/components",
            "/v1/docs",
            "/version-info",
        ] {
            assert_eq!(route(path), Route::AsIs, "{path}");
        }
    }

    #[test]
    fn other_node_paths_are_not_served() {
        for path in [
            "/v1/components/cvc/",
            "/v1/components/cvc/faults",
            "/v1/components/cvc/data-lists",
            "/v1/components/cvc/docs",
            "/v1/components/cvc/status/x-boot-id",
            "/v1/components/cvc/updatesx",
            "/v1/components/cvc/subcomponents/",
            "/v1/components/cvc/subcomponents/cvc",
        ] {
            assert!(matches!(route(path), Route::NotFound(_)), "{path}");
        }
    }

    #[test]
    fn flat_and_root_forms_are_not_served_on_v1() {
        assert_eq!(
            route("/v1/components/vm1/data/x"),
            Route::NotFound(
                "No resource at /v1/components/vm1/data/x: on /v1 this server is node 'cvc', \
                 whose path for it is /v1/components/cvc/subcomponents/vm1/data/x"
                    .into()
            )
        );
        for path in [
            "/v1/components/host",
            // Any other id alike, so a flat path tells nothing of the members.
            "/v1/components/nope/data",
            "/v1/status",
            "/v1/status/restart",
            "/v1/updates/u1/status",
            "/v1/operations/x-update-commit/executions",
            "/v1/data/x-update-state",
        ] {
            assert!(matches!(route(path), Route::NotFound(_)), "{path}");
        }
        // The frozen legacy tree has no node: its paths are routed as they are.
        for path in [
            "/vehicle/v1/components/vm1/data",
            "/vehicle/v1/updates",
            "/vehicle/v1/components/cvc/subcomponents/vm1",
        ] {
            assert_eq!(route(path), Route::AsIs, "{path}");
        }
    }

    #[test]
    fn node_form_maps_flat_and_root_paths() {
        let node = NodeTopology::new("cvc", "CVC");
        assert_eq!(
            node.node_form("/v1/components/{component_id}/data")
                .as_deref(),
            Some("/v1/components/cvc/subcomponents/{component_id}/data")
        );
        assert_eq!(
            node.node_form("/v1/updates/u1/status").as_deref(),
            Some("/v1/components/cvc/updates/u1/status")
        );
        assert_eq!(
            node.node_form("/v1/data/x-update-state").as_deref(),
            Some("/v1/components/cvc/data/x-update-state")
        );
        assert_eq!(node.node_form("/v1/components/cvc/updates"), None);
        assert_eq!(node.node_form("/v1/components"), None);
        assert_eq!(node.node_form("/v1/docs"), None);
        assert_eq!(node.node_form("/vehicle/v1/updates"), None);
    }
}
