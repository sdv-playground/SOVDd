//! Spec-compliant `/updates` collection — ISO 17978-3 §7.13 (UpdateStatusBody).
//!
//! F.D2 is a thin alias over the existing /files + /flash backend with
//! the SOVD layer doing the bookkeeping that the spec wire expects.
//! Multipart-inline transport only — URL-ref is F.D7.
//!
//! Wire → backend mapping:
//!
//! | Wire verb                       | Backend |
//! |---------------------------------|---------|
//! | `POST /updates`                 | (none — SOVD-side allocation of update_id) |
//! | `PUT /bulk-data/{part_id}`      | `receive_package_stream` (returns file_id, recorded per part) |
//! | `POST /executions {verify}`     | `verify_package` per part, then `start_flash` (allocates transfer_id) |
//! | `POST /executions {finalize}`   | `finalize_flash` + `activate` |
//! | `POST /executions {commit}`     | `commit_flash` |
//! | `POST /executions {rollback}`   | `rollback_flash` |
//! | `POST /executions {abort}`      | `abort_flash(transfer_id)` if known; SOVD state cleared |
//! | `DELETE /updates/{id}`          | same as abort |
//!
//! The dispatcher / per-part SUIT awareness arrives in F.D3.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use chrono::Utc;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sovd_core::{OperationStatus, PackageStream};
use uuid::Uuid;

