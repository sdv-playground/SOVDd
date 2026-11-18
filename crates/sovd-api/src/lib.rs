//! sovd-api - SOVD REST API layer with generic handlers
//!
//! This crate provides the HTTP API layer that uses the DiagnosticBackend trait
//! to serve SOVD endpoints. It is backend-agnostic.
//!
//! # Usage
//!
//! ```ignore
//! use sovd_api::{create_router, AppState};
//! use sovd_uds::UdsBackend;
//!
//! let backend = UdsBackend::new(config).await?;
//! let state = AppState::new(backend);
//! let router = create_router(state);
//! ```

pub mod auth;
pub mod error;
pub mod handlers;
pub mod state;
pub mod topology;
pub mod workshop_ca;

pub use auth::{
    AccessRequest, AuthConfig, AuthContext, AuthError, AuthMode, Authorizer, Capability,
    ClientContext, IssuerConfig,
};
pub use error::ApiError;
pub use state::{AppState, CapabilityExtensions};
pub use topology::{with_node_topology, NodeTopology};

// Re-export DidStore from sovd-conv for convenience
pub use sovd_conv::{DataType, DidDefinition, DidStore};

use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post, put};
use axum::Router;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

/// The base path ("tree") an entity-API request came in on. Both bases serve
/// the identical entity API over one shared [`AppState`]; the router tags each
/// request with its `Tree` as an `axum::Extension`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tree {
    /// `/v1` — the spec base (ISO 17978-3 §5.6), advertised first.
    V1,
    /// `/vehicle/v1` — the original base, kept for existing clients.
    Legacy,
}

impl Tree {
    /// Every mounted tree, in advertised (`/version-info`) order.
    pub const ALL: [Tree; 2] = [Tree::V1, Tree::Legacy];

    /// The base path this tree is mounted under.
    pub const fn base(self) -> &'static str {
        match self {
            Tree::V1 => "/v1",
            Tree::Legacy => "/vehicle/v1",
        }
    }

    /// `{base}/components/{id}` — a component's URL under this tree; while a
    /// request is served in node mode ([`topology`]), a backend's URL is its
    /// subcomponent URL, `{base}/components/{node}/subcomponents/{id}`, and the
    /// node's own id stays `{base}/components/{node}`.
    pub fn component(self, id: &str) -> String {
        match topology::current() {
            Some(node) if node.id() != id => node.subcomponent_href(self, id),
            _ => format!("{}/components/{id}", self.base()),
        }
    }

    /// `{base}/components/{component_id}/apps/{app_id}` — a sub-entity's URL
    /// under this tree.
    pub fn app(self, component_id: &str, app_id: &str) -> String {
        format!("{}/apps/{app_id}", self.component(component_id))
    }

    /// Longest-match: returns the tree and the remainder (starting with '/')
    /// for a path under a base. `None` for the bare base and other paths.
    pub fn split_path(path: &str) -> Option<(Tree, &str)> {
        // Longest base first, so a future base nested in another can't shadow it.
        [Tree::Legacy, Tree::V1].into_iter().find_map(|tree| {
            let rest = path.strip_prefix(tree.base())?;
            rest.starts_with('/').then_some((tree, rest))
        })
    }
}

