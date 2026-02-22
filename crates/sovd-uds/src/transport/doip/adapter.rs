//! DoIP Transport Adapter Implementation
//!
//! Supports TLS auto-negotiation, multi-ECU, keep-alive, and auto-reconnect.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use doip_definitions::payload::{
    ActivationCode, ActivationType, AliveCheckRequest, AliveCheckResponse, DiagnosticMessage,
    DoipPayload, RoutingActivationRequest,
};
use doip_sockets::tcp::{DoIpSslStream, TcpStream as DoIpTcpStream};
use tokio::sync::{broadcast, Mutex, RwLock};
use tracing::{error, info, warn};

use crate::config::DoIpConfig;
use crate::transport::{AddressInfo, IncomingMessage, TransportAdapter, TransportError};

const DOIP_PORT_TLS: u16 = 3496;
const MAX_RECONNECT_ATTEMPTS: u32 = 3;
const RECONNECT_DELAY_MS: u64 = 1000;

/// Connection type - plaintext or TLS
enum DoIpConnection {
    Plaintext(DoIpTcpStream),
    Tls(DoIpSslStream),
}

impl DoIpConnection {
    async fn send(&mut self, payload: DoipPayload) -> Result<(), TransportError> {
        match self {
            Self::Plaintext(s) => s.send(payload).await,
            Self::Tls(s) => s.send(payload).await,
        }
        .map_err(|e| TransportError::SendFailed(e.to_string()))
    }

    async fn read(&mut self) -> Result<Option<DoipPayload>, TransportError> {
        let result = match self {
            Self::Plaintext(s) => s.read().await,
            Self::Tls(s) => s.read().await,
        };
        match result {
            Some(Ok(msg)) => Ok(Some(msg.payload)),
            Some(Err(e)) => Err(TransportError::ReceiveFailed(e.to_string())),
            None => Ok(None),
        }
    }
}

