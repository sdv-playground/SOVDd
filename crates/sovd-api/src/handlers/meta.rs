//! Server-level meta endpoints — ISO 17978-3 §7.4 + §7.5.
//!
//! These are version-INDEPENDENT in the spec (their path doesn't
//! change across API editions), which is why the version-info route
//! is mounted at `/version-info` not `/vehicle/v1/version-info`.

use axum::extract::State;
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};

use crate::auth::ClientContext;
use crate::error::ApiError;
use crate::state::{AppState, CapabilityExtensions};
use crate::topology::NodeTopology;
use crate::Tree;

/// One entry of `/version-info`'s `sovd_info[]` array — ISO 17978-3
/// §7.4.2 Table 37 (SOVDInfo). Describes one supported SOVD version.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SovdInfo {
    /// Version of the SOVD standard supported, as a SemVer 2.0.0 string
    /// (Table 37 `version`, convention M).
    pub version: String,
    /// Version-specific base URI for interacting with the server; a
    /// uri-reference that carries the `v1` URI version segment, e.g.
    /// `"/vehicle/v1"` (Table 37 `base_uri`, convention M).
    pub base_uri: String,
    /// Vendor-specific information (Table 37 `vendor_info`, convention O).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vendor_info: Option<VendorInfo>,
}

/// Vendor-specific server info — ISO 17978-3 §7.4.2 Table 38 (VendorInfo).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VendorInfo {
    /// Name of the vendor of the SOVD server (Table 38 `name`, M).
    pub name: String,
    /// Version of the SOVD server — a vendor-specific string
    /// (Table 38 `version`, M).
    pub version: String,
}

/// Response body for `GET /version-info` — ISO 17978-3 §7.4.2 Table 36.
///
/// The single top-level element is `sovd_info` (SOVDInfo[]). We always
/// return the list; the optional `include-schema` query parameter (which
/// would add a `schema` sibling) is not implemented — unconditionally
/// returning the version list is the useful reading of Table 36.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionInfoResponse {
    /// Supported SOVD versions (Table 36 `sovd_info`).
    pub sovd_info: Vec<SovdInfo>,
}

/// Single source of truth for the SOVD API base paths this server mounts.
///
/// `version_info` (§7.4) derives its `sovd_info` array from this slice.  The
/// router in `lib.rs::create_router` mounts the identical entity API under
/// both bases — `/v1` (the spec base, §5.6) and `/vehicle/v1` (the original
/// base, kept for existing clients) — one row per [`crate::Tree`], in
/// [`crate::Tree::ALL`] order, so `/v1` is advertised first and the list can't
/// drift from the actual surface (C-005).  Add a row here (and a `Tree`
/// variant) when introducing a new base or API edition.
///
/// Each tuple is `(version_identifier, base_uri, sovd_version)`:
///   * `version_identifier` — the URI version segment (§5.6 rule: value `v1`
///     for this edition). Not serialized on its own — it is carried inside
///     `base_uri`; kept here as the internal routing key.
///   * `base_uri` — the version-specific base URI (`/v1` or `/vehicle/v1`),
///     emitted as SOVDInfo `base_uri` (§7.4.2 Table 37).
///   * `sovd_version` — the SOVD standard version this maps to, as a SemVer
///     2.0.0 string, emitted as SOVDInfo `version` (Table 37). Aligned with
///     ASAM SOVD 1.1 (info.x-sovd-version) but expressed as `1.1.0` so it is
///     valid SemVer 2.0.0.
pub const API_VERSIONS: &[(&str, &str, &str)] = &[
    ("v1", Tree::V1.base(), "1.1.0"),
    ("v1", Tree::Legacy.base(), "1.1.0"),
];

