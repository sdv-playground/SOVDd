//! Subscription management for UDS periodic data streaming
//!
//! This module provides real-time data streaming using UDS 0x2A
//! (ReadDataByPeriodicIdentifier) for efficient ECU data collection.

mod manager;

pub use manager::{StreamError, StreamManager, StreamSubscription};
