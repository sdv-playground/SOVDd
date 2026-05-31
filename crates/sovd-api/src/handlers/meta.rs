//! Server-level meta endpoints — ISO 17978-3 §7.4 + §7.5.
//!
//! These are version-INDEPENDENT in the spec (their path doesn't
//! change across API editions), which is why the version-info route
//! is mounted at `/version-info` not `/vehicle/v1/version-info`.

use axum::http::{StatusCode, Uri};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::error::ApiError;

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

/// Router fallback for unknown paths — emit `GenericError` with the
/// spec-conforming shape instead of axum's plain-text default.
pub async fn not_found_fallback(uri: Uri) -> ApiError {
    ApiError::NotFound(format!("No resource at {}", uri.path()))
}

/// Router fallback for matched paths with disallowed methods.
/// Spec §5.8 405 carries `GenericError`.
pub async fn method_not_allowed_fallback(
    uri: Uri,
) -> (StatusCode, axum::Json<sovd_core::GenericError>) {
    let err = sovd_core::GenericError::vendor(
        "method-not-allowed",
        format!("Method not allowed on {}", uri.path()),
    );
    (StatusCode::METHOD_NOT_ALLOWED, axum::Json(err))
}

/// One row of the path inventory consumed by [`capability_description`].
struct PathEntry {
    /// HTTP method (uppercase).
    method: &'static str,
    /// Path template — placeholders use OpenAPI `{name}` syntax.
    path: &'static str,
    /// One-line operation summary.
    summary: &'static str,
}

