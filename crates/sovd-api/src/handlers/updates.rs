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
use crate::state::{AppState, UpdatePart, UpdateState, UpdatesEntry};

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// Body for `POST /updates`.  Manifest is optional in F.D2 — recorded
/// and echoed back, but the dispatcher does not yet route by it.
#[derive(Debug, Deserialize, Default)]
pub struct RegisterUpdateRequest {
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
    let _ = state.get_backend(&component_id)?;

    let update_id = Uuid::new_v4().to_string();
    let manifest = body.and_then(|Json(b)| b.manifest);

    {
        let mut store = state.updates.0.lock();
        store.insert(
            update_id.clone(),
            UpdatesEntry {
                component_id: component_id.clone(),
                parts: Vec::new(),
                manifest,
                state: UpdateState::Registered,
                transfer_id: None,
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
    let transfer_id = {
        let store = state.updates.0.lock();
        let entry = store
            .get(&update_id)
            .filter(|e| e.component_id == component_id)
            .ok_or_else(|| ApiError::NotFound(format!("update {update_id} not found")))?;
        entry.transfer_id.clone()
    };
    if let Some(tid) = transfer_id {
        let _ = backend.abort_flash(&tid).await;
    }
    state.updates.0.lock().remove(&update_id);
    Ok(StatusCode::NO_CONTENT)
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
    let (current_state, file_ids, transfer_id) = {
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
                .map(|p| p.file_id.clone())
                .collect::<Vec<_>>(),
            entry.transfer_id.clone(),
        )
    };

    let started_at = Utc::now();
    let exec_id = Uuid::new_v4().to_string();

    let (next_state, new_transfer_id, message) = match request.action.as_str() {
        "verify" => {
            if file_ids.is_empty() {
                return Err(ApiError::BadRequest(
                    "verify called before any /bulk-data part uploaded".into(),
                ));
            }
            for fid in &file_ids {
                backend.verify_package(fid).await?;
            }
            let tid = backend.start_flash().await?;
            (
                UpdateState::Verified,
                Some(tid),
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
            backend.finalize_flash().await?;
            backend.activate().await?;
            (
                UpdateState::Finalized,
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
