//! Session manager for UDS communication

use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{debug, error, info};

use super::SessionState;
use crate::config::SessionConfig;
use crate::transport::TransportAdapter;
use crate::uds::{ServiceIds, UdsService};

/// Security access state for tracking two-step client-driven flow
#[derive(Debug, Clone)]
pub struct SecurityAccessState {
    /// Security level being accessed
    pub level: u8,
    /// Pending seed (if seed was requested but key not yet sent)
    pub pending_seed: Option<Vec<u8>>,
    /// Whether security is currently unlocked
    pub unlocked: bool,
}

impl Default for SecurityAccessState {
    fn default() -> Self {
        Self {
            level: 0,
            pending_seed: None,
            unlocked: false,
        }
    }
}

/// Link control state for tracking baud rate transitions
#[derive(Debug, Clone)]
pub struct LinkState {
    /// Current baud rate in bits per second
    pub current_baud_rate: u32,
    /// Pending baud rate (verified but not yet transitioned)
    pub pending_baud_rate: Option<u32>,
}

impl Default for LinkState {
    fn default() -> Self {
        Self {
            current_baud_rate: 500000, // Default CAN 500kbps
            pending_baud_rate: None,
        }
    }
}

/// Manages UDS session state, keepalive, and security access
pub struct SessionManager {
    transport: Arc<dyn TransportAdapter>,
    config: SessionConfig,
    uds: UdsService,
    current_state: RwLock<SessionState>,
    security_state: RwLock<SecurityAccessState>,
    link_state: RwLock<LinkState>,
    keepalive_handle: Mutex<Option<JoinHandle<()>>>,
}

impl SessionManager {
    pub fn new(transport: Arc<dyn TransportAdapter>, config: SessionConfig) -> Self {
        Self::with_service_ids(transport, config, ServiceIds::default())
    }

    /// Create a session manager with custom service IDs (for OEM-specific implementations)
    pub fn with_service_ids(
        transport: Arc<dyn TransportAdapter>,
        config: SessionConfig,
        service_ids: ServiceIds,
    ) -> Self {
        let uds = UdsService::with_service_ids(transport.clone(), service_ids);
        Self {
            transport,
            config,
            uds,
            current_state: RwLock::new(SessionState::Default),
            security_state: RwLock::new(SecurityAccessState::default()),
            link_state: RwLock::new(LinkState::default()),
            keepalive_handle: Mutex::new(None),
        }
    }

    /// Get the current session state
    pub fn current_state(&self) -> SessionState {
        self.current_state.read().clone()
    }

    /// Get the current security access state
    pub fn security_state(&self) -> SecurityAccessState {
        self.security_state.read().clone()
    }

    /// Get the current link state
    pub fn link_state(&self) -> LinkState {
        self.link_state.read().clone()
    }

    /// Set the pending baud rate
    pub fn set_pending_baud_rate(&self, baud_rate: Option<u32>) {
        self.link_state.write().pending_baud_rate = baud_rate;
    }

    /// Set the current baud rate
    pub fn set_current_baud_rate(&self, baud_rate: u32) {
        self.link_state.write().current_baud_rate = baud_rate;
    }

    /// Request a seed for security access (UDS 0x27 step 1)
    pub async fn request_security_seed(&self, level: u8) -> Result<Vec<u8>, SessionError> {
        let seed = self
            .uds
            .security_access_request_seed(level)
            .await
            .map_err(|e| SessionError::SecurityAccessFailed(format!("Request seed: {}", e)))?;

        if seed.is_empty() || seed.iter().all(|&b| b == 0) {
            // Zero seed means already unlocked
            debug!("Security already unlocked (zero seed)");
            let mut state = self.security_state.write();
            state.level = level;
            state.pending_seed = None;
            state.unlocked = true;
            return Ok(vec![]);
        }

        // Store the pending seed
        {
            let mut state = self.security_state.write();
            state.level = level;
            state.pending_seed = Some(seed.clone());
            state.unlocked = false;
        }

        info!(level, seed_len = seed.len(), "Security seed requested");
        Ok(seed)
    }