/// Build the `version-info` body from [`API_VERSIONS`].
pub fn build_version_info() -> VersionInfoResponse {
    VersionInfoResponse {
        sovd_info: API_VERSIONS
            .iter()
            .map(|(_id, base_uri, sovd_version)| SovdInfo {
                version: (*sovd_version).to_string(),
                base_uri: (*base_uri).to_string(),
                vendor_info: Some(VendorInfo {
                    name: "SOVDd".to_string(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                }),
            })
            .collect(),
    }
}

/// GET /version-info  — list ALL supported SOVD API editions (§7.4.2, C-005).
///
/// The path itself is version-independent (mounted at `/version-info`, not
/// under any `/vehicle/{v}`) so it stays constant across editions per §5.6.
pub async fn version_info() -> Json<VersionInfoResponse> {
    Json(build_version_info())
}

/// Router fallback — three roles.
///
/// 1. **Scoped capability description** (ISO 17978-3 §6.3.3/7.5, C-063):
///    any `GET {entity-path}/docs` at arbitrary depth returns `200` with
///    an OpenAPI 3.1.0 document whose `paths` are scoped to that entity
///    path.  This is handled here in the fallback rather than as a real
///    route because axum's `{*wildcard}` must be the final path segment,
///    so a `/{*path}/docs` route is inexpressible.  The global
///    `{base}/docs` (both bases) is a real route and never reaches this
///    fallback.
///    Scope is computed purely by path-template matching — entity
///    existence is not validated, and an empty scope still yields a valid
///    OpenAPI doc with empty `paths` (never a 404).
///
/// 2. **The node-level `data` and `operations` collections** of a server
///    configured with a node: `GET /v1/components/{node}/data` (routed as
///    `/v1/data`, see [`crate::topology`]) reaches the fallback when no
///    embedder route serves `/v1/data`, and is answered by
///    [`crate::handlers::components::node_collection`]; `operations` alike.
///    `GET /v1/data` likewise on a server with a node-level data provider
///    ([`AppState::with_node_data`]), with or without a node.
///
/// 3. **Spec-conforming 404** for everything else — emit `GenericError`
///    with the spec shape instead of axum's plain-text default.
pub async fn not_found_fallback(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    client: Option<Extension<ClientContext>>,
) -> Response {
    let path = uri.path();
    if let (Method::GET, Some((Tree::V1, rest @ ("/data" | "/operations")))) =
        (method, Tree::split_path(path))
    {
        if state.node().is_some() || (rest == "/data" && state.node_data().is_some()) {
            let client = client.as_ref().map(|Extension(c)| c);
            return crate::handlers::components::node_collection(&state, client, &rest[1..])
                .await
                .into_response();
        }
    }
    if let Some(entity) = path.strip_suffix("/docs") {
        // Strip a trailing slash if the entity itself ended with one
        // (e.g. `/vehicle/v1/components/foo//docs` is unusual but cheap
        // to tolerate).  Require a non-empty entity so a bare `/docs`
        // request still 404s rather than aliasing the global doc.
        let entity = entity.strip_suffix('/').unwrap_or(entity);
        if !entity.is_empty() {
            // Render under the entity's own tree; a path under no base keeps
            // the `/vehicle/v1` rendering it always had.
            let tree = Tree::split_path(entity).map_or(Tree::Legacy, |(tree, _)| tree);
            let mut doc =
                build_capability_doc_for(tree, Some(entity), state.capability_extensions());
            if let Some(node) = state.node().filter(|_| tree == Tree::V1) {
                to_node_forms(&mut doc, node);
            }
            return Json(doc).into_response();
        }
    }
    let path = crate::topology::served_form(path.to_owned());
    ApiError::NotFound(format!("No resource at {}", path)).into_response()
}

/// Router fallback for matched paths with disallowed methods.
/// Spec §5.8 405 carries `GenericError`.
pub async fn method_not_allowed_fallback(
    uri: Uri,
) -> (StatusCode, axum::Json<sovd_core::GenericError>) {
    let err = sovd_core::GenericError::vendor(
        "method-not-allowed",
        format!(
            "Method not allowed on {}",
            crate::topology::served_form(uri.path().to_owned())
        ),
    );
    (StatusCode::METHOD_NOT_ALLOWED, axum::Json(err))
}

/// The legacy-only restart execution stub (see `restart_status_routes`).
const LEGACY_RESTART_EXECUTION: &str = "/components/{component_id}/status/restart/{exec_id}";

/// Whether `entry` is listed on `/v1` only: the part read-back (the frozen
/// legacy tree keeps its 405 there) and execute, whose `/v1` form is
/// always orchestrated (the frozen legacy document never listed it).
fn v1_only(entry: &PathEntry) -> bool {
    entry.method == "GET" && entry.path.ends_with("/bulk-data/{part_id}") || is_execute(entry)
}

fn is_execute(entry: &PathEntry) -> bool {
    entry.method == "PUT" && entry.path.ends_with("/updates/{update_id}/execute")
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

/// ISO 17978-3 Table 23 `x-sovd-data-category` — best-effort annotation for a
/// path item, computed from the path template.
///
/// Table 23 marks `x-sovd-data-category` **M** for a data resource. Returns
/// the annotation for the `/data/{param_id}` resource and `None` for every
/// other path.
///
/// NOTE (C-024): the `/data/{param_id}` template is shared by *every* DID, so
/// a single precise per-DID category isn't expressible on one templated path
/// item. We emit the placeholder token `"x-multiple"` to signal "this
/// resource is category-tagged; the concrete category varies per DID — read
/// `ValueMetaData.category` from `GET /data`". A per-DID-accurate
/// `x-sovd-data-category` would require the doc emitter to introspect the
/// router/DidStore and emit one concrete path item per registered DID. The
/// `/data` LIST item carries the precise category today; only this templated
/// per-resource path item is imprecise. Tracked under C-024.
fn x_sovd_data_category_for(path: &str) -> Option<&'static str> {
    if path.ends_with("/data/{param_id}") {
        // Custom-extension category token (Table 70 `x-<ext>-…` form) meaning
        // "varies per DID"; not one of the four standard values on purpose.
        // The `x-sovd-` prefix is RESERVED for spec-defined names (§5.4.5,
        // C-026) — a custom token must carry the plain `x-` prefix instead.
        Some("x-multiple")
    } else {
        None
    }
}

/// Summary of the `/v1`-only execute operations.
const EXECUTE_SUMMARY: &str =
    "Execute a prepared SW update (§7.18.6) — 202 + Location. Orchestrated: a banked update is settled only by the tester's verdict (x-update-commit / x-update-rollback).";

/// Curated path inventory.  Maintained alongside the router in
/// `lib.rs::create_router`; the doc emitter walks this slice rather
/// than the axum Router (axum 0.8 doesn't expose its routing table).
///
/// Templates are base-relative (`/components/...`) and prefixed with the
/// requested tree's base at render time ([`render_template`]), except the
/// [`ROOT_PATHS`], which are served at the server root under no base.
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
        path: "/docs",
        summary: "OpenAPI capability description (§7.5).",
    },
    // SOVDServer entity (§7.6.3)
    PathEntry {
        method: "GET",
        path: "/",
        summary: "Read the SOVD server's capabilities (§7.6.3).",
    },
    // components / entities
    PathEntry {
        method: "GET",
        path: "/components",
        summary: "List components (§7.6).",
    },
    PathEntry {
        method: "GET",
        path: "/components/{component_id}",
        summary: "Read component capabilities (§7.6).",
    },
    PathEntry {
        method: "GET",
        path: "/components/{component_id}/apps",
        summary: "List apps hosted on a component (§7.6).",
    },
    PathEntry {
        method: "GET",
        path: "/components/{component_id}/apps/{app_id}",
        summary: "Read app capabilities.",
    },
    PathEntry {
        method: "GET",
        path: "/components/{component_id}/apps/{app_id}/apps",
        summary: "List nested sub-apps.",
    },
    // data (§7.10)
    PathEntry {
        method: "GET",
        path: "/components/{component_id}/data",
        summary: "List data parameters (§7.10).",
    },
    PathEntry {
        method: "GET",
        path: "/components/{component_id}/data/{param_id}",
        summary: "Read a parameter; `?raw=true` for raw bytes.",
    },
    PathEntry {
        method: "PUT",
        path: "/components/{component_id}/data/{param_id}",
        summary: "Write a parameter — 204 No Content.",
    },
    // dynamic data lists (§5.3.6 + §7.14)
    PathEntry {
        method: "POST",
        path: "/components/{component_id}/operations/define-data/executions",
        summary: "Define a dynamic data list (UDS 0x2C 0x02).",
    },
    PathEntry {
        method: "GET",
        path: "/components/{component_id}/data-lists",
        summary: "List dynamic data lists.",
    },
    PathEntry {
        method: "GET",
        path: "/components/{component_id}/data-lists/{list_id}",
        summary: "Read a dynamic data list value (UDS 0x22).",
    },
    PathEntry {
        method: "DELETE",
        path: "/components/{component_id}/data-lists/{list_id}",
        summary: "Clear a dynamic data list (UDS 0x2C 0x03).",
    },
    // faults (§7.8)
    PathEntry {
        method: "GET",
        path: "/components/{component_id}/faults",
        summary: "List faults / DTCs (§7.8).",
    },
    PathEntry {
        method: "DELETE",
        path: "/components/{component_id}/faults",
        summary: "Clear all faults — 204.",
    },
    PathEntry {
        method: "GET",
        path: "/components/{component_id}/faults/{fault_id}",
        summary: "Read one fault.",
    },
    // operations (§7.14)
    PathEntry {
        method: "GET",
        path: "/components/{component_id}/operations",
        summary: "List operations (§7.14). Includes IO controls (C-133).",
    },
    PathEntry {
        method: "GET",
        path: "/components/{component_id}/operations/{operation_id}",
        summary: "Get operation detail (IO state for outputs).",
    },
    PathEntry {
        method: "POST",
        path: "/components/{component_id}/operations/{operation_id}/executions",
        summary: "Start an operation execution — 200/202 + Location.",
    },
    PathEntry {
        method: "GET",
        path: "/components/{component_id}/operations/{operation_id}/executions/{exec_id}",
        summary: "Poll an execution.",
    },
    PathEntry {
        method: "DELETE",
        path: "/components/{component_id}/operations/{operation_id}/executions/{exec_id}",
        summary: "Stop an execution — 204.",
    },
    // I/O controls share the /operations collection (ISO 17978-3 C-133).
    // updates (§7.13) — spec-compliant SW update wire (F.D2 thin alias).
    PathEntry {
        method: "GET",
        path: "/components/{component_id}/updates",
        summary: "List available SW updates (§7.13).",
    },
    PathEntry {
        method: "POST",
        path: "/components/{component_id}/updates",
        summary: "Register a new SW update — 201 + Location.",
    },
    PathEntry {
        method: "GET",
        path: "/components/{component_id}/updates/{update_id}",
        summary: "Update status (state, parts, manifest).",
    },
    PathEntry {
        method: "DELETE",
        path: "/components/{component_id}/updates/{update_id}",
        summary: "Abort update and discard staging — 204.",
    },
    PathEntry {
        method: "GET",
        path: "/components/{component_id}/updates/{update_id}/bulk-data",
        summary: "List uploaded parts.",
    },
    PathEntry {
        method: "POST",
        path: "/components/{component_id}/updates/{update_id}/bulk-data",
        summary: "Upload a part named by Content-Disposition (§7.20). 201 + Location + ETag.",
    },
    PathEntry {
        method: "PUT",
        path: "/components/{component_id}/updates/{update_id}/bulk-data/{part_id}",
        summary: "Upload a part (legacy alias of POST bulk-data). 201 + ETag.",
    },
    PathEntry {
        method: "GET",
        path: "/components/{component_id}/updates/{update_id}/bulk-data/{part_id}",
        summary: "Read an uploaded part back: the bytes as received, with Content-Type and ETag. 404 once it is no longer kept.",
    },
    PathEntry {
        method: "PUT",
        path: "/components/{component_id}/updates/{update_id}/execute",
        summary: EXECUTE_SUMMARY,
    },
    // Node-level updates (§7.18.1.1) — alias of the per-component collection;
    // a registration names its component in `target`.
    PathEntry {
        method: "GET",
        path: "/updates",
        summary: "List available SW updates (§7.13).",
    },
    PathEntry {
        method: "POST",
        path: "/updates",
        summary: "Register a new SW update — 201 + Location.",
    },
    PathEntry {
        method: "GET",
        path: "/updates/{update_id}",
        summary: "Update status (state, parts, manifest).",
    },
    PathEntry {
        method: "DELETE",
        path: "/updates/{update_id}",
        summary: "Abort update and discard staging — 204.",
    },
    PathEntry {
        method: "GET",
        path: "/updates/{update_id}/bulk-data",
        summary: "List uploaded parts.",
    },
    PathEntry {
        method: "POST",
        path: "/updates/{update_id}/bulk-data",
        summary: "Upload a part named by Content-Disposition (§7.20). 201 + Location + ETag.",
    },
    PathEntry {
        method: "PUT",
        path: "/updates/{update_id}/bulk-data/{part_id}",
        summary: "Upload a part (legacy alias of POST bulk-data). 201 + ETag.",
    },
    PathEntry {
        method: "GET",
        path: "/updates/{update_id}/bulk-data/{part_id}",
        summary: "Read an uploaded part back: the bytes as received, with Content-Type and ETag. 404 once it is no longer kept.",
    },
    PathEntry {
        method: "PUT",
        path: "/updates/{update_id}/execute",
        summary: EXECUTE_SUMMARY,
    },
    // logs (§7.21)
    PathEntry {
        method: "GET",
        path: "/components/{component_id}/logs",
        summary: "List logs.",
    },
    PathEntry {
        method: "GET",
        path: "/components/{component_id}/logs/entries",
        summary: "List log entries (§7.21).",
    },
    PathEntry {
        method: "GET",
        path: "/components/{component_id}/logs/config",
        summary: "Read log configuration.",
    },
    PathEntry {
        method: "PUT",
        path: "/components/{component_id}/logs/config",
        summary: "Set log configuration — 204.",
    },
    PathEntry {
        method: "DELETE",
        path: "/components/{component_id}/logs/config",
        summary: "Reset log configuration — 204.",
    },
    // bulk-data (§7.20)
    PathEntry {
        method: "GET",
        path: "/components/{component_id}/bulk-data",
        summary: "List bulk-data categories (§7.20).",
    },
    PathEntry {
        method: "GET",
        path: "/components/{component_id}/bulk-data/{category}",
        summary: "List bulk-data items; q: created-before/after.",
    },
    PathEntry {
        method: "GET",
        path: "/components/{component_id}/bulk-data/{category}/{bulk_data_id}",
        summary: "Download a bulk-data item (200/202/307).",
    },
    // scripts (§7.15) — developer-registered tests (discovery + execution wired)
    PathEntry {
        method: "GET",
        path: "/components/{component_id}/scripts",
        summary: "List registered tests (§7.15); q: tags.",
    },
    PathEntry {
        method: "GET",
        path: "/components/{component_id}/scripts/{script_id}",
        summary: "Read one registered test's metadata.",
    },
    PathEntry {
        method: "POST",
        path: "/components/{component_id}/scripts/{script_id}/executions",
        summary: "Run a registered test — 202 + Location.",
    },
    PathEntry {
        method: "GET",
        path: "/components/{component_id}/scripts/{script_id}/executions",
        summary: "List a test's execution ids.",
    },
    PathEntry {
        method: "GET",
        path: "/components/{component_id}/scripts/{script_id}/executions/{exec_id}",
        summary: "Poll a test run: status, verdict, log-cursor bracket.",
    },
    // configurations (§7.12)
    PathEntry {
        method: "GET",
        path: "/components/{component_id}/configurations",
        summary: "List configurations (§7.12.2); q: include-schema.",
    },
    PathEntry {
        method: "DELETE",
        path: "/components/{component_id}/configurations",
        summary: "Reset all configurations to default — 204.",
    },
    PathEntry {
        method: "GET",
        path: "/components/{component_id}/configurations/{configuration_id}",
        summary: "Read a configuration — JSON parameters or bulk (406 on Accept mismatch).",
    },
    PathEntry {
        method: "PUT",
        path: "/components/{component_id}/configurations/{configuration_id}",
        summary: "Write a configuration as a whole — 204.",
    },
    PathEntry {
        method: "DELETE",
        path: "/components/{component_id}/configurations/{configuration_id}",
        summary: "Reset one configuration to default — 204.",
    },
    // clear-data (§7.13)
    PathEntry {
        method: "GET",
        path: "/components/{component_id}/clear-data",
        summary: "List supported clear-data types (§7.13).",
    },
    PathEntry {
        method: "GET",
        path: "/components/{component_id}/clear-data/status",
        summary: "Read clear-data status.",
    },
    PathEntry {
        method: "PUT",
        path: "/components/{component_id}/clear-data/{action}",
        summary: "Run a clear-data action — 202.",
    },
    // cyclic-subscriptions (§7.10)
    PathEntry {
        method: "GET",
        path: "/components/{component_id}/cyclic-subscriptions",
        summary: "List cyclic subscriptions.",
    },
    PathEntry {
        method: "POST",
        path: "/components/{component_id}/cyclic-subscriptions",
        summary: "Create a cyclic subscription — 201 + Location.",
    },
    PathEntry {
        method: "GET",
        path: "/components/{component_id}/cyclic-subscriptions/{subscription_id}",
        summary: "Read a subscription.",
    },
    PathEntry {
        method: "PUT",
        path: "/components/{component_id}/cyclic-subscriptions/{subscription_id}",
        summary: "Update subscription cadence/duration.",
    },
    PathEntry {
        method: "DELETE",
        path: "/components/{component_id}/cyclic-subscriptions/{subscription_id}",
        summary: "Cancel a subscription — 204.",
    },
    // C-025: no `streams/{id}` entry — SSE delivery is the
    // `cyclic-subscriptions/{id}` resource itself (GET with
    // `Accept: text/event-stream`), not a separate non-standard name.
    // reset (§7.19)
    PathEntry {
        method: "PUT",
        path: "/components/{component_id}/status/restart",
        summary: "Restart the entity — 202 + Location.",
    },
    PathEntry {
        method: "GET",
        path: LEGACY_RESTART_EXECUTION,
        summary: "Poll restart status.",
    },
    // modes (§7.16)
    PathEntry {
        method: "GET",
        path: "/components/{component_id}/modes/session",
        summary: "Read session mode.",
    },
    PathEntry {
        method: "PUT",
        path: "/components/{component_id}/modes/session",
        summary: "Change session mode.",
    },
    PathEntry {
        method: "GET",
        path: "/components/{component_id}/modes/security",
        summary: "Read security mode.",
    },
    PathEntry {
        method: "PUT",
        path: "/components/{component_id}/modes/security",
        summary: "Request seed / send key (UDS 0x27).",
    },
    PathEntry {
        method: "GET",
        path: "/components/{component_id}/modes/comm-ctrl",
        summary: "Read communication-control mode.",
    },
    PathEntry {
        method: "PUT",
        path: "/components/{component_id}/modes/comm-ctrl",
        summary: "Set communication-control mode (UDS 0x28).",
    },
    PathEntry {
        method: "GET",
        path: "/components/{component_id}/modes/dtcsetting",
        summary: "Read DTC-setting mode.",
    },
    PathEntry {
        method: "PUT",
        path: "/components/{component_id}/modes/dtcsetting",
        summary: "Set DTC-setting mode on/off (UDS 0x85).",
    },
];

