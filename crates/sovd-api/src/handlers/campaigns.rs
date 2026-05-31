//! Spec-compliant `/campaigns` collection — heterogeneous multi-target
//! update orchestration.  Companion to `/updates` (F.D2) which still
//! handles per-target staging; this collection bundles them so
//! `apply` / `commit` / `rollback` fans out across all members.
//!
//! ## F.D4 scope (thin coordinator)
//!
//! Members are referenced by their existing
//! `(component_id, update_id)` tuples — clients open each `/updates`
//! first, then POST a campaign listing them (inside-out registration).
//! Wire-shape pattern matches `/updates`: single collection, executions
//! sub-resource, lifecycle verbs (`stage` / `apply` / `commit` /
//! `rollback` / `abort`).
//!
//! Actions fan out **sequentially** across members and short-circuit on
//! the first failure (best-effort recovery in F.D5).  Banked-vs-Singleshot
//! ordering — defer Singleshot finalize until after the Banked reboot
//! per `tasks/sw-update-architecture.md` §5 — is documented but not yet
//! implemented; the SOVD wire doesn't carry component capabilities
//! today.  F.D5 wires the orchestrator and adds the ordering.

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sovd_core::OperationStatus;
use uuid::Uuid;

use crate::error::ApiError;
use crate::state::{AppState, CampaignMember, CampaignState, CampaignsEntry};

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct RegisterCampaignRequest {
    /// Members to coordinate.  Each entry references an existing
    /// `/updates` resource by its `(component_id, update_id)` tuple.
    pub members: Vec<CampaignMemberRequest>,
    /// Optional opaque manifest description — recorded and echoed back
    /// for clients that want to associate a fleet-side manifest with
    /// the campaign.  Not interpreted by the SOVD layer.
    #[serde(default)]
    pub manifest: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct CampaignMemberRequest {
    pub component_id: String,
    pub update_id: String,
}

#[derive(Debug, Serialize)]
pub struct RegisterCampaignResponse {
    pub campaign_id: String,
    pub href: String,
    pub executions_href: String,
}

#[derive(Debug, Serialize)]
pub struct CampaignsListResponse {
    pub items: Vec<CampaignSummary>,
}

#[derive(Debug, Serialize)]
pub struct CampaignSummary {
    pub campaign_id: String,
    pub state: String,
    pub members: usize,
    pub href: String,
}

#[derive(Debug, Serialize)]
pub struct CampaignStatusResponse {
    pub campaign_id: String,
    pub state: String,
    pub members: Vec<CampaignMemberStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest: Option<serde_json::Value>,
    pub href: String,
}

#[derive(Debug, Serialize)]
pub struct CampaignMemberStatus {
    pub component_id: String,
    pub update_id: String,
    /// Mirror of the underlying `/updates/{id}` state — refreshed each
    /// time the campaign's status is read so clients see drift
    /// without per-member polling.
    pub update_state: String,
    pub update_href: String,
}

#[derive(Debug, Deserialize)]
pub struct ExecutionRequest {
    pub action: String,
}

#[derive(Debug, Serialize)]
pub struct CampaignExecution {
    pub execution_id: String,
    pub campaign_id: String,
    pub action: String,
    pub status: OperationStatus,
    /// Per-member outcomes (in the order members were registered).
    pub members: Vec<MemberExecutionOutcome>,
    pub started_at: String,
    pub completed_at: String,
}

#[derive(Debug, Serialize)]
pub struct MemberExecutionOutcome {
    pub component_id: String,
    pub update_id: String,
    pub status: OperationStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// POST /vehicle/v1/campaigns
pub async fn register_campaign(
    State(state): State<AppState>,
    Json(request): Json<RegisterCampaignRequest>,
) -> Result<impl IntoResponse, ApiError> {
    if request.members.is_empty() {
        return Err(ApiError::BadRequest(
            "campaign must have at least one member".into(),
        ));
    }

    // Validate every member references a known update / component
    // pair before allocating the campaign id.  We borrow each lock
    // in turn so the validation pass is cheap and side-effect-free.
    {
        let store = state.updates.0.lock();
        for m in &request.members {
            let entry = store
                .get(&m.update_id)
                .ok_or_else(|| ApiError::BadRequest(format!("update {} not found", m.update_id)))?;
            if entry.component_id != m.component_id {
                return Err(ApiError::UnsupportedMediaType(format!(
                    "update {} belongs to component {:?}, not {:?}",
                    m.update_id, entry.component_id, m.component_id
                )));
            }
        }
    }

    let campaign_id = Uuid::new_v4().to_string();
    let members = request
        .members
        .into_iter()
        .map(|m| CampaignMember {
            component_id: m.component_id,
            update_id: m.update_id,
        })
        .collect();

    state.campaigns.0.lock().insert(
        campaign_id.clone(),
        CampaignsEntry {
            members,
            manifest: request.manifest,
            state: CampaignState::Registered,
        },
    );

    let base = format!("/vehicle/v1/campaigns/{}", campaign_id);
    let resp = RegisterCampaignResponse {
        campaign_id: campaign_id.clone(),
        href: base.clone(),
        executions_href: format!("{base}/executions"),
    };
    let mut headers = HeaderMap::new();
    headers.insert(
        header::LOCATION,
        HeaderValue::from_str(&base)
            .map_err(|e| ApiError::Internal(format!("bad Location header: {e}")))?,
    );
    Ok((StatusCode::CREATED, headers, Json(resp)))
}

/// GET /vehicle/v1/campaigns
pub async fn list_campaigns(State(state): State<AppState>) -> Json<CampaignsListResponse> {
    let store = state.campaigns.0.lock();
    let items = store
        .iter()
        .map(|(id, e)| CampaignSummary {
            campaign_id: id.clone(),
            state: e.state.as_str().to_string(),
            members: e.members.len(),
            href: format!("/vehicle/v1/campaigns/{}", id),
        })
        .collect();
    Json(CampaignsListResponse { items })
}

/// GET /vehicle/v1/campaigns/{campaign_id}
pub async fn get_campaign(
    State(state): State<AppState>,
    Path(campaign_id): Path<String>,
) -> Result<Json<CampaignStatusResponse>, ApiError> {
    let campaign = state
        .campaigns
        .0
        .lock()
        .get(&campaign_id)
        .cloned()
        .ok_or_else(|| ApiError::NotFound(format!("campaign {campaign_id} not found")))?;

    let updates_store = state.updates.0.lock();
    let members = campaign
        .members
        .iter()
        .map(|m| {
            let update_state = updates_store
                .get(&m.update_id)
                .map(|e| e.state.as_str().to_string())
                .unwrap_or_else(|| "missing".to_string());
            CampaignMemberStatus {
                component_id: m.component_id.clone(),
                update_id: m.update_id.clone(),
                update_state,
                update_href: format!(
                    "/vehicle/v1/components/{}/updates/{}",
                    m.component_id, m.update_id
                ),
            }
        })
        .collect();

    Ok(Json(CampaignStatusResponse {
        campaign_id: campaign_id.clone(),
        state: campaign.state.as_str().to_string(),
        members,
        manifest: campaign.manifest.clone(),
        href: format!("/vehicle/v1/campaigns/{campaign_id}"),
    }))
}

/// DELETE /vehicle/v1/campaigns/{campaign_id}
///
/// Removes the SOVD-side campaign bookkeeping.  Does **not** abort the
/// member `/updates` — clients can keep them or DELETE them
/// individually.  Use `POST /executions {action: "abort"}` if you want
/// the abort fan-out.
pub async fn delete_campaign(
    State(state): State<AppState>,
    Path(campaign_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let removed = state.campaigns.0.lock().remove(&campaign_id);
    if removed.is_none() {
        return Err(ApiError::NotFound(format!(
            "campaign {campaign_id} not found"
        )));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// POST /vehicle/v1/campaigns/{campaign_id}/executions
pub async fn post_execution(
    State(state): State<AppState>,
    Path(campaign_id): Path<String>,
    Json(request): Json<ExecutionRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let members = {
        let store = state.campaigns.0.lock();
        let entry = store
            .get(&campaign_id)
            .ok_or_else(|| ApiError::NotFound(format!("campaign {campaign_id} not found")))?;
        entry.members.clone()
    };

    let started_at = Utc::now();
    let exec_id = Uuid::new_v4().to_string();

    let (member_action, next_state, fail_state) = match request.action.as_str() {
        "stage" => ("stage", CampaignState::Staged, CampaignState::Failed),
        "apply" => ("finalize", CampaignState::Finalized, CampaignState::Failed),
        "commit" => ("commit", CampaignState::Committed, CampaignState::Failed),
        "rollback" => ("rollback", CampaignState::RolledBack, CampaignState::Failed),
        "abort" => ("abort", CampaignState::Aborted, CampaignState::Failed),
        other => {
            return Err(ApiError::BadRequest(format!(
                "unknown campaign action {other:?}; want stage|apply|commit|rollback|abort"
            )));
        }
    };

    let mut outcomes: Vec<MemberExecutionOutcome> = Vec::with_capacity(members.len());
    let mut overall_status = OperationStatus::Completed;

    for m in &members {
        let outcome = run_member_action(&state, m, member_action).await;
        if matches!(outcome.status, OperationStatus::Failed) {
            overall_status = OperationStatus::Failed;
        }
        outcomes.push(outcome);
        if overall_status == OperationStatus::Failed {
            // Short-circuit per F.D4 thin scope; F.D5's orchestrator
            // adds rollback-on-partial-failure.
            break;
        }
    }

    {
        let mut store = state.campaigns.0.lock();
        if let Some(entry) = store.get_mut(&campaign_id) {
            entry.state = if overall_status == OperationStatus::Failed {
                fail_state
            } else {
                next_state
            };
        }
    }

    let completed_at = Utc::now();
    let execution = CampaignExecution {
        execution_id: exec_id.clone(),
        campaign_id: campaign_id.clone(),
        action: request.action,
        status: overall_status,
        members: outcomes,
        started_at: started_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        completed_at: completed_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    };

    let href = format!("/vehicle/v1/campaigns/{campaign_id}/executions/{exec_id}");
    let mut headers = HeaderMap::new();
    headers.insert(
        header::LOCATION,
        HeaderValue::from_str(&href)
            .map_err(|e| ApiError::Internal(format!("bad Location header: {e}")))?,
    );
    Ok((StatusCode::OK, headers, Json(execution)))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn run_member_action(
    state: &AppState,
    member: &CampaignMember,
    action: &str,
) -> MemberExecutionOutcome {
    // `stage` is special — it doesn't drive an /executions verb on the
    // member; it asserts the underlying update has reached a "verified"
    // state so the campaign can move forward.  This keeps stage idempotent
    // and decoupled from upload mechanics.
    if action == "stage" {
        let store = state.updates.0.lock();
        return match store.get(&member.update_id) {
            Some(entry) => {
                let state_str = entry.state.as_str();
                let ok = matches!(
                    state_str,
                    "verified" | "finalized" | "committed" | "rolledback"
                );
                MemberExecutionOutcome {
                    component_id: member.component_id.clone(),
                    update_id: member.update_id.clone(),
                    status: if ok {
                        OperationStatus::Completed
                    } else {
                        OperationStatus::Failed
                    },
                    message: Some(format!("update is in state {state_str}")),
                }
            }
            None => MemberExecutionOutcome {
                component_id: member.component_id.clone(),
                update_id: member.update_id.clone(),
                status: OperationStatus::Failed,
                message: Some("update no longer exists".into()),
            },
        };
    }

    // For all other verbs, delegate to the member's own
    // `/updates/{id}/executions` path by calling the same handler.
    use super::updates;
    let result = updates::post_execution(
        axum::extract::State(state.clone()),
        axum::extract::Path((member.component_id.clone(), member.update_id.clone())),
        axum::extract::Query(updates::ExecutionQuery::default()),
        axum::Json(updates::ExecutionRequest {
            action: action.to_string(),
        }),
    )
    .await;

    match result {
        Ok(_) => MemberExecutionOutcome {
            component_id: member.component_id.clone(),
            update_id: member.update_id.clone(),
            status: OperationStatus::Completed,
            message: None,
        },
        Err(e) => MemberExecutionOutcome {
            component_id: member.component_id.clone(),
            update_id: member.update_id.clone(),
            status: OperationStatus::Failed,
            message: Some(format!("{e:?}")),
        },
    }
}
