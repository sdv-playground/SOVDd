//! Node/subcomponent topology: one node component whose subcomponents are the backends.
//!
//! By default every backend is a top-level component (`{base}/components/{id}`). A server
//! configured with a [`NodeTopology`] ([`crate::AppState::with_node`]) is one node instead, as a
//! vehicle node's SOVD server is in ISO 17978-3's entity model and as x-update's discovery
//! expects (a node lists exactly one component):
//!
//! - `{base}/components` lists the node alone;
//! - `{base}/components/{node}` is the node's document, linking its `subcomponents`;
//! - `{base}/components/{node}/subcomponents` lists the backends;
//! - `{base}/components/{node}/subcomponents/{id}/...` is backend `id`'s entity API, everything a
//!   top-level component serves;
//! - `{base}/components/{node}/...` (other than `subcomponents`) is the node's own resources, served
//!   by the node's host backend when it names one ([`NodeTopology::with_host`]);
//! - `{base}/components/{id}/...` of a backend answers 404: a node has no other top-level
//!   component.
//!
//! The routes stay the flat ones: [`with_node_topology`] rewrites a node-form path onto its flat
//! route before routing, so handlers, authorization (a subcomponent's scope is still
//! `component:{id}`) and every merged-in vendor route serve the node form unchanged. It has to wrap
//! the embedder's *final* router, after its own routes are merged in. Every URL a response renders
//! through [`crate::Tree::component`] takes the node form while a request is in node mode.

use std::sync::Arc;

use axum::extract::Request;
use axum::http::Uri;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::Router;

use crate::error::ApiError;
use crate::state::AppState;
use crate::Tree;

/// The node a server is: its id and name in `{base}/components`, and the backend, if any, that
/// serves the node's own resources (`{base}/components/{node}/data`, `.../operations`, ...).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeTopology {
    id: String,
    name: String,
    host: Option<String>,
}

impl NodeTopology {
    /// A node named `name` with the id `id`, which no backend may also have.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            host: None,
        }
    }

    /// The backend whose resources are the node's own: `{base}/components/{node}/{rest}` is served
    /// as that backend's `{rest}`. It stays one of the subcomponents too.
    pub fn with_host(mut self, backend_id: impl Into<String>) -> Self {
        self.host = Some(backend_id.into());
        self
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn host(&self) -> Option<&str> {
        self.host.as_deref()
    }

    /// `{base}/components/{node}`.
    pub fn href(&self, tree: Tree) -> String {
        format!("{}/components/{}", tree.base(), self.id)
    }

    /// `{base}/components/{node}/subcomponents`.
    pub fn subcomponents_href(&self, tree: Tree) -> String {
        format!("{}/subcomponents", self.href(tree))
    }

    /// The node-form URL of backend `id`: `{base}/components/{node}/subcomponents/{id}`.
    pub fn subcomponent_href(&self, tree: Tree, id: &str) -> String {
        format!("{}/{id}", self.subcomponents_href(tree))
    }

    /// The path `path` takes before routing: `Ok(None)` to route it as it is, `Ok(Some(flat))` to
    /// route it as `flat`, `Err` for a path a node does not serve.
    fn rewrite(&self, path: &str) -> Result<Option<String>, ApiError> {
        let Some((tree, rest)) = Tree::split_path(path) else {
            return Ok(None);
        };
        let Some(entity) = rest.strip_prefix("/components/") else {
            return Ok(None);
        };
        let (id, tail) = entity
            .split_once('/')
            .map_or((entity, ""), |(id, tail)| (id, tail));
        if id != self.id {
            return Err(ApiError::NotFound(format!(
                "Component not found: {id} (a subcomponent of {} at {})",
                self.id,
                self.subcomponents_href(tree)
            )));
        }
        let flat =
            |backend: &str, rest: &str| Some(format!("{}/components/{backend}{rest}", tree.base()));
        Ok(
            match tail
                .split_once('/')
                .map_or((tail, ""), |(head, rest)| (head, rest))
            {
                // The node document and the subcomponents listing are node routes of their own.
                ("", _) | ("subcomponents", "") => None,
                ("subcomponents", sub) => {
                    let (backend, rest) = sub.split_once('/').map_or((sub, ""), |(b, r)| (b, r));
                    if backend.is_empty() {
                        None
                    } else if rest.is_empty() {
                        flat(backend, "")
                    } else {
                        flat(backend, &format!("/{rest}"))
                    }
                }
                _ => match &self.host {
                    Some(host) => flat(host, &format!("/{tail}")),
                    None => None,
                },
            },
        )
    }
}

