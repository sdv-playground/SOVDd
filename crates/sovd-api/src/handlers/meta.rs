//! Server-level meta endpoints — ISO 17978-3 §7.4 + §7.5.
//!
//! These are version-INDEPENDENT in the spec (their path doesn't
//! change across API editions), which is why the version-info route
//! is mounted at `/version-info` not `/vehicle/v1/version-info`.

use axum::Json;
use serde::{Deserialize, Serialize};

/// One row of `/version-info` — describes a supported SOVD API edition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionEntry {
    /// URI version segment, e.g. `"v1"`.
    pub version_identifier: String,
    /// Base path this version serves at, e.g. `"/vehicle/v1"`.
    pub base_path: String,
    /// Spec edition / x-sovd-version this maps to.
    pub x_sovd_version: String,
}

/// Response body for `GET /version-info`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionInfoResponse {
    /// All API versions this server serves.
    pub versions: Vec<VersionEntry>,
}

/// GET /version-info  — list supported SOVD API editions (§7.4, C-005).
pub async fn version_info() -> Json<VersionInfoResponse> {
    Json(VersionInfoResponse {
        versions: vec![VersionEntry {
            version_identifier: "v1".to_string(),
            base_path: "/vehicle/v1".to_string(),
            x_sovd_version: "1.1".to_string(),
        }],
    })
}

/// GET /vehicle/v1/docs — minimal OpenAPI 3.1.0 capability description (§7.5).
///
/// This is a stub: it advertises the SOVDd build version, the API
/// edition, and an empty `paths` object so the document is
/// well-formed.  A full emitter (one that walks the router and
/// produces actual path entries) is tracked separately — that's the
/// "OpenAPI capability description" listed in the migration plan.
pub async fn capability_description() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "openapi": "3.1.0",
        "info": {
            "title": "SOVDd",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "SOVD (ISO 17978-3) diagnostic server.",
            "x-sovd-version": "1.1",
        },
        "servers": [
            {"url": "/vehicle/v1"},
        ],
        "paths": {},
    }))
}
