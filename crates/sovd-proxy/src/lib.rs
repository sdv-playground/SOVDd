//! sovd-proxy - SOVD Proxy Backend
//!
//! Implements `DiagnosticBackend` by proxying all operations over HTTP
//! to a remote SOVD server via `SovdClient`. This enables tier-1 supplier
//! containers to be integrated into OEM vehicle gateways without direct
//! CAN bus access.

mod proxy;

pub use proxy::SovdProxyBackend;
