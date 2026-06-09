//! sovd-core - Core traits and types for SOVD servers
//!
//! This crate provides the fundamental abstractions that allow different backends
//! (UDS, HPC, etc.) to implement the SOVD API.

pub mod backend;
pub mod error;
pub mod models;
pub mod routing;

pub use backend::{
    default_descriptor_from_context, ActivationState, DiagnosticBackend, EntityStatus,
    EntityStatusBody, FlashProgress, FlashState, FlashStatus, PackageInfo, PackageStatus,
    PackageStream, ResetKind, SoftwareInfo, UpdatePackageContext, UpdatePackageDescriptor,
    UpdatePartRef, VerifyResult,
};
pub use error::{BackendError, BackendResult};
pub use models::*;