/// Curated path inventory.  Maintained alongside the router in
/// `lib.rs::create_router`; the doc emitter walks this slice rather
/// than the axum Router (axum 0.8 doesn't expose its routing table).
const PATHS: &[PathEntry] = &[
    // health + meta
    PathEntry {
        method: "GET",
        path: "/health",
        summary: "Server liveness.",
    },
    PathEntry {
        method: "GET",
        path: "/version-info",
        summary: "List supported SOVD API editions (§7.4).",
    },
    PathEntry {
        method: "GET",
        path: "/vehicle/v1/docs",
        summary: "OpenAPI capability description (§7.5).",
    },
    // components / entities
    PathEntry {
        method: "GET",
        path: "/vehicle/v1/components",
        summary: "List components (§7.6).",
    },
    PathEntry {
        method: "GET",
        path: "/vehicle/v1/components/{component_id}",
        summary: "Read component capabilities (§7.6).",
    },
    PathEntry {
        method: "GET",
        path: "/vehicle/v1/components/{component_id}/apps",
        summary: "List apps hosted on a component (§7.6).",
    },
    PathEntry {
        method: "GET",
        path: "/vehicle/v1/components/{component_id}/apps/{app_id}",
        summary: "Read app capabilities.",
    },
    PathEntry {
        method: "GET",
        path: "/vehicle/v1/components/{component_id}/apps/{app_id}/apps",
        summary: "List nested sub-apps.",
    },
    // data (§7.10)
    PathEntry {
        method: "GET",
        path: "/vehicle/v1/components/{component_id}/data",
        summary: "List data parameters (§7.10).",
    },
    PathEntry {
        method: "GET",
        path: "/vehicle/v1/components/{component_id}/data/{param_id}",
        summary: "Read a parameter; `?raw=true` for raw bytes.",
    },
    PathEntry {
        method: "PUT",
        path: "/vehicle/v1/components/{component_id}/data/{param_id}",
        summary: "Write a parameter — 204 No Content.",
    },
    // dynamic data lists (§5.3.6 + §7.14)
    PathEntry {
        method: "POST",
        path: "/vehicle/v1/components/{component_id}/operations/define-data/executions",
        summary: "Define a dynamic data list (UDS 0x2C 0x02).",
    },
    PathEntry {
        method: "GET",
        path: "/vehicle/v1/components/{component_id}/data-lists",
        summary: "List dynamic data lists.",
    },
    PathEntry {
        method: "GET",
        path: "/vehicle/v1/components/{component_id}/data-lists/{list_id}",
        summary: "Read a dynamic data list value (UDS 0x22).",
    },
    PathEntry {
        method: "DELETE",
        path: "/vehicle/v1/components/{component_id}/data-lists/{list_id}",
        summary: "Clear a dynamic data list (UDS 0x2C 0x03).",
    },
    // faults (§7.8)
    PathEntry {
        method: "GET",
        path: "/vehicle/v1/components/{component_id}/faults",
        summary: "List faults / DTCs (§7.8).",
    },
    PathEntry {
        method: "DELETE",
        path: "/vehicle/v1/components/{component_id}/faults",
        summary: "Clear all faults — 204.",
    },
    PathEntry {
        method: "GET",
        path: "/vehicle/v1/components/{component_id}/faults/{fault_id}",
        summary: "Read one fault.",
    },
    // operations (§7.14)
    PathEntry {
        method: "GET",
        path: "/vehicle/v1/components/{component_id}/operations",
        summary: "List operations (§7.14).",
    },
    PathEntry {
        method: "POST",
        path: "/vehicle/v1/components/{component_id}/operations/{operation_id}/executions",
        summary: "Start an operation execution — 200/202 + Location.",
    },
    PathEntry {
        method: "GET",
        path:
            "/vehicle/v1/components/{component_id}/operations/{operation_id}/executions/{exec_id}",
        summary: "Poll an execution.",
    },
    PathEntry {
        method: "DELETE",
        path:
            "/vehicle/v1/components/{component_id}/operations/{operation_id}/executions/{exec_id}",
        summary: "Stop an execution — 204.",
    },
    // outputs (non-spec; move to operations in a follow-up)
    PathEntry {
        method: "GET",
        path: "/vehicle/v1/components/{component_id}/outputs",
        summary: "List I/O controls (UDS 0x2F).",
    },
    PathEntry {
        method: "GET",
        path: "/vehicle/v1/components/{component_id}/outputs/{output_id}",
        summary: "Read I/O state.",
    },
    PathEntry {
        method: "POST",
        path: "/vehicle/v1/components/{component_id}/outputs/{output_id}",
        summary: "Control an output.",
    },
    // logs (§7.21)
    PathEntry {
        method: "GET",
        path: "/vehicle/v1/components/{component_id}/logs",
        summary: "List logs.",
    },
    PathEntry {
        method: "GET",
        path: "/vehicle/v1/components/{component_id}/logs/entries",
        summary: "List log entries (§7.21).",
    },
    PathEntry {
        method: "GET",
        path: "/vehicle/v1/components/{component_id}/logs/config",
        summary: "Read log configuration.",
    },
    PathEntry {
        method: "PUT",
        path: "/vehicle/v1/components/{component_id}/logs/config",
        summary: "Set log configuration — 204.",
    },
    PathEntry {
        method: "DELETE",
        path: "/vehicle/v1/components/{component_id}/logs/config",
        summary: "Reset log configuration — 204.",
    },
    // clear-data (§7.13)
    PathEntry {
        method: "GET",
        path: "/vehicle/v1/components/{component_id}/clear-data",
        summary: "List supported clear-data types (§7.13).",
    },
    PathEntry {
        method: "GET",
        path: "/vehicle/v1/components/{component_id}/clear-data/status",
        summary: "Read clear-data status.",
    },
    PathEntry {
        method: "PUT",
        path: "/vehicle/v1/components/{component_id}/clear-data/{action}",
        summary: "Run a clear-data action — 202.",
    },
    // cyclic-subscriptions (§7.10)
    PathEntry {
        method: "GET",
        path: "/vehicle/v1/components/{component_id}/cyclic-subscriptions",
        summary: "List cyclic subscriptions.",
    },
    PathEntry {
        method: "POST",
        path: "/vehicle/v1/components/{component_id}/cyclic-subscriptions",
        summary: "Create a cyclic subscription — 201 + Location.",
    },
    PathEntry {
        method: "GET",
        path: "/vehicle/v1/components/{component_id}/cyclic-subscriptions/{subscription_id}",
        summary: "Read a subscription.",
    },
    PathEntry {
        method: "PUT",
        path: "/vehicle/v1/components/{component_id}/cyclic-subscriptions/{subscription_id}",
        summary: "Update subscription cadence/duration.",
    },
    PathEntry {
        method: "DELETE",
        path: "/vehicle/v1/components/{component_id}/cyclic-subscriptions/{subscription_id}",
        summary: "Cancel a subscription — 204.",
    },
    PathEntry {
        method: "GET",
        path: "/vehicle/v1/components/{component_id}/streams/{subscription_id}",
        summary: "SSE stream for a cyclic subscription.",
    },
    // reset (§7.19)
    PathEntry {
        method: "PUT",
        path: "/vehicle/v1/components/{component_id}/status/restart",
        summary: "Restart the entity — 202 + Location.",
    },
    PathEntry {
        method: "GET",
        path: "/vehicle/v1/components/{component_id}/status/restart/{exec_id}",
        summary: "Poll restart status.",
    },
    // modes (§7.16)
    PathEntry {
        method: "GET",
        path: "/vehicle/v1/components/{component_id}/modes/session",
        summary: "Read session mode.",
    },
    PathEntry {
        method: "PUT",
        path: "/vehicle/v1/components/{component_id}/modes/session",
        summary: "Change session mode.",
    },
    PathEntry {
        method: "GET",
        path: "/vehicle/v1/components/{component_id}/modes/security",
        summary: "Read security mode.",
    },
    PathEntry {
        method: "PUT",
        path: "/vehicle/v1/components/{component_id}/modes/security",
        summary: "Request seed / send key (UDS 0x27).",
    },
];

