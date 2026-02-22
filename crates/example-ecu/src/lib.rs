//! example-ecu - ECU Simulator Library
//!
//! Provides components for simulating vehicle ECUs for testing SOVD implementations.
//!
//! # Modules
//!
//! - [`sw_package`] - Firmware image binary format (build, parse, verify)
//! - [`uds`] - UDS protocol constants and helpers

pub mod sw_package;
pub mod uds;

pub use sw_package::{FirmwareImage, FirmwareImageError, FirmwareImageResult};
