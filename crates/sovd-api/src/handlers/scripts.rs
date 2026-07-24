//! Script handlers — SOVD §7.15 ("Manage & execute scripts", dev/production).
//!
//! We use `scripts` for developer-registered TESTS: a script = a registered test
//! (from a layer manifest, discovered by the backend's guest test-agent proxy),
//! an execution = a test run. This slice is DISCOVERY only:
//!   GET /{entity}/scripts            → list registered tests (+ ?tags= filter)
//!   GET /{entity}/scripts/{id}       → one test's metadata
//! read/upload/execute/executions come in later slices (see
//! tasks/sovd-tests-as-operations-design.md). `list_scripts` defaults to empty on
//! the trait, so a backend with no test surface serves an empty collection.

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct ScriptsResponse {
    pub items: Vec<ScriptRef>,
}

#[derive(Debug, Serialize)]
pub struct ScriptRef {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Resource URL for this script.
    pub href: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct ScriptsQuery {
    /// §6.2.7 `tags` filter (case-sensitive). One value this slice.
    pub tags: Option<String>,
}

/// GET /vehicle/v1/components/:component_id/scripts
pub async fn list_scripts(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
    Query(query): Query<ScriptsQuery>,
) -> Result<Json<ScriptsResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;
    let base = format!("/vehicle/v1/components/{component_id}/scripts");
    let items = backend
        .list_scripts()
        .await?
        .into_iter()
        // §6.2.7 tags filter: keep only scripts carrying the requested tag.
        .filter(|s| match &query.tags {
            Some(want) => s.tags.iter().any(|t| t == want),
            None => true,
        })
        .map(|s| ScriptRef {
            href: format!("{base}/{}", s.id),
            id: s.id,
            title: s.title,
            tags: s.tags,
        })
        .collect();
    Ok(Json(ScriptsResponse { items }))
}

/// GET /vehicle/v1/components/:component_id/scripts/:script_id
/// One script's metadata (re-derived from the list — the backend has no
/// per-script read yet; a `logs/entries`-style linkage).
pub async fn read_script(
    State(state): State<AppState>,
    Path((component_id, script_id)): Path<(String, String)>,
) -> Result<Json<ScriptRef>, ApiError> {
    let backend = state.get_backend(&component_id)?;
    let base = format!("/vehicle/v1/components/{component_id}/scripts");
    backend
        .list_scripts()
        .await?
        .into_iter()
        .find(|s| s.id == script_id)
        .map(|s| {
            Json(ScriptRef {
                href: format!("{base}/{}", s.id),
                id: s.id,
                title: s.title,
                tags: s.tags,
            })
        })
        .ok_or_else(|| ApiError::NotFound(format!("Script not found: {script_id}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovd_core::{
        BackendError, BackendResult, Capabilities, DataValue, DiagnosticBackend, EntityInfo,
        FaultFilter, FaultsResult, OperationExecution, OperationInfo, ParameterInfo, ScriptInfo,
    };
    use std::collections::HashMap;
    use std::sync::Arc;

    /// Backend exposing two registered tests as scripts (one tagged `smoke`).
    struct ScriptBackend {
        info: EntityInfo,
        caps: Capabilities,
    }
    impl ScriptBackend {
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
                caps: Capabilities::default(),
            }
        }
    }
    #[async_trait::async_trait]
    impl DiagnosticBackend for ScriptBackend {
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
            Ok(FaultsResult { faults: vec![], status_availability_mask: None })
        }
        async fn list_operations(&self) -> BackendResult<Vec<OperationInfo>> {
            Ok(vec![])
        }
        async fn start_operation(&self, op: &str, _p: &[u8]) -> BackendResult<OperationExecution> {
            Err(BackendError::OperationNotFound(op.into()))
        }
        async fn list_scripts(&self) -> BackendResult<Vec<ScriptInfo>> {
            Ok(vec![
                ScriptInfo { id: "guest-hal.smoke".into(), title: Some("Smoke".into()), tags: vec!["smoke".into()] },
                ScriptInfo { id: "guest-hal.slow".into(), title: None, tags: vec!["slow".into()] },
            ])
        }
    }

    fn state() -> AppState {
        let mut b: HashMap<String, Arc<dyn DiagnosticBackend>> = HashMap::new();
        b.insert("vm1".into(), Arc::new(ScriptBackend::new()));
        AppState::new(b)
    }

    #[tokio::test]
    async fn lists_scripts_with_href() {
        let r = list_scripts(State(state()), Path("vm1".into()), Query(ScriptsQuery::default()))
            .await
            .unwrap()
            .0;
        assert_eq!(r.items.len(), 2);
        assert!(r.items.iter().any(|s| s.id == "guest-hal.smoke"
            && s.href.ends_with("/scripts/guest-hal.smoke")));
    }

    #[tokio::test]
    async fn tags_filter_selects() {
        let r = list_scripts(
            State(state()),
            Path("vm1".into()),
            Query(ScriptsQuery { tags: Some("smoke".into()) }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(r.items.len(), 1);
        assert_eq!(r.items[0].id, "guest-hal.smoke");
    }

    #[tokio::test]
    async fn read_one_and_404() {
        let ok = read_script(State(state()), Path(("vm1".into(), "guest-hal.slow".into())))
            .await
            .unwrap()
            .0;
        assert_eq!(ok.id, "guest-hal.slow");
        let err = read_script(State(state()), Path(("vm1".into(), "nope".into())))
            .await
            .expect_err("unknown script");
        assert!(matches!(err, ApiError::NotFound(_)));
    }
}
