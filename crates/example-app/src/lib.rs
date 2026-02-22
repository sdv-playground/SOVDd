//! example-app - Example diagnostic app entity
//!
//! An "app" entity that authenticates requests, exposes synthetic telemetry,
//! and contains a managed ECU sub-entity that proxies diagnostics, intercepts
//! OTA packages, and manages flash transfers to an upstream ECU.

pub mod auth;
pub mod backend;
pub mod config;
pub mod managed_ecu;