    /// Send a key for security access (UDS 0x27 step 2)
    pub async fn send_security_key(&self, level: u8, key: &[u8]) -> Result<(), SessionError> {
        // Verify we have a pending seed for this level
        {
            let state = self.security_state.read();
            if state.pending_seed.is_none() {
                return Err(SessionError::SecurityAccessFailed(
                    "No pending seed - call request_security_seed first".to_string(),
                ));
            }
            if state.level != level {
                return Err(SessionError::SecurityAccessFailed(format!(
                    "Level mismatch: expected {}, got {}",
                    state.level, level
                )));
            }
        }

        // Send key to ECU
        self.uds
            .security_access_send_key(level, key)
            .await
            .map_err(|e| SessionError::SecurityAccessFailed(format!("Send key: {}", e)))?;

        // Update state
        {
            let mut state = self.security_state.write();
            state.pending_seed = None;
            state.unlocked = true;
        }

        info!(level, "Security access granted via client-provided key");
        Ok(())
    }

    /// Get available security levels (from config)
    pub fn available_security_levels(&self) -> Vec<u8> {
        if let Some(ref security) = self.config.security {
            vec![security.level]
        } else {
            vec![]
        }
    }

    /// Get the current UDS session ID
    pub fn current_session_id(&self) -> u8 {
        match *self.current_state.read() {
            SessionState::Default => 0x01,
            SessionState::Programming => 0x02,
            SessionState::Extended => 0x03,
            SessionState::Engineering { .. } => 0x60,
        }
    }

    /// Change the diagnostic session (UDS 0x10)
    pub async fn change_session(&self, session_id: u8) -> Result<(), SessionError> {
        // Skip if already in the requested session — avoids resetting security
        // access state, which per ISO 14229 is cleared on every session transition.
        if self.current_session_id() == session_id {
            info!(
                session_id = format!("0x{:02X}", session_id),
                "Already in requested session, skipping (security preserved)"
            );
            return Ok(());
        }

        self.diagnostic_session_control(session_id).await?;

        // Update internal state
        let new_state = match session_id {
            0x01 => {
                self.stop_keepalive().await;
                SessionState::Default
            }
            0x02 => {
                self.start_keepalive().await;
                SessionState::Programming
            }
            0x03 => {
                self.start_keepalive().await;
                SessionState::Extended
            }
            _ => {
                // Custom sessions (like 0x60) treated as engineering
                self.start_keepalive().await;
                SessionState::Engineering { security_level: 0 }
            }
        };

        *self.current_state.write() = new_state;

        // Per ISO 14229: security access resets on session change
        *self.security_state.write() = SecurityAccessState::default();
        info!(
            session_id = format!("0x{:02X}", session_id),
            "Session changed (security re-locked)"
        );

        Ok(())
    }

    /// Ensure we're in the default diagnostic session
    pub async fn ensure_default_session(&self) -> Result<(), SessionError> {
        let current = self.current_state();
        if current == SessionState::Default {
            return Ok(());
        }

        self.stop_keepalive().await;
        self.diagnostic_session_control(self.config.default_session)
            .await?;
        *self.current_state.write() = SessionState::Default;

        info!("Transitioned to default session");
        Ok(())
    }

    /// Ensure we're in an extended diagnostic session
    pub async fn ensure_extended_session(&self) -> Result<(), SessionError> {
        let current = self.current_state();
        if matches!(
            current,
            SessionState::Programming | SessionState::Extended | SessionState::Engineering { .. }
        ) {
            return Ok(());
        }

        self.diagnostic_session_control(self.config.extended_session)
            .await?;
        *self.current_state.write() = SessionState::Extended;

        self.start_keepalive().await;
        info!("Transitioned to extended session");
        Ok(())
    }

