//! Bulk-data handlers — SOVD §7.20 (ISO 17978-3, conformance C-120).
//!
//! The general large-payload collection. §7.21 logs are the first consumer (a
//! `logs` category whose items are downloadable log files — the spec-native
//! "get all logs"). Routes:
//!
//! * `GET /{entity}/bulk-data`                    — list categories
//! * `GET /{entity}/bulk-data/{category}`         — list items (q: created-before/after)
//! * `GET /{entity}/bulk-data/{category}/{id}`    — download (200 | 202 | 307)
//!
//! Upload/delete (POST/DELETE) are spec-optional and not wired yet — logs are
//! read-only. Gated on `capabilities().bulk_data`.

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sovd_core::{BulkDataDownload, BulkDataFilter};

use crate::error::ApiError;
use crate::state::AppState;

/// `GET /{entity}/bulk-data` — the category list.
#[derive(Debug, Serialize)]
pub struct CategoriesResponse {
    pub items: Vec<CategoryRef>,
}

#[derive(Debug, Serialize)]
pub struct CategoryRef {
    /// Category id (the `{category}` path segment).
    pub id: String,
    /// Collection URL for the category's items.
    pub href: String,
}

/// `GET /{entity}/bulk-data/{category}` — the item list.
#[derive(Debug, Serialize)]
pub struct ItemsResponse {
    pub items: Vec<ItemRef>,
}

#[derive(Debug, Serialize)]
pub struct ItemRef {
    pub id: String,
    pub size: u64,
    pub created: DateTime<Utc>,
    pub mime: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Download URL for this item.
    pub href: String,
}

/// Query params for the item list (`created-before` / `created-after`, RFC 3339).
#[derive(Debug, Default, Deserialize)]
pub struct ItemsQuery {
    #[serde(rename = "created-before")]
    pub created_before: Option<DateTime<Utc>>,
    #[serde(rename = "created-after")]
    pub created_after: Option<DateTime<Utc>>,
}

/// Reject a request against a backend that doesn't advertise bulk-data, so the
/// route is a clean 501 rather than a confusing empty/404 from the default trait
/// methods.
fn require_bulk_data(
    state: &AppState,
    component_id: &str,
) -> Result<std::sync::Arc<dyn sovd_core::DiagnosticBackend>, ApiError> {
    let backend = state.get_backend(component_id)?;
    if !backend.capabilities().bulk_data {
        return Err(ApiError::NotImplemented(
            "This component does not support bulk-data".to_string(),
        ));
    }
    Ok(backend.clone())
}