/// GET /vehicle/v1/docs — capability description (§7.5).
///
/// Today this is a curated OpenAPI 3.1.0 document built from the
/// `PATHS` table above + a small set of reusable schemas
/// (`GenericError`, `Fault`, `OperationExecution`, `CyclicSubscription`).
/// A full path-walker that introspects the axum router is a TODO —
/// axum 0.8 doesn't expose its routing table.
pub async fn capability_description() -> Json<serde_json::Value> {
    let mut paths = serde_json::Map::new();
    for entry in PATHS {
        let op = serde_json::json!({
            "summary": entry.summary,
            "responses": {
                "default": {
                    "description": "See §5.8 status codes; non-2xx bodies are GenericError.",
                    "content": {
                        "application/json": {
                            "schema": { "$ref": "#/components/schemas/GenericError" }
                        }
                    }
                }
            }
        });
        let path_entry = paths
            .entry(entry.path.to_string())
            .or_insert_with(|| serde_json::json!({}));
        path_entry
            .as_object_mut()
            .unwrap()
            .insert(entry.method.to_ascii_lowercase(), op);
    }

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
        "paths": paths,
        "components": {
            "schemas": {
                "GenericError": {
                    "type": "object",
                    "required": ["error_code", "message"],
                    "properties": {
                        "error_code": { "type": "string", "description": "ISO 17978-3 Table 18 token." },
                        "vendor_code": { "type": "string", "description": "Required iff error_code == \"vendor-specific\"." },
                        "message": { "type": "string" },
                        "translation_id": { "type": "string" },
                        "parameters": {
                            "type": "object",
                            "additionalProperties": { "type": "array", "items": { "type": "string" } }
                        }
                    }
                },
                "Component": {
                    "type": "object",
                    "required": ["id", "type"],
                    "properties": {
                        "id": { "type": "string" },
                        "name": { "type": "string" },
                        "type": { "type": "string" },
                        "href": { "type": "string", "format": "uri-reference" }
                    }
                },
                "Fault": {
                    "type": "object",
                    "required": ["id", "code", "fault_name", "severity"],
                    "properties": {
                        "id": { "type": "string" },
                        "code": { "type": "string" },
                        "fault_name": { "type": "string" },
                        "severity": { "type": "integer", "minimum": 1, "maximum": 4, "description": "1=FATAL, 2=ERROR, 3=WARN, 4=INFO." },
                        "active": { "type": "boolean" },
                        "status": { "type": "object" },
                        "href": { "type": "string", "format": "uri-reference" }
                    }
                },
                "OperationExecution": {
                    "type": "object",
                    "required": ["execution_id", "operation_id", "status", "started_at"],
                    "properties": {
                        "execution_id": { "type": "string" },
                        "operation_id": { "type": "string" },
                        "status": { "type": "string", "enum": ["running", "completed", "failed", "stopped"] },
                        "result": {},
                        "error": { "type": "string" },
                        "started_at": { "type": "string", "format": "date-time" },
                        "completed_at": { "type": "string", "format": "date-time" }
                    }
                },
                "CyclicSubscription": {
                    "type": "object",
                    "required": ["subscription_id", "component_id", "resource", "interval", "protocol", "status", "created_at"],
                    "properties": {
                        "subscription_id": { "type": "string" },
                        "component_id": { "type": "string" },
                        "resource": { "type": "string" },
                        "interval": { "type": "string", "enum": ["fast", "normal", "slow"] },
                        "protocol": { "type": "string" },
                        "status": { "type": "string" },
                        "created_at": { "type": "string", "format": "date-time" },
                        "expires_at": { "type": "string", "format": "date-time" }
                    }
                },
                "EventEnvelope": {
                    "type": "object",
                    "required": ["timestamp"],
                    "properties": {
                        "timestamp": { "type": "string", "format": "date-time" },
                        "payload": {},
                        "error": { "$ref": "#/components/schemas/GenericError" }
                    }
                }
            }
        }
    }))
}