/// `PATHS` entries served at the server root rather than under a base.
const ROOT_PATHS: &[&str] = &["/health", "/version-info"];

/// The absolute path of a `PATHS` template under `tree`.
fn render_template(tree: Tree, template: &str) -> String {
    if ROOT_PATHS.contains(&template) {
        template.to_string()
    } else {
        format!("{}{template}", tree.base())
    }
}

/// GET {base}/docs — capability description (§7.5).
///
/// Curated OpenAPI 3.1.0 document built from the `PATHS` table above +
/// a small set of reusable schemas (`GenericError`, `Fault`,
/// `OperationExecution`, `CyclicSubscription`).  This route serves the
/// *global* doc (every path); per-entity scoped docs are served by
/// [`not_found_fallback`] on `{entity}/docs`.  A full path-walker that
/// introspects the axum router is a TODO — axum 0.8 doesn't expose its
/// routing table.
///
/// On `/v1` of a server configured with a node, the doc lists the node form
/// instead of the flat and root forms ([`add_node_paths`]).
pub async fn capability_description(
    State(state): State<AppState>,
    Extension(tree): Extension<Tree>,
) -> Json<serde_json::Value> {
    let mut doc = build_capability_doc_for(tree, None, state.capability_extensions());
    if tree == Tree::V1 && state.node_data().is_some() {
        add_node_data_paths(&mut doc);
    }
    if let Some(node) = state.node().filter(|_| tree == Tree::V1) {
        add_node_paths(&mut doc, node);
    }
    Json(doc)
}

