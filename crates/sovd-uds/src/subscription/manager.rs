//! Stream manager for UDS periodic data
//!
//! Handles UDS 0x2A ReadDataByPeriodicIdentifier for efficient streaming.
//! Returns raw DID data - conversions are applied at the API layer.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use chrono::Utc;
use parking_lot::RwLock;
use sovd_core::DataPoint;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::config::UdsBackendConfig;
use crate::transport::{IncomingMessage, TransportAdapter};
use crate::uds::{PeriodicRate, ServiceIds, UdsService};

/// Parse a hex DID string to u16
fn parse_did(did_str: &str) -> Option<u16> {
    let cleaned = did_str.trim_start_matches("0x").trim_start_matches("0X");
    u16::from_str_radix(cleaned, 16).ok()
}

/// A subscription to periodic data
#[derive(Debug, Clone)]
pub struct StreamSubscription {
    pub id: String,
    /// DIDs to stream (as hex strings)
    pub dids: Vec<String>,
    pub rate_hz: u32,
}

/// Manages streaming subscriptions using UDS 0x2A
pub struct StreamManager {
    transport: Arc<dyn TransportAdapter>,
    #[allow(dead_code)]
    config: UdsBackendConfig,
    uds: UdsService,

    /// Active subscriptions
    subscriptions: Arc<RwLock<HashMap<String, SubscriptionState>>>,

    /// Broadcast channel for each subscription
    streams: Arc<RwLock<HashMap<String, broadcast::Sender<DataPoint>>>>,

    /// Current periodic configuration (merged from all subscriptions)
    active_periodic: RwLock<ActivePeriodicConfig>,

    /// Sequence counter for data points
    sequence: Arc<AtomicU64>,

    /// Background listener task handle
    listener_handle: RwLock<Option<JoinHandle<()>>>,
}

struct SubscriptionState {
    subscription: StreamSubscription,
    did_set: HashSet<u16>,
}

#[derive(Debug, Default)]
struct ActivePeriodicConfig {
    /// Active periodic DIDs
    active_dids: HashSet<u16>,
}

impl StreamManager {
    pub fn new(transport: Arc<dyn TransportAdapter>, config: UdsBackendConfig) -> Self {
        // Create UDS service with configured service IDs (for OEM variants like Vortex Motors)
        let service_ids = ServiceIds::from_overrides(&config.service_overrides);
        let uds = UdsService::with_service_ids(transport.clone(), service_ids);

        let manager = Self {
            transport,
            config,
            uds,
            subscriptions: Arc::new(RwLock::new(HashMap::new())),
            streams: Arc::new(RwLock::new(HashMap::new())),
            active_periodic: RwLock::new(ActivePeriodicConfig::default()),
            sequence: Arc::new(AtomicU64::new(0)),
            listener_handle: RwLock::new(None),
        };

        // Start the incoming message listener
        manager.start_listener();

        manager
    }

    /// Create a new subscription and return a receiver for the stream
    ///
    /// DIDs are provided as hex strings (e.g., "F405", "0xF40C")
    pub async fn subscribe(
        &self,
        dids: Vec<String>,
        rate_hz: u32,
    ) -> Result<broadcast::Receiver<DataPoint>, StreamError> {
        // Parse and validate DIDs
        let mut did_set = HashSet::new();
        for did_str in &dids {
            let did = parse_did(did_str).ok_or_else(|| StreamError::InvalidDid(did_str.clone()))?;
            did_set.insert(did);
        }

        // Create subscription
        let id = Uuid::new_v4().to_string();
        let subscription = StreamSubscription {
            id: id.clone(),
            dids: dids.clone(),
            rate_hz,
        };

        // Create broadcast channel for this subscription
        let (tx, rx) = broadcast::channel(1024);

        // Store subscription state
        let state = SubscriptionState {
            subscription: subscription.clone(),
            did_set: did_set.clone(),
        };

        {
            self.subscriptions.write().insert(id.clone(), state);
            self.streams.write().insert(id.clone(), tx);
        }

        // Reconfigure ECU periodic
        if let Err(e) = self.reconfigure_periodic().await {
            warn!(?e, "Failed to configure ECU periodic");
            // Clean up on failure
            self.subscriptions.write().remove(&id);
            self.streams.write().remove(&id);
            return Err(e);
        }

        info!(subscription_id = %id, dids = ?dids, %rate_hz, "Stream subscription created");

        Ok(rx)
    }

    /// Remove a subscription
    pub async fn unsubscribe(&self, id: &str) -> Result<(), StreamError> {
        {
            self.subscriptions.write().remove(id);
            self.streams.write().remove(id);
        }

        // Reconfigure ECU if needed
        self.reconfigure_periodic().await?;

        info!(subscription_id = %id, "Stream subscription removed");
        Ok(())
    }

