//! SOVD Client Library
//!
//! Provides a typed HTTP client for communicating with SOVD-compliant servers.
//!
//! # Example
//!
//! ```rust,no_run
//! use sovd_client::SovdClient;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let client = SovdClient::new("http://localhost:9080")?;
//!
//!     // List components
//!     let components = client.list_components().await?;
//!
//!     // Read a parameter by semantic name (SOVD-compliant)
//!     let rpm = client.read_data("engine_ecu", "engine_rpm").await?;
//!
//!     // Or by raw DID (for private data)
//!     let temp = client.read_data("engine_ecu", "F405").await?;
//!
//!     Ok(())
//! }
//! ```
//!
//! # Path dialects
//!
//! A client speaks one of two SOVD trees ([`Dialect`], module [`paths`]):
//!
//! - **`legacy`** (default): `/vehicle/v1`, flat. Every constructor builds a legacy
//!   client, and its requests are exactly those of the client before dialects existed.
//! - **`v1`**: the ISO 17978-3 tree. [`SovdClient::with_dialect`] /
//!   [`FlashClient::with_dialect`] discover once whether the server presents a node
//!   (`GET /v1/components` lists one item whose `subcomponents` answer). On a node
//!   server every path is the node form — members
//!   `/v1/components/{node}/subcomponents/{id}/…`, node-level resources
//!   `/v1/components/{node}/{status|updates|operations|data}…` — and
//!   [`SovdClient::list_components`] lists the node's subcomponents; without a node,
//!   flat `/v1/components/{id}/…` and `/v1/…`. The resolved [`Paths`] can be handed to
//!   further clients ([`SovdClient::with_paths`], [`flash::FlashConfigBuilder::paths`]).
//!
//! What `v1` changes for callers, beyond the paths: `ecu_reset` (both clients) accepts
//! the `/v1` answer (202, no body) and reports `status: "running"` with `href` = the
//! `Location`; [`FlashClient::execute`] is always orchestrated (the server requires
//! it), so it returns at `awaiting-verdict` for a banked update whatever its argument.
//!
//! ```rust,no_run
//! # async fn f() -> anyhow::Result<()> {
//! use sovd_client::{Dialect, SovdClient};
//! let client = SovdClient::new("https://device:8443")?
//!     .with_dialect(Dialect::V1)
//!     .await?;
//! let status = client.read_status("vm1").await?; // node form on a node server
//! # Ok(()) }
//! ```
//!
//! # Private Data / Client-Side Conversion
//!
//! For scenarios where the server doesn't have DID definitions (private data),
//! use raw reads with client-side conversion via the `sovd-conv` crate:
//!
//! ```rust,ignore
//! // Enable the "conversion" feature in Cargo.toml:
//! // sovd-client = { version = "...", features = ["conversion"] }
//!
//! use sovd_client::conv::{DidStore, DidDefinition, DataType};
//!
//! // Set up client-side conversions (your private definitions)
//! let store = DidStore::new();
//! store.register(0xF405, DidDefinition::scaled(DataType::Uint8, 1.0, -40.0)
//!     .with_name("Coolant Temp")
//!     .with_unit("°C"));
//!
//! // Read raw and convert locally
//! let response = client.read_data_raw("ecu", "F405").await?;
//! let raw = response.raw_bytes()?;
//! let value = store.decode(0xF405, &raw)?; // Returns 92
//! ```
//!
//! # Testing
//!
//! The `testing` module (behind the `test-util` feature) provides utilities
//! for integration testing:
//!
//! ```rust,ignore
//! // Enable the "test-util" feature in [dev-dependencies]:
//! // sovd-client = { version = "...", features = ["test-util"] }
//!
//! use sovd_client::testing::TestServer;
//! use sovd_api::{create_router, AppState};
//!
//! let server = TestServer::start(create_router(state)).await?;
//! let components = server.client.list_components().await?;
//! ```

mod client;
mod error;
pub mod flash;
pub mod paths;
pub mod streaming;
#[cfg(feature = "test-util")]
pub mod testing;
mod types;

pub use client::SovdClient;
pub use error::{Result, SovdClientError};
pub use paths::{Dialect, Paths};
pub use types::*;

// Re-export flash client for convenience
pub use flash::{FlashClient, FlashConfig, FlashError};

// Re-export streaming types for convenience
pub use streaming::{StreamError, StreamEvent, Subscription};

// Re-export core types for convenience
pub use sovd_core::models::{DataValue, EntityInfo, Fault};

// Re-export sovd-conv when "conversion" feature is enabled
#[cfg(feature = "conversion")]
pub use sovd_conv as conv;
