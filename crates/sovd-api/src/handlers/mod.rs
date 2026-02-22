//! HTTP request handlers for SOVD API
//!
//! These handlers use the DiagnosticBackend trait and are backend-agnostic.

pub mod apps;
pub mod components;
pub mod data;
pub mod data_definitions;
pub mod definitions;
pub mod discovery;
pub mod faults;
pub mod files;
pub mod flash;
pub mod logs;
pub mod modes;
pub mod operations;
pub mod outputs;
pub mod reset;
pub mod software;
pub mod streams;
pub mod sub_entity;
pub mod subscriptions;