    /// Ensure we're in the engineering session (required for data logging)
    pub async fn ensure_engineering_session(&self) -> Result<(), SessionError> {
        let current = self.current_state();
        if matches!(current, SessionState::Engineering { .. }) {
            return Ok(());
        }

        // First, ensure we're in extended session
        if current == SessionState::Default {
            self.diagnostic_session_control(self.config.extended_session)
                .await?;
            debug!("Transitioned to extended session");
        }

        // Check if security access is required
        if let Some(ref security_config) = self.config.security {
            if security_config.enabled {
                let security_state = self.security_state.read();
                if !security_state.unlocked {
                    return Err(SessionError::SecurityAccessFailed(
                        "Security access required. Use PUT /modes/security to authenticate first."
                            .to_string(),
                    ));
                }
                debug!(level = security_state.level, "Security already unlocked");
            }
        }

        // Transition to engineering session
        self.diagnostic_session_control(self.config.engineering_session)
            .await?;

        *self.current_state.write() = SessionState::Engineering {
            security_level: self.config.security.as_ref().map(|s| s.level).unwrap_or(0),
        };

        self.start_keepalive().await;
        info!("Transitioned to engineering session");
        Ok(())
    }

    async fn diagnostic_session_control(&self, session: u8) -> Result<(), SessionError> {
        tracing::info!(
            "diagnostic_session_control: sending UDS 0x10 with session={:#04x}",
            session
        );
        self.uds
            .diagnostic_session_control(session)
            .await
            .map_err(|e| {
                SessionError::TransitionFailed(format!("Session 0x{:02X}: {}", session, e))
            })?;
        Ok(())
    }

    async fn start_keepalive(&self) {
        if !self.config.keepalive.enabled {
            return;
        }

        self.stop_keepalive().await;

        let transport = self.transport.clone();
        let interval = Duration::from_millis(self.config.keepalive.interval_ms);
        let suppress_response = self.config.keepalive.suppress_response;

        let handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            let request = if suppress_response {
                vec![0x3E, 0x80]
            } else {
                vec![0x3E, 0x00]
            };

            loop {
                ticker.tick().await;

                if suppress_response {
                    if let Err(e) = transport.send(&request).await {
                        error!(?e, "Tester present send failed");
                    }
                } else {
                    match transport
                        .send_receive(&request, Duration::from_millis(1000))
                        .await
                    {
                        Ok(_) => {
                            debug!("Tester present OK");
                        }
                        Err(e) => {
                            error!(?e, "Tester present failed");
                        }
                    }
                }
            }
        });

        *self.keepalive_handle.lock().await = Some(handle);
        debug!(
            interval_ms = self.config.keepalive.interval_ms,
            "Keepalive started"
        );
    }

    /// Reset tracked session/security state to default after an ECU reset.
    ///
    /// Per ISO 14229, ECU reset returns the ECU to default session with
    /// security locked. This updates the SessionManager's bookkeeping
    /// without sending any UDS commands (the ECU may be rebooting).
    /// Handles both API-triggered resets and external power cycles.
    pub async fn notify_ecu_reset(&self) {
        self.stop_keepalive().await;
        *self.current_state.write() = SessionState::Default;
        *self.security_state.write() = SecurityAccessState::default();
        info!("Session state reset to default (ECU reset detected)");
    }

    async fn stop_keepalive(&self) {
        let mut handle = self.keepalive_handle.lock().await;
        if let Some(h) = handle.take() {
            h.abort();
            debug!("Keepalive stopped");
        }
    }
}

impl Drop for SessionManager {
    fn drop(&mut self) {
        if let Some(handle) = self.keepalive_handle.get_mut().take() {
            handle.abort();
        }
    }
}

/// Session management errors
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("Session transition failed: {0}")]
    TransitionFailed(String),

    #[error("Security access failed: {0}")]
    SecurityAccessFailed(String),
}