    /// Get a receiver for an existing subscription
    pub fn get_stream(&self, id: &str) -> Option<broadcast::Receiver<DataPoint>> {
        self.streams.read().get(id).map(|tx| tx.subscribe())
    }

    /// Reconfigure ECU periodic based on all active subscriptions
    async fn reconfigure_periodic(&self) -> Result<(), StreamError> {
        debug!("Reconfiguring ECU periodic");

        // Collect all DIDs needed, grouped by rate
        let mut rate_groups: HashMap<u32, HashSet<u16>> = HashMap::new();

        {
            let subs = self.subscriptions.read();
            for state in subs.values() {
                let rate = state.subscription.rate_hz;
                let group = rate_groups.entry(rate).or_default();
                group.extend(&state.did_set);
            }
        }

        // Stop current periodic DIDs
        let active_dids_to_stop: Vec<u16> = {
            self.active_periodic
                .read()
                .active_dids
                .iter()
                .cloned()
                .collect()
        };

        for did in active_dids_to_stop {
            // For 0x2A, periodic IDs are typically 1-byte
            let pid = (did & 0xFF) as u8;
            if let Err(e) = self.uds.stop_periodic(&[pid]).await {
                warn!(?e, "Failed to stop periodic for DID 0x{:04X}", did);
            }
        }

        // Start new periodic configuration
        let mut active_dids = HashSet::new();

        for (rate_hz, dids) in &rate_groups {
            if dids.is_empty() {
                continue;
            }

            let rate = PeriodicRate::from(*rate_hz);
            let pids: Vec<u8> = dids.iter().map(|did| (*did & 0xFF) as u8).collect();

            match self.uds.start_periodic(rate, &pids).await {
                Ok(_) => {
                    active_dids.extend(dids);
                    debug!(rate_hz, dids = ?pids, "Started periodic");
                }
                Err(e) => {
                    error!(?e, rate_hz, "Failed to start periodic");
                    return Err(StreamError::UdsError(e.to_string()));
                }
            }
        }

        // Update stored config
        *self.active_periodic.write() = ActivePeriodicConfig { active_dids };

        Ok(())
    }

    /// Start the background listener for incoming ECU data
    fn start_listener(&self) {
        let mut incoming_rx = self.transport.subscribe();
        let subscriptions = self.subscriptions.clone();
        let streams = self.streams.clone();
        let sequence = self.sequence.clone();

        let handle = tokio::spawn(async move {
            loop {
                match incoming_rx.recv().await {
                    Ok(msg) => {
                        Self::handle_incoming_message(&msg, &subscriptions, &streams, &sequence);
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!(skipped = n, "Incoming message listener lagged");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        info!("Incoming message channel closed");
                        break;
                    }
                }
            }
        });

        *self.listener_handle.write() = Some(handle);
    }

    fn handle_incoming_message(
        msg: &IncomingMessage,
        subscriptions: &RwLock<HashMap<String, SubscriptionState>>,
        streams: &RwLock<HashMap<String, broadcast::Sender<DataPoint>>>,
        sequence: &AtomicU64,
    ) {
        // Parse incoming UDS message
        // Periodic data format (0x2A response): [DID_LO] [DATA...]
        // Note: The first byte is typically the low byte of the periodic identifier

        if msg.data.is_empty() {
            return;
        }

        let first_byte = msg.data[0];

        // Skip if this looks like a normal response (positive or negative)
        // Positive responses start with 0x40+ of the request SID
        // Negative responses start with 0x7F
        if first_byte == 0x7F || first_byte >= 0x40 {
            return;
        }

        // Try to match periodic identifier to a DID
        let did_lo = first_byte;
        let data = &msg.data[1..];

        // Find subscriptions that include this DID (matching low byte)
        let subs = subscriptions.read();
        let streams_guard = streams.read();

        for (sub_id, state) in subs.iter() {
            for &did in &state.did_set {
                if (did & 0xFF) as u8 == did_lo {
                    // Create data point with raw hex data
                    // Conversion will be applied at the API layer
                    let data_point = DataPoint {
                        id: format!("{:04X}", did),
                        value: serde_json::json!(hex::encode(data)),
                        unit: None,
                        timestamp: Utc::now(),
                    };

                    if let Some(tx) = streams_guard.get(sub_id) {
                        let _ = tx.send(data_point);
                    }
                    break;
                }
            }
        }

        let _ = sequence.fetch_add(1, Ordering::SeqCst);
    }
}

impl Drop for StreamManager {
    fn drop(&mut self) {
        if let Some(handle) = self.listener_handle.write().take() {
            handle.abort();
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StreamError {
    #[error("Invalid DID format: {0}")]
    InvalidDid(String),

    #[error("UDS error: {0}")]
    UdsError(String),

    #[error("Transport error: {0}")]
    TransportError(String),
}
