//! Mock transport adapter for testing

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use parking_lot::RwLock;
use tokio::sync::broadcast;

use super::{AddressInfo, IncomingMessage, TransportAdapter, TransportError};
use crate::config::MockConfig;

/// Mock transport adapter for testing
pub struct MockTransportAdapter {
    config: MockConfig,
    connected: AtomicBool,
    incoming_tx: broadcast::Sender<IncomingMessage>,
    /// Predefined responses for testing (request -> response mapping)
    responses: RwLock<Vec<(Vec<u8>, Vec<u8>)>>,
}

impl MockTransportAdapter {
    pub fn new(config: &MockConfig) -> Self {
        let (incoming_tx, _) = broadcast::channel(256);
        Self {
            config: config.clone(),
            connected: AtomicBool::new(true),
            incoming_tx,
            responses: RwLock::new(Self::default_responses()),
        }
    }

    /// Add a mock response for a given request
    pub fn add_response(&self, request: Vec<u8>, response: Vec<u8>) {
        self.responses.write().push((request, response));
    }

    /// Inject an incoming message (simulates ECU sending periodic data)
    pub fn inject_incoming(&self, data: Vec<u8>) {
        let msg = IncomingMessage {
            timestamp: Instant::now(),
            data,
            source: AddressInfo::default(),
        };
        let _ = self.incoming_tx.send(msg);
    }

    /// Set connection state
    pub fn set_connected(&self, connected: bool) {
        self.connected.store(connected, Ordering::SeqCst);
    }

    fn default_responses() -> Vec<(Vec<u8>, Vec<u8>)> {
        vec![
            // Diagnostic Session Control - Default (0x10 01 -> 0x50 01)
            (vec![0x10, 0x01], vec![0x50, 0x01, 0x00, 0x19, 0x01, 0xF4]),
            // Diagnostic Session Control - Extended (0x10 03 -> 0x50 03)
            (vec![0x10, 0x03], vec![0x50, 0x03, 0x00, 0x19, 0x01, 0xF4]),
            // Diagnostic Session Control - Engineering (0x10 60 -> 0x50 60)
            (vec![0x10, 0x60], vec![0x50, 0x60, 0x00, 0x19, 0x01, 0xF4]),
            // Tester Present (0x3E 00 -> 0x7E 00)
            (vec![0x3E, 0x00], vec![0x7E, 0x00]),
            // Tester Present suppress response (0x3E 80 -> no response needed)
            (vec![0x3E, 0x80], vec![]),
            // ReadDataByIdentifier - VIN (0x22 F1 90 -> 0x62 F1 90 + 17-byte VIN)
            (vec![0x22, 0xF1, 0x90], {
                let mut resp = vec![0x62, 0xF1, 0x90];
                resp.extend_from_slice(b"1HGCM82633A123456"); // 17-char mock VIN
                resp
            }),
            // ReadDataByIdentifier - Engine RPM (0x22 F4 0C -> 0x62 F4 0C + 2-byte RPM)
            (
                vec![0x22, 0xF4, 0x0C],
                vec![0x62, 0xF4, 0x0C, 0x0B, 0xB8], // 3000 RPM (0x0BB8 * 0.25)
            ),
            // ReadDataByIdentifier - Coolant Temp (0x22 F4 05 -> 0x62 F4 05 + 1-byte temp)
            (
                vec![0x22, 0xF4, 0x05],
                vec![0x62, 0xF4, 0x05, 0x5A], // 90 - 40 = 50°C (0x5A = 90)
            ),
            // ReadDataByIdentifier - ECU HW Number (0x22 F1 91)
            (vec![0x22, 0xF1, 0x91], {
                let mut resp = vec![0x62, 0xF1, 0x91];
                resp.extend_from_slice(b"HW-12345");
                resp
            }),
            // ReadDataByIdentifier - ECU SW Version (0x22 F1 95)
            (vec![0x22, 0xF1, 0x95], {
                let mut resp = vec![0x62, 0xF1, 0x95];
                resp.extend_from_slice(b"SW-1.0.0");
                resp
            }),
            // ReadDTCInformation - ReportDTCByStatusMask (0x19 02 FF -> 0x59 02 + DTCs)
            (
                vec![0x19, 0x02],
                vec![
                    0x59, 0x02, 0xFF, // Service response + sub-function + status availability
                    0x01, 0x23, 0x45, 0x09, // DTC 0x012345, status 0x09 (active)
                    0x06, 0x78, 0x90, 0x28, // DTC 0x067890, status 0x28 (confirmed)
                ],
            ),
            // ClearDiagnosticInformation (0x14 FF FF FF -> 0x54)
            (vec![0x14, 0xFF, 0xFF, 0xFF], vec![0x54]),
            // RoutineControl - Start (0x31 01 -> 0x71 01)
            (
                vec![0x31, 0x01, 0xFF, 0x00],
                vec![0x71, 0x01, 0xFF, 0x00, 0x00], // Routine started OK
            ),
        ]
    }

    fn find_response(&self, request: &[u8]) -> Option<Vec<u8>> {
        let responses = self.responses.read();

        // First try exact match
        for (req, resp) in responses.iter() {
            if req == request {
                return Some(resp.clone());
            }
        }

        // Then try prefix match for variable-length requests
        for (req, resp) in responses.iter() {
            if request.starts_with(req) {
                return Some(resp.clone());
            }
        }

        // Generate default response based on service ID
        if !request.is_empty() {
            let service_id = request[0];
            // Positive response = service_id + 0x40
            let positive_response = service_id.wrapping_add(0x40);
            return Some(vec![positive_response]);
        }

        None
    }
}

#[async_trait]
impl TransportAdapter for MockTransportAdapter {
    async fn send_receive(
        &self,
        request: &[u8],
        _timeout: Duration,
    ) -> Result<Vec<u8>, TransportError> {
        if !self.connected.load(Ordering::SeqCst) {
            return Err(TransportError::ConnectionClosed);
        }

        // Simulate latency
        if self.config.latency_ms > 0 {
            tokio::time::sleep(Duration::from_millis(self.config.latency_ms)).await;
        }

        self.find_response(request)
            .ok_or_else(|| TransportError::ReceiveFailed("No mock response configured".to_string()))
    }

    async fn send(&self, request: &[u8]) -> Result<(), TransportError> {
        if !self.connected.load(Ordering::SeqCst) {
            return Err(TransportError::ConnectionClosed);
        }

        // Simulate latency
        if self.config.latency_ms > 0 {
            tokio::time::sleep(Duration::from_millis(self.config.latency_ms)).await;
        }

        tracing::debug!(?request, "Mock transport: sent message");
        Ok(())
    }

    fn subscribe(&self) -> broadcast::Receiver<IncomingMessage> {
        self.incoming_tx.subscribe()
    }

    async fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    async fn reconnect(&self) -> Result<(), TransportError> {
        self.connected.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn address_info(&self) -> AddressInfo {
        AddressInfo {
            tx_id: 0x7E0,
            rx_id: 0x7E8,
        }
    }
}
