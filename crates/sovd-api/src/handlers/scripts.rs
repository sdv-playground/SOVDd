//! Script handlers — SOVD §7.15 ("Manage & execute scripts", dev/production).
//!
//! We use `scripts` for developer-registered TESTS: a script = a registered test
//! (from a layer manifest, discovered by the backend's guest test-agent proxy),
//! an execution = a test run. This slice is DISCOVERY only:
//!   GET /{entity}/scripts            → list registered tests (+ ?tags= filter)
//!   GET /{entity}/scripts/{id}       → one test's metadata
//! An execution = a test RUN, wired to the backend's async 202+poll surface
//! (mirrors §7.14 operations / reset.rs):
//!   POST /{entity}/scripts/{id}/executions            → 202 + Location
//!   GET  /{entity}/scripts/{id}/executions            → list execution ids
//!   GET  /{entity}/scripts/{id}/executions/{exec-id}  → status + result
//! `list_scripts`/`start_script`/... default on the trait, so a backend with no
//! test surface serves an empty collection / honest 501.

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use sovd_core::{ScriptExecution, ScriptStatus, ScriptVerdict};

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

/// Wire body for one script (test) execution — §7.15 execution status + result.
/// Maps the core [`ScriptExecution`] (like the other handlers map core → a
/// response type); adds the resource `href`.
#[derive(Debug, Serialize)]
pub struct ScriptExecutionResponse {
    pub exec_id: String,
    pub script_id: String,
    pub status: ScriptStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<ScriptVerdict>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout_tail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr_tail: Option<String>,
    pub started: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended: Option<String>,
    /// Vendor log-cursor bracket — see [`ScriptExecution`].
    #[serde(rename = "x-log-from", skip_serializing_if = "Option::is_none")]
    pub log_from: Option<String>,
    #[serde(rename = "x-log-to", skip_serializing_if = "Option::is_none")]
    pub log_to: Option<String>,
    pub href: String,
}

