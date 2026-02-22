//! Session management for UDS communication
//!
//! This module handles diagnostic session control, security access,
//! and keepalive for maintaining ECU communication sessions.

mod manager;

pub use manager::{LinkState, SecurityAccessState, SessionError, SessionManager};

/// UDS session state
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    /// Default session (0x01)
    Default,
    /// Programming session (0x02)
    Programming,
    /// Extended diagnostic session (0x03)
    Extended,
    /// Engineering/development session (0x60) with security level
    Engineering { security_level: u8 },
}

impl Default for SessionState {
    fn default() -> Self {
        Self::Default
    }
}