/// DoIP Transport Adapter with TLS, multi-ECU, and auto-reconnect support
pub struct DoIpAdapter {
    config: DoIpConfig,
    connection: Arc<Mutex<Option<DoIpConnection>>>,
    connected: Arc<AtomicBool>,
    use_tls: Arc<AtomicBool>,
    ecu_channels: Arc<RwLock<HashMap<u16, broadcast::Sender<IncomingMessage>>>>,
    incoming_tx: broadcast::Sender<IncomingMessage>,
    address_info: AddressInfo,
    receiver_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    keepalive_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl DoIpAdapter {
    /// Create a new DoIP adapter and connect to the gateway
    pub async fn new(config: &DoIpConfig) -> Result<Self, TransportError> {
        let (incoming_tx, _) = broadcast::channel(256);

        let address_info = AddressInfo {
            tx_id: config.source_address as u32,
            rx_id: config.target_address as u32,
        };

        let mut ecu_channels = HashMap::new();
        let (primary_tx, _) = broadcast::channel(64);
        ecu_channels.insert(config.target_address, primary_tx);

        let adapter = Self {
            config: config.clone(),
            connection: Arc::new(Mutex::new(None)),
            connected: Arc::new(AtomicBool::new(false)),
            use_tls: Arc::new(AtomicBool::new(false)),
            ecu_channels: Arc::new(RwLock::new(ecu_channels)),
            incoming_tx,
            address_info,
            receiver_handle: Mutex::new(None),
            keepalive_handle: Mutex::new(None),
        };

        adapter.connect_with_retry(MAX_RECONNECT_ATTEMPTS).await?;
        Ok(adapter)
    }

    /// Add an ECU target for multi-ECU support
    pub async fn add_ecu(&self, target_address: u16) {
        let mut channels = self.ecu_channels.write().await;
        if !channels.contains_key(&target_address) {
            let (tx, _) = broadcast::channel(64);
            channels.insert(target_address, tx);
        }
    }

    /// Connect with retry logic
    async fn connect_with_retry(&self, max_attempts: u32) -> Result<(), TransportError> {
        let mut last_error = TransportError::ConnectionFailed("No attempt".into());

        for attempt in 1..=max_attempts {
            match self.connect().await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    warn!(attempt, max_attempts, %e, "Connection failed");
                    last_error = e;
                    if attempt < max_attempts {
                        tokio::time::sleep(Duration::from_millis(RECONNECT_DELAY_MS)).await;
                    }
                }
            }
        }
        Err(last_error)
    }

    /// Connect with TLS auto-negotiation
    async fn connect(&self) -> Result<(), TransportError> {
        let addr = format!("{}:{}", self.config.gateway_host, self.config.gateway_port);
        let timeout = Duration::from_millis(self.config.connect_timeout_ms);

        info!(%addr, "Connecting to DoIP gateway");

        // Try plaintext first
        let conn = tokio::time::timeout(timeout, DoIpTcpStream::connect(&addr))
            .await
            .map_err(|_| TransportError::Timeout("Connection timeout".into()))?
            .map_err(|e| TransportError::ConnectionFailed(e.to_string()))?;

        *self.connection.lock().await = Some(DoIpConnection::Plaintext(conn));

        // Routing activation (may trigger TLS upgrade)
        match self.routing_activation().await {
            Ok(()) => self.use_tls.store(false, Ordering::SeqCst),
            Err(TransportError::TlsRequired) => {
                info!("TLS required, reconnecting with encryption");
                self.connect_tls().await?;
            }
            Err(e) => return Err(e),
        }

        self.connected.store(true, Ordering::SeqCst);
        self.start_receiver().await;

        if self.config.keepalive_interval_secs > 0 {
            self.start_keepalive().await;
        }

        info!(tls = self.use_tls.load(Ordering::SeqCst), "DoIP connected");
        Ok(())
    }

    /// Connect using TLS
    async fn connect_tls(&self) -> Result<(), TransportError> {
        let addr = format!("{}:{}", self.config.gateway_host, DOIP_PORT_TLS);
        let timeout = Duration::from_millis(self.config.connect_timeout_ms);

        let conn = tokio::time::timeout(timeout, DoIpSslStream::connect(&addr))
            .await
            .map_err(|_| TransportError::Timeout("TLS timeout".into()))?
            .map_err(|e| TransportError::ConnectionFailed(format!("TLS: {}", e)))?;

        *self.connection.lock().await = Some(DoIpConnection::Tls(conn));
        self.use_tls.store(true, Ordering::SeqCst);
        self.routing_activation().await
    }

    /// Perform routing activation handshake
    async fn routing_activation(&self) -> Result<(), TransportError> {
        let activation_type = match self.config.activation_type {
            0x01 => ActivationType::WwhObd,
            _ => ActivationType::Default,
        };

        let payload = DoipPayload::RoutingActivationRequest(RoutingActivationRequest {
            source_address: self.config.source_address.to_be_bytes(),
            activation_type,
            buffer: [0; 4],
        });

        let mut guard = self.connection.lock().await;
        let conn = guard.as_mut().ok_or(TransportError::ConnectionClosed)?;
        conn.send(payload).await?;

        let timeout = Duration::from_millis(self.config.activation_timeout_ms);
        let response = tokio::time::timeout(timeout, conn.read())
            .await
            .map_err(|_| TransportError::Timeout("Activation timeout".into()))?
            .map_err(|e| TransportError::ReceiveFailed(e.to_string()))?
            .ok_or(TransportError::ConnectionClosed)?;

        match response {
            DoipPayload::RoutingActivationResponse(resp) => match resp.activation_code {
                ActivationCode::SuccessfullyActivated
                | ActivationCode::ActivatedConfirmationRequired => Ok(()),
                ActivationCode::DeniedRequestEncryptedTLSConnection => {
                    Err(TransportError::TlsRequired)
                }
                code => Err(TransportError::ConnectionFailed(format!("{:?}", code))),
            },
            DoipPayload::GenericNack(n) => Err(TransportError::ConnectionFailed(format!(
                "NACK: {:?}",
                n.nack_code
            ))),
            _ => Err(TransportError::ProtocolError("Unexpected response".into())),
        }
    }

    /// Start background receiver task
    async fn start_receiver(&self) {
        let connection = self.connection.clone();
        let connected = self.connected.clone();
        let incoming_tx = self.incoming_tx.clone();
        let ecu_channels = self.ecu_channels.clone();
        let source_address = self.config.source_address;

        let handle = tokio::spawn(async move {
            while connected.load(Ordering::SeqCst) {
                let mut guard = connection.lock().await;
                let Some(conn) = guard.as_mut() else { break };

                match tokio::time::timeout(Duration::from_millis(100), conn.read()).await {
                    Ok(Ok(Some(payload))) => {
                        Self::handle_message(
                            payload,
                            &incoming_tx,
                            &ecu_channels,
                            source_address,
                            conn,
                        )
                        .await;
                    }
                    Ok(Ok(None)) => {
                        connected.store(false, Ordering::SeqCst);
                        break;
                    }
                    Ok(Err(e)) if !e.to_string().contains("timed out") => {
                        error!(%e, "Receive error");
                        connected.store(false, Ordering::SeqCst);
                        break;
                    }
                    _ => {}
                }
                drop(guard);
                tokio::task::yield_now().await;
            }
        });

        *self.receiver_handle.lock().await = Some(handle);
    }

    /// Start keep-alive task
    async fn start_keepalive(&self) {
        let connection = self.connection.clone();
        let connected = self.connected.clone();
        let interval = Duration::from_secs(self.config.keepalive_interval_secs);

        let handle = tokio::spawn(async move {
            let mut timer = tokio::time::interval(interval);
            timer.tick().await; // skip first

            while connected.load(Ordering::SeqCst) {
                timer.tick().await;
                if let Some(conn) = connection.lock().await.as_mut() {
                    let _ = conn
                        .send(DoipPayload::AliveCheckRequest(AliveCheckRequest {}))
                        .await;
                }
            }
        });

        *self.keepalive_handle.lock().await = Some(handle);
    }

    /// Handle incoming message
    async fn handle_message(
        payload: DoipPayload,
        incoming_tx: &broadcast::Sender<IncomingMessage>,
        ecu_channels: &RwLock<HashMap<u16, broadcast::Sender<IncomingMessage>>>,
        source_address: u16,
        conn: &mut DoIpConnection,
    ) {
        match payload {
            DoipPayload::DiagnosticMessage(diag) => {
                let ecu_addr = u16::from_be_bytes(diag.source_address);
                let msg = IncomingMessage {
                    timestamp: Instant::now(),
                    data: diag.message.to_vec(),
                    source: AddressInfo {
                        tx_id: source_address as u32,
                        rx_id: ecu_addr as u32,
                    },
                };
                let _ = incoming_tx.send(msg.clone());
                if let Some(tx) = ecu_channels.read().await.get(&ecu_addr) {
                    let _ = tx.send(msg);
                }
            }
            DoipPayload::AliveCheckRequest(_) => {
                let _ = conn
                    .send(DoipPayload::AliveCheckResponse(AliveCheckResponse {
                        source_address: source_address.to_be_bytes(),
                    }))
                    .await;
            }
            DoipPayload::DiagnosticMessageNack(n) => warn!(?n.nack_code, "Diag NACK"),
            DoipPayload::GenericNack(n) => warn!(?n.nack_code, "Generic NACK"),
            _ => {}
        }
    }

    /// Send diagnostic message
    async fn send_diagnostic(&self, target: u16, data: &[u8]) -> Result<(), TransportError> {
        let payload = DoipPayload::DiagnosticMessage(DiagnosticMessage {
            source_address: self.config.source_address.to_be_bytes(),
            target_address: target.to_be_bytes(),
            message: data.to_vec().into(),
        });
        self.connection
            .lock()
            .await
            .as_mut()
            .ok_or(TransportError::ConnectionClosed)?
            .send(payload)
            .await
    }

    /// Wait for response matching request SID
    async fn wait_for_response(
        &self,
        sid: u8,
        timeout: Duration,
    ) -> Result<Vec<u8>, TransportError> {
        let mut rx = self.incoming_tx.subscribe();
        let expected = sid.wrapping_add(0x40);
        let start = Instant::now();

        loop {
            let remaining = timeout.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                return Err(TransportError::Timeout("Response timeout".into()));
            }

            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Ok(msg)) => {
                    if let Some(&first) = msg.data.first() {
                        if first == expected {
                            return Ok(msg.data);
                        }
                        if first == 0x7F && msg.data.get(1) == Some(&sid) {
                            if msg.data.get(2) == Some(&0x78) {
                                continue; // response pending
                            }
                            return Ok(msg.data);
                        }
                    }
                }
                Ok(Err(_)) => return Err(TransportError::ConnectionClosed),
                Err(_) => return Err(TransportError::Timeout("Response timeout".into())),
            }
        }
    }

    /// Send to specific ECU and wait for response
    pub async fn send_receive_to(
        &self,
        target: u16,
        request: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>, TransportError> {
        if !self.connected.load(Ordering::SeqCst) {
            return Err(TransportError::ConnectionClosed);
        }
        self.add_ecu(target).await;
        self.send_diagnostic(target, request).await?;
        self.wait_for_response(request.first().copied().unwrap_or(0), timeout)
            .await
    }

    pub fn is_tls(&self) -> bool {
        self.use_tls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl TransportAdapter for DoIpAdapter {
    async fn send_receive(
        &self,
        request: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>, TransportError> {
        self.send_receive_to(self.config.target_address, request, timeout)
            .await
    }

    async fn send(&self, request: &[u8]) -> Result<(), TransportError> {
        if !self.connected.load(Ordering::SeqCst) {
            return Err(TransportError::ConnectionClosed);
        }
        self.send_diagnostic(self.config.target_address, request)
            .await
    }

    fn subscribe(&self) -> broadcast::Receiver<IncomingMessage> {
        self.incoming_tx.subscribe()
    }

    async fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    async fn reconnect(&self) -> Result<(), TransportError> {
        self.connected.store(false, Ordering::SeqCst);
        *self.connection.lock().await = None;
        if let Some(h) = self.receiver_handle.lock().await.take() {
            h.abort();
        }
        if let Some(h) = self.keepalive_handle.lock().await.take() {
            h.abort();
        }
        self.connect_with_retry(MAX_RECONNECT_ATTEMPTS).await
    }

    fn address_info(&self) -> AddressInfo {
        self.address_info.clone()
    }
}

impl Drop for DoIpAdapter {
    fn drop(&mut self) {
        self.connected.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_address_info() {
        let info = AddressInfo {
            tx_id: 0x0E80,
            rx_id: 0x0010,
        };
        assert_eq!(info.tx_id, 0x0E80);
    }
}
