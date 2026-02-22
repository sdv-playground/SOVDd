//! Integration tests for SOVD server
//!
//! This crate contains end-to-end tests that exercise the full stack:
//! - HTTP API layer
//! - UDS protocol handling
//! - Transport (SocketCAN/DoIP)
//!
//! # Running Tests
//!
//! Most tests require a virtual CAN interface (vcan0):
//!
//! ```bash
//! # Set up vcan0 (requires sudo)
//! sudo modprobe vcan
//! sudo ip link add dev vcan0 type vcan
//! sudo ip link set up vcan0
//!
//! # Run tests (single-threaded due to shared resources)
//! cargo test -p sovd-tests -- --test-threads=1
//! ```
//!
//! # Test Structure
//!
//! - `e2e_test.rs` - Full stack tests with example-ecu simulator
//! - `gateway_e2e_test.rs` - Gateway aggregation tests
//! - `api_integration_test.rs` - API tests with mock backend

// This crate only contains tests, no library code
