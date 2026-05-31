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

pub mod error;
pub mod handlers;
pub mod state;

pub use error::ApiError;
pub use state::AppState;

// Re-export DidStore from sovd-conv for convenience
pub use sovd_conv::{DataType, DidDefinition, DidStore};

use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post, put};
use axum::Router;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

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
        // Spec §7.5 — capability description (online OpenAPI doc)
        // scoped to the entity path it's read from.  Minimal stub
        // today; full path-walking emitter tracked separately.
        .route(
            "/vehicle/v1/docs",
            get(handlers::meta::capability_description),
        )
        // Component routes
        .route(
            "/vehicle/v1/components",
            get(handlers::components::list_components),
        )
        .route(
            "/vehicle/v1/components/{component_id}",
            get(handlers::components::get_component),
        )
        // Data routes
        .route(
            "/vehicle/v1/components/{component_id}/data",
            get(handlers::data::list_parameters),
        )
        .route(
            "/vehicle/v1/components/{component_id}/data/{param_id}",
            get(handlers::data::read_parameter).put(handlers::data::write_parameter),
        )
        // Gateway data routes (nested path: child_id/param_id)
        .route(
            "/vehicle/v1/components/{component_id}/data/{child_id}/{child_param_id}",
            get(handlers::data::read_gateway_parameter)
                .put(handlers::data::write_gateway_parameter),
        )
        // Deep gateway data routes (3-level nesting: gw_id/child_id/param_id)
        .route(
            "/vehicle/v1/components/{component_id}/data/{gw_id}/{child_id}/{param_id}",
            get(handlers::data::read_deep_gateway_parameter)
                .put(handlers::data::write_deep_gateway_parameter),
        )
        // Raw DID access via the spec `?raw=true` query on the standard
        // data parameter route (ISO 17978-3 §7.10). Hex DID strings like
        // "F405" resolve through DidStore the same as semantic names; raw
        // bytes come back when the caller passes `?raw=true`.
        // Fault routes
        .route(
            "/vehicle/v1/components/{component_id}/faults",
            get(handlers::faults::list_faults).delete(handlers::faults::clear_faults),
        )
        .route(
            "/vehicle/v1/components/{component_id}/faults/{fault_id}",
            get(handlers::faults::get_fault),
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
            "/vehicle/v1/components/{component_id}/operations/define-data/executions",
            post(handlers::data_lists::define_data),
        )
        .route(
            "/vehicle/v1/components/{component_id}/data-lists",
            get(handlers::data_lists::list_data_lists),
        )
        .route(
            "/vehicle/v1/components/{component_id}/data-lists/{list_id}",
            get(handlers::data_lists::read_data_list).delete(handlers::data_lists::clear_data_list),
        )
        // Log routes (primarily for HPC and message passing) +
        // spec §7.21 logs/entries + logs/config sub-resources.
        .route(
            "/vehicle/v1/components/{component_id}/logs",
            get(handlers::logs::get_logs),
        )
        .route(
            "/vehicle/v1/components/{component_id}/logs/entries",
            get(handlers::logs_ext::list_log_entries),
        )
        .route(
            "/vehicle/v1/components/{component_id}/logs/config",
            get(handlers::logs_ext::get_log_config)
                .put(handlers::logs_ext::put_log_config)
                .delete(handlers::logs_ext::reset_log_config),
        )
        .route(
            "/vehicle/v1/components/{component_id}/logs/{log_id}",
            get(handlers::logs::get_log).delete(handlers::logs::delete_log),
        )
        // Spec §7.13 clear-data collection — stub today.
        .route(
            "/vehicle/v1/components/{component_id}/clear-data",
            get(handlers::clear_data::list_clear_data_types),
        )
        .route(
            "/vehicle/v1/components/{component_id}/clear-data/status",
            get(handlers::clear_data::clear_data_status),
        )
        .route(
            "/vehicle/v1/components/{component_id}/clear-data/{action}",
            put(handlers::clear_data::clear_data_action),
        )
        // Operation routes — ISO 17978-3 §7.14 executions sub-resource.
        .route(
            "/vehicle/v1/components/{component_id}/operations",
            get(handlers::operations::list_operations),
        )
        .route(
            "/vehicle/v1/components/{component_id}/operations/{operation_id}/executions",
            post(handlers::operations::start_operation_execution),
        )
        .route(
            "/vehicle/v1/components/{component_id}/operations/{operation_id}/executions/{exec_id}",
            get(handlers::operations::get_operation_execution)
                .delete(handlers::operations::stop_operation_execution),
        )
        // Output routes (I/O control)
        .route(
            "/vehicle/v1/components/{component_id}/outputs",
            get(handlers::outputs::list_outputs),
        )
        .route(
            "/vehicle/v1/components/{component_id}/outputs/{output_id}",
            get(handlers::outputs::get_output).post(handlers::outputs::control_output),
        )
        // Sub-entity routes (apps/containers for HPC)
        .route(
            "/vehicle/v1/components/{component_id}/apps",
            get(handlers::apps::list_apps),
        )
        .route(
            "/vehicle/v1/components/{component_id}/apps/{app_id}",
            get(handlers::apps::get_app),
        )
        .route(
            "/vehicle/v1/components/{component_id}/apps/{app_id}/apps",
            get(handlers::apps::list_sub_entity_apps),
        )
        // Sub-entity file routes (SOVD spec: sub-entities inherit all resources)
        .route(
            "/vehicle/v1/components/{component_id}/apps/{app_id}/files",
            post(handlers::sub_entity::upload_file).get(handlers::sub_entity::list_files),
        )
        .route(
            "/vehicle/v1/components/{component_id}/apps/{app_id}/files/{file_id}",
            get(handlers::sub_entity::get_file).delete(handlers::sub_entity::delete_file),
        )
        .route(
            "/vehicle/v1/components/{component_id}/apps/{app_id}/files/{file_id}/verify",
            post(handlers::sub_entity::verify_file),
        )
        // Sub-entity flash routes
        .route(
            "/vehicle/v1/components/{component_id}/apps/{app_id}/flash/transfer",
            post(handlers::sub_entity::start_flash).get(handlers::sub_entity::list_transfers),
        )
        .route(
            "/vehicle/v1/components/{component_id}/apps/{app_id}/flash/transfer/{transfer_id}",
            get(handlers::sub_entity::get_transfer).delete(handlers::sub_entity::abort_transfer),
        )
        .route(
            "/vehicle/v1/components/{component_id}/apps/{app_id}/flash/transferexit",
            put(handlers::sub_entity::transfer_exit),
        )
        .route(
            "/vehicle/v1/components/{component_id}/apps/{app_id}/flash/validate",
            post(handlers::sub_entity::validate_flash),
        )
        .route(
            "/vehicle/v1/components/{component_id}/apps/{app_id}/flash/invalidate",
            post(handlers::sub_entity::invalidate_flash),
        )
        .route(
            "/vehicle/v1/components/{component_id}/apps/{app_id}/flash/activate",
            post(handlers::sub_entity::activate_flash),
        )
        .route(
            "/vehicle/v1/components/{component_id}/apps/{app_id}/flash/commit",
            post(handlers::sub_entity::commit_flash),
        )
        .route(
            "/vehicle/v1/components/{component_id}/apps/{app_id}/flash/rollback",
            post(handlers::sub_entity::rollback_flash),
        )
        .route(
            "/vehicle/v1/components/{component_id}/apps/{app_id}/flash/activation",
            get(handlers::sub_entity::get_activation_state),
        )
        // Sub-entity ECU reset — same shape as the entity-root form
        // (PUT status/restart returns 202 + Location; GET on the
        // exec sub-resource is the stateless `completed` stub).
        .route(
            "/vehicle/v1/components/{component_id}/apps/{app_id}/status/restart",
            put(handlers::sub_entity::status_restart),
        )
        .route(
            "/vehicle/v1/components/{component_id}/apps/{app_id}/status/restart/{exec_id}",
            get(handlers::sub_entity::status_restart_execution),
        )
        // Sub-entity mode routes
        .route(
            "/vehicle/v1/components/{component_id}/apps/{app_id}/modes/session",
            get(handlers::sub_entity::get_session_mode).put(handlers::sub_entity::put_session_mode),
        )
        .route(
            "/vehicle/v1/components/{component_id}/apps/{app_id}/modes/security",
            get(handlers::sub_entity::get_security_mode)
                .put(handlers::sub_entity::put_security_mode),
        )
        // Sub-entity data routes
        .route(
            "/vehicle/v1/components/{component_id}/apps/{app_id}/data",
            get(handlers::sub_entity::list_sub_entity_parameters),
        )
        .route(
            "/vehicle/v1/components/{component_id}/apps/{app_id}/data/{param_id}",
            get(handlers::sub_entity::read_sub_entity_parameter)
                .put(handlers::sub_entity::write_sub_entity_parameter),
        )
        // Sub-entity fault routes
        .route(
            "/vehicle/v1/components/{component_id}/apps/{app_id}/faults",
            get(handlers::sub_entity::list_sub_entity_faults)
                .delete(handlers::sub_entity::clear_sub_entity_faults),
        )
        .route(
            "/vehicle/v1/components/{component_id}/apps/{app_id}/faults/{fault_id}",
            get(handlers::sub_entity::get_sub_entity_fault),
        )
        // Sub-entity operation routes — same executions sub-resource
        // pattern as the entity-root operations (§7.14).
        .route(
            "/vehicle/v1/components/{component_id}/apps/{app_id}/operations",
            get(handlers::sub_entity::list_sub_entity_operations),
        )
        .route(
            "/vehicle/v1/components/{component_id}/apps/{app_id}/operations/{operation_id}/executions",
            post(handlers::sub_entity::start_sub_entity_operation),
        )
        .route(
            "/vehicle/v1/components/{component_id}/apps/{app_id}/operations/{operation_id}/executions/{exec_id}",
            get(handlers::sub_entity::get_sub_entity_operation_execution)
                .delete(handlers::sub_entity::stop_sub_entity_operation_execution),
        )
        // Cyclic subscriptions — ISO 17978-3 §7.10. The subscription
        // resource itself is component-scoped (one `resource` per sub);
        // SSE delivery is on the matching `streams/{id}` URL.
        .route(
            "/vehicle/v1/components/{component_id}/cyclic-subscriptions",
            get(handlers::subscriptions::list_cyclic_subscriptions)
                .post(handlers::subscriptions::create_cyclic_subscription),
        )
        .route(
            "/vehicle/v1/components/{component_id}/cyclic-subscriptions/{subscription_id}",
            get(handlers::subscriptions::get_cyclic_subscription)
                .delete(handlers::subscriptions::delete_cyclic_subscription),
        )
        // SSE stream delivery for a cyclic subscription.
        .route(
            "/vehicle/v1/components/{component_id}/streams/{subscription_id}",
            get(handlers::streams::stream_subscription),
        )
        // Inline `streams` reader (non-spec convenience kept for the
        // query-style "subscribe without state" use case).
        .route(
            "/vehicle/v1/components/{component_id}/streams",
            get(handlers::streams::stream_data),
        )
        // ECU Reset — ISO 17978-3 §7.19. PUT returns 202 + Location to
        // the status sub-resource; the GET on the sub-resource is a stub
        // that reads `completed` (reset is fire-and-forget, the ECU is
        // rebooting by the time we'd report progress).
        .route(
            "/vehicle/v1/components/{component_id}/status/restart",
            put(handlers::reset::status_restart),
        )
        .route(
            "/vehicle/v1/components/{component_id}/status/restart/{exec_id}",
            get(handlers::reset::status_restart_execution),
        )
        // Mode routes (session, security, link control)
        .route(
            "/vehicle/v1/components/{component_id}/modes/session",
            get(handlers::modes::get_session_mode).put(handlers::modes::put_session_mode),
        )
        .route(
            "/vehicle/v1/components/{component_id}/modes/security",
            get(handlers::modes::get_security_mode).put(handlers::modes::put_security_mode),
        )
        .route(
            "/vehicle/v1/components/{component_id}/modes/link",
            get(handlers::modes::get_link_mode).put(handlers::modes::put_link_mode),
        )
        // Discovery routes
        .route(
            "/vehicle/v1/discovery",
            post(handlers::discovery::discover_ecus),
        )
        // File (package) management routes - async flash flow
        .route(
            "/vehicle/v1/components/{component_id}/files",
            post(handlers::files::upload_file).get(handlers::files::list_files),
        )
        .route(
            "/vehicle/v1/components/{component_id}/files/{file_id}",
            get(handlers::files::get_file).delete(handlers::files::delete_file),
        )
        .route(
            "/vehicle/v1/components/{component_id}/files/{file_id}/verify",
            post(handlers::files::verify_file),
        )
        // Flash transfer routes - async flash flow
        .route(
            "/vehicle/v1/components/{component_id}/flash/transfer",
            post(handlers::flash::start_flash).get(handlers::flash::list_transfers),
        )
        .route(
            "/vehicle/v1/components/{component_id}/flash/transfer/{transfer_id}",
            get(handlers::flash::get_transfer).delete(handlers::flash::abort_transfer),
        )
        .route(
            "/vehicle/v1/components/{component_id}/flash/transferexit",
            put(handlers::flash::transfer_exit),
        )
        // Flash validate/activate routes (orchestrator-driven multi-cycle flow)
        .route(
            "/vehicle/v1/components/{component_id}/flash/validate",
            post(handlers::flash::validate_flash),
        )
        .route(
            "/vehicle/v1/components/{component_id}/flash/invalidate",
            post(handlers::flash::invalidate_flash),
        )
        .route(
            "/vehicle/v1/components/{component_id}/flash/activate",
            post(handlers::flash::activate_flash),
        )
        // Flash commit/rollback routes
        .route(
            "/vehicle/v1/components/{component_id}/flash/commit",
            post(handlers::flash::commit_flash),
        )
        .route(
            "/vehicle/v1/components/{component_id}/flash/rollback",
            post(handlers::flash::rollback_flash),
        )
        .route(
            "/vehicle/v1/components/{component_id}/flash/activation",
            get(handlers::flash::get_activation_state),
        )
        // Admin routes - DID definitions management
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
        // Middleware
        .layer(DefaultBodyLimit::disable()) // SOVD streaming uploads (ASAM SOVD chunked transfer)
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .with_state(state)
}