/// Create the SOVD REST API router with the given application state
pub fn create_router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        // Health check
        .route("/health", get(|| async { "OK" }))
        // Spec §7.4 — version-info is version-INDEPENDENT (constant
        // path across all API editions) and lists every version this
        // server serves.  C-005 mandatory.
        .route("/version-info", get(handlers::meta::version_info))
        // Vendor-extension discovery (Phase B).  Conformance scanners
        // enumerate documented deviations here rather than flag them
        // as unknown surface.  tasks/spec-aligned-updates-wire.md §4.1.
        //
        // C-025 scope note: `.well-known/sovd-extensions` is NOT an
        // entity-resource collection from Tables 8/10 — it's a
        // server-level resource published under the RFC 8615
        // (`/.well-known/`) standard mechanism, off the
        // `/vehicle/v1/components/{id}/…` entity tree. C-025 governs the
        // collection/resource names that hang off entities; this is
        // outside that surface, so it does not violate it.
        .route(
            "/.well-known/sovd-extensions",
            get(handlers::meta::sovd_extensions),
        )
        // Admin routes - DID definitions management.
        //
        // C-025 scope note: `/admin/*` is a server administration API,
        // NOT an entity-resource collection from Tables 8/10. It is
        // rooted off the `/vehicle/v1/components/{id}/…` entity tree (no
        // `{entity-path}` prefix) and manages server-global DID
        // definitions, not a resource of any one entity. C-025 constrains
        // the names that appear *on entities*; the admin surface is
        // outside that scope (and is gated by an `admin:*` scope under
        // C-030). So `admin`/`definitions` here is not a C-025 violation.
        .route(
            "/admin/definitions",
            get(handlers::definitions::list_definitions)
                .post(handlers::definitions::upload_definitions)
                .delete(handlers::definitions::clear_definitions),
        )
        .route(
            "/admin/definitions/{did}",
            get(handlers::definitions::get_definition)
                .put(handlers::definitions::put_definition)
                .delete(handlers::definitions::delete_definition),
        )
        // The entity API, mounted under every base path (`Tree::ALL`).
        .merge(api_routes(Tree::V1))
        .merge(api_routes(Tree::Legacy))
        // Fallback for unknown paths / methods so the error body
        // matches the spec `GenericError` shape (axum's defaults are
        // plain text otherwise).
        .fallback(handlers::meta::not_found_fallback)
        .method_not_allowed_fallback(handlers::meta::method_not_allowed_fallback)
        // Middleware (request order, outermost first: cors → trace → auth → body-limit)
        .layer(DefaultBodyLimit::disable()) // SOVD streaming uploads (ASAM SOVD chunked transfer)
        // Client→SOVDd JWT-bearer auth (ISO 17978-3 C-030/C-032). Public
        // resources + CORS preflight pass through; see `auth::require_auth`.
        // No-op when `[server.auth]` is absent/disabled (open surface).
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_auth,
        ))
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .with_state(state)
}

