//! sovd-uds - UDS/CAN diagnostic backend for SOVD
//!
//! This crate provides the UDS backend implementation that communicates
//! with traditional ECUs over CAN/ISO-TP.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                      UdsBackend                              │
//! │  Implements DiagnosticBackend trait                         │
//! │                                                             │
//! │  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────┐ │
//! │  │ EcuConfig   │  │SessionMgr   │  │SubscriptionMgr     │ │
//! │  │ (params)    │  │ (state)     │  │ (periodic data)    │ │
//! │  └─────────────┘  └─────────────┘  └─────────────────────┘ │
//! │                          │                                  │
//! │                    ┌─────┴─────┐                            │
//! │                    │UdsService │                            │
//! │                    │(protocol) │                            │
//! │                    └─────┬─────┘                            │
//! │                          │                                  │
//! │                 ┌────────┴────────┐                         │
//! │                 │TransportAdapter │                         │
//! │                 │(SocketCAN/etc)  │                         │
//! │                 └─────────────────┘                         │
//! └─────────────────────────────────────────────────────────────┘
//! ```

pub mod backend;
pub mod config;
pub mod error;
pub mod output_conv;
pub mod session;
pub mod subscription;
pub mod transport;
pub mod uds;

pub use backend::UdsBackend;
pub use config::UdsBackendConfig;
pub use error::UdsBackendError;
pub use session::{SessionError, SessionManager, SessionState};
pub use subscription::{StreamError, StreamManager, StreamSubscription};
pub use transport::{create_transport, TransportAdapter, TransportError};
pub use uds::{NegativeResponseCode, ServiceIds, UdsError, UdsService};

// Re-export CAN bus scanner (Linux + socketcan feature only)
#[cfg(all(target_os = "linux", feature = "socketcan"))]
pub use transport::socketcan::scanner;

// Re-export for convenience
pub use sovd_core::{
    BackendError, BackendResult, Capabilities, DataValue, DiagnosticBackend, EntityInfo, Fault,
    OperationExecution, OperationInfo, ParameterInfo,
};
