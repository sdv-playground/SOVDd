//! Transport adapter trait and types

use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::broadcast;

use super::TransportError;

/// Incoming message from the transport layer
#[derive(Debug, Clone)]
pub struct IncomingMessage {
    /// Timestamp when the message was received
    pub timestamp: Instant,
    /// Raw UDS payload data
    pub data: Vec<u8>,
    /// Source address information
    pub source: AddressInfo,
}

/// Address information for CAN/ISO-TP or SOME/IP
#[derive(Debug, Clone, Default)]
pub struct AddressInfo {
    /// Transmit ID (tester -> ECU)
    pub tx_id: u32,
    /// Receive ID (ECU -> tester)
    pub rx_id: u32,
}

/// Transport-agnostic interface for UDS communication
///
/// This trait abstracts the underlying transport mechanism (SocketCAN, SOME/IP, etc.)
/// and provides a unified interface for sending/receiving UDS messages.
#[async_trait]
pub trait TransportAdapter: Send + Sync {
    /// Send a UDS request and wait for a response
    ///
    /// # Arguments
    /// * `request` - The raw UDS request bytes
    /// * `timeout` - Maximum time to wait for a response
    ///
    /// # Returns
    /// The raw UDS response bytes, or an error
    async fn send_receive(
        &self,
        request: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>, TransportError>;

    /// Send a UDS request without waiting for a response
    ///
    /// Useful for tester present with suppress positive response,
    /// or when setting up periodic identifiers.
    async fn send(&self, request: &[u8]) -> Result<(), TransportError>;

    /// Subscribe to incoming messages
    ///
    /// Returns a broadcast receiver that will receive all incoming
    /// UDS messages (useful for periodic data from 0x2A).
    fn subscribe(&self) -> broadcast::Receiver<IncomingMessage>;

    /// Check if the transport is connected
    async fn is_connected(&self) -> bool;

    /// Attempt to reconnect if disconnected
    async fn reconnect(&self) -> Result<(), TransportError>;

    /// Get the current address configuration
    fn address_info(&self) -> AddressInfo;
}
