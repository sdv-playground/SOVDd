//! Transport layer for UDS communication
//!
//! This module provides transport adapters for communicating with ECUs:
//! - SocketCAN adapter for CAN/ISO-TP (Linux only)
//! - DoIP adapter for Diagnostics over IP (ISO 13400)
//! - Mock adapter for testing
//!
//! # Example
//!
//! ```ignore
//! use sovd_uds::transport::{create_transport, TransportAdapter};
//! use sovd_uds::config::TransportConfig;
//!
//! let config = TransportConfig::Mock(Default::default());
//! let transport = create_transport(&config).await?;
//! let response = transport.send_receive(&[0x22, 0xF1, 0x90], Duration::from_secs(5)).await?;
//! ```

mod adapter;
pub mod error;
pub mod mock;

#[cfg(all(target_os = "linux", feature = "socketcan"))]
pub mod socketcan;

#[cfg(feature = "doip")]
pub mod doip;

pub use adapter::{AddressInfo, IncomingMessage, TransportAdapter};
pub use error::TransportError;

use std::sync::Arc;

use crate::config::TransportConfig;

/// Create a transport adapter based on configuration
pub async fn create_transport(
    config: &TransportConfig,
) -> Result<Arc<dyn TransportAdapter>, TransportError> {
    match config {
        #[cfg(all(target_os = "linux", feature = "socketcan"))]
        TransportConfig::SocketCan(cfg) => {
            let adapter = socketcan::SocketCanAdapter::new(cfg).await?;
            Ok(Arc::new(adapter))
        }
        #[cfg(not(all(target_os = "linux", feature = "socketcan")))]
        TransportConfig::SocketCan(_) => Err(TransportError::Unsupported(
            "SocketCAN requires Linux and the 'socketcan' feature".to_string(),
        )),
        #[cfg(feature = "doip")]
        TransportConfig::DoIp(cfg) => {
            let adapter = doip::DoIpAdapter::new(cfg).await?;
            Ok(Arc::new(adapter))
        }
        #[cfg(not(feature = "doip"))]
        TransportConfig::DoIp(_) => Err(TransportError::Unsupported(
            "DoIP requires the 'doip' feature".to_string(),
        )),
        TransportConfig::Mock(cfg) => {
            let adapter = mock::MockTransportAdapter::new(cfg);
            Ok(Arc::new(adapter))
        }
    }
}