/// GET /vehicle/v1/components/:component_id/bulk-data
pub async fn list_categories(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<Json<CategoriesResponse>, ApiError> {
    let backend = require_bulk_data(&state, &component_id)?;
    let base = format!("/vehicle/v1/components/{component_id}/bulk-data");
    let items = backend
        .list_bulk_data_categories()
        .await?
        .into_iter()
        .map(|c| CategoryRef {
            href: format!("{base}/{}", c.name),
            id: c.name,
        })
        .collect();
    Ok(Json(CategoriesResponse { items }))
}

/// GET /vehicle/v1/components/:component_id/bulk-data/:category
pub async fn list_items(
    State(state): State<AppState>,
    Path((component_id, category)): Path<(String, String)>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<ItemsResponse>, ApiError> {
    let backend = require_bulk_data(&state, &component_id)?;
    let filter = BulkDataFilter {
        created_before: query.created_before,
        created_after: query.created_after,
    };
    let base = format!("/vehicle/v1/components/{component_id}/bulk-data/{category}");
    let items = backend
        .list_bulk_data(&category, &filter)
        .await?
        .into_iter()
        .map(|it| ItemRef {
            href: format!("{base}/{}", it.id),
            id: it.id,
            size: it.size,
            created: it.created,
            mime: it.mime,
            source: it.source,
        })
        .collect();
    Ok(Json(ItemsResponse { items }))
}

/// GET /vehicle/v1/components/:component_id/bulk-data/:category/:id
///
/// §7.20 download: `200` with the bytes inline, `307` redirect to a direct URL,
/// or `202` when the payload is being staged asynchronously (poll `Location`).
pub async fn download(
    State(state): State<AppState>,
    Path((component_id, category, id)): Path<(String, String, String)>,
) -> Result<Response, ApiError> {
    let backend = require_bulk_data(&state, &component_id)?;
    match backend.get_bulk_data(&category, &id).await? {
        BulkDataDownload::Inline { mime, bytes } => {
            let len = bytes.len();
            Ok(Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, mime)
                .header(header::CONTENT_LENGTH, len)
                .body(Body::from(bytes))
                .map_err(|e| ApiError::Internal(format!("build download response: {e}")))?)
        }
        // 307: the client re-requests the direct URL. Preserves the method.
        BulkDataDownload::Redirect { location } => Ok((
            StatusCode::TEMPORARY_REDIRECT,
            [(header::LOCATION, location)],
        )
            .into_response()),
        // 202: payload is being prepared; the client polls Location until ready.
        BulkDataDownload::Async { location } => Ok((
            StatusCode::ACCEPTED,
            [(header::LOCATION, location)],
        )
            .into_response()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovd_core::{
        BackendError, BackendResult, BulkCategory, BulkDataItem, Capabilities, DataValue,
        DiagnosticBackend, EntityInfo, FaultFilter, FaultsResult, OperationExecution, OperationInfo,
        ParameterInfo,
    };
    use std::collections::HashMap;
    use std::sync::Arc;

    /// A backend that exposes a single `logs` bulk-data category with one item
    /// whose bytes are "hello". Everything else is the minimal required stub.
    struct BulkBackend {
        info: EntityInfo,
        caps: Capabilities,
    }

    impl BulkBackend {
        fn new() -> Self {
            let caps = Capabilities {
                bulk_data: true,
                ..Capabilities::default()
            };
            Self {
                info: EntityInfo {
                    id: "vm1".into(),
                    name: "vm1".into(),
                    entity_type: "component".into(),
                    description: None,
                    href: "/vehicle/v1/components/vm1".into(),
                    status: Some("online".into()),
                },
                caps,
            }
        }
    }

    #[async_trait::async_trait]
    impl DiagnosticBackend for BulkBackend {
        fn entity_info(&self) -> &EntityInfo {
            &self.info
        }
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        async fn list_parameters(&self) -> BackendResult<Vec<ParameterInfo>> {
            Ok(vec![])
        }
        async fn read_data(&self, _ids: &[String]) -> BackendResult<Vec<DataValue>> {
            Ok(vec![])
        }
        async fn get_faults(&self, _f: Option<&FaultFilter>) -> BackendResult<FaultsResult> {
            Ok(FaultsResult {
                faults: vec![],
                status_availability_mask: None,
            })
        }
        async fn list_operations(&self) -> BackendResult<Vec<OperationInfo>> {
            Ok(vec![])
        }
        async fn start_operation(&self, op: &str, _p: &[u8]) -> BackendResult<OperationExecution> {
            Err(BackendError::OperationNotFound(op.into()))
        }
        // ---- bulk-data surface under test ----
        async fn list_bulk_data_categories(&self) -> BackendResult<Vec<BulkCategory>> {
            Ok(vec![BulkCategory { name: "logs".into() }])
        }
        async fn list_bulk_data(
            &self,
            category: &str,
            _f: &BulkDataFilter,
        ) -> BackendResult<Vec<BulkDataItem>> {
            if category != "logs" {
                return Err(BackendError::EntityNotFound(format!("category {category}")));
            }
            Ok(vec![BulkDataItem {
                id: "svc".into(),
                size: 5,
                created: DateTime::<Utc>::UNIX_EPOCH,
                mime: "text/plain".into(),
                source: Some("svc".into()),
            }])
        }
        async fn get_bulk_data(&self, category: &str, id: &str) -> BackendResult<BulkDataDownload> {
            if category == "logs" && id == "svc" {
                Ok(BulkDataDownload::Inline {
                    mime: "text/plain".into(),
                    bytes: b"hello".to_vec(),
                })
            } else {
                Err(BackendError::EntityNotFound(format!("{category}/{id}")))
            }
        }
    }

    fn state() -> AppState {
        let mut b: HashMap<String, Arc<dyn DiagnosticBackend>> = HashMap::new();
        b.insert("vm1".into(), Arc::new(BulkBackend::new()));
        // A second component WITHOUT bulk-data, to prove the 501 gate.
        AppState::new(b)
    }

    #[tokio::test]
    async fn categories_then_items_then_download() {
        let st = state();
        let cats = list_categories(State(st.clone()), Path("vm1".into()))
            .await
            .expect("categories")
            .0;
        assert_eq!(cats.items.len(), 1);
        assert_eq!(cats.items[0].id, "logs");
        assert!(cats.items[0].href.ends_with("/bulk-data/logs"));

        let items = list_items(
            State(st.clone()),
            Path(("vm1".into(), "logs".into())),
            Query(ItemsQuery::default()),
        )
        .await
        .expect("items")
        .0;
        assert_eq!(items.items.len(), 1);
        assert_eq!(items.items[0].id, "svc");
        assert_eq!(items.items[0].size, 5);
        assert!(items.items[0].href.ends_with("/bulk-data/logs/svc"));

        let resp = download(
            State(st),
            Path(("vm1".into(), "logs".into(), "svc".into())),
        )
        .await
        .expect("download")
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/plain"
        );
    }

    #[tokio::test]
    async fn unknown_category_is_404() {
        let st = state();
        let err = list_items(
            State(st),
            Path(("vm1".into(), "nope".into())),
            Query(ItemsQuery::default()),
        )
        .await
        .expect_err("unknown category");
        assert!(matches!(err, ApiError::NotFound(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn bulk_data_uncapable_component_is_501() {
        // The default backend (no bulk_data cap) must 501 on the collection.
        let mut b: HashMap<String, Arc<dyn DiagnosticBackend>> = HashMap::new();
        // reuse BulkBackend but flip the cap off
        let mut be = BulkBackend::new();
        be.caps.bulk_data = false;
        b.insert("vm1".into(), Arc::new(be));
        let st = AppState::new(b);
        let err = list_categories(State(st), Path("vm1".into()))
            .await
            .expect_err("no bulk-data cap");
        assert!(matches!(err, ApiError::NotImplemented(_)), "got {err:?}");
    }
}
