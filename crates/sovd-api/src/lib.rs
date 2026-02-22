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

use axum::routing::{delete, get, post, put};
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
        // Component routes
        .route(
            "/vehicle/v1/components",
            get(handlers::components::list_components),
        )
        .route(
            "/vehicle/v1/components/:component_id",
            get(handlers::components::get_component),
        )
        // Data routes
        .route(
            "/vehicle/v1/components/:component_id/data",
            get(handlers::data::list_parameters),
        )
        .route(
            "/vehicle/v1/components/:component_id/data/:param_id",
            get(handlers::data::read_parameter).put(handlers::data::write_parameter),
        )
        // Gateway data routes (nested path: child_id/param_id)
        .route(
            "/vehicle/v1/components/:component_id/data/:child_id/:child_param_id",
            get(handlers::data::read_gateway_parameter)
                .put(handlers::data::write_gateway_parameter),
        )
        // Deep gateway data routes (3-level nesting: gw_id/child_id/param_id)
        .route(
            "/vehicle/v1/components/:component_id/data/:gw_id/:child_id/:param_id",
            get(handlers::data::read_deep_gateway_parameter)
                .put(handlers::data::write_deep_gateway_parameter),
        )
        // Raw DID access (reads any DID, applies conversion if registered)
        .route(
            "/vehicle/v1/components/:component_id/did/:did",
            get(handlers::data::read_did).put(handlers::data::write_did),
        )
        // Fault routes
        .route(
            "/vehicle/v1/components/:component_id/faults",
            get(handlers::faults::list_faults).delete(handlers::faults::clear_faults),
        )
        .route(
            "/vehicle/v1/components/:component_id/faults/:fault_id",
            get(handlers::faults::get_fault),
        )
        // Active DTCs only (convenience endpoint)
        .route(
            "/vehicle/v1/components/:component_id/dtcs",
            get(handlers::faults::list_active_dtcs),
        )
        // Data definition routes (DDID)
        .route(
            "/vehicle/v1/components/:component_id/data-definitions",
            post(handlers::data_definitions::create_data_definition),
        )
        .route(
            "/vehicle/v1/components/:component_id/data-definitions/:ddid",
            delete(handlers::data_definitions::delete_data_definition),
        )
        // Log routes (primarily for HPC and message passing)
        .route(
            "/vehicle/v1/components/:component_id/logs",
            get(handlers::logs::get_logs),
        )
        .route(
            "/vehicle/v1/components/:component_id/logs/:log_id",
            get(handlers::logs::get_log).delete(handlers::logs::delete_log),
        )
        // Operation routes
        .route(
            "/vehicle/v1/components/:component_id/operations",
            get(handlers::operations::list_operations),
        )
        .route(
            "/vehicle/v1/components/:component_id/operations/:operation_id",
            post(handlers::operations::execute_operation),
        )
        // Output routes (I/O control)
        .route(
            "/vehicle/v1/components/:component_id/outputs",
            get(handlers::outputs::list_outputs),
        )
        .route(
            "/vehicle/v1/components/:component_id/outputs/:output_id",
            get(handlers::outputs::get_output).post(handlers::outputs::control_output),
        )
        // Sub-entity routes (apps/containers for HPC)
        .route(
            "/vehicle/v1/components/:component_id/apps",
            get(handlers::apps::list_apps),
        )
        .route(
            "/vehicle/v1/components/:component_id/apps/:app_id",
            get(handlers::apps::get_app),
        )
        .route(
            "/vehicle/v1/components/:component_id/apps/:app_id/apps",
            get(handlers::apps::list_sub_entity_apps),
        )
        // Sub-entity file routes (SOVD spec: sub-entities inherit all resources)
        .route(
            "/vehicle/v1/components/:component_id/apps/:app_id/files",
            post(handlers::sub_entity::upload_file).get(handlers::sub_entity::list_files),
        )
        .route(
            "/vehicle/v1/components/:component_id/apps/:app_id/files/:file_id",
            get(handlers::sub_entity::get_file).delete(handlers::sub_entity::delete_file),
        )
        .route(
            "/vehicle/v1/components/:component_id/apps/:app_id/files/:file_id/verify",
            post(handlers::sub_entity::verify_file),
        )
        // Sub-entity flash routes
        .route(
            "/vehicle/v1/components/:component_id/apps/:app_id/flash/transfer",
            post(handlers::sub_entity::start_flash).get(handlers::sub_entity::list_transfers),
        )
        .route(
            "/vehicle/v1/components/:component_id/apps/:app_id/flash/transfer/:transfer_id",
            get(handlers::sub_entity::get_transfer).delete(handlers::sub_entity::abort_transfer),
        )
        .route(
            "/vehicle/v1/components/:component_id/apps/:app_id/flash/transferexit",
            put(handlers::sub_entity::transfer_exit),
        )
        .route(
            "/vehicle/v1/components/:component_id/apps/:app_id/flash/commit",
            post(handlers::sub_entity::commit_flash),
        )
        .route(
            "/vehicle/v1/components/:component_id/apps/:app_id/flash/rollback",
            post(handlers::sub_entity::rollback_flash),
        )
        .route(
            "/vehicle/v1/components/:component_id/apps/:app_id/flash/activation",
            get(handlers::sub_entity::get_activation_state),
        )
        // Sub-entity reset route
        .route(
            "/vehicle/v1/components/:component_id/apps/:app_id/reset",
            post(handlers::sub_entity::ecu_reset),
        )
        // Sub-entity mode routes
        .route(
            "/vehicle/v1/components/:component_id/apps/:app_id/modes/session",
            get(handlers::sub_entity::get_session_mode).put(handlers::sub_entity::put_session_mode),
        )
        .route(
            "/vehicle/v1/components/:component_id/apps/:app_id/modes/security",
            get(handlers::sub_entity::get_security_mode)
                .put(handlers::sub_entity::put_security_mode),
        )
        // Sub-entity data routes
        .route(
            "/vehicle/v1/components/:component_id/apps/:app_id/data",
            get(handlers::sub_entity::list_sub_entity_parameters),
        )
        .route(
            "/vehicle/v1/components/:component_id/apps/:app_id/data/:param_id",
            get(handlers::sub_entity::read_sub_entity_parameter)
                .put(handlers::sub_entity::write_sub_entity_parameter),
        )
        // Sub-entity fault routes
        .route(
            "/vehicle/v1/components/:component_id/apps/:app_id/faults",
            get(handlers::sub_entity::list_sub_entity_faults)
                .delete(handlers::sub_entity::clear_sub_entity_faults),
        )
        .route(
            "/vehicle/v1/components/:component_id/apps/:app_id/faults/:fault_id",
            get(handlers::sub_entity::get_sub_entity_fault),
        )
        // Sub-entity operation routes
        .route(
            "/vehicle/v1/components/:component_id/apps/:app_id/operations",
            get(handlers::sub_entity::list_sub_entity_operations),
        )
        .route(
            "/vehicle/v1/components/:component_id/apps/:app_id/operations/:operation_id",
            post(handlers::sub_entity::execute_sub_entity_operation),
        )
        // Component-level streaming routes (SSE for real-time data)
        .route(
            "/vehicle/v1/components/:component_id/subscriptions",
            post(handlers::streams::create_subscription),
        )
        .route(
            "/vehicle/v1/components/:component_id/streams",
            get(handlers::streams::stream_data),
        )
        // Global subscription routes
        .route(
            "/vehicle/v1/subscriptions",
            get(handlers::subscriptions::list_subscriptions)
                .post(handlers::subscriptions::create_subscription),
        )
        .route(
            "/vehicle/v1/subscriptions/:subscription_id",
            get(handlers::subscriptions::get_subscription)
                .delete(handlers::subscriptions::delete_subscription),
        )
        // Global stream routes (for subscriptions created via /vehicle/v1/subscriptions)
        .route(
            "/vehicle/v1/streams/:subscription_id",
            get(handlers::streams::stream_subscription),
        )
        // ECU Reset route
        .route(
            "/vehicle/v1/components/:component_id/reset",
            post(handlers::reset::ecu_reset),
        )
        // Mode routes (session, security, link control)
        .route(
            "/vehicle/v1/components/:component_id/modes/session",
            get(handlers::modes::get_session_mode).put(handlers::modes::put_session_mode),
        )
        .route(
            "/vehicle/v1/components/:component_id/modes/security",
            get(handlers::modes::get_security_mode).put(handlers::modes::put_security_mode),
        )
        .route(
            "/vehicle/v1/components/:component_id/modes/link",
            get(handlers::modes::get_link_mode).put(handlers::modes::put_link_mode),
        )
        // Discovery routes
        .route(
            "/vehicle/v1/discovery",
            post(handlers::discovery::discover_ecus),
        )
        // File (package) management routes - async flash flow
        .route(
            "/vehicle/v1/components/:component_id/files",
            post(handlers::files::upload_file).get(handlers::files::list_files),
        )
        .route(
            "/vehicle/v1/components/:component_id/files/:file_id",
            get(handlers::files::get_file).delete(handlers::files::delete_file),
        )
        .route(
            "/vehicle/v1/components/:component_id/files/:file_id/verify",
            post(handlers::files::verify_file),
        )
        // Flash transfer routes - async flash flow
        .route(
            "/vehicle/v1/components/:component_id/flash/transfer",
            post(handlers::flash::start_flash).get(handlers::flash::list_transfers),
        )
        .route(
            "/vehicle/v1/components/:component_id/flash/transfer/:transfer_id",
            get(handlers::flash::get_transfer).delete(handlers::flash::abort_transfer),
        )
        .route(
            "/vehicle/v1/components/:component_id/flash/transferexit",
            put(handlers::flash::transfer_exit),
        )
        // Flash commit/rollback routes
        .route(
            "/vehicle/v1/components/:component_id/flash/commit",
            post(handlers::flash::commit_flash),
        )
        .route(
            "/vehicle/v1/components/:component_id/flash/rollback",
            post(handlers::flash::rollback_flash),
        )
        .route(
            "/vehicle/v1/components/:component_id/flash/activation",
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
            "/admin/definitions/:did",
            get(handlers::definitions::get_definition)
                .put(handlers::definitions::put_definition)
                .delete(handlers::definitions::delete_definition),
        )
        // Middleware
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .with_state(state)
}
