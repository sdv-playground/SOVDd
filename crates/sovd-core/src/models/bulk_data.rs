//! Bulk-data models — SOVD §7.20 (ISO 17978-3, conformance C-120).
//!
//! Bulk-data is the standard's mechanism for transferring large, opaque payloads
//! (log files, captured traces, calibration blobs) that don't fit the small-JSON
//! resource shapes. A backend exposes named CATEGORIES; each category lists
//! downloadable ITEMS; an item is fetched whole via
//! `GET /{entity}/bulk-data/{category}/{id}`.
//!
//! §7.21 logging (C-121) is the first consumer: a component's log files are a
//! `logs` category, and "get all logs" = list + download those items — the
//! spec-native alternative to the inline `LogPage` cursor (which stays as a
//! non-normative quick view). See tasks/bulk-data-design.md.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One bulk-data category exposed by an entity (e.g. `logs`). The download
/// collection lives at `/{entity}/bulk-data/{name}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BulkCategory {
    /// Category id, unique within the entity (the `{category}` path segment).
    pub name: String,
}

/// Metadata for one downloadable bulk-data item. The bytes are fetched
/// separately via [`crate::DiagnosticBackend::get_bulk_data`] — this is the
/// catalog entry only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BulkDataItem {
    /// Item id, unique within its category (the `{bulk-data-id}` path segment).
    /// Stateless where possible (e.g. base64url of a relative path), so it stays
    /// stable across restarts without server-side bookkeeping.
    pub id: String,
    /// Payload size in bytes (best effort; may be 0 if not cheaply known).
    pub size: u64,
    /// Creation/last-modified time (RFC 3339 UTC). For a log file this is its
    /// mtime — the field `created-before` / `created-after` filter against.
    pub created: DateTime<Utc>,
    /// MIME type of the payload (e.g. `text/plain` for a log file,
    /// `application/octet-stream` for opaque binary).
    pub mime: String,
    /// Optional producer/source label (e.g. the log source name). Omitted when
    /// not meaningful.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Filters for listing a category (`GET /bulk-data/{category}` query params).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BulkDataFilter {
    /// Keep items created strictly before this time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_before: Option<DateTime<Utc>>,
    /// Keep items created strictly after this time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_after: Option<DateTime<Utc>>,
}

/// The result of a download request. The §7.20 download endpoint may answer
/// `200` (bytes inline), `307` (redirect to a direct URL), or `202` (payload is
/// being staged asynchronously — poll the given location). A backend returns the
/// variant that fits; the API layer maps it to the HTTP response.
#[derive(Debug, Clone)]
pub enum BulkDataDownload {
    /// Serve the bytes directly (`200`). The simple, default case.
    Inline { mime: String, bytes: Vec<u8> },
    /// Redirect the client to a direct URL (`307`) — e.g. a large file served
    /// out-of-band. The client re-requests `location`.
    Redirect { location: String },
    /// The payload is being prepared (`202`); the client polls `location` until
    /// it becomes downloadable. Used when materialising a blob (e.g. a guest log
    /// export) takes time.
    Async { location: String },
}