tokio::task_local! {
    static NODE: Option<Arc<NodeTopology>>;
}

/// The node the request being served addresses, while [`with_node_topology`] serves it.
pub(crate) fn current() -> Option<Arc<NodeTopology>> {
    NODE.try_with(Clone::clone).ok().flatten()
}

/// `router` serving `state`'s node topology: node-form paths are routed onto the flat routes, flat
/// component paths answer 404, and the response renders node-form URLs. A no-op for a state without
/// a node. Apply it last, around the router with every route merged in.
pub fn with_node_topology(router: Router, state: &AppState) -> Router {
    let Some(node) = state.node().cloned() else {
        return router;
    };
    Router::new()
        .fallback_service(router)
        .layer(middleware::from_fn(move |req: Request, next: Next| {
            let node = node.clone();
            async move { serve(node, req, next).await }
        }))
}

async fn serve(node: Arc<NodeTopology>, mut req: Request, next: Next) -> Response {
    match node.rewrite(req.uri().path()) {
        Err(error) => return error.into_response(),
        Ok(Some(flat)) => {
            let uri = match req.uri().query() {
                Some(query) => format!("{flat}?{query}"),
                None => flat,
            };
            match uri.parse::<Uri>() {
                Ok(uri) => *req.uri_mut() = uri,
                Err(_) => {
                    return ApiError::BadRequest(format!("invalid path {}", req.uri().path()))
                        .into_response()
                }
            }
        }
        Ok(None) => {}
    }
    NODE.scope(Some(node), next.run(req)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node() -> NodeTopology {
        NodeTopology::new("cvc", "CVC").with_host("host")
    }

    #[test]
    fn a_subcomponent_path_is_routed_as_its_flat_path() {
        let node = node();
        assert_eq!(
            node.rewrite("/v1/components/cvc/subcomponents/os/data/sw-version")
                .unwrap(),
            Some("/v1/components/os/data/sw-version".into())
        );
        assert_eq!(
            node.rewrite("/vehicle/v1/components/cvc/subcomponents/os")
                .unwrap(),
            Some("/vehicle/v1/components/os".into())
        );
    }

    #[test]
    fn the_node_document_and_listing_are_routed_as_they_are() {
        let node = node();
        assert_eq!(node.rewrite("/v1/components/cvc").unwrap(), None);
        assert_eq!(node.rewrite("/v1/components/cvc/").unwrap(), None);
        assert_eq!(
            node.rewrite("/v1/components/cvc/subcomponents").unwrap(),
            None
        );
        assert_eq!(
            node.rewrite("/v1/components/cvc/subcomponents/").unwrap(),
            None
        );
        assert_eq!(node.rewrite("/v1/components").unwrap(), None);
        assert_eq!(node.rewrite("/v1/updates").unwrap(), None);
        assert_eq!(node.rewrite("/version-info").unwrap(), None);
    }

    #[test]
    fn the_nodes_own_resources_are_its_hosts() {
        assert_eq!(
            node().rewrite("/v1/components/cvc/data/x-boot-id").unwrap(),
            Some("/v1/components/host/data/x-boot-id".into())
        );
        // Without a host the node has none, and the path is routed as it is (404).
        let bare = NodeTopology::new("cvc", "CVC");
        assert_eq!(bare.rewrite("/v1/components/cvc/data").unwrap(), None);
    }

    #[test]
    fn a_flat_component_path_is_not_served() {
        assert!(node().rewrite("/v1/components/os/data").is_err());
        assert!(node().rewrite("/vehicle/v1/components/os").is_err());
    }
}
