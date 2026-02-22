//! SocketCAN adapter using ISO-TP for UDS communication

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use parking_lot::Mutex;
use socketcan::ExtendedId;
use socketcan_isotp::IsoTpSocket;
use tokio::sync::broadcast::{self, error as broadcast_error};
use tokio::task::JoinHandle;

use crate::config::SocketCanConfig;
use crate::transport::{AddressInfo, IncomingMessage, TransportAdapter, TransportError};

/// SocketCAN adapter using ISO-TP for UDS communication
pub struct SocketCanAdapter {
    config: SocketCanConfig,
    socket: Arc<Mutex<IsoTpSocket>>,
    address_info: AddressInfo,
    connected: AtomicBool,
    incoming_tx: broadcast::Sender<IncomingMessage>,
    listener_handle: Mutex<Option<JoinHandle<()>>>,
}

impl SocketCanAdapter {
    pub async fn new(config: &SocketCanConfig) -> Result<Self, TransportError> {
        let tx_id = parse_can_id(&config.isotp.tx_id)?;
        let rx_id = parse_can_id(&config.isotp.rx_id)?;

        let mut socket = Self::create_socket(config, tx_id, rx_id)?;

        // Drain any stale data from the socket (from previous sessions/processes)
        Self::drain_socket(&mut socket);

        let (incoming_tx, _) = broadcast::channel(1024);

        let adapter = Self {
            config: config.clone(),
            socket: Arc::new(Mutex::new(socket)),
            address_info: AddressInfo { tx_id, rx_id },
            connected: AtomicBool::new(true),
            incoming_tx,
            listener_handle: Mutex::new(None),
        };

        // Start background listener for incoming messages
        adapter.start_listener();

        Ok(adapter)
    }

    /// Drain any pending data from the socket to clear stale messages
    fn drain_socket(socket: &mut IsoTpSocket) {
        loop {
            match socket.read() {
                Ok(data) if !data.is_empty() => {
                    tracing::debug!(data = ?data, "Drained stale message from socket");
                }
                Ok(_) | Err(_) => {
                    // No more data or error (likely WouldBlock on non-blocking socket)
                    break;
                }
            }
        }
    }

    fn create_socket(
        config: &SocketCanConfig,
        tx_id: u32,
        rx_id: u32,
    ) -> Result<IsoTpSocket, TransportError> {
        // Convert u32 to ExtendedId for 29-bit CAN IDs
        let ext_rx_id = ExtendedId::new(rx_id).ok_or_else(|| {
            TransportError::InvalidConfig(format!("Invalid extended CAN ID: 0x{:X}", rx_id))
        })?;
        let ext_tx_id = ExtendedId::new(tx_id).ok_or_else(|| {
            TransportError::InvalidConfig(format!("Invalid extended CAN ID: 0x{:X}", tx_id))
        })?;

        let socket = IsoTpSocket::open(&config.interface, ext_rx_id, ext_tx_id).map_err(|e| {
            TransportError::ConnectionFailed(format!("Failed to open ISO-TP socket: {}", e))
        })?;

        // Set socket to non-blocking for async operation
        socket.set_nonblocking(true).map_err(|e| {
            TransportError::InvalidConfig(format!("Failed to set non-blocking: {}", e))
        })?;

        Ok(socket)
    }