/// The node-level data paths of a global `/v1` doc of a server with a
/// node-level data provider ([`AppState::with_node_data`]): `GET /v1/data`
/// and `GET /v1/data/{data_id}`, unless an embedder contributed its own.
fn add_node_data_paths(doc: &mut serde_json::Value) {
    let Some(paths) = doc.get_mut("paths").and_then(|p| p.as_object_mut()) else {
        return;
    };
    paths
        .entry(format!("{}/data", Tree::V1.base()))
        .or_insert_with(
            || serde_json::json!({ "get": operation("List the node-level data items.") }),
        );
    paths
        .entry(format!("{}/data/{{data_id}}", Tree::V1.base()))
        .or_insert_with(|| {
            serde_json::json!({
                "get": operation("Read a node-level data item (ReadValue); `?include-schema=true` adds its schema."),
                // ISO 17978-3 Table 23, as on the component's `/data/{param_id}`.
                "x-sovd-data-category": "x-multiple",
            })
        });
}

/// Build the `/vehicle/v1` OpenAPI 3.1.0 capability description (§7.5,
/// C-063) with no embedder extensions — a thin wrapper over
/// [`build_capability_doc_for`].
pub fn build_capability_doc(scope: Option<&str>) -> serde_json::Value {
    build_capability_doc_ext(scope, &CapabilityExtensions::default())
}