impl ScriptExecutionResponse {
    fn from_core(exec: ScriptExecution, base: &str) -> Self {
        let href = format!("{base}/{}/executions/{}", exec.script_id, exec.exec_id);
        ScriptExecutionResponse {
            exec_id: exec.exec_id,
            script_id: exec.script_id,
            status: exec.status,
            verdict: exec.verdict,
            exit_code: exec.exit_code,
            duration_ms: exec.duration_ms,
            message: exec.message,
            stdout_tail: exec.stdout_tail,
            stderr_tail: exec.stderr_tail,
            started: exec.started,
            ended: exec.ended,
            log_from: exec.log_from,
            log_to: exec.log_to,
            href,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ScriptExecutionsResponse {
    /// Execution ids for this script (newest-agnostic; backend order).
    pub items: Vec<ScriptExecutionRef>,
}

#[derive(Debug, Serialize)]
pub struct ScriptExecutionRef {
    pub exec_id: String,
    pub href: String,
}

/// POST /vehicle/v1/components/:component_id/scripts/:script_id/executions
///
/// §7.15 script.execute. Runs the registered test via the backend and returns
/// **202 Accepted** + a `Location` header to the execution resource, mirroring
/// reset.rs / operations.rs. NOTE: the guest test-agent run is SYNCHRONOUS, so
/// `start_script` blocks until the test finishes and returns an execution that
/// is already `Done` — we still answer 202 + Location (the spec's async shape),
/// and the client's immediately-following `GET .../executions/{exec}` reads the
/// terminal result. A truly long-running test therefore holds this request open
/// for its duration (bounded by the entry's `timeout_ms` in the agent).
pub async fn execute_script(
    State(state): State<AppState>,
    Path((component_id, script_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    let backend = state.get_backend(&component_id)?;
    let base = format!("/vehicle/v1/components/{component_id}/scripts");
    let exec = backend.start_script(&script_id).await?;
    let resp = ScriptExecutionResponse::from_core(exec, &base);

    let mut headers = HeaderMap::new();
    headers.insert(
        header::LOCATION,
        HeaderValue::from_str(&resp.href)
            .map_err(|e| ApiError::Internal(format!("bad Location header: {e}")))?,
    );
    Ok((StatusCode::ACCEPTED, headers, Json(resp)))
}

/// GET /vehicle/v1/components/:component_id/scripts/:script_id/executions
/// §7.15 executions.query — the execution ids recorded for a script.
pub async fn list_executions(
    State(state): State<AppState>,
    Path((component_id, script_id)): Path<(String, String)>,
) -> Result<Json<ScriptExecutionsResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;
    let base = format!("/vehicle/v1/components/{component_id}/scripts");
    let items = backend
        .list_script_executions(&script_id)
        .await?
        .into_iter()
        .map(|exec_id| ScriptExecutionRef {
            href: format!("{base}/{script_id}/executions/{exec_id}"),
            exec_id,
        })
        .collect();
    Ok(Json(ScriptExecutionsResponse { items }))
}

/// GET /vehicle/v1/components/:component_id/scripts/:script_id/executions/:exec_id
/// §7.15 execution.status — one execution's status + result envelope.
pub async fn get_execution(
    State(state): State<AppState>,
    Path((component_id, script_id, exec_id)): Path<(String, String, String)>,
) -> Result<Json<ScriptExecutionResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;
    let base = format!("/vehicle/v1/components/{component_id}/scripts");
    let exec = backend.get_script_execution(&script_id, &exec_id).await?;
    Ok(Json(ScriptExecutionResponse::from_core(exec, &base)))
}

/// PUT /vehicle/v1/components/:component_id/scripts/:script_id/executions/:exec_id
/// §7.15 execution.terminate (OPTIONAL). The guest run is synchronous — an
/// execution is already terminal by the time it exists — so there is nothing to
/// terminate. Honest 501 rather than pretending to stop a finished run.
pub async fn terminate_execution(
    State(state): State<AppState>,
    Path((component_id, _script_id, _exec_id)): Path<(String, String, String)>,
) -> Result<StatusCode, ApiError> {
    // Validate the component exists before answering.
    state.get_backend(&component_id)?;
    Err(ApiError::NotImplemented(
        "scripts.execution.terminate not supported: guest test runs are synchronous \
         (an execution is already terminal)"
            .into(),
    ))
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

    /// A backend `start_script` synchronously "runs" (a la the guest proxy): the
    /// returned execution is already `Done` with a pass verdict + a log bracket.
    fn done_execution(script_id: &str) -> ScriptExecution {
        ScriptExecution {
            exec_id: format!("{script_id}-1"),
            script_id: script_id.to_string(),
            status: ScriptStatus::Done,
            verdict: Some(ScriptVerdict::Pass),
            exit_code: Some(0),
            duration_ms: Some(12),
            message: None,
            stdout_tail: Some("ok\n".into()),
            stderr_tail: None,
            started: "2026-07-25T00:00:00Z".into(),
            ended: Some("2026-07-25T00:00:00Z".into()),
            log_from: Some("cursor-before".into()),
            log_to: Some("cursor-after".into()),
        }
    }

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
        async fn list_scripts(&self) -> BackendResult<Vec<ScriptInfo>> {
            Ok(vec![
                ScriptInfo {
                    id: "guest-hal.smoke".into(),
                    title: Some("Smoke".into()),
                    tags: vec!["smoke".into()],
                },
                ScriptInfo {
                    id: "guest-hal.slow".into(),
                    title: None,
                    tags: vec!["slow".into()],
                },
            ])
        }
        async fn start_script(&self, script_id: &str) -> BackendResult<ScriptExecution> {
            Ok(done_execution(script_id))
        }
        async fn get_script_execution(
            &self,
            script_id: &str,
            exec_id: &str,
        ) -> BackendResult<ScriptExecution> {
            if exec_id == format!("{script_id}-1") {
                Ok(done_execution(script_id))
            } else {
                Err(BackendError::EntityNotFound(exec_id.into()))
            }
        }
        async fn list_script_executions(&self, script_id: &str) -> BackendResult<Vec<String>> {
            Ok(vec![format!("{script_id}-1")])
        }
    }

    fn state() -> AppState {
        let mut b: HashMap<String, Arc<dyn DiagnosticBackend>> = HashMap::new();
        b.insert("vm1".into(), Arc::new(ScriptBackend::new()));
        AppState::new(b)
    }

    #[tokio::test]
    async fn lists_scripts_with_href() {
        let r = list_scripts(
            State(state()),
            Path("vm1".into()),
            Query(ScriptsQuery::default()),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(r.items.len(), 2);
        assert!(r
            .items
            .iter()
            .any(|s| s.id == "guest-hal.smoke" && s.href.ends_with("/scripts/guest-hal.smoke")));
    }

    #[tokio::test]
    async fn tags_filter_selects() {
        let r = list_scripts(
            State(state()),
            Path("vm1".into()),
            Query(ScriptsQuery {
                tags: Some("smoke".into()),
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(r.items.len(), 1);
        assert_eq!(r.items[0].id, "guest-hal.smoke");
    }

    #[tokio::test]
    async fn read_one_and_404() {
        let ok = read_script(
            State(state()),
            Path(("vm1".into(), "guest-hal.slow".into())),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(ok.id, "guest-hal.slow");
        let err = read_script(State(state()), Path(("vm1".into(), "nope".into())))
            .await
            .expect_err("unknown script");
        assert!(matches!(err, ApiError::NotFound(_)));
    }

    #[tokio::test]
    async fn execute_returns_202_with_location() {
        use axum::http::{header, StatusCode};
        let resp = execute_script(
            State(state()),
            Path(("vm1".into(), "guest-hal.smoke".into())),
        )
        .await
        .unwrap()
        .into_response();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        let loc = resp
            .headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap();
        // The guest run is synchronous → Location points at an already-Done exec.
        assert_eq!(
            loc,
            "/vehicle/v1/components/vm1/scripts/guest-hal.smoke/executions/guest-hal.smoke-1"
        );
    }

    #[tokio::test]
    async fn get_execution_carries_verdict_and_bracket() {
        let r = get_execution(
            State(state()),
            Path((
                "vm1".into(),
                "guest-hal.smoke".into(),
                "guest-hal.smoke-1".into(),
            )),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(r.status, ScriptStatus::Done);
        assert_eq!(r.verdict, Some(ScriptVerdict::Pass));
        assert_eq!(r.exit_code, Some(0));
        assert_eq!(r.log_from.as_deref(), Some("cursor-before"));
        assert_eq!(r.log_to.as_deref(), Some("cursor-after"));

        // Unknown exec id → 404.
        let err = get_execution(
            State(state()),
            Path(("vm1".into(), "guest-hal.smoke".into(), "nope".into())),
        )
        .await
        .expect_err("unknown exec");
        assert!(matches!(err, ApiError::NotFound(_)));
    }

    #[tokio::test]
    async fn list_executions_returns_ids() {
        let r = list_executions(
            State(state()),
            Path(("vm1".into(), "guest-hal.smoke".into())),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(r.items.len(), 1);
        assert_eq!(r.items[0].exec_id, "guest-hal.smoke-1");
        assert!(r.items[0].href.ends_with("/executions/guest-hal.smoke-1"));
    }
}