    fn start_listener(&self) {
        let socket = self.socket.clone();
        let incoming_tx = self.incoming_tx.clone();
        let address_info = self.address_info.clone();
        let connected = Arc::new(AtomicBool::new(true));
        let connected_clone = connected.clone();

        // Store the connected flag so we can stop the listener later
        // Note: This replaces any previous listener's flag

        let handle = tokio::task::spawn_blocking(move || {
            while connected_clone.load(Ordering::SeqCst) {
                let mut socket_guard = socket.lock();
                match socket_guard.read() {
                    Ok(data) if !data.is_empty() => {
                        tracing::debug!(data = ?data, "Incoming message received");
                        let msg = IncomingMessage {
                            timestamp: Instant::now(),
                            data: data.to_vec(),
                            source: address_info.clone(),
                        };

                        if incoming_tx.send(msg).is_err() {
                            // No receivers, but that's okay
                        }
                    }
                    Ok(_) => {
                        // No data, sleep briefly
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        // Non-blocking socket, no data available
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(e) => {
                        tracing::error!(?e, "SocketCAN read error");
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
                drop(socket_guard);
            }
            tracing::debug!("SocketCAN listener stopped");
        });

        *self.listener_handle.lock() = Some(handle);
    }
}

#[async_trait]
impl TransportAdapter for SocketCanAdapter {
    async fn send_receive(
        &self,
        request: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>, TransportError> {
        if !self.connected.load(Ordering::SeqCst) {
            return Err(TransportError::ConnectionClosed);
        }

        // Subscribe to incoming messages BEFORE sending
        let mut rx = self.incoming_tx.subscribe();

        // Send the request
        self.send(request).await?;

        // Expected response service ID (request SID + 0x40)
        let request_sid = request.first().copied().unwrap_or(0);
        let expected_positive = request_sid + 0x40;

        // Wait for matching response
        let deadline = Instant::now() + timeout;

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(TransportError::Timeout("Response timeout".to_string()));
            }

            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Ok(msg)) => {
                    // Check if this is the response we're waiting for
                    if let Some(&first_byte) = msg.data.first() {
                        // Positive response (SID + 0x40)
                        if first_byte == expected_positive {
                            return Ok(msg.data);
                        }
                        // Negative response (0x7F)
                        if first_byte == 0x7F {
                            if msg.data.get(1) == Some(&request_sid) {
                                return Ok(msg.data);
                            }
                        }
                        // Otherwise, it's a different message (e.g., periodic data)
                        // Continue waiting for our response
                        tracing::debug!(
                            data = ?msg.data,
                            expected = expected_positive,
                            "Ignoring non-matching response"
                        );
                    }
                }
                Ok(Err(broadcast_error::RecvError::Lagged(_))) => {
                    // Missed some messages, continue
                    continue;
                }
                Ok(Err(broadcast_error::RecvError::Closed)) => {
                    return Err(TransportError::ConnectionClosed);
                }
                Err(_) => {
                    // Timeout
                    return Err(TransportError::Timeout("Response timeout".to_string()));
                }
            }
        }
    }

    async fn send(&self, request: &[u8]) -> Result<(), TransportError> {
        if !self.connected.load(Ordering::SeqCst) {
            return Err(TransportError::ConnectionClosed);
        }

        let socket = self.socket.clone();
        let request = request.to_vec();

        tokio::task::spawn_blocking(move || {
            let socket_guard = socket.lock();
            socket_guard
                .write(&request)
                .map_err(|e| TransportError::SendFailed(e.to_string()))
        })
        .await
        .map_err(|e| TransportError::SendFailed(format!("Task join error: {}", e)))??;

        Ok(())
    }

    fn subscribe(&self) -> broadcast::Receiver<IncomingMessage> {
        self.incoming_tx.subscribe()
    }

    async fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    async fn reconnect(&self) -> Result<(), TransportError> {
        let tx_id = parse_can_id(&self.config.isotp.tx_id)?;
        let rx_id = parse_can_id(&self.config.isotp.rx_id)?;

        let socket = Self::create_socket(&self.config, tx_id, rx_id)?;
        *self.socket.lock() = socket;
        self.connected.store(true, Ordering::SeqCst);

        self.start_listener();

        Ok(())
    }

    fn address_info(&self) -> AddressInfo {
        self.address_info.clone()
    }
}

impl Drop for SocketCanAdapter {
    fn drop(&mut self) {
        self.connected.store(false, Ordering::SeqCst);
    }
}

/// Parse a CAN ID from string (supports hex with 0x prefix)
fn parse_can_id(s: &str) -> Result<u32, TransportError> {
    let s = s.trim();
    let (s, radix) = if s.starts_with("0x") || s.starts_with("0X") {
        (&s[2..], 16)
    } else {
        (s, 10)
    };

    u32::from_str_radix(s, radix)
        .map_err(|e| TransportError::InvalidConfig(format!("Invalid CAN ID '{}': {}", s, e)))
}