/// The entity API routes (capability doc + `components/...` tree) mounted
/// under `tree`'s base path. Built per base rather than via `Router::nest`
/// so the URI keeps its full original path — `auth::require_auth` and the
/// router fallbacks see `/v1/...` / `/vehicle/v1/...` unchanged. Every
/// request routed here carries its [`Tree`] as an `axum::Extension`.
fn api_routes(tree: Tree) -> Router<AppState> {
    Router::new()
        // Spec §7.5 — capability description (online OpenAPI doc).
        // This route serves the GLOBAL doc (every path).  Per-entity
        // scoped `{path}/docs` (C-063) is served by the router fallback
        // (`not_found_fallback`), since axum's `{*wildcard}` can't be a
        // non-final segment to express `/{*path}/docs` as a real route.
        .route(
            &format!("{}{}", tree.base(), "/docs"),
            get(handlers::meta::capability_description),
        )
        // §7.6.3 Table 53 — the SOVD server's own capability document,
        // served on both `{base}/` and `{base}`.
        .route(
            &format!("{}{}", tree.base(), "/"),
            get(handlers::root::get_root),
        )
        .route(tree.base(), get(handlers::root::get_root))
        // Component routes
        .route(
            &format!("{}{}", tree.base(), "/components"),
            get(handlers::components::list_components),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}"),
            get(handlers::components::get_component),
        )
        // A node's subcomponents (node topology; 404 otherwise).
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/subcomponents"),
            get(handlers::components::list_subcomponents),
        )
        // Data routes
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/data"),
            get(handlers::data::list_parameters),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/data/{param_id}"),
            get(handlers::data::read_parameter).put(handlers::data::write_parameter),
        )
        // Child-ECU data behind a gateway is addressed via the sub-entity
        // path (`/apps/{child}/data/{param}`), NOT a flat
        // `/data/{child}/{param}` route.  The dedicated flat gateway
        // routes were retired for C-021 (one canonical data-addressing
        // path); see handlers::sub_entity.
        // Raw DID access via the spec `?raw=true` query on the standard
        // data parameter route (ISO 17978-3 §7.10). Hex DID strings like
        // "F405" resolve through DidStore the same as semantic names; raw
        // bytes come back when the caller passes `?raw=true`.
        // Fault routes
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/faults"),
            get(handlers::faults::list_faults).delete(handlers::faults::clear_faults),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/faults/{fault_id}"),
            get(handlers::faults::get_fault).delete(handlers::faults::delete_fault),
        )
        // Active-only DTCs are exposed via the spec faults filter:
        //   GET /faults?active_only=true
        // No dedicated /dtcs route — kept the codebase one collection
        // shorter (ISO 17978-3 §5.3.6).
        // Dynamic data lists — ISO 17978-3 §5.3.6 (`data-lists` collection)
        // + §7.14 (`operations.executions` for defining new lists). The UDS
        // 0x2C define/clear flow maps onto:
        //   POST /operations/define-data/executions  → 0x2C 0x02 (define)
        //   GET  /data-lists                         → list defined DDIDs
        //   GET  /data-lists/{list_id}               → 0x22 read of DDID
        //   DELETE /data-lists/{list_id}             → 0x2C 0x03 (clear)
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/operations/define-data/executions"),
            post(handlers::data_lists::define_data),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/data-lists"),
            get(handlers::data_lists::list_data_lists),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/data-lists/{list_id}"),
            get(handlers::data_lists::read_data_list).delete(handlers::data_lists::clear_data_list),
        )
        // Log routes (primarily for HPC and message passing) +
        // spec §7.21 logs/entries + logs/config sub-resources.
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/logs"),
            get(handlers::logs::get_logs),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/logs/entries"),
            get(handlers::logs_ext::list_log_entries),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/logs/config"),
            get(handlers::logs_ext::get_log_config)
                .put(handlers::logs_ext::put_log_config)
                .delete(handlers::logs_ext::reset_log_config),
        )
        // Per-source model (vendor extension): the source CATALOG + one source's entries.
        // Static two-segment `/logs/sources` + three-segment `/logs/sources/{name}`
        // — matchit gives statics priority over the `/logs/{log_id}` catch-all,
        // and the 3-segment form can't collide with the 2-segment {log_id} anyway.
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/logs/sources"),
            get(handlers::logs_ext::list_log_sources),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/logs/sources/{source}"),
            get(handlers::logs::get_source_logs),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/logs/{log_id}"),
            get(handlers::logs::get_log).delete(handlers::logs::delete_log),
        )
        // §7.20 bulk-data collection (C-120): the spec-native large-payload
        // download path. Logs are its first consumer (a `logs` category).
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/bulk-data"),
            get(handlers::bulk_data::list_categories),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/bulk-data/{category}"),
            get(handlers::bulk_data::list_items),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/bulk-data/{category}/{bulk_data_id}"),
            get(handlers::bulk_data::download),
        )
        // §7.12 configurations — read/write/reset an entity's configuration
        // resources. No POST: the spec defines no way to *create* one.
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/configurations"),
            get(handlers::configurations::list_configurations)
                .delete(handlers::configurations::reset_configurations),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/configurations/{configuration_id}"),
            get(handlers::configurations::read_configuration)
                .put(handlers::configurations::write_configuration)
                .delete(handlers::configurations::reset_configuration),
        )
        // -----------------------------------------------------------------
        // F.5 stub collections (spec presence, backend wiring TODO).
        // -----------------------------------------------------------------
        // §7.17 locks
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/locks"),
            get(handlers::stubs::list_locks).post(handlers::stubs::acquire_lock),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/locks/{lock_id}"),
            get(handlers::stubs::read_lock)
                .put(handlers::stubs::extend_or_break_lock)
                .delete(handlers::stubs::release_lock),
        )
        // §7.11 triggers
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/triggers"),
            get(handlers::stubs::list_triggers).post(handlers::stubs::create_trigger),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/triggers/{trigger_id}"),
            get(handlers::stubs::read_trigger).delete(handlers::stubs::delete_trigger),
        )
        // §7.22 communication-logs
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/communication-logs"),
            get(handlers::stubs::list_communication_logs)
                .post(handlers::stubs::create_communication_log),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/communication-logs/{communication_log_id}"),
            get(handlers::stubs::read_communication_log)
                .put(handlers::stubs::control_communication_log)
                .delete(handlers::stubs::delete_communication_log),
        )
        // §7.15 scripts — used for developer-registered TESTS. Discovery
        // (list/read) + execution (execute/executions/status) both wired to the
        // backend — see handlers::scripts. A test run = a script execution,
        // async 202 + Location + poll (mirrors §7.14 operations).
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/scripts"),
            get(handlers::scripts::list_scripts),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/scripts/{script_id}"),
            get(handlers::scripts::read_script),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/scripts/{script_id}/executions"),
            post(handlers::scripts::execute_script).get(handlers::scripts::list_executions),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/scripts/{script_id}/executions/{exec_id}"),
            get(handlers::scripts::get_execution).put(handlers::scripts::terminate_execution),
        )
        // §7.9 diagnostics — READ-ONLY system probes (mem/df/du/…), proxied from
        // a guest's diag-agent. Discovery (list, +?tags=) + gather-one; both
        // default on the trait, so a backend with no diag surface serves empty /
        // not-supported. See handlers::diagnostics + tasks/diag-agent-design.md.
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/diagnostics"),
            get(handlers::diagnostics::list_diagnostics),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/diagnostics/{probe_id}"),
            get(handlers::diagnostics::read_diagnostic),
        )
        // §7.9 data-categories (real, DID-derived) + Table 9 data-groups stub
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/data-categories"),
            get(handlers::data::list_data_categories),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/data-groups"),
            get(handlers::stubs::list_data_groups),
        )
        // §7.16 modes/comm-ctrl + modes/dtcsetting — UDS CommunicationControl
        // (0x28) + ControlDTCSetting (0x85), mapped exactly per ISO 17978-3
        // Table 343 (C-130). The former `communication-control`/`dtc-setting`
        // names are gone (now 404).
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/modes/comm-ctrl"),
            get(handlers::modes::get_comm_control_mode)
                .put(handlers::modes::put_comm_control_mode),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/modes/dtcsetting"),
            get(handlers::modes::get_dtc_setting_mode).put(handlers::modes::put_dtc_setting_mode),
        )
        // Spec §7.13 clear-data collection — stub today.
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/clear-data"),
            get(handlers::clear_data::list_clear_data_types),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/clear-data/status"),
            get(handlers::clear_data::clear_data_status),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/clear-data/{action}"),
            put(handlers::clear_data::clear_data_action),
        )
        // Operation routes — ISO 17978-3 §7.14 executions sub-resource.
        // The collection now serves both UDS RoutineControl (0x31) and
        // InputOutputControl (0x2F) per C-133.
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/operations"),
            get(handlers::operations::list_operations),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/operations/{operation_id}"),
            get(handlers::operations::get_operation),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/operations/{operation_id}/executions"),
            post(handlers::operations::start_operation_execution),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/operations/{operation_id}/executions/{exec_id}"),
            get(handlers::operations::get_operation_execution)
                .delete(handlers::operations::stop_operation_execution),
        )
        // Legacy /outputs routes removed in Phase F.7.  IO control
        // (UDS 0x2F) lives under /operations per C-133.
        // Sub-entity routes (apps/containers for HPC)
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/apps"),
            get(handlers::apps::list_apps),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/apps/{app_id}"),
            get(handlers::apps::get_app),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/apps/{app_id}/apps"),
            get(handlers::apps::list_sub_entity_apps),
        )
        // F.D8b: sub-entity /apps/{app_id}/files + /apps/{app_id}/flash
        // routes retired along with their entity-root counterparts.
        // No tests + no known callers used the sub-entity form, and
        // the /updates collection isn't yet plumbed under /apps; when
        // a real sub-entity OTA need shows up, /apps/{app_id}/updates
        // will be added per the F.D2 wire shape.
        // Sub-entity ECU reset — same shape as the entity-root form
        // (PUT status/restart returns 202 + Location; GET on the
        // exec sub-resource is the stateless `completed` stub).
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/apps/{app_id}/status/restart"),
            put(handlers::sub_entity::status_restart),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/apps/{app_id}/status/restart/{exec_id}"),
            get(handlers::sub_entity::status_restart_execution),
        )
        // Sub-entity mode routes
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/apps/{app_id}/modes/session"),
            get(handlers::sub_entity::get_session_mode).put(handlers::sub_entity::put_session_mode),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/apps/{app_id}/modes/security"),
            get(handlers::sub_entity::get_security_mode)
                .put(handlers::sub_entity::put_security_mode),
        )
        // Sub-entity data routes
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/apps/{app_id}/data"),
            get(handlers::sub_entity::list_sub_entity_parameters),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/apps/{app_id}/data/{param_id}"),
            get(handlers::sub_entity::read_sub_entity_parameter)
                .put(handlers::sub_entity::write_sub_entity_parameter),
        )
        // Sub-entity configuration routes — §7.12 under §6.5, same shape as
        // the entity-root form.
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/apps/{app_id}/configurations"),
            get(handlers::configurations::list_sub_entity_configurations)
                .delete(handlers::configurations::reset_sub_entity_configurations),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/apps/{app_id}/configurations/{configuration_id}"),
            get(handlers::configurations::read_sub_entity_configuration)
                .put(handlers::configurations::write_sub_entity_configuration)
                .delete(handlers::configurations::reset_sub_entity_configuration),
        )
        // Sub-entity fault routes
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/apps/{app_id}/faults"),
            get(handlers::sub_entity::list_sub_entity_faults)
                .delete(handlers::sub_entity::clear_sub_entity_faults),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/apps/{app_id}/faults/{fault_id}"),
            get(handlers::sub_entity::get_sub_entity_fault),
        )
        // Sub-entity operation routes — same executions sub-resource
        // pattern as the entity-root operations (§7.14).
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/apps/{app_id}/operations"),
            get(handlers::sub_entity::list_sub_entity_operations),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/apps/{app_id}/operations/{operation_id}/executions"),
            post(handlers::sub_entity::start_sub_entity_operation),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/apps/{app_id}/operations/{operation_id}/executions/{exec_id}"),
            get(handlers::sub_entity::get_sub_entity_operation_execution)
                .delete(handlers::sub_entity::stop_sub_entity_operation_execution),
        )
        // Cyclic subscriptions — ISO 17978-3 §7.10. The subscription
        // resource itself is component-scoped (one `resource` per sub).
        // Per §7.10.3 the temporary subscription resource IS the SSE
        // stream: GET it with `Accept: text/event-stream` for the event
        // stream, or without for the subscription details (content
        // negotiation in `get_cyclic_subscription`). There is no separate
        // `streams` resource — `streams` is not a standardized name
        // (C-025); both the inline-`?parameters=` reader and the cyclic
        // `streams/{id}` delivery URL were retired.
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/cyclic-subscriptions"),
            get(handlers::subscriptions::list_cyclic_subscriptions)
                .post(handlers::subscriptions::create_cyclic_subscription),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/cyclic-subscriptions/{subscription_id}"),
            get(handlers::subscriptions::get_cyclic_subscription)
                .put(handlers::subscriptions::update_cyclic_subscription)
                .delete(handlers::subscriptions::delete_cyclic_subscription),
        )
        // ECU Reset — ISO 17978-3 §7.19. PUT returns 202 + Location to
        // the status sub-resource; the GET on the sub-resource is a stub
        // that reads `completed` (reset is fire-and-forget, the ECU is
        // rebooting by the time we'd report progress).
        // §7.19.2 — read an entity's runtime status (EntityStatus ready/notReady
        // + control links + vendor x-<vendor>-* runtime fields). Orchestrators read
        // this to verify a reset actually took effect (boot counter incremented).
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/status"),
            get(handlers::reset::status_read),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/status/restart"),
            put(handlers::reset::status_restart),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/status/restart/{exec_id}"),
            get(handlers::reset::status_restart_execution),
        )
        // Mode routes (session, security).  Per ISO 17978-3 Table 343 /
        // C-130 the UDS service→mode mapping covers session (0x10),
        // security (0x27/0x29), comm-ctrl (0x28) and dtcsetting
        // (0x85). LinkControl (0x87) has NO standardized mode name — it
        // is "not represented" — so the former `modes/link` route was
        // dropped for C-025 (only standardized mode names on the entity)
        // and C-130 (no special UDS methods).
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/modes/session"),
            get(handlers::modes::get_session_mode).put(handlers::modes::put_session_mode),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/modes/security"),
            get(handlers::modes::get_security_mode).put(handlers::modes::put_security_mode),
        )
        // C-025: no `POST /vehicle/v1/discovery`. Bus discovery is not a
        // SOVD entity resource; clients enumerate the entity tree via
        // `GET /vehicle/v1/components`. The handler + module were removed.
        // /flash + /files retired in F.D8b — sovd-client::FlashClient
        // routes via /updates internally now.  See
        // commit history if you need the legacy handlers.
        // Spec-compliant `/updates` collection — F.D2 thin alias over
        // the existing flash backend.  ISO 17978-3 §7.13.  Multipart-
        // inline transport (§3.1 of the SW-update design doc).
        // URL-referenced manifests land in F.D7.  /flash + /files
        // remain wired for now; they retire at F.D8.
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/updates"),
            post(handlers::updates::register_update).get(handlers::updates::list_updates),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/updates/{update_id}"),
            get(handlers::updates::get_update).delete(handlers::updates::delete_update),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/updates/{update_id}/bulk-data"),
            get(handlers::updates::list_bulk_data).post(handlers::updates::post_bulk_data_part),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/updates/{update_id}/bulk-data/{part_id}"),
            put(handlers::updates::put_bulk_data_part).get(handlers::updates::get_bulk_data_part),
        )
        // ISO 17978-3 §7.18 spec verbs — async 202 + Location :: /status.
        // The F.D8b vendor-extension `/executions{action}` wire (deprecated
        // through Phase A–D) was removed in Phase E.  Callers use
        // PUT prepare / execute / x-ota-commit / x-ota-rollback /
        // x-ota-force-rollback as appropriate.
        // and stays alive (Deprecation header) for the migration window.
        // tasks/spec-aligned-updates-wire.md UPDATE-WIRE-001.
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/updates/{update_id}/prepare"),
            put(handlers::updates::put_prepare),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/updates/{update_id}/execute"),
            put(handlers::updates::put_execute),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/updates/{update_id}/automated"),
            put(handlers::updates::put_automated),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/updates/{update_id}/status"),
            get(handlers::updates::get_status),
        )
        // Phase B — orchestrated-mode vendor verbs.  Only meaningful
        // for entries that ran `PUT /execute?x-ota-control=orchestrated`
        // and are paused at `substate=awaiting-verdict`.  See
        // tasks/spec-aligned-updates-wire.md §2.2.
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/updates/{update_id}/x-ota-commit"),
            put(handlers::updates::put_x_ota_commit),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/updates/{update_id}/x-ota-rollback"),
            put(handlers::updates::put_x_ota_rollback),
        )
        // x-update aliases of the verdict verbs (same semantics as
        // x-ota-commit / x-ota-rollback).
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/updates/{update_id}/x-update-commit"),
            put(handlers::updates::put_x_update_commit),
        )
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/updates/{update_id}/x-update-rollback"),
            put(handlers::updates::put_x_update_rollback),
        )
        // Unconditional trial-state clear — separate from x-ota-rollback
        // (which is the orchestrator's verdict on an awaiting-verdict
        // entry).  Used to unstick a previous flash that left the
        // backend in trial without an active execute task.  Lives at
        // the component root because by definition no /updates entry
        // exists for the stuck trial.
        .route(
            &format!("{}{}", tree.base(), "/components/{component_id}/x-ota-force-rollback"),
            put(handlers::updates::put_x_ota_force_rollback),
        )
        // Node-level `/updates` collection — ISO 17978-3 §7.18.1.1 names the
        // server root its primary location. Aliases the per-component
        // collections above (registration names its component in `target`);
        // no `x-ota-force-rollback`, which is component-rooted. See
        // docs/x-update.md "Node-level collection".
        .route(
            &format!("{}{}", tree.base(), "/updates"),
            post(handlers::updates::register_node_update).get(handlers::updates::list_node_updates),
        )
        .route(
            &format!("{}{}", tree.base(), "/updates/{update_id}"),
            get(handlers::updates::get_node_update).delete(handlers::updates::delete_node_update),
        )
        .route(
            &format!("{}{}", tree.base(), "/updates/{update_id}/bulk-data"),
            get(handlers::updates::list_node_bulk_data)
                .post(handlers::updates::post_node_bulk_data_part),
        )
        .route(
            &format!("{}{}", tree.base(), "/updates/{update_id}/bulk-data/{part_id}"),
            put(handlers::updates::put_node_bulk_data_part)
                .get(handlers::updates::get_node_bulk_data_part),
        )
        .route(
            &format!("{}{}", tree.base(), "/updates/{update_id}/prepare"),
            put(handlers::updates::put_node_prepare),
        )
        .route(
            &format!("{}{}", tree.base(), "/updates/{update_id}/execute"),
            put(handlers::updates::put_node_execute),
        )
        .route(
            &format!("{}{}", tree.base(), "/updates/{update_id}/automated"),
            put(handlers::updates::put_node_automated),
        )
        .route(
            &format!("{}{}", tree.base(), "/updates/{update_id}/status"),
            get(handlers::updates::get_node_status),
        )
        .route(
            &format!("{}{}", tree.base(), "/updates/{update_id}/x-ota-commit"),
            put(handlers::updates::put_node_x_ota_commit),
        )
        .route(
            &format!("{}{}", tree.base(), "/updates/{update_id}/x-ota-rollback"),
            put(handlers::updates::put_node_x_ota_rollback),
        )
        .route(
            &format!("{}{}", tree.base(), "/updates/{update_id}/x-update-commit"),
            put(handlers::updates::put_node_x_update_commit),
        )
        .route(
            &format!("{}{}", tree.base(), "/updates/{update_id}/x-update-rollback"),
            put(handlers::updates::put_node_x_update_rollback),
        )
        .layer(axum::Extension(tree))
}

// F.D8b: `legacy_flash_files_router` + the SetResponseHeaderLayer
// stack it carried (Deprecation / Sunset / Link → successor-version
// per RFC 8594 + RFC 9745) were retired.  See git history for the
// transitional shape.  sovd-client::FlashClient routes through
// /updates now and these legacy URL paths no longer
// exist.
