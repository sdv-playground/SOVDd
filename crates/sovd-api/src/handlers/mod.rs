//! HTTP request handlers for SOVD API
//!
//! These handlers use the DiagnosticBackend trait and are backend-agnostic.

pub mod apps;
pub mod bulk_data;
pub mod clear_data;
pub mod components;
pub mod data;
pub mod data_lists;
pub mod definitions;
pub mod faults;
// F.D8b: handlers::files + handlers::flash deleted.  The legacy
// wire shapes they served are replaced by /updates (F.D2).
// C-025: handlers::discovery (POST /discovery) + handlers::streams
// (inline + cyclic `/streams`) deleted — `discovery` / `streams` are
// not standardized entity-resource names. Bus discovery isn't a SOVD
// resource (clients use GET /components); SSE delivery is now the
// `cyclic-subscriptions/{id}` resource itself under content negotiation.
pub mod logs;
pub mod logs_ext;
pub mod meta;
pub mod modes;
pub mod operations;
pub mod reset;
pub mod software;
pub mod stubs;
pub mod sub_entity;
pub mod subscriptions;
pub mod updates;
