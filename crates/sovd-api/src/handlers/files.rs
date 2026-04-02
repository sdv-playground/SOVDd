//! File (package) management handlers for async flash flow
//!
//! Provides endpoints for uploading, listing, verifying, and deleting software packages.
//! Supports streaming uploads via HTTP/1.1 chunked transfer encoding per ASAM SOVD.

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use futures::StreamExt;
use serde::Serialize;
use sovd_core::{PackageInfo, PackageStream, VerifyResult};

use crate::error::ApiError;
use crate::state::AppState;

/// Response for file upload
#[derive(Debug, Serialize)]
pub struct UploadFileResponse {
    /// Package ID for subsequent operations
    pub file_id: String,
    /// Size of uploaded data
    pub size: usize,
    /// URL to verify this file
    pub verify_url: String,
    /// URL to get file info
    pub href: String,
}

/// Response for listing files
#[derive(Debug, Serialize)]
pub struct ListFilesResponse {
    /// List of uploaded files
    pub files: Vec<FileInfo>,
}

/// File information (wraps PackageInfo with HATEOAS links)
#[derive(Debug, Serialize)]
pub struct FileInfo {
    /// Package information
    #[serde(flatten)]
    pub info: PackageInfo,
    /// URL to this file
    pub href: String,
    /// URL to verify this file
    pub verify_url: String,
}

/// POST /vehicle/v1/components/:component_id/files
/// Upload a software package (supports streaming via chunked transfer encoding)
pub async fn upload_file(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Result<(StatusCode, Json<UploadFileResponse>), ApiError> {
    let backend = state.get_backend(&component_id)?;

    let content_length = headers
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());

    tracing::info!(
        component_id = %component_id,
        content_length = ?content_length,
        "File upload started"
    );

    // Convert axum Body to PackageStream for streaming to backend.
    // Backends that support streaming process the stream directly.
    // Backends that don't will collect to bytes via the default trait impl.
    let stream = body.into_data_stream();
    let pkg_stream: PackageStream = Box::pin(
        stream.map(|r| r.map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)),
    );

    let (file_id, size) = match backend
        .receive_package_stream(pkg_stream, content_length)
        .await
    {
        Ok(id) => {
            let size = content_length.unwrap_or(0) as usize;
            (id, size)
        }
        Err(e) => return Err(ApiError::from(e)),
    };

    tracing::info!(
        component_id = %component_id,
        file_id = %file_id,
        size,
        "File uploaded"
    );

    let response = UploadFileResponse {
        file_id: file_id.clone(),
        size,
        verify_url: format!(
            "/vehicle/v1/components/{}/files/{}/verify",
            component_id, file_id
        ),
        href: format!("/vehicle/v1/components/{}/files/{}", component_id, file_id),
    };

    Ok((StatusCode::CREATED, Json(response)))
}

/// GET /vehicle/v1/components/:component_id/files
/// List all uploaded files
pub async fn list_files(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<Json<ListFilesResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;

    let packages = backend.list_packages().await.map_err(ApiError::from)?;

    let files: Vec<FileInfo> = packages
        .into_iter()
        .map(|info| {
            let file_id = info.id.clone();
            FileInfo {
                info,
                href: format!("/vehicle/v1/components/{}/files/{}", component_id, file_id),
                verify_url: format!(
                    "/vehicle/v1/components/{}/files/{}/verify",
                    component_id, file_id
                ),
            }
        })
        .collect();

    Ok(Json(ListFilesResponse { files }))
}

/// GET /vehicle/v1/components/:component_id/files/:file_id
/// Get information about a specific file
pub async fn get_file(
    State(state): State<AppState>,
    Path((component_id, file_id)): Path<(String, String)>,
) -> Result<Json<FileInfo>, ApiError> {
    let backend = state.get_backend(&component_id)?;

    let info = backend
        .get_package(&file_id)
        .await
        .map_err(ApiError::from)?;

    Ok(Json(FileInfo {
        info,
        href: format!("/vehicle/v1/components/{}/files/{}", component_id, file_id),
        verify_url: format!(
            "/vehicle/v1/components/{}/files/{}/verify",
            component_id, file_id
        ),
    }))
}

/// POST /vehicle/v1/components/:component_id/files/:file_id/verify
/// Verify a software package
pub async fn verify_file(
    State(state): State<AppState>,
    Path((component_id, file_id)): Path<(String, String)>,
) -> Result<Json<VerifyResult>, ApiError> {
    let backend = state.get_backend(&component_id)?;

    let result = backend
        .verify_package(&file_id)
        .await
        .map_err(ApiError::from)?;

    tracing::info!(
        component_id = %component_id,
        file_id = %file_id,
        valid = result.valid,
        "File verified"
    );

    Ok(Json(result))
}

/// DELETE /vehicle/v1/components/:component_id/files/:file_id
/// Delete a software package
pub async fn delete_file(
    State(state): State<AppState>,
    Path((component_id, file_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let backend = state.get_backend(&component_id)?;

    backend
        .delete_package(&file_id)
        .await
        .map_err(ApiError::from)?;

    tracing::info!(
        component_id = %component_id,
        file_id = %file_id,
        "File deleted"
    );

    Ok(StatusCode::NO_CONTENT)
}
