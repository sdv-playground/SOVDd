//! DoIP (Diagnostics over IP) Transport Adapter
//!
//! Implementation of the TransportAdapter trait for DoIP communication
//! per ISO 13400. Uses the `doip-definitions` and `doip-sockets` crates.
//!
//! # Features
//!
//! - TCP connection to DoIP gateway (plaintext and TLS)
//! - TLS auto-negotiation (falls back to TLS if required)
//! - Routing activation handshake
//! - Diagnostic message send/receive
//! - Multi-ECU support (multiple targets per gateway connection)
//! - Active keep-alive (periodic alive check requests)
//! - Automatic reconnection with retry logic
//! - Vehicle discovery via UDP broadcast (VIR/VAM)
//!
//! # Example Configuration
//!
//! ```toml
//! [transport]
//! type = "doip"
//! gateway_host = "192.168.1.100"
//! gateway_port = 13400
//! source_address = 0x0E80
//! target_address = 0x0010
//! ```
//!
//! # Vehicle Discovery
//!
//! ```ignore
//! use sovd_uds::transport::doip::discovery::{discover_gateways, DiscoveryConfig};
//!
//! let config = DiscoveryConfig::default();
//! let gateways = discover_gateways(&config).await?;
//!
//! for gateway in &gateways {
//!     println!("Found {} at {} (VIN: {})",
//!         gateway.logical_address_hex(),
//!         gateway.ip,
//!         gateway.vin_string()
//!     );
//! }
//! ```

mod adapter;
pub mod discovery;

pub use adapter::DoIpAdapter;
pub use discovery::{discover_gateways, DiscoveredGateway};
