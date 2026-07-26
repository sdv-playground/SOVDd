//! Diagnostic handlers — SOVD §7.9 (read-only system introspection).
//!
//! Exposes a guest's READ-ONLY system probes (memory / disk / log usage /
//! processes / boot log …) as a discoverable `diagnostics` collection:
//!   GET /{entity}/diagnostics            → list registered probes (+ ?tags=)
//!   GET /{entity}/diagnostics/{id}       → gather that probe now → result
//! `list_diagnostics` / `read_diagnostic` default on the trait, so a backend
//! with no diag surface serves an empty collection / honest not-supported. A
//! machine-manager guest backend overrides them to proxy its in-guest
//! diag-agent. See `tasks/diag-agent-design.md`.

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use sovd_core::DiagnosticResult;

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct DiagnosticsResponse {
    pub items: Vec<DiagnosticRef>,
}

#[derive(Debug, Serialize)]
pub struct DiagnosticRef {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Resource URL for this probe.
    pub href: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct DiagnosticsQuery {
    /// §6.2.7 `tags` filter (case-sensitive).
    pub tags: Option<String>,
}

/// GET /vehicle/v1/components/:component_id/diagnostics
pub async fn list_diagnostics(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
    Query(query): Query<DiagnosticsQuery>,
) -> Result<Json<DiagnosticsResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;
    let base = format!("/vehicle/v1/components/{component_id}/diagnostics");
    let items = backend
        .list_diagnostics()
        .await?
        .into_iter()
        // §6.2.7 tags filter: keep only probes carrying the requested tag.
        .filter(|d| match &query.tags {
            Some(want) => d.tags.iter().any(|t| t == want),
            None => true,
        })
        .map(|d| DiagnosticRef {
            href: format!("{base}/{}", d.id),
            id: d.id,
            title: d.title,
            tags: d.tags,
        })
        .collect();
    Ok(Json(DiagnosticsResponse { items }))
}

/// GET /vehicle/v1/components/:component_id/diagnostics/:probe_id
/// Gather one probe now — the backend proxies the diag-agent's `GET
/// /probes/{id}` and returns the [`DiagnosticResult`] verbatim (ok / output /
/// message). A probe that fails to gather is a 200 with `ok=false` (the read
/// SUCCEEDED, the probe reports its own failure) — not an HTTP error.
pub async fn read_diagnostic(
    State(state): State<AppState>,
    Path((component_id, probe_id)): Path<(String, String)>,
) -> Result<Json<DiagnosticResult>, ApiError> {
    let backend = state.get_backend(&component_id)?;
    let result = backend.read_diagnostic(&probe_id).await?;
    Ok(Json(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovd_core::{
        BackendError, BackendResult, Capabilities, DataValue, DiagnosticBackend, DiagnosticInfo,
        EntityInfo, FaultFilter, FaultsResult, OperationExecution, OperationInfo, ParameterInfo,
    };
    use std::collections::HashMap;
    use std::sync::Arc;

    /// Backend exposing two probes (one tagged `smoke`, one `storage`) and one
    /// gatherable result — the guest-proxy shape.
    struct DiagBackend {
        info: EntityInfo,
        caps: Capabilities,
    }
    impl DiagBackend {
        fn new() -> Self {
            Self {
                info: EntityInfo {
                    id: "vm1".into(),
                    name: "vm1".into(),
                    entity_type: "component".into(),
                    description: None,
                    href: "/vehicle/v1/components/vm1".into(),
                    status: Some("online".into()),
                },
                caps: Capabilities {
                    diagnostics: true,
                    ..Default::default()
                },
            }
        }
    }
    #[async_trait::async_trait]
    impl DiagnosticBackend for DiagBackend {
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
            Err(BackendError::NotSupported(op.to_string()))
        }
        async fn list_diagnostics(&self) -> BackendResult<Vec<DiagnosticInfo>> {
            Ok(vec![
                DiagnosticInfo {
                    id: "guest-hal.mem".into(),
                    title: Some("Memory".into()),
                    tags: vec!["health".into(), "smoke".into()],
                },
                DiagnosticInfo {
                    id: "guest-hal.disk".into(),
                    title: None,
                    tags: vec!["storage".into()],
                },
            ])
        }
        async fn read_diagnostic(&self, probe_id: &str) -> BackendResult<DiagnosticResult> {
            match probe_id {
                "guest-hal.mem" => Ok(DiagnosticResult {
                    id: probe_id.into(),
                    ok: true,
                    output: "MemTotal: 2048 kB\n".into(),
                    message: None,
                }),
                _ => Err(BackendError::EntityNotFound(probe_id.to_string())),
            }
        }
    }

    fn state() -> AppState {
        let mut b: HashMap<String, Arc<dyn DiagnosticBackend>> = HashMap::new();
        b.insert("vm1".into(), Arc::new(DiagBackend::new()));
        AppState::new(b)
    }

    #[tokio::test]
    async fn lists_probes_with_href() {
        let r = list_diagnostics(
            State(state()),
            Path("vm1".into()),
            Query(DiagnosticsQuery::default()),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(r.items.len(), 2);
        assert!(r
            .items
            .iter()
            .any(|d| d.id == "guest-hal.mem" && d.href.ends_with("/diagnostics/guest-hal.mem")));
    }

    #[tokio::test]
    async fn tags_filter_selects() {
        let r = list_diagnostics(
            State(state()),
            Path("vm1".into()),
            Query(DiagnosticsQuery {
                tags: Some("storage".into()),
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(r.items.len(), 1);
        assert_eq!(r.items[0].id, "guest-hal.disk");
    }

    #[tokio::test]
    async fn reads_a_probe_result() {
        let r = read_diagnostic(State(state()), Path(("vm1".into(), "guest-hal.mem".into())))
            .await
            .unwrap()
            .0;
        assert!(r.ok);
        assert!(r.output.contains("MemTotal"));
    }

    #[tokio::test]
    async fn unknown_probe_is_error() {
        let r = read_diagnostic(State(state()), Path(("vm1".into(), "nope".into()))).await;
        assert!(r.is_err());
    }
}
