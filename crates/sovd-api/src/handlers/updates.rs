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
use futures::StreamExt;
use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sovd_core::{PackageStream, UpdatePackageContext, UpdatePackageDescriptor, UpdatePartRef};
use uuid::Uuid;

use crate::error::ApiError;
use crate::state::{AppState, Phase, Status, UpdatePart, UpdateState, UpdatesEntry};

/// Reserved update-package id (§7.18.1.5). On servers that self-select,
/// `GET /updates/autonomous` resolves to a concrete installable id; SOVDd
/// performs no self-governed selection, so `autonomous` is never a real
/// package and is refused as a registrable installable id.
const AUTONOMOUS_PACKAGE_ID: &str = "autonomous";

/// Percent-encode set for a package-id (or part-id) URL path segment. Mirrors
/// the client's `PART_SEGMENT_ENCODE` so server-emitted hrefs round-trip ids
/// carrying reserved chars (e.g. SUIT ids like `#kernel`).
const ID_SEGMENT_ENCODE: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`')
    .add(b'{')
    .add(b'}')
    .add(b'/')
    .add(b'%');

/// Percent-encode an id for safe interpolation into an href/Location path.
fn enc(segment: &str) -> String {
    utf8_percent_encode(segment, ID_SEGMENT_ENCODE).to_string()
}

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
    /// Client-declared stable package id (§7.18 Table 261 `id`). When present
    /// it becomes the catalog key / URL id; absent → the server mints a UUID.
    /// The reserved id `autonomous` is refused (400, §7.18.1.5).
    #[serde(default)]
    pub id: Option<String>,
    /// Optional component id the manifest is addressed to.  When
    /// present, MUST match the path's `{component_id}` — otherwise the
    /// server returns 415 Unsupported Media Type before allocating an
    /// update_id.  Absent means "trust the path" (F.D2 behaviour).
    #[serde(default)]
    pub target: Option<String>,
    /// Pass-through register document.  The §7.18.3 descriptor default impl
    /// reads declared `update_name`/`size`/`automated`/`notes`/`duration`/
    /// `preconditions` from it; a format-aware backend may parse more.
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

/// `GET /updates` — ISO 17978-3 §7.18.2 Table 257: a catalog of update
/// package ids. (`autonomous` is included only on servers that self-select;
/// SOVDd does not, so it never appears here.)
#[derive(Debug, Serialize)]
pub struct UpdatesListResponse {
    pub items: Vec<String>,
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
    /// Vendor extension (ISO 17978-3 §5.4.5 permits `x-<ext>-` fields):
    /// the component's declared `ResetKind` from its `ActivationState`,
    /// captured once at register time. Lets the campaign orchestrator
    /// route RT/host-os components through a coalesced ECU reset instead
    /// of defaulting every component to `Local`.
    #[serde(rename = "x-sumo-reset-kind", skip_serializing_if = "Option::is_none")]
    pub reset_kind: Option<sovd_core::ResetKind>,
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

/// `PUT /updates/{id}/execute` query parameters.
///
/// `x-sumo-control=orchestrated` opts the request into the Phase B
/// orchestrated-extension flow: the execute task pauses post-activation
/// at `substate=awaiting-verdict` and waits for the orchestrator to
/// issue `PUT /x-sumo-commit` or `/x-sumo-rollback` before transitioning
/// to a terminal status.  Absent or any other value → standard
/// server-driven flow (Phase A behaviour).
///
/// `tasks/spec-aligned-updates-wire.md` §2.2.
#[derive(Debug, Deserialize, Default)]
pub struct ExecuteQuery {
    #[serde(rename = "x-sumo-control", default)]
    pub control: Option<String>,
}

impl ExecuteQuery {
    fn is_orchestrated(&self) -> bool {
        self.control.as_deref() == Some("orchestrated")
    }
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

    // Resolve the stable package id: a client-declared id (Table 261 `id`)
    // becomes the catalog key; absent → mint a UUID. `autonomous` is reserved
    // (§7.18.1.5) and cannot be registered as an installable package.
    let update_id = match req.id.as_deref() {
        Some(AUTONOMOUS_PACKAGE_ID) => {
            return Err(ApiError::BadRequest(
                "'autonomous' is a reserved update-package id and cannot be registered \
                 (§7.18.1.5)"
                    .into(),
            ));
        }
        Some(id) if !id.is_empty() => id.to_string(),
        _ => Uuid::new_v4().to_string(),
    };
    let manifest = req.manifest;

    // Collision handling on a (now possibly client-stable) id, scoped to the
    // component. A still-active package → 409 update-process-in-progress
    // (C-111). A terminal or untouched-Registered entry → tear down any
    // residue and replace (idempotent re-flash).
    let prior = {
        let store = state.updates.0.lock();
        store
            .get(&update_id)
            .filter(|e| e.component_id == component_id)
            .map(|e| {
                (
                    e.status,
                    e.state,
                    e.substate,
                    e.transfer_id.clone(),
                    e.task_handle.clone(),
                )
            })
    };
    if let Some((status, ustate, substate, prior_tid, prior_task)) = prior {
        let terminal = matches!(
            ustate,
            UpdateState::Committed | UpdateState::RolledBack | UpdateState::Aborted
        ) || status == Status::Failed;
        let untouched =
            ustate == UpdateState::Registered && status == Status::Pending && substate.is_none();
        if !(terminal || untouched) {
            return Err(ApiError::UpdateInProgress(format!(
                "update package {update_id:?} on {component_id:?} is in progress; \
                 finish, abort, or roll it back before re-registering"
            )));
        }
        // Replaceable: tear down any residue before re-inserting fresh.
        if let Some(handle) = prior_task {
            handle.abort();
        }
        if let Some(tid) = prior_tid {
            let _ = backend.abort_flash(&tid).await;
        }
    }

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
        Ok(tid) => Some(tid),
        // Backends that don't preallocate (`NotSupported`), or that require an
        // already-verified package (`InvalidRequest`, the tier-1 supplier
        // pattern), legitimately skip this step — the session opens later at
        // verify/prepare.
        Err(sovd_core::BackendError::NotSupported(_))
        | Err(sovd_core::BackendError::InvalidRequest(_)) => None,
        // Any other error is real — surface it. In particular `Busy` (409) means
        // the bank set is in trial mode and must be committed/rolled back first;
        // falling through here would let the later bulk-data PUT hit the
        // unguarded legacy path and wipe the rollback bank.
        Err(e) => return Err(e.into()),
    };

    // Capture the component's declared ResetKind once, while it's idle
    // (cheap here; `get_activation_state` can be slow mid-flash, so we
    // never re-read it per status-poll). Surfaced on the wire as
    // `x-sumo-reset-kind` for the campaign orchestrator. Done before
    // taking the `state.updates` lock — never `.await` while holding it.
    let reset_kind = backend
        .get_activation_state()
        .await
        .ok()
        .map(|a| a.reset_kind);

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
                reset_kind,
                transfer_id,
                task_handle: None,
                verdict_tx: None,
            },
        );
    }

    let base = format!(
        "/vehicle/v1/components/{}/updates/{}",
        component_id,
        enc(&update_id)
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

/// `GET /updates` query (§7.18.2 / Table 255). `origin` (Table 254) selects
/// the environment; `target-version` is accepted for wire-compat (SOVDd's
/// staged packages are version-pinned by their manifest, not filtered here).
#[derive(Debug, Deserialize, Default)]
pub struct ListUpdatesQuery {
    #[serde(default)]
    pub origin: Option<String>,
    #[serde(rename = "target-version", default)]
    pub target_version: Option<String>,
}

/// Validate a Table 254 `UpdateOrigins` token and report whether SOVDd's
/// (proximity-origin) staged catalog applies. `None` defaults to `remote`
/// (Table 255) → not applicable. The reserved `x-sovd-` prefix and malformed
/// tokens are rejected with 400 (§7.18.1.2 / C-026).
fn origin_lists_catalog(raw: Option<&str>) -> Result<bool, ApiError> {
    let Some(value) = raw else {
        return Ok(false); // default `remote` — no SOVDd-pulled remote catalog
    };
    match value {
        "remote" => Ok(false),
        "proximity" => Ok(true),
        other if other.starts_with("x-sovd-") => Err(ApiError::BadRequest(format!(
            "origin {other:?} uses the reserved x-sovd- prefix (§7.18.1.2)"
        ))),
        other
            if other.starts_with("x-")
                && other.matches('-').count() >= 2
                && other
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') =>
        {
            Ok(true) // custom x-<ext>- origin: workshop-defined → applies
        }
        other => Err(ApiError::BadRequest(format!(
            "origin {other:?} is not a valid UpdateOrigins value \
             (remote|proximity|x-<ext>-<name>)"
        ))),
    }
}

/// GET /vehicle/v1/components/{component_id}/updates — §7.18.2 Table 257.
/// Returns the catalog of update-package ids (strings) for this component.
pub async fn list_updates(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
    Query(query): Query<ListUpdatesQuery>,
) -> Result<Json<UpdatesListResponse>, ApiError> {
    let _ = state.get_backend(&component_id)?;

    // SOVDd's `/updates` tracks workshop-pushed (proximity) staging sessions;
    // there is no server-pulled `remote` catalog, so a `remote` query (the
    // Table 255 default) lists nothing (200, C-044).
    if !origin_lists_catalog(query.origin.as_deref())? {
        return Ok(Json(UpdatesListResponse { items: Vec::new() }));
    }

    let store = state.updates.0.lock();
    let mut items: Vec<String> = store
        .iter()
        .filter(|(_, e)| e.component_id == component_id)
        .map(|(id, _)| id.clone())
        .collect();
    items.sort(); // deterministic (HashMap iteration order is not)
    Ok(Json(UpdatesListResponse { items }))
}

/// GET /vehicle/v1/components/{component_id}/updates/{update_id} — §7.18.3
/// Table 261 detail body, resolved via the backend's package describer.
pub async fn get_update(
    State(state): State<AppState>,
    Path((component_id, update_id)): Path<(String, String)>,
) -> Result<Json<UpdatePackageDescriptor>, ApiError> {
    let backend = state.get_backend(&component_id)?;

    // §7.18.1.5: `autonomous` is a read-only indirection to a concrete id on
    // servers that self-select. SOVDd performs no self-governed selection, so
    // it is never a real package here → not applicable.
    if update_id == AUTONOMOUS_PACKAGE_ID {
        return Err(ApiError::NotFound(format!(
            "autonomous update package not supported on component {component_id:?} \
             (this entity does not perform self-governed updates)"
        )));
    }

    // Snapshot what the describer needs, then DROP the lock before the
    // (async, possibly I/O-bound) describe call — never hold a parking_lot
    // guard across `.await`.
    let (register_body, parts): (Option<serde_json::Value>, Vec<UpdatePart>) = {
        let store = state.updates.0.lock();
        let entry = store
            .get(&update_id)
            .filter(|e| e.component_id == component_id)
            .ok_or_else(|| ApiError::NotFound(format!("update {update_id} not found")))?;
        (entry.manifest.clone(), entry.parts.clone())
    };

    let part_refs: Vec<UpdatePartRef<'_>> = parts
        .iter()
        .map(|p| UpdatePartRef {
            part_id: &p.part_id,
            size: p.size,
            sha256: &p.sha256,
            file_id: &p.file_id,
        })
        .collect();
    let ctx = UpdatePackageContext {
        id: &update_id,
        component_id: &component_id,
        register_body: register_body.as_ref(),
        parts: &part_refs,
    };
    let descriptor = backend.describe_update_package(&ctx).await?;
    Ok(Json(descriptor))
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
        component_id,
        enc(update_id)
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
            return Err(ApiError::UpdatePreparationInProgress(format!(
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

        // If register_update couldn't preallocate the backend flash
        // session (NotSupported / not-yet-ready), retry now — by this
        // point any verified-package precondition the backend wanted
        // has had a chance to land via the bulk-data uploads.
        let active_transfer = match transfer_id {
            Some(tid) => Some(tid),
            None => match backend.start_flash().await {
                Ok(tid) => {
                    let tid_clone = tid.clone();
                    let _ = mutate_entry(&task_state, &task_update_id, |entry| {
                        entry.transfer_id = Some(tid_clone);
                    });
                    Some(tid)
                }
                Err(sovd_core::BackendError::NotSupported(_)) => None,
                Err(e) => {
                    let _ = mutate_entry(&task_state, &task_update_id, |entry| {
                        entry.status = Status::Failed;
                        entry.step = Some("start_flash failed".into());
                        entry.error = Some(crate::state::UpdateError {
                            error_code: "update-preparation-failed".into(),
                            message: format!("start_flash: {e:?}"),
                            parameters: None,
                        });
                        entry.task_handle = None;
                    });
                    return;
                }
            },
        };

        if let Some(tid) = active_transfer.as_deref() {
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
/// — ISO 17978-3 §7.18.6.  Phase A: standard mode (server-driven
/// finalize → auto-commit for singleshot, finalize → validate →
/// activate for banked).  Phase B adds `?x-sumo-control=orchestrated`
/// for orchestrator-driven trial verdict on banked components.
pub async fn put_execute(
    State(state): State<AppState>,
    Path((component_id, update_id)): Path<(String, String)>,
    Query(query): Query<ExecuteQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let backend = state.get_backend(&component_id)?;
    let is_singleshot = backend.update_shape() == "singleshot";
    // Orchestrated control mode is only meaningful for banked
    // components — singleshot has no trial phase to pause at.  We
    // accept the query param on singleshot but silently treat it as
    // standard mode so callers can use one verb across both shapes.
    let orchestrated = query.is_orchestrated() && !is_singleshot;

    let prior_phase = {
        let mut store = state.updates.0.lock();
        let entry = store
            .get_mut(&update_id)
            .filter(|e| e.component_id == component_id)
            .ok_or_else(|| ApiError::NotFound(format!("update {update_id} not found")))?;
        if matches!(entry.status, Status::InProgress) {
            return Err(ApiError::UpdateExecutionInProgress(format!(
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

        // Banked: validate + activate (legacy state-machine
        // transitions) bring the backend to AwaitingReboot.  In
        // standard mode we leave the entry at execute/completed and
        // the orchestrator drives reset + post-reset health check via
        // the legacy /executions wire (or the entity-restart endpoint).
        // In orchestrated mode (Phase B) we pause after activate at
        // substate=awaiting-verdict and wait for the orchestrator to
        // post commit / rollback via the x-sumo verbs.
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
        if !orchestrated {
            let _ = mutate_entry(&task_state, &task_update_id, |entry| {
                entry.status = Status::Completed;
                entry.progress = Some(100);
                entry.step = Some("staged for reset".into());
                entry.state = UpdateState::Finalized; // legacy wire alignment
                entry.task_handle = None;
            });
            return;
        }

        // Orchestrated mode: install verdict channel and pause until
        // the orchestrator commits or rolls back (or the watchdog
        // fires).  Watch::changed() returns Ok on any transition
        // away from Pending, including Commit and Rollback; we
        // re-read .borrow() to discriminate.
        let (verdict_tx, mut verdict_rx) =
            tokio::sync::watch::channel(crate::state::Verdict::Pending);
        let _ = mutate_entry(&task_state, &task_update_id, |entry| {
            entry.step = Some("trial boot active, awaiting orchestrator verdict".into());
            entry.progress = Some(85);
            entry.substate = Some("awaiting-verdict");
            entry.verdict_tx = Some(verdict_tx);
        });

        // Phase B watchdog: default 10 min, configurable via
        // AppState::updates_config.  On expiry the verdict is forced
        // to Rollback so the device doesn't stay stuck in trial.
        let watchdog = task_state.updates_config.orchestrated_watchdog;
        let verdict = tokio::select! {
            res = verdict_rx.changed() => {
                if res.is_err() {
                    // Sender dropped (entry deleted) — treat as rollback intent.
                    crate::state::Verdict::Rollback
                } else {
                    *verdict_rx.borrow()
                }
            }
            _ = tokio::time::sleep(watchdog) => {
                tracing::warn!(
                    update_id = %task_update_id,
                    "orchestrator verdict watchdog fired — auto-rollback"
                );
                crate::state::Verdict::Rollback
            }
        };

        match verdict {
            crate::state::Verdict::Commit => {
                let _ = mutate_entry(&task_state, &task_update_id, |entry| {
                    entry.step = Some("committing".into());
                    entry.substate = Some("committing");
                });
                if let Err(e) = backend.commit_flash().await {
                    let _ = mutate_entry(&task_state, &task_update_id, |entry| {
                        entry.status = Status::Failed;
                        entry.step = Some("commit_flash failed".into());
                        entry.substate = None;
                        entry.verdict_tx = None;
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
                    entry.substate = None;
                    entry.verdict_tx = None;
                    entry.state = UpdateState::Committed;
                    entry.task_handle = None;
                });
            }
            crate::state::Verdict::Rollback => {
                let _ = mutate_entry(&task_state, &task_update_id, |entry| {
                    entry.step = Some("rolling back".into());
                    entry.substate = Some("rolling-back");
                });
                let rb_result = backend.rollback_flash().await;
                let _ = mutate_entry(&task_state, &task_update_id, |entry| {
                    entry.status = Status::Failed;
                    entry.substate = None;
                    entry.verdict_tx = None;
                    entry.error = Some(crate::state::UpdateError {
                        error_code: "x-sumo-verdict-rollback".into(),
                        message: match rb_result {
                            Ok(()) => "rolled back by orchestrator verdict".into(),
                            Err(e) => format!("rollback_flash failed: {e}"),
                        },
                        parameters: None,
                    });
                    entry.state = UpdateState::RolledBack;
                    entry.task_handle = None;
                });
            }
            crate::state::Verdict::Pending => {
                // Should never happen — changed() only returns when
                // the value transitions away from the initial state.
                let _ = mutate_entry(&task_state, &task_update_id, |entry| {
                    entry.status = Status::Failed;
                    entry.substate = None;
                    entry.verdict_tx = None;
                    entry.error = Some(crate::state::UpdateError {
                        error_code: "internal-server-error".into(),
                        message: "verdict channel woke without transition".into(),
                        parameters: None,
                    });
                    entry.task_handle = None;
                });
            }
        }
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
        return Err(ApiError::UpdateAutomatedNotSupported(
            "the package cannot be installed automatically".into(),
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
        // Kick off execute via the standard handler.  /automated is
        // server-driven by definition, so always use standard mode
        // (empty query → not orchestrated).
        let _ = put_execute(
            State(task_state),
            Path((task_component_id, task_update_id)),
            Query(ExecuteQuery::default()),
        )
        .await;
    });

    let (status, headers) = accepted_with_status_location(&component_id, &update_id)?;
    Ok((status, headers).into_response())
}

/// `PUT /vehicle/v1/components/{component_id}/updates/{update_id}/x-sumo-commit`
///
/// Vendor extension (`x-sumo-` prefix per spec §extension rules)
/// used by orchestrators driving the execute phase under
/// `?x-sumo-control=orchestrated`.  Sends a `Commit` verdict to the
/// paused execute task; rejected with `409` if the entry isn't in
/// `awaiting-verdict`.  Returns `202 + Location: .../status`.
pub async fn put_x_sumo_commit(
    State(state): State<AppState>,
    Path((component_id, update_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    post_verdict(
        &state,
        &component_id,
        &update_id,
        crate::state::Verdict::Commit,
    )?;
    let (status, headers) = accepted_with_status_location(&component_id, &update_id)?;
    Ok((status, headers))
}

/// `PUT /vehicle/v1/components/{component_id}/updates/{update_id}/x-sumo-rollback`
///
/// Symmetric to `x-sumo-commit`; sends a `Rollback` verdict.
pub async fn put_x_sumo_rollback(
    State(state): State<AppState>,
    Path((component_id, update_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    post_verdict(
        &state,
        &component_id,
        &update_id,
        crate::state::Verdict::Rollback,
    )?;
    let (status, headers) = accepted_with_status_location(&component_id, &update_id)?;
    Ok((status, headers))
}

/// `PUT /vehicle/v1/components/{component_id}/x-sumo-force-rollback`
///
/// Vendor extension for the trial-recovery edge case: a previous flash
/// session left the backend in trial state (post-finalize, uncommitted)
/// without leaving any SOVDd-side `/updates` entry at
/// `awaiting-verdict`.  Calls `backend.rollback_flash()`
/// unconditionally so the next `start_flash` won't 409 with "trial
/// mode".  Idempotent.  Returns 204.
///
/// Distinct from `x-sumo-rollback` which posts a verdict to an active
/// execute task — `force-rollback` doesn't care about any task and
/// just clears whatever backend trial state is stuck on this
/// component.  Lives at the component root (not under `/updates/{id}`)
/// because by definition there may be no in-flight update_id.
pub async fn put_x_sumo_force_rollback(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let backend = state.get_backend(&component_id)?;
    backend.rollback_flash().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Common verdict-posting path for the two x-sumo verbs.  Validates
/// that the entry is paused at `substate=awaiting-verdict` and posts
/// the new verdict via the entry's watch channel.
fn post_verdict(
    state: &AppState,
    component_id: &str,
    update_id: &str,
    verdict: crate::state::Verdict,
) -> Result<(), ApiError> {
    let _ = state.get_backend(component_id)?;
    let store = state.updates.0.lock();
    let entry = store
        .get(update_id)
        .filter(|e| e.component_id == component_id)
        .ok_or_else(|| ApiError::NotFound(format!("update {update_id} not found")))?;
    if entry.substate != Some("awaiting-verdict") {
        return Err(ApiError::Conflict(format!(
            "x-sumo verdict requires execute/awaiting-verdict, got {}/{} substate={:?}",
            entry.phase.as_str(),
            entry.status.as_str(),
            entry.substate
        )));
    }
    match entry.verdict_tx.as_ref() {
        Some(tx) => {
            // send_replace overwrites the current value and wakes any
            // waiter.  Returns the previous value; we don't care.
            let _ = tx.send_replace(verdict);
            Ok(())
        }
        None => Err(ApiError::Conflict(
            "x-sumo verdict: no orchestrator channel on entry".into(),
        )),
    }
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
        reset_kind: entry.reset_kind,
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
        component_id,
        enc(&update_id),
        enc(&part_id)
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
            component_id,
            enc(update_id),
            enc(&p.part_id)
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