/// Build the `/vehicle/v1` capability description — [`build_capability_doc_for`]
/// with [`Tree::Legacy`].
pub fn build_capability_doc_ext(
    scope: Option<&str>,
    extensions: &CapabilityExtensions,
) -> serde_json::Value {
    build_capability_doc_for(Tree::Legacy, scope, extensions)
}

/// Build the OpenAPI 3.1.0 capability description (§7.5, C-063) for `tree`,
/// merging any embedder-contributed `extensions` (opaque to SOVDd — see
/// [`CapabilityExtensions`]).
///
/// `servers` is `tree`'s base and every `PATHS` template is rendered under
/// it ([`render_template`]). A contributed path under either base is rebased
/// onto `tree`; any other contributed path is kept verbatim. The `/v1`-only
/// `extensions.v1_paths` are merged likewise on [`Tree::V1`] only.
///
/// `scope == None` → the global document: every entry in `PATHS`.
///
/// `scope == Some(entity_path)` → the document scoped to that entity
/// path under `tree` (e.g. `/v1/components/vtx_ecm`).  Only `PATHS` whose
/// template is *at or under* that entity path are emitted, with the
/// concrete ids substituted in for the matched prefix.  The envelope
/// (`openapi`/`info`/`servers`/`components`) is identical in both modes.
///
/// Contributed `extensions.paths` are scoped by the SAME algorithm below (so
/// an entity-scoped doc carries only the contributed ops at or under that
/// entity); contributed `extensions.schemas` are merged verbatim into
/// `components/schemas`. Empty extensions reproduce the document byte-for-byte.
///
/// ## Scoping algorithm
///
/// Split `entity_path` into segments `E`.  For each `PathEntry`, split
/// its rendered template into segments `T`.  The entry is in-scope iff
/// `T.len() >= E.len()` and for every `i in 0..E.len()`, `T[i] == E[i]`
/// **or** `T[i]` is a `{param}` placeholder.  The emitted path is the
/// concrete `E[..]` prefix joined with the template tail `T[E.len()..]`
/// (placeholders in the tail are preserved).  E.g. template
/// `/v1/components/{component_id}/data/{param_id}` scoped to
/// `/v1/components/vtx_ecm` emits `/v1/components/vtx_ecm/data/{param_id}`.
pub fn build_capability_doc_for(
    tree: Tree,
    scope: Option<&str>,
    extensions: &CapabilityExtensions,
) -> serde_json::Value {
    // Pre-split the requested entity path (if any) into non-empty
    // segments.  Leading/trailing slashes drop out cleanly.
    let scope_segs: Vec<&str> = scope
        .map(|s| s.split('/').filter(|seg| !seg.is_empty()).collect())
        .unwrap_or_default();

    let mut paths = serde_json::Map::new();
    for entry in PATHS {
        // The restart execution stub is mounted on the legacy tree only.
        if tree == Tree::V1 && entry.path == LEGACY_RESTART_EXECUTION {
            continue;
        }
        if tree == Tree::Legacy && v1_only(entry) {
            continue;
        }
        let emitted = match emit_scoped_path(&render_template(tree, entry.path), &scope_segs) {
            Some(p) => p,
            None => continue, // out of scope for this entity path
        };
        let mut op = operation(entry.summary);
        // Public resources (§5.4.4) override the global bearer requirement
        // with an empty security set — they need no authentication.
        if matches!(entry.path, "/health" | "/version-info" | "/docs") {
            op.as_object_mut()
                .unwrap()
                .insert("security".to_string(), serde_json::json!([]));
        }
        let path_entry = paths
            .entry(emitted)
            .or_insert_with(|| serde_json::json!({}));
        let path_obj = path_entry.as_object_mut().unwrap();
        path_obj.insert(entry.method.to_ascii_lowercase(), op);
        // ISO 17978-3 Table 23: `x-sovd-data-category` is a Path Item Object
        // extension (sibling to the verbs). Best-effort per
        // `x_sovd_data_category_for` — see its C-024 note.
        if let Some(cat) = x_sovd_data_category_for(entry.path) {
            path_obj.insert(
                "x-sovd-data-category".to_string(),
                serde_json::Value::String(cat.to_string()),
            );
        }
    }

    // Merge embedder-contributed path items, scoped identically to native
    // paths, the `/v1`-only ones on `/v1` only. Their content is opaque here —
    // the embedder owns its meaning.
    let v1_paths = extensions.v1_paths.iter().filter(|_| tree == Tree::V1);
    for (template, item) in extensions.paths.iter().chain(v1_paths) {
        let template = match Tree::split_path(template) {
            Some((_, rest)) => format!("{}{rest}", tree.base()),
            None => template.clone(),
        };
        let Some(emitted) = emit_scoped_path(&template, &scope_segs) else {
            continue;
        };
        let slot = paths
            .entry(emitted)
            .or_insert_with(|| serde_json::json!({}));
        if let (Some(dst), Some(src)) = (slot.as_object_mut(), item.as_object()) {
            for (k, v) in src {
                dst.insert(k.clone(), v.clone());
            }
        }
    }

    let mut doc = serde_json::json!({
        "openapi": "3.1.0",
        "info": {
            "title": "SOVDd",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "SOVD (ISO 17978-3) diagnostic server.",
            "x-sovd-version": "1.1",
        },
        "servers": [
            {"url": tree.base()},
        ],
        "security": [{ "bearerAuth": [] }],
        "paths": paths,
        "components": {
            "securitySchemes": {
                "bearerAuth": {
                    "type": "http",
                    "scheme": "bearer",
                    "bearerFormat": "JWT",
                    "description": "ISO 17978-3 §5.4.4 — bearer JWT in the Authorization header. When [server.auth] is configured, the server validates the token against trusted OIDC issuers (signature/aud/iss/exp) and authorizes per-component via `component:<id>` / `component:*` scopes; public resources (health, version-info, docs, .well-known) need none. Obtain a token from your OIDC provider's authorize/token endpoints (§7.23 — see the issuer's /.well-known/openid-configuration). C-030/031/032.",
                },
            },
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
                    "required": ["code", "fault_name", "severity", "href"],
                    "properties": {
                        "code": { "type": "string" },
                        "fault_name": { "type": "string" },
                        "severity": { "type": "integer", "minimum": 1, "maximum": 4, "description": "1=FATAL, 2=ERROR, 3=WARN, 4=INFO." },
                        "scope": { "type": "string" },
                        "display_code": { "type": "string" },
                        "symptom": { "type": "string" },
                        "fault_translation_id": { "type": "string" },
                        "symptom_translation_id": { "type": "string" },
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
    });

    // Merge embedder-contributed schemas into `components/schemas` (verbatim).
    if !extensions.schemas.is_empty() {
        if let Some(schemas) = doc
            .get_mut("components")
            .and_then(|c| c.get_mut("schemas"))
            .and_then(|s| s.as_object_mut())
        {
            for (k, v) in &extensions.schemas {
                schemas.insert(k.clone(), v.clone());
            }
        }
    }

    doc
}

/// The Operation Object of a path the doc lists, summarized by `summary`.
fn operation(summary: &str) -> serde_json::Value {
    serde_json::json!({
        "summary": summary,
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
    })
}

/// The served paths of a global `/v1` doc of a server configured with `node`
/// (see [`crate::topology`]): the node's document, subcomponents listing and
/// `data` and `operations` collections, and every path [`to_node_forms`] maps.
fn add_node_paths(doc: &mut serde_json::Value, node: &NodeTopology) {
    to_node_forms(doc, node);
    let Some(paths) = doc.get_mut("paths").and_then(|p| p.as_object_mut()) else {
        return;
    };
    paths.insert(
        node.href(),
        serde_json::json!({ "get": operation("Read the node's document (its node-level resources and subcomponents).") }),
    );
    paths.insert(
        node.subcomponents_href(),
        serde_json::json!({ "get": operation("List the node's subcomponents.") }),
    );
    // The node-level collections, unless an embedder contributed its own.
    for (collection, summary) in [
        ("data", "List the node-level data items."),
        ("operations", "List the node-level operations."),
    ] {
        paths
            .entry(format!("{}/{collection}", node.href()))
            .or_insert_with(|| serde_json::json!({ "get": operation(summary) }));
    }
}

/// Every path of a `/v1` doc of a server configured with `node` that has a
/// node form (a backend's `/v1/components/{component_id}/...`, a node-level
/// `/v1/updates/...`, ..., embedder-contributed ones included) under that node
/// form, the only one served.
fn to_node_forms(doc: &mut serde_json::Value, node: &NodeTopology) {
    let Some(paths) = doc.get_mut("paths").and_then(|p| p.as_object_mut()) else {
        return;
    };
    *paths = std::mem::take(paths)
        .into_iter()
        .map(|(path, item)| (node.node_form(&path).unwrap_or(path), item))
        .collect();
}

/// Compute the emitted OpenAPI path for `template` under `scope_segs`.
///
/// Returns `Some(path)` if the template is in-scope per the algorithm
/// documented on [`build_capability_doc`], or `None` if it's out of
/// scope.  When `scope_segs` is empty (global doc) every template is in
/// scope and emitted verbatim.
fn emit_scoped_path(template: &str, scope_segs: &[&str]) -> Option<String> {
    if scope_segs.is_empty() {
        // Global doc — emit the template verbatim (today's behaviour).
        return Some(template.to_string());
    }
    let tmpl_segs: Vec<&str> = template.split('/').filter(|s| !s.is_empty()).collect();
    // The template must be at least as deep as the entity path …
    if tmpl_segs.len() < scope_segs.len() {
        return None;
    }
    // … and every prefix segment must match literally or be a `{param}`.
    for (i, want) in scope_segs.iter().enumerate() {
        let have = tmpl_segs[i];
        let is_placeholder = have.starts_with('{') && have.ends_with('}');
        if !is_placeholder && have != *want {
            return None;
        }
    }
    // Emit: concrete entity prefix + the template tail (placeholders in
    // the tail are preserved verbatim).
    let mut out = String::new();
    for seg in scope_segs {
        out.push('/');
        out.push_str(seg);
    }
    for seg in &tmpl_segs[scope_segs.len()..] {
        out.push('/');
        out.push_str(seg);
    }
    Some(out)
}

/// `GET /.well-known/sovd-extensions` — discovery doc listing the
/// vendor extensions this server adds to the spec wire.  Lets
/// conformance scanners enumerate documented deviations rather than
/// flagging them as unknown surface.  See
/// `tasks/spec-aligned-updates-wire.md` §4.1.
///
/// The route is root-level (no tree), so the paths below are written under a
/// `{base}` placeholder, standing for each of the listed `base_uris`.
pub async fn sovd_extensions() -> Json<serde_json::Value> {
    let base_uris: Vec<&str> = Tree::ALL.iter().map(|tree| tree.base()).collect();
    Json(serde_json::json!({
        "vendor": "sumo",
        "base_uris": base_uris,
        "extensions": {
            "x-ota-control": {
                "kind":   "query-param + lifecycle verbs",
                "where":  "PUT {base}/components/{id}/updates/{update_id}/execute",
                "values": ["orchestrated"],
                "implied_on": ["/v1"],
                "verbs": [
                    "PUT {base}/components/{id}/updates/{update_id}/x-ota-commit",
                    "PUT {base}/components/{id}/updates/{update_id}/x-ota-rollback",
                    "PUT {base}/components/{id}/x-ota-force-rollback"
                ],
                "fields": ["x-ota-substate"],
                "spec":   "tasks/spec-aligned-updates-wire.md sec 2.2",
                "summary": "Execute-phase control for orchestrators \
                            that drive the trial verdict (commit / \
                            rollback) out-of-band. Implied on /v1, \
                            where every execute is orchestrated: the \
                            parameter may be left out, and any other \
                            value answers 400 incomplete-request \
                            (opt-in on the deprecated /vehicle/v1 tree). \
                            x-ota-force-rollback unconditionally clears \
                            stuck backend trial state when no in-flight \
                            update_id exists."
            },
            "x-update": {
                "kind":   "lifecycle verb + status member + capability flag",
                "verbs": [
                    "PUT {base}/components/{id}/updates/{update_id}/x-update-commit",
                    "PUT {base}/components/{id}/updates/{update_id}/x-update-rollback",
                    "POST {base}/operations/x-update-commit/executions (served by the embedding machine manager)",
                    "POST {base}/operations/x-update-rollback/executions (served by the embedding machine manager)"
                ],
                "fields": ["x-update.state", "x-update.reset_kind", "capabilities.x-update.verdict"],
                "spec":   "docs/x-update.md",
                "summary": "Aliases of the x-ota verdict surface: explicit \
                            commit and rollback verbs, verb-named \
                            states, and whether a verdict is awaited is an \
                            executor property advertised in capabilities, \
                            not a registration option."
            },
            "x-ota-bulk-data": {
                "kind":      "sub-resource",
                "endpoints": [
                    "POST {base}/components/{id}/updates/{update_id}/bulk-data",
                    "PUT {base}/components/{id}/updates/{update_id}/bulk-data/{part_id}",
                    "GET {base}/components/{id}/updates/{update_id}/bulk-data"
                ],
                "spec":      "tasks/spec-aligned-updates-wire.md sec 2.3",
                "summary": "Client streams update bytes to the server. \
                            Spec model assumes server-pulls-from-OTA \
                            backend; bulk-data is the reverse channel \
                            for workstation / workshop deployments."
            },
            "x-multiple": {
                "kind":  "value token",
                "where": "x-sovd-data-category on the templated \
                          /data/{param_id} path item in the capability \
                          description (GET {base}/docs)",
                "summary": "Placeholder category meaning 'varies per DID' — \
                            the shared path template covers DIDs of \
                            different categories; read the precise \
                            category per parameter from GET .../data."
            }
        }
    }))
}
