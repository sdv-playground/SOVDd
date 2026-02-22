//! sovd-core - Core traits and types for SOVD servers
//!
//! This crate provides the fundamental abstractions that allow different backends
//! (UDS, HPC, etc.) to implement the SOVD API.

pub mod backend;
pub mod error;
pub mod models;
pub mod routing;

pub use backend::{
    ActivationState, DiagnosticBackend, FlashProgress, FlashState, FlashStatus, PackageInfo,
    PackageStatus, SoftwareInfo, VerifyResult,
};
pub use error::{BackendError, BackendResult};
pub use models::*;