use crate::error::ApiError;
use crate::state::{AppState, Phase, Status, UpdatePart, UpdateState, UpdatesEntry};

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// Body for `POST /updates`.
///
/// `manifest` is the wire-level update description (not the raw SUIT
/// envelope — the envelope arrives in /bulk-data parts).  F.D2 records
/// it and echoes it back; F.D3 adds the optional `target` field which
/// the SOVD dispatcher validates against the path's component_id and
/// rejects on mismatch with HTTP 415.
///
/// Other manifest fields (parts list, version, security_ver, ...) are
/// not yet consumed at the SOVD layer; they ride along for the
/// downstream backend.
#[derive(Debug, Deserialize, Default)]
pub struct RegisterUpdateRequest {
    /// Optional component id the manifest is addressed to.  When
    /// present, MUST match the path's `{component_id}` — otherwise the
    /// server returns 415 Unsupported Media Type before allocating an
    /// update_id.  Absent means "trust the path" (F.D2 behaviour).
    #[serde(default)]
    pub target: Option<String>,
    /// Pass-through manifest document.  Schema is intentionally open
    /// in F.D2/F.D3 — the dispatcher (F.D4+) tightens it.
    #[serde(default)]
    pub manifest: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct RegisterUpdateResponse {
    pub update_id: String,
    pub href: String,
    pub bulk_data_href: String,
    pub executions_href: String,
}

#[derive(Debug, Serialize)]
pub struct UpdatesListResponse {
    pub items: Vec<UpdateSummary>,
}

#[derive(Debug, Serialize)]
pub struct UpdateSummary {
    pub update_id: String,
    pub state: String,
    pub href: String,
}

#[derive(Debug, Serialize)]
pub struct UpdateStatusResponse {
    pub update_id: String,
    pub state: String,
    pub parts_uploaded: usize,
    pub parts: Vec<PartStatusEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transfer_id: Option<String>,
    pub href: String,
}

/// ISO 17978-3 §7.18.7 Table 270 — body of `GET /updates/{id}/status`.
#[derive(Debug, Serialize)]
pub struct UpdateStatusBody {
    pub phase: &'static str,
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<crate::state::UpdateError>,
    /// Vendor extension; populated only when control mode is
    /// orchestrated (Phase B).
    #[serde(rename = "x-sumo-substate", skip_serializing_if = "Option::is_none")]
    pub substate: Option<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct BulkDataListResponse {
    pub items: Vec<PartStatusEntry>,
}

#[derive(Debug, Serialize)]
pub struct PartStatusEntry {
    pub part_id: String,
    pub size: u64,
    pub sha256: String,
    pub href: String,
}

#[derive(Debug, Serialize)]
pub struct PartUploadResponse {
    pub part_id: String,
    pub size: u64,
    pub sha256: String,
    pub href: String,
}

#[derive(Debug, Deserialize)]
pub struct ExecutionRequest {
    pub action: String,
}

#[derive(Debug, Serialize)]
pub struct UpdateExecution {
    pub execution_id: String,
    pub update_id: String,
    pub action: String,
    pub status: OperationStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub started_at: String,
    pub completed_at: String,
}

#[derive(Debug, Deserialize, Default)]
pub struct ExecutionQuery {
    #[serde(default)]
    pub refresh: bool,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// POST /vehicle/v1/components/{component_id}/updates
pub async fn register_update(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
    body: Option<Json<RegisterUpdateRequest>>,
) -> Result<impl IntoResponse, ApiError> {
    // Verify the component exists before allocating an id.
    let backend = state.get_backend(&component_id)?;

    let req = body.map(|Json(b)| b).unwrap_or_default();

    // F.D3 dispatcher target validation.  If the manifest carries an
    // explicit `target`, it MUST match the addressed component.  We
    // reject the mismatch up-front with 415 so the caller doesn't burn
    // bandwidth uploading a payload the backend would refuse anyway.
    if let Some(target) = req.target.as_deref() {
        if target != component_id {
            return Err(ApiError::UnsupportedMediaType(format!(
                "manifest target {:?} does not match addressed component {:?}",
                target, component_id
            )));
        }
    }

    let update_id = Uuid::new_v4().to_string();
    let manifest = req.manifest;

    // Open the backend's flash session up-front. Backends such as
    // `VmBackend` need to be in their `AwaitingManifest` state
    // before the first `receive_package_stream` call, otherwise the
    // upload falls into the "legacy integrated envelope" path that
    // doesn't run the staging pipeline. Calling start_flash here
    // mirrors the legacy /flash wire's ordering (start_flash →
    // upload → finalize) while keeping /updates' separate endpoints.
    //
    // Best-effort: backends that don't preallocate (`NotSupported`) or
    // that require an already-verified package (tier-1 supplier
    // pattern in `ManagedEcuBackend`, errors with `InvalidRequest`)
    // simply skip this step. The actual flash session will be opened
    // later when the package is ready (during the /executions wire's
    // `verify` action or at `PUT /prepare`).
    let transfer_id = match backend.start_flash().await {
        Ok(id) => Some(id),
        Err(_) => None,
    };

    {
        let mut store = state.updates.0.lock();
        store.insert(
            update_id.clone(),
            UpdatesEntry {
                component_id: component_id.clone(),
                parts: Vec::new(),
                manifest,
                state: UpdateState::Registered,
                phase: Phase::default(),
                status: Status::default(),
                progress: None,
                step: None,
                error: None,
                substate: None,
                transfer_id,
                task_handle: None,
            },
        );
    }

    let base = format!(
        "/vehicle/v1/components/{}/updates/{}",
        component_id, update_id
    );
    let resp = RegisterUpdateResponse {
        update_id: update_id.clone(),
        href: base.clone(),
        bulk_data_href: format!("{base}/bulk-data"),
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

/// GET /vehicle/v1/components/{component_id}/updates
pub async fn list_updates(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<Json<UpdatesListResponse>, ApiError> {
    let _ = state.get_backend(&component_id)?;
    let store = state.updates.0.lock();
    let items: Vec<UpdateSummary> = store
        .iter()
        .filter(|(_, e)| e.component_id == component_id)
        .map(|(id, e)| UpdateSummary {
            update_id: id.clone(),
            state: e.state.as_str().to_string(),
            href: format!("/vehicle/v1/components/{}/updates/{}", component_id, id),
        })
        .collect();
    Ok(Json(UpdatesListResponse { items }))
}

/// GET /vehicle/v1/components/{component_id}/updates/{update_id}
pub async fn get_update(
    State(state): State<AppState>,
    Path((component_id, update_id)): Path<(String, String)>,
) -> Result<Json<UpdateStatusResponse>, ApiError> {
    let _ = state.get_backend(&component_id)?;
    let store = state.updates.0.lock();
    let entry = store
        .get(&update_id)
        .filter(|e| e.component_id == component_id)
        .ok_or_else(|| ApiError::NotFound(format!("update {update_id} not found")))?;
    let parts: Vec<PartStatusEntry> = entry
        .parts
        .iter()
        .map(|p| part_status_entry(&component_id, &update_id, p))
        .collect();
    Ok(Json(UpdateStatusResponse {
        update_id: update_id.clone(),
        state: entry.state.as_str().to_string(),
        parts_uploaded: parts.len(),
        parts,
        manifest: entry.manifest.clone(),
        transfer_id: entry.transfer_id.clone(),
        href: format!(
            "/vehicle/v1/components/{}/updates/{}",
            component_id, update_id
        ),
    }))
}

/// DELETE /vehicle/v1/components/{component_id}/updates/{update_id}
pub async fn delete_update(
    State(state): State<AppState>,
    Path((component_id, update_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let backend = state.get_backend(&component_id)?;
    let (transfer_id, abort_handle) = {
        let store = state.updates.0.lock();
        let entry = store
            .get(&update_id)
            .filter(|e| e.component_id == component_id)
            .ok_or_else(|| ApiError::NotFound(format!("update {update_id} not found")))?;
        (entry.transfer_id.clone(), entry.task_handle.clone())
    };
    if let Some(handle) = abort_handle {
        handle.abort();
    }
    if let Some(tid) = transfer_id {
        let _ = backend.abort_flash(&tid).await;
    }
    state.updates.0.lock().remove(&update_id);
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// ISO 17978-3 §7.18 spec-conformant lifecycle: PUT prepare / execute /
// automated + GET status.  Async (202 + Location → poll /status).  See
// `tasks/spec-aligned-updates-wire.md` UPDATE-WIRE-001.
// ---------------------------------------------------------------------------

/// Common 202-response shape for all three lifecycle PUTs.
fn accepted_with_status_location(
    component_id: &str,
    update_id: &str,
) -> Result<(StatusCode, HeaderMap), ApiError> {
    let location = format!(
        "/vehicle/v1/components/{}/updates/{}/status",
        component_id, update_id
    );
    let mut headers = HeaderMap::new();
    headers.insert(
        header::LOCATION,
        HeaderValue::from_str(&location)
            .map_err(|e| ApiError::Internal(format!("bad Location header: {e}")))?,
    );
    Ok((StatusCode::ACCEPTED, headers))
}

/// Update the entry's wire-state, holding the lock for as short as
/// possible.  Returns Err if the entry has been deleted out from under
/// us (which the spawned task should treat as a cancellation).
fn mutate_entry<F>(state: &AppState, update_id: &str, f: F) -> Result<(), &'static str>
where
    F: FnOnce(&mut UpdatesEntry),
{
    let mut store = state.updates.0.lock();
    match store.get_mut(update_id) {
        Some(entry) => {
            f(entry);
            Ok(())
        }
        None => Err("update entry vanished mid-task"),
    }
}

/// `PUT /vehicle/v1/components/{component_id}/updates/{update_id}/prepare`
/// — ISO 17978-3 §7.18.5.  Spawns a background task that re-verifies
/// every uploaded part against its recorded SHA-256 and waits for the
/// backend's staging pipeline to settle.  Returns immediately with
/// `202 Accepted` + `Location: .../status`.
pub async fn put_prepare(
    State(state): State<AppState>,
    Path((component_id, update_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    // Validate component exists up-front; the actual backend handle
    // is re-acquired inside the spawned task.
    let _ = state.get_backend(&component_id)?;

    // Snapshot the parts list + transfer_id under the lock; bail if
    // the entry isn't in a startable phase/status.
    let (parts, transfer_id) = {
        let mut store = state.updates.0.lock();
        let entry = store
            .get_mut(&update_id)
            .filter(|e| e.component_id == component_id)
            .ok_or_else(|| ApiError::NotFound(format!("update {update_id} not found")))?;
        if matches!(entry.status, Status::InProgress) {
            return Err(ApiError::Conflict(format!(
                "update {update_id} is already in {} phase, status inProgress",
                entry.phase.as_str()
            )));
        }
        if entry.parts.is_empty() {
            return Err(ApiError::BadRequest(
                "prepare called before any /bulk-data part uploaded".into(),
            ));
        }
        entry.phase = Phase::Prepare;
        entry.status = Status::InProgress;
        entry.progress = Some(0);
        entry.step = Some("starting prepare".into());
        entry.error = None;
        let parts = entry
            .parts
            .iter()
            .map(|p| (p.part_id.clone(), p.file_id.clone(), p.sha256.clone()))
            .collect::<Vec<_>>();
        (parts, entry.transfer_id.clone())
    };

    // Spawn the prepare task. Use AbortHandle so DELETE can cancel.
    let task_state = state.clone();
    let task_update_id = update_id.clone();
    let task_component_id = component_id.clone();
    let join = tokio::spawn(async move {
        let backend = match task_state.get_backend(&task_component_id) {
            Ok(b) => b,
            Err(e) => {
                let _ = mutate_entry(&task_state, &task_update_id, |entry| {
                    entry.status = Status::Failed;
                    entry.step = Some("backend missing".into());
                    entry.error = Some(crate::state::UpdateError {
                        error_code: "internal-server-error".into(),
                        message: format!("{e:?}"),
                        parameters: None,
                    });
                    entry.task_handle = None;
                });
                return;
            }
        };

        let total = parts.len() as u64;
        for (idx, (part_id, file_id, sha256)) in parts.iter().enumerate() {
            let _ = mutate_entry(&task_state, &task_update_id, |entry| {
                entry.step = Some(format!("verifying part {part_id}"));
                entry.progress = Some(((idx as u64 * 100) / total) as u8);
            });
            let verify = match backend.verify_part(file_id, sha256).await {
                Ok(()) => Ok(()),
                Err(sovd_core::BackendError::NotSupported(_)) => {
                    if part_id == "manifest" {
                        backend.verify_package(file_id).await.map(|_| ())
                    } else {
                        Ok(())
                    }
                }
                Err(e) => Err(e),
            };
            if let Err(e) = verify {
                let _ = mutate_entry(&task_state, &task_update_id, |entry| {
                    entry.status = Status::Failed;
                    entry.step = Some(format!("part {part_id} verify failed"));
                    entry.error = Some(crate::state::UpdateError {
                        error_code: "update-preparation-failed".into(),
                        message: format!("verify part {part_id}: {e}"),
                        parameters: None,
                    });
                    entry.task_handle = None;
                });
                return;
            }
        }

        if let Some(tid) = transfer_id.as_deref() {
            let _ = mutate_entry(&task_state, &task_update_id, |entry| {
                entry.step = Some("waiting for staging pipeline".into());
                entry.progress = Some(80);
            });
            if let Err(e) = await_flash_settled(backend.as_ref(), tid).await {
                let _ = mutate_entry(&task_state, &task_update_id, |entry| {
                    entry.status = Status::Failed;
                    entry.step = Some("staging pipeline failed".into());
                    entry.error = Some(crate::state::UpdateError {
                        error_code: "update-preparation-failed".into(),
                        message: format!("settle: {e:?}"),
                        parameters: None,
                    });
                    entry.task_handle = None;
                });
                return;
            }
        }

        let _ = mutate_entry(&task_state, &task_update_id, |entry| {
            entry.status = Status::Completed;
            entry.progress = Some(100);
            entry.step = Some("prepared".into());
            entry.state = UpdateState::Verified; // legacy wire alignment
            entry.task_handle = None;
        });
    });

    let abort = join.abort_handle();
    {
        let mut store = state.updates.0.lock();
        if let Some(entry) = store.get_mut(&update_id) {
            entry.task_handle = Some(abort);
        }
    }

    let (status, headers) = accepted_with_status_location(&component_id, &update_id)?;
    Ok((status, headers))
}

/// `PUT /vehicle/v1/components/{component_id}/updates/{update_id}/execute`
/// — ISO 17978-3 §7.18.6.  Phase A: standard mode only (server-driven
/// finalize → auto-commit for singleshot, finalize → reset → auto-probe →
/// commit for banked).  Phase B adds `?x-sumo-control=orchestrated`
/// for orchestrator-driven trial verdict.
pub async fn put_execute(
    State(state): State<AppState>,
    Path((component_id, update_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    let backend = state.get_backend(&component_id)?;
    let is_singleshot = backend.update_shape() == "singleshot";

    let prior_phase = {
        let mut store = state.updates.0.lock();
        let entry = store
            .get_mut(&update_id)
            .filter(|e| e.component_id == component_id)
            .ok_or_else(|| ApiError::NotFound(format!("update {update_id} not found")))?;
        if matches!(entry.status, Status::InProgress) {
            return Err(ApiError::Conflict(format!(
                "update {update_id} is already in {} phase, status inProgress",
                entry.phase.as_str()
            )));
        }
        // Spec §7.18.6: execute requires prepare to have completed.
        if !(entry.phase == Phase::Prepare && entry.status == Status::Completed
            || entry.phase == Phase::Execute && entry.status == Status::Failed)
        {
            return Err(ApiError::Conflict(format!(
                "execute requires prepare/completed, got {}/{}",
                entry.phase.as_str(),
                entry.status.as_str()
            )));
        }
        let prior = entry.phase;
        entry.phase = Phase::Execute;
        entry.status = Status::InProgress;
        entry.progress = Some(0);
        entry.step = Some("starting execute".into());
        entry.error = None;
        prior
    };
    let _ = prior_phase;

    let task_state = state.clone();
    let task_update_id = update_id.clone();
    let task_component_id = component_id.clone();
    let join = tokio::spawn(async move {
        let backend = match task_state.get_backend(&task_component_id) {
            Ok(b) => b,
            Err(e) => {
                let _ = mutate_entry(&task_state, &task_update_id, |entry| {
                    entry.status = Status::Failed;
                    entry.error = Some(crate::state::UpdateError {
                        error_code: "internal-server-error".into(),
                        message: format!("{e:?}"),
                        parameters: None,
                    });
                    entry.task_handle = None;
                });
                return;
            }
        };

        // finalize_flash: writes live for singleshot, stages bank pointer for banked.
        let _ = mutate_entry(&task_state, &task_update_id, |entry| {
            entry.step = Some("finalizing".into());
            entry.progress = Some(20);
        });
        if let Err(e) = backend.finalize_flash().await {
            let _ = mutate_entry(&task_state, &task_update_id, |entry| {
                entry.status = Status::Failed;
                entry.step = Some("finalize_flash failed".into());
                entry.error = Some(crate::state::UpdateError {
                    error_code: "update-execution-failed".into(),
                    message: format!("finalize: {e}"),
                    parameters: None,
                });
                entry.task_handle = None;
            });
            return;
        }

        if is_singleshot {
            // Singleshot: finalize_flash already wrote live and
            // transitioned the backend to Activated. commit_flash
            // raises the security floor; failure here is bookkeeping
            // and should not auto-rollback the install.
            let _ = mutate_entry(&task_state, &task_update_id, |entry| {
                entry.step = Some("committing".into());
                entry.progress = Some(90);
            });
            if let Err(e) = backend.commit_flash().await {
                let _ = mutate_entry(&task_state, &task_update_id, |entry| {
                    entry.status = Status::Failed;
                    entry.step = Some("commit_flash failed".into());
                    entry.error = Some(crate::state::UpdateError {
                        error_code: "update-execution-failed".into(),
                        message: format!("commit: {e}"),
                        parameters: None,
                    });
                    entry.task_handle = None;
                });
                return;
            }
            let _ = mutate_entry(&task_state, &task_update_id, |entry| {
                entry.status = Status::Completed;
                entry.progress = Some(100);
                entry.step = Some("completed".into());
                entry.state = UpdateState::Committed; // legacy wire alignment
                entry.task_handle = None;
            });
            return;
        }

        // Banked Phase A: validate + activate (legacy state-machine
        // transitions), then leave the entry at execute/completed
        // *without* a reset.  The orchestrator still drives the
        // device reset and post-reset health check via the legacy
        // /executions wire (or the dedicated entity-restart endpoint)
        // during the migration window.  Phase B adds the orchestrated
        // execute flow that handles reset+verdict on the server side.
        let _ = mutate_entry(&task_state, &task_update_id, |entry| {
            entry.step = Some("validating".into());
            entry.progress = Some(50);
        });
        match backend.validate().await {
            Ok(()) => {}
            Err(sovd_core::BackendError::NotSupported(_)) => {}
            Err(e) => {
                let _ = mutate_entry(&task_state, &task_update_id, |entry| {
                    entry.status = Status::Failed;
                    entry.step = Some("validate failed".into());
                    entry.error = Some(crate::state::UpdateError {
                        error_code: "update-execution-failed".into(),
                        message: format!("validate: {e}"),
                        parameters: None,
                    });
                    entry.task_handle = None;
                });
                return;
            }
        }
        let _ = mutate_entry(&task_state, &task_update_id, |entry| {
            entry.step = Some("activating".into());
            entry.progress = Some(80);
        });
        match backend.activate().await {
            Ok(()) => {}
            Err(sovd_core::BackendError::NotSupported(_)) => {}
            Err(e) => {
                let _ = mutate_entry(&task_state, &task_update_id, |entry| {
                    entry.status = Status::Failed;
                    entry.step = Some("activate failed".into());
                    entry.error = Some(crate::state::UpdateError {
                        error_code: "update-execution-failed".into(),
                        message: format!("activate: {e}"),
                        parameters: None,
                    });
                    entry.task_handle = None;
                });
                return;
            }
        }
        let _ = mutate_entry(&task_state, &task_update_id, |entry| {
            entry.status = Status::Completed;
            entry.progress = Some(100);
            entry.step = Some("staged for reset".into());
            entry.state = UpdateState::Finalized; // legacy wire alignment
            entry.task_handle = None;
        });
    });

    {
        let mut store = state.updates.0.lock();
        if let Some(entry) = store.get_mut(&update_id) {
            entry.task_handle = Some(join.abort_handle());
        }
    }

    let (status, headers) = accepted_with_status_location(&component_id, &update_id)?;
    Ok((status, headers))
}

/// `PUT /vehicle/v1/components/{component_id}/updates/{update_id}/automated`
/// — ISO 17978-3 §7.18.4.  Server-driven prepare+execute chain.
/// Phase A: thin wrapper that runs prepare followed by execute when
/// prepare completes.  Returns `409` if the package isn't marked
/// `automated`.
pub async fn put_automated(
    State(state): State<AppState>,
    Path((component_id, update_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    // Phase A: only block if the package declared automated=false in
    // its registration manifest.  Default true unless explicitly
    // disabled, to match Table 261's default.
    let allowed = {
        let store = state.updates.0.lock();
        let entry = store
            .get(&update_id)
            .filter(|e| e.component_id == component_id)
            .ok_or_else(|| ApiError::NotFound(format!("update {update_id} not found")))?;
        entry
            .manifest
            .as_ref()
            .and_then(|m| m.get("automated"))
            .and_then(|v| v.as_bool())
            .unwrap_or(true)
    };
    if !allowed {
        return Err(ApiError::Conflict(
            "update-automated-not-supported: the package cannot be installed automatically".into(),
        ));
    }

    // Run prepare; if it succeeds, chain into execute.  Reuses the
    // same task machinery so both phases drive the same wire fields.
    let prepare_resp = put_prepare(
        State(state.clone()),
        Path((component_id.clone(), update_id.clone())),
    )
    .await?
    .into_response();
    if prepare_resp.status() != StatusCode::ACCEPTED {
        return Ok(prepare_resp);
    }

    // Chain: wait for prepare to complete, then kick off execute.
    let task_state = state.clone();
    let task_update_id = update_id.clone();
    let task_component_id = component_id.clone();
    tokio::spawn(async move {
        // Poll our own store for prepare/completed before invoking execute.
        loop {
            let ready = {
                let store = task_state.updates.0.lock();
                match store.get(&task_update_id) {
                    Some(e) => match (e.phase, e.status) {
                        (Phase::Prepare, Status::Completed) => Some(true),
                        (Phase::Prepare, Status::Failed) => Some(false),
                        _ => None,
                    },
                    None => Some(false),
                }
            };
            match ready {
                Some(true) => break,
                Some(false) => return,
                None => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
            }
        }
        // Kick off execute via the standard handler.
        let _ = put_execute(State(task_state), Path((task_component_id, task_update_id))).await;
    });

    let (status, headers) = accepted_with_status_location(&component_id, &update_id)?;
    Ok((status, headers).into_response())
}

/// `GET /vehicle/v1/components/{component_id}/updates/{update_id}/status`
/// — ISO 17978-3 §7.18.7.  Returns Table 270's `UpdateStatusBody`.
pub async fn get_status(
    State(state): State<AppState>,
    Path((component_id, update_id)): Path<(String, String)>,
) -> Result<Json<UpdateStatusBody>, ApiError> {
    let _ = state.get_backend(&component_id)?;
    let store = state.updates.0.lock();
    let entry = store
        .get(&update_id)
        .filter(|e| e.component_id == component_id)
        .ok_or_else(|| ApiError::NotFound(format!("update {update_id} not found")))?;
    Ok(Json(UpdateStatusBody {
        phase: entry.phase.as_str(),
        status: entry.status.as_str(),
        progress: entry.progress,
        step: entry.step.clone(),
        error: if entry.status == Status::Failed {
            entry.error.clone()
        } else {
            None
        },
        substate: entry.substate,
    }))
}

/// GET /vehicle/v1/components/{component_id}/updates/{update_id}/bulk-data
pub async fn list_bulk_data(
    State(state): State<AppState>,
    Path((component_id, update_id)): Path<(String, String)>,
) -> Result<Json<BulkDataListResponse>, ApiError> {
    let store = state.updates.0.lock();
    let entry = store
        .get(&update_id)
        .filter(|e| e.component_id == component_id)
        .ok_or_else(|| ApiError::NotFound(format!("update {update_id} not found")))?;
    let items: Vec<PartStatusEntry> = entry
        .parts
        .iter()
        .map(|p| part_status_entry(&component_id, &update_id, p))
        .collect();
    Ok(Json(BulkDataListResponse { items }))
}

/// PUT /vehicle/v1/components/{component_id}/updates/{update_id}/bulk-data/{part_id}
pub async fn put_bulk_data_part(
    State(state): State<AppState>,
    Path((component_id, update_id, part_id)): Path<(String, String, String)>,
    headers: HeaderMap,
    body: Body,
) -> Result<impl IntoResponse, ApiError> {
    let backend = state.get_backend(&component_id)?;
    {
        let store = state.updates.0.lock();
        let entry = store
            .get(&update_id)
            .filter(|e| e.component_id == component_id)
            .ok_or_else(|| ApiError::NotFound(format!("update {update_id} not found")))?;
        if !matches!(
            entry.state,
            UpdateState::Registered | UpdateState::Uploading
        ) {
            return Err(ApiError::Conflict(format!(
                "update {update_id} is in state {} — parts can only be uploaded \
                 while Registered or Uploading",
                entry.state.as_str()
            )));
        }
    }

    let content_length = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());

    let hasher = Arc::new(Mutex::new(Sha256::new()));
    let size_counter = Arc::new(AtomicU64::new(0));
    let hasher_clone = hasher.clone();
    let size_clone = size_counter.clone();
    let data_stream = body.into_data_stream().map(move |chunk_res| {
        let chunk =
            chunk_res.map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
        size_clone.fetch_add(chunk.len() as u64, Ordering::Relaxed);
        if let Ok(mut h) = hasher_clone.lock() {
            h.update(&chunk);
        }
        Ok(chunk)
    });
    let pkg_stream: PackageStream = Box::pin(data_stream);
    let file_id = backend
        .receive_package_stream(pkg_stream, content_length)
        .await?;

    let final_size = size_counter.load(Ordering::Relaxed);
    let digest = hasher
        .lock()
        .map_err(|e| ApiError::Internal(format!("hasher mutex poisoned: {e}")))?
        .clone()
        .finalize();
    let sha256 = hex::encode(digest);

    {
        let mut store = state.updates.0.lock();
        if let Some(entry) = store.get_mut(&update_id) {
            entry.parts.retain(|p| p.part_id != part_id);
            entry.parts.push(UpdatePart {
                part_id: part_id.clone(),
                size: final_size,
                sha256: sha256.clone(),
                file_id,
            });
            entry.state = UpdateState::Uploading;
        }
    }

    let href = format!(
        "/vehicle/v1/components/{}/updates/{}/bulk-data/{}",
        component_id, update_id, part_id
    );
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::ETAG,
        HeaderValue::from_str(&format!("\"{sha256}\""))
            .map_err(|e| ApiError::Internal(format!("bad ETag header: {e}")))?,
    );
    let resp = PartUploadResponse {
        part_id,
        size: final_size,
        sha256,
        href,
    };
    Ok((StatusCode::CREATED, response_headers, Json(resp)))
}

/// POST /vehicle/v1/components/{component_id}/updates/{update_id}/executions
pub async fn post_execution(
    State(state): State<AppState>,
    Path((component_id, update_id)): Path<(String, String)>,
    Query(_query): Query<ExecutionQuery>,
    Json(request): Json<ExecutionRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let backend = state.get_backend(&component_id)?;

    // Snapshot the entry we need under the lock without keeping it
    // held across awaits.
    let (current_state, parts, transfer_id) = {
        let store = state.updates.0.lock();
        let entry = store
            .get(&update_id)
            .filter(|e| e.component_id == component_id)
            .ok_or_else(|| ApiError::NotFound(format!("update {update_id} not found")))?;
        (
            entry.state,
            entry
                .parts
                .iter()
                .map(|p| (p.part_id.clone(), p.file_id.clone(), p.sha256.clone()))
                .collect::<Vec<_>>(),
            entry.transfer_id.clone(),
        )
    };

    let started_at = Utc::now();
    let exec_id = Uuid::new_v4().to_string();

    let (next_state, new_transfer_id, message) = match request.action.as_str() {
        "verify" => {
            if parts.is_empty() {
                return Err(ApiError::BadRequest(
                    "verify called before any /bulk-data part uploaded".into(),
                ));
            }
            // Re-verify every uploaded part against the SHA-256 the
            // server recorded at upload time.  Backends that route
            // detached payloads through streaming (VmBackend) override
            // `verify_part` to re-read from disk and re-hash; backends
            // without that surface area get a fallback to the
            // legacy single-package `verify_package` on the manifest
            // part (singleshot integrated-envelope flows).
            let mut verified_any = false;
            for (part_id, file_id, sha256) in &parts {
                match backend.verify_part(file_id, sha256).await {
                    Ok(()) => verified_any = true,
                    Err(sovd_core::BackendError::NotSupported(_)) => {
                        // Fall back to verify_package for the manifest
                        // part only — singleshot integrated-envelope
                        // backends know how to re-verify it.
                        if part_id == "manifest" {
                            backend.verify_package(file_id).await?;
                            verified_any = true;
                        }
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            if !verified_any {
                return Err(ApiError::Conflict(
                    "verify: backend supports neither verify_part nor verify_package".into(),
                ));
            }
            // start_flash already ran in register_update (POST
            // /updates).  For backends that surface a transfer_id,
            // wait here for the staging pipeline to settle before
            // declaring the update verified.
            if let Some(tid) = &transfer_id {
                await_flash_settled(backend.as_ref(), tid).await?;
            }
            (
                UpdateState::Verified,
                transfer_id.clone(),
                Some("verified".to_string()),
            )
        }
        "finalize" => {
            if current_state != UpdateState::Verified {
                return Err(ApiError::Conflict(format!(
                    "finalize requires state=verified, got {}",
                    current_state.as_str()
                )));
            }
            // `finalize` writes the staged image to its final home:
            //   - Banked backends: stages the next-boot bank pointer
            //     (FlashState → AwaitingReboot).  The orchestrator then
            //     drives ecu_reset and reads back the post-reset state.
            //   - Singleshot backends: writes the artifact live
            //     (FlashState → Activated).  No reset needed.
            //
            // `validate` and `activate` are exposed as separate
            // /executions actions for orchestrators that want to drive
            // the FSM step-by-step with observable state in between.
            // This handler does NOT auto-chain them — the orchestrator
            // is in charge of the lifecycle ordering.
            backend.finalize_flash().await?;
            (
                UpdateState::Finalized,
                transfer_id.clone(),
                Some("finalized".to_string()),
            )
        }
        "validate" => {
            // Pre-finalize checkpoint: re-verify the staged image and
            // move FlashState to Validated. Orchestrators use this to
            // pause the lifecycle for a re-verification window before
            // committing to a reset.
            backend.validate().await?;
            (
                current_state,
                transfer_id.clone(),
                Some("validated".to_string()),
            )
        }
        "invalidate" => {
            // Demote a Validated transfer back to AwaitingActivation.
            // Used when the bank can't be hardware-sealed and a power
            // cycle could have introduced drift since validate().
            backend.invalidate().await?;
            (
                current_state,
                transfer_id.clone(),
                Some("invalidated".to_string()),
            )
        }
        "activate" => {
            // Banked: stages the bank pointer flip (FlashState →
            // AwaitingReboot — orchestrator must follow with
            // ecu_reset).  Singleshot: writes live (FlashState →
            // Activated).  Requires a prior validate() to land in
            // Validated.
            backend.activate().await?;
            (
                current_state,
                transfer_id.clone(),
                Some("activated".to_string()),
            )
        }
        "commit" => {
            backend.commit_flash().await?;
            (
                UpdateState::Committed,
                transfer_id.clone(),
                Some("committed".to_string()),
            )
        }
        "rollback" => {
            backend.rollback_flash().await?;
            (
                UpdateState::RolledBack,
                transfer_id.clone(),
                Some("rolled back".to_string()),
            )
        }
        "abort" => {
            if let Some(tid) = &transfer_id {
                backend.abort_flash(tid).await?;
            }
            (
                UpdateState::Aborted,
                transfer_id.clone(),
                Some("aborted".to_string()),
            )
        }
        other => {
            return Err(ApiError::BadRequest(format!(
                "unknown action {other:?}; want one of verify|finalize|commit|rollback|abort"
            )));
        }
    };

    {
        let mut store = state.updates.0.lock();
        if let Some(entry) = store.get_mut(&update_id) {
            entry.state = next_state;
            if let Some(tid) = new_transfer_id {
                entry.transfer_id = Some(tid);
            }
        }
    }

    let completed_at = Utc::now();
    let execution = UpdateExecution {
        execution_id: exec_id.clone(),
        update_id: update_id.clone(),
        action: request.action,
        status: OperationStatus::Completed,
        message,
        started_at: started_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        completed_at: completed_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    };

    let href = format!(
        "/vehicle/v1/components/{}/updates/{}/executions/{}",
        component_id, update_id, exec_id
    );
    let mut headers = HeaderMap::new();
    headers.insert(
        header::LOCATION,
        HeaderValue::from_str(&href)
            .map_err(|e| ApiError::Internal(format!("bad Location header: {e}")))?,
    );
    // UPDATE-WIRE-001 (tasks/spec-aligned-updates-wire.md) deprecates
    // the /executions verb-bag in favour of the spec's PUT prepare /
    // execute / automated.  RFC 8594 Deprecation + RFC 9745 Sunset
    // signal to clients to migrate before the deprecation window
    // closes.
    headers.insert("deprecation", HeaderValue::from_static("true"));
    headers.insert(
        "link",
        HeaderValue::from_static(
            "</vehicle/v1/components>; rel=\"successor-version\"; \
             title=\"Use PUT /updates/{id}/prepare and /execute (ISO 17978-3 sec 7.18)\"",
        ),
    );
    Ok((StatusCode::OK, headers, Json(execution)))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn part_status_entry(component_id: &str, update_id: &str, p: &UpdatePart) -> PartStatusEntry {
    PartStatusEntry {
        part_id: p.part_id.clone(),
        size: p.size,
        sha256: p.sha256.clone(),
        href: format!(
            "/vehicle/v1/components/{}/updates/{}/bulk-data/{}",
            component_id, update_id, p.part_id
        ),
    }
}

/// Block until the backend's flash transfer reaches a settled state
/// (`AwaitingActivation` or beyond, or a terminal failure).  The
/// `backend.start_flash` call spawns the actual UDS download as a
/// background task; this helper bridges that asynchrony for the
/// /updates wire which is otherwise synchronous.  Bounded by a 30 s
/// wait — beyond that the caller can re-issue `verify` (idempotent)
/// or `abort`.
async fn await_flash_settled(
    backend: &dyn sovd_core::DiagnosticBackend,
    transfer_id: &str,
) -> Result<(), ApiError> {
    use sovd_core::FlashState;
    for _ in 0..300 {
        let status = backend.get_flash_status(transfer_id).await?;
        if matches!(
            status.state,
            FlashState::AwaitingActivation
                | FlashState::Validated
                | FlashState::AwaitingReboot
                | FlashState::Activated
                | FlashState::Complete
                | FlashState::Committed
        ) {
            return Ok(());
        }
        if matches!(status.state, FlashState::Failed) {
            return Err(ApiError::Conflict(format!(
                "flash transfer failed during verify: state={:?}",
                status.state
            )));
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    Err(ApiError::GatewayTimeout(
        "flash transfer did not settle within 30s after start_flash".into(),
    ))
}
