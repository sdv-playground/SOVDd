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
    /// Opaque cursor for the NEXT page; feed back as `?x-sumo-after=`. `null`
    /// once the caller has reached the head — a paging loop stops here. Absent
    /// when the backend doesn't paginate. Vendor extension (§6.2.7 `x-<ext>-`;
    /// the SOVD log spec has no cursor).
    #[serde(rename = "x-sumo-next-cursor", skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Oldest position still available; if a caller's `x-sumo-after` predates it,
    /// history in between rotated away (gap detection). Absent when unknown.
    #[serde(rename = "x-sumo-oldest-cursor", skip_serializing_if = "Option::is_none")]
    pub oldest_cursor: Option<String>,
    /// The cursor at the current HEAD ("now"): poll `x-sumo-after=<this>` to
    /// follow only new entries. Present even when `next_cursor` is null (head
    /// reached), so a follower has a resume point. Absent when the backend can't
    /// name its tip. Vendor extension.
    #[serde(rename = "x-sumo-tip-cursor", skip_serializing_if = "Option::is_none")]
    pub tip_cursor: Option<String>,
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
    /// RFC 3339, or a position sentinel: `BEGIN` (oldest, no lower bound),
    /// `END` (device now), `END-<N>{s,m,h}` (now minus a duration — the last N of
    /// THIS boot). Resolved server-side against the device clock — see
    /// [`resolve_time_bound`]. A cursor (`after`) is the reboot-safe resume tool;
    /// these time bounds are a within-boot convenience.
    pub since: Option<String>,
    pub until: Option<String>,
    pub pattern: Option<String>,
    pub limit: Option<usize>,
    pub tail: Option<usize>,
    /// Filter by log type (e.g., "engine_dump", "diagnostic")
    #[serde(rename = "type")]
    pub log_type: Option<String>,
    /// Filter by retrieval status (pending, retrieved)
    pub status: Option<String>,
    /// Opaque pagination cursor — return entries strictly after this position.
    /// Omit to start at the oldest available. Never parsed by the client. Vendor
    /// extension (§6.2.7): the wire name is `x-sumo-after`.
    #[serde(rename = "x-sumo-after")]
    pub after: Option<String>,
}

/// Resolve a `since`/`until` value to an absolute time. Accepts RFC 3339, or a
/// position sentinel resolved against the DEVICE clock (the server is the
/// device, and log entries are stamped with the same clock, so "now" and
/// "now − N" bound this boot's entries correctly):
///
///   BEGIN        → no bound (None) — the oldest available (a lower-bound `since`
///                  of BEGIN means "from the start"; an upper-bound `until` of
///                  BEGIN is meaningless but harmlessly yields no bound).
///   END | NOW    → device now.
///   END-<N>{s,m,h,d} | NOW-<N>…  → now minus the duration.
///
/// `None` input → `None` (unbounded). A malformed value → `BadRequest` (400), so
/// a typo surfaces instead of silently widening the query. Case-insensitive
/// sentinels; RFC 3339 is tried after the keyword forms.
fn resolve_time_bound(raw: Option<&str>) -> Result<Option<DateTime<Utc>>, ApiError> {
    let Some(s) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let upper = s.to_ascii_uppercase();
    if upper == "BEGIN" {
        return Ok(None); // oldest — no lower bound
    }
    // END / NOW, optionally minus a duration: END-10m, NOW-2h, END-30s, END-1d.
    for kw in ["END", "NOW"] {
        if upper == kw {
            return Ok(Some(Utc::now()));
        }
        if let Some(rest) = upper.strip_prefix(kw).and_then(|r| r.strip_prefix('-')) {
            let secs = parse_duration_secs(rest).ok_or_else(|| {
                ApiError::BadRequest(format!(
                    "bad time value {s:?}: expected {kw}-<N>{{s,m,h,d}} (e.g. {kw}-10m)"
                ))
            })?;
            return Ok(Some(Utc::now() - chrono::Duration::seconds(secs as i64)));
        }
    }
    // Otherwise a literal RFC 3339 timestamp.
    DateTime::parse_from_rfc3339(s)
        .map(|t| Some(t.with_timezone(&Utc)))
        .map_err(|_| {
            ApiError::BadRequest(format!(
                "bad time value {s:?}: expected RFC 3339, or BEGIN / END / END-<N>{{s,m,h,d}}"
            ))
        })
}

/// Parse a compact duration like `10m`, `2h`, `30s`, `1d` into seconds.
fn parse_duration_secs(s: &str) -> Option<u64> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.len().checked_sub(1)?);
    let n: u64 = num.parse().ok()?;
    let mult = match unit {
        "s" | "S" => 1,
        "m" | "M" => 60,
        "h" | "H" => 3_600,
        "d" | "D" => 86_400,
        _ => return None,
    };
    n.checked_mul(mult)
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
        // Resolve BEGIN/END/END-Nm sentinels (or RFC 3339) server-side against
        // the device clock. A bad value is a 400, not a silent drop.
        since: resolve_time_bound(query.since.as_deref())?,
        until: resolve_time_bound(query.until.as_deref())?,
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
        after: query.after,
    };

    // Paged path: a non-paging backend's default impl returns everything in one
    // terminal page (next_cursor = None), so this is byte-compatible for existing
    // clients while giving cursor-aware clients pagination + gap detection.
    let page = backend.get_logs_paged(&filter).await?;
    let total_count = page.items.len();
    let items: Vec<LogEntryResponse> = page.items.iter().map(LogEntryResponse::from).collect();

    Ok(Json(LogsResponse {
        items,
        total_count,
        next_cursor: page.next_cursor,
        oldest_cursor: page.oldest_cursor,
        tip_cursor: page.tip_cursor,
    }))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_time_bound_sentinels_and_rfc3339() {
        // BEGIN and absent → unbounded.
        assert!(resolve_time_bound(Some("BEGIN")).unwrap().is_none());
        assert!(resolve_time_bound(Some("begin")).unwrap().is_none()); // case-insensitive
        assert!(resolve_time_bound(None).unwrap().is_none());
        assert!(resolve_time_bound(Some("  ")).unwrap().is_none()); // blank → unbounded

        // END / NOW → ~device now (within a generous window of the call).
        let before = Utc::now();
        let end = resolve_time_bound(Some("END")).unwrap().unwrap();
        let after = Utc::now();
        assert!(end >= before && end <= after, "END resolves to ~now");
        assert!(resolve_time_bound(Some("now")).unwrap().is_some());

        // END-<N> subtracts the duration.
        let now = Utc::now();
        let ten_min = resolve_time_bound(Some("END-10m")).unwrap().unwrap();
        let delta = (now - ten_min).num_seconds();
        assert!((595..=605).contains(&delta), "END-10m ≈ 600s ago, got {delta}");
        // units s/h/d all parse.
        assert!(resolve_time_bound(Some("NOW-30s")).unwrap().is_some());
        assert!(resolve_time_bound(Some("END-2h")).unwrap().is_some());
        assert!(resolve_time_bound(Some("END-1d")).unwrap().is_some());

        // RFC 3339 passes through.
        let t = resolve_time_bound(Some("2026-07-24T10:00:00Z")).unwrap().unwrap();
        assert_eq!(t.to_rfc3339(), "2026-07-24T10:00:00+00:00");
    }

    #[test]
    fn resolve_time_bound_rejects_garbage() {
        for bad in ["END-10x", "END-", "yesterday", "END-abc", "10m", "2026-13-99"] {
            assert!(
                matches!(resolve_time_bound(Some(bad)), Err(ApiError::BadRequest(_))),
                "{bad:?} should be a 400"
            );
        }
    }
}
