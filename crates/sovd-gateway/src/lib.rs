//! sovd-gateway - Federated SOVD backend aggregation
//!
//! This crate provides the GatewayBackend that aggregates multiple
//! diagnostic backends (UDS ECUs, proxied SOVD servers, nested gateways)
//! into a unified SOVD interface.
//!
//! # Architecture
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────────┐
//! │                        SOVD Gateway                              │
//! │                                                                  │
//! │  ┌──────────────────────────────────────────────────────────┐  │
//! │  │                    GatewayBackend                         │  │
//! │  │  - Aggregates multiple backends                           │  │
//! │  │  - Routes requests to appropriate backend                 │  │
//! │  │  - Provides unified view of all entities                  │  │
//! │  └───────────────────────────┬──────────────────────────────┘  │
//! │                              │                                  │
//! │              ┌───────────────┼───────────────┐                  │
//! │              │               │               │                  │
//! │              ▼               ▼               ▼                  │
//! │  ┌───────────────┐  ┌───────────────┐  ┌────────────────────┐ │
//! │  │  UdsBackend   │  │  UdsBackend   │  │  SovdProxyBackend  │ │
//! │  │  (Engine ECU) │  │  (Trans ECU)  │  │  (Supplier app)    │ │
//! │  └───────────────┘  └───────────────┘  └────────────────────┘ │
//! │                                                                  │
//! └──────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Usage
//!
//! ```rust,ignore
//! use sovd_gateway::GatewayBackend;
//! use sovd_uds::UdsBackend;
//!
//! // Create gateway
//! let mut gateway = GatewayBackend::new("vehicle", "Vehicle Gateway", None);
//!
//! // Register backends
//! let engine_ecu = UdsBackend::new(engine_config).await?;
//! gateway.register_backend(Arc::new(engine_ecu));
//!
//! // Now gateway can serve requests for all registered backends
//! let params = gateway.list_parameters().await?;
//! // Returns: ["engine_ecu/rpm", "engine_ecu/coolant_temp", ...]
//! ```

mod gateway;

pub use gateway::GatewayBackend;

// Re-export core types for convenience
pub use sovd_core::{BackendError, BackendResult, Capabilities, DiagnosticBackend, EntityInfo};
