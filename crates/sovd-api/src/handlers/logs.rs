//! Log handlers (primarily for HPC backends and message passing)
//!
//! Supports:
//! - GET /logs - list logs with filtering
//! - GET /logs/{id} - get single log entry (JSON) or binary content (Accept: application/octet-stream)
//! - DELETE /logs/{id} - delete/acknowledge log entry

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sovd_core::{LogEntry, LogFilter, LogPriority, LogStatus};

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Serialize)]
pub struct LogsResponse {
    pub items: Vec<LogEntryResponse>,
    pub total_count: usize,
}

#[derive(Serialize)]
pub struct LogEntryResponse {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub priority: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "type")]
    pub log_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Deserialize, Default)]
pub struct LogFilterQuery {
    pub priority: Option<String>,
    pub source: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub pattern: Option<String>,
    pub limit: Option<usize>,
    pub tail: Option<usize>,
    /// Filter by log type (e.g., "engine_dump", "diagnostic")
    #[serde(rename = "type")]
    pub log_type: Option<String>,
    /// Filter by retrieval status (pending, retrieved)
    pub status: Option<String>,
}

impl From<&LogEntry> for LogEntryResponse {
    fn from(entry: &LogEntry) -> Self {
        Self {
            id: entry.id.clone(),
            timestamp: entry.timestamp,
            priority: match entry.priority {
                LogPriority::Emergency => "emergency",
                LogPriority::Alert => "alert",
                LogPriority::Critical => "critical",
                LogPriority::Error => "error",
                LogPriority::Warning => "warning",
                LogPriority::Notice => "notice",
                LogPriority::Info => "info",
                LogPriority::Debug => "debug",
            }
            .to_string(),
            message: entry.message.clone(),
            source: entry.source.clone(),
            pid: entry.pid,
            log_type: entry.log_type.clone(),
            size: entry.size,
            status: entry.status.map(|s| match s {
                LogStatus::Pending => "pending".to_string(),
                LogStatus::Retrieved => "retrieved".to_string(),
                LogStatus::Processed => "processed".to_string(),
            }),
            href: entry.href.clone(),
            metadata: entry.metadata.clone(),
        }
    }
}

/// GET /vehicle/v1/components/:component_id/logs
/// Get logs (primarily for HPC backends)
pub async fn get_logs(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
    Query(query): Query<LogFilterQuery>,
) -> Result<Json<LogsResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;

    // Check if this backend supports logs
    if !backend.capabilities().logs {
        return Err(ApiError::NotImplemented(
            "This component does not support logs".to_string(),
        ));
    }

    let filter = LogFilter {
        priority: query.priority.and_then(|s| match s.as_str() {
            "emergency" => Some(LogPriority::Emergency),
            "alert" => Some(LogPriority::Alert),
            "critical" => Some(LogPriority::Critical),
            "error" => Some(LogPriority::Error),
            "warning" => Some(LogPriority::Warning),
            "notice" => Some(LogPriority::Notice),
            "info" => Some(LogPriority::Info),
            "debug" => Some(LogPriority::Debug),
            _ => None,
        }),
        source: query.source,
        since: query.since,
        until: query.until,
        pattern: query.pattern,
        limit: query.limit,
        tail: query.tail,
        log_type: query.log_type,
        status: query.status.and_then(|s| match s.as_str() {
            "pending" => Some(LogStatus::Pending),
            "retrieved" => Some(LogStatus::Retrieved),
            "processed" => Some(LogStatus::Processed),
            _ => None,
        }),
    };

    let logs = backend.get_logs(&filter).await?;
    let total_count = logs.len();

    let items: Vec<LogEntryResponse> = logs.iter().map(LogEntryResponse::from).collect();

    Ok(Json(LogsResponse { items, total_count }))
}

/// Path parameters for log routes with ID
#[derive(Deserialize)]
pub struct LogPathParams {
    pub component_id: String,
    pub log_id: String,
}

/// GET /vehicle/v1/components/:component_id/logs/:log_id
/// Get a single log entry or its binary content
///
/// Content negotiation:
/// - Accept: application/json -> returns log metadata as JSON
/// - Accept: application/octet-stream -> returns raw binary content
pub async fn get_log(
    State(state): State<AppState>,
    Path(params): Path<LogPathParams>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let backend = state.get_backend(&params.component_id)?;

    // Check if this backend supports logs
    if !backend.capabilities().logs {
        return Err(ApiError::NotImplemented(
            "This component does not support logs".to_string(),
        ));
    }

    // Check Accept header for content negotiation
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json");

    if accept.contains("application/octet-stream") {
        // Return binary content
        let content = backend.get_log_content(&params.log_id).await?;
        let content_len = content.len();

        Ok(Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .header(header::CONTENT_LENGTH, content_len)
            .body(Body::from(content))
            .unwrap())
    } else {
        // Return JSON metadata
        let log = backend.get_log(&params.log_id).await?;
        let response = LogEntryResponse::from(&log);
        Ok(Json(response).into_response())
    }
}

/// DELETE /vehicle/v1/components/:component_id/logs/:log_id
/// Delete/acknowledge a log entry
pub async fn delete_log(
    State(state): State<AppState>,
    Path(params): Path<LogPathParams>,
) -> Result<StatusCode, ApiError> {
    let backend = state.get_backend(&params.component_id)?;

    // Check if this backend supports logs
    if !backend.capabilities().logs {
        return Err(ApiError::NotImplemented(
            "This component does not support logs".to_string(),
        ));
    }

    backend.delete_log(&params.log_id).await?;

    Ok(StatusCode::NO_CONTENT)
}
