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
//! The `testing` module provides utilities for integration testing:
//!
//! ```rust,ignore
//! use sovd_client::testing::TestServer;
//! use sovd_api::{create_router, AppState};
//!
//! let server = TestServer::start(create_router(state)).await?;
//! let components = server.client.list_components().await?;
//! ```

mod client;
mod error;
pub mod flash;
pub mod streaming;
pub mod testing;
mod types;

pub use client::SovdClient;
pub use error::{Result, SovdClientError};
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
