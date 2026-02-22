//! Flash Client Module
//!
//! Provides a configurable client for OTA flash operations that works with
//! both OpenSOVD (direct SOVD→UDS) and manufacturer containers (e.g., Vortex Motors).
//!
//! # Configuration
//!
//! The client uses YAML configuration to define endpoint paths, allowing it to
//! work with different server implementations:
//!
//! ```yaml
//! connection:
//!   base_url: "http://localhost:8080"
//!   api_key: "secret"  # optional
//!
//! endpoints:
//!   files: "/files"
//!   flash: "/flash"
//!
//! timeouts:
//!   upload_ms: 300000
//!   flash_poll_ms: 500
//!   request_ms: 30000
//! ```
//!
//! # Example
//!
//! ```rust,ignore
//! use sovd_client::flash::{FlashClient, FlashConfig};
//!
//! let config = FlashConfig::from_yaml_file("flash-config.yaml")?;
//! let client = FlashClient::new(config)?;
//!
//! // Phase 1: Upload package (async)
//! let upload = client.upload_file(&package_bytes).await?;
//! client.poll_upload_complete(&upload.id).await?;
//! client.verify_file(&upload.file_id).await?;
//!
//! // Phase 2: Flash to ECU (async)
//! let transfer = client.start_flash(&upload.file_id).await?;
//! client.poll_flash_complete(&transfer.id).await?;
//!
//! // Phase 3: Finalize
//! client.transfer_exit().await?;
//! client.ecu_reset().await?;
//! ```

mod client;
mod config;
mod types;

pub use client::*;
pub use config::*;
pub use types::*;
