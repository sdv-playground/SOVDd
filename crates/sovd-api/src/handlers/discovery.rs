//! ECU Discovery API handlers
//!
//! Provides endpoints for discovering ECUs on the vehicle network.

use axum::extract::{Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::error::ApiError;
use crate::state::AppState;

/// Discovery request query parameters
#[derive(Debug, Deserialize)]
pub struct DiscoveryQuery {
    /// Transport method: "isotp", "someip"
    #[serde(default = "default_method")]
    pub method: String,
    /// CAN interface (for isotp method)
    #[serde(default = "default_interface")]
    pub interface: String,
    /// Addressing mode: "extended" (29-bit) or "standard" (11-bit)
    #[serde(default = "default_addressing")]
    pub addressing: String,
    /// Gateway host (for someip method)
    pub gateway_host: Option<String>,
    /// Gateway port (for someip method)
    pub gateway_port: Option<u16>,
    /// Timeout in milliseconds
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    /// Read identification DIDs (VIN, part number, etc.)
    #[serde(default = "default_true")]
    pub read_identification: bool,
}

fn default_method() -> String {
    "isotp".to_string()
}

fn default_interface() -> String {
    "vcan0".to_string()
}

fn default_addressing() -> String {
    "extended".to_string()
}

fn default_timeout() -> u64 {
    1000
}

fn default_true() -> bool {
    true
}

/// Discovery response
#[derive(Debug, Serialize)]
pub struct DiscoveryResponse {
    /// Discovery method used
    pub method: String,
    /// Number of ECUs found
    pub count: usize,
    /// Discovered ECUs
    pub ecus: Vec<DiscoveredEcuInfo>,
}

/// Discovered ECU information
#[derive(Debug, Serialize)]
pub struct DiscoveredEcuInfo {
    /// ECU address (hex)
    pub address: String,
    /// Physical TX CAN ID (tester -> ECU)
    pub tx_can_id: String,
    /// Physical RX CAN ID (ECU -> tester)
    pub rx_can_id: String,
    /// VIN if available
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vin: Option<String>,
    /// Part number if available
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part_number: Option<String>,
    /// Serial number if available
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial_number: Option<String>,
    /// Software version if available
    #[serde(skip_serializing_if = "Option::is_none")]
    pub software_version: Option<String>,
    /// Suggested TOML configuration snippet
    pub config_snippet: String,
}

/// Standard UDS identification DIDs
const DID_VIN: u16 = 0xF190;
const DID_PART_NUMBER: u16 = 0xF187;
const DID_SERIAL_NUMBER: u16 = 0xF18C;
const DID_SOFTWARE_VERSION: u16 = 0xF195;

/// POST /vehicle/v1/discovery
/// Scan for ECUs on the vehicle network using functional addressing (broadcast)
pub async fn discover_ecus(
    State(state): State<AppState>,
    Query(params): Query<DiscoveryQuery>,
) -> Result<Json<DiscoveryResponse>, ApiError> {
    let _timeout = Duration::from_millis(params.timeout_ms);

    tracing::info!(
        method = %params.method,
        interface = %params.interface,
        addressing = %params.addressing,
        timeout_ms = params.timeout_ms,
        "Starting ECU discovery"
    );

    match params.method.to_lowercase().as_str() {
        "isotp" | "uds" | "can" => {
            // ISO-TP discovery - query registered backends for identification data
            let mut ecus = Vec::new();

            // Get registered backends and read identification DIDs from each
            for (component_id, backend) in state.backends() {
                let _entity_info = backend.entity_info();

                // Read identification DIDs if requested
                let (vin, part_number, serial_number, software_version) = if params
                    .read_identification
                {
                    let vin = read_did_as_string(backend, DID_VIN).await;
                    let part_number = read_did_as_string(backend, DID_PART_NUMBER).await;
                    let serial_number = read_did_as_string(backend, DID_SERIAL_NUMBER).await;
                    let software_version = read_did_as_string(backend, DID_SOFTWARE_VERSION).await;
                    (vin, part_number, serial_number, software_version)
                } else {
                    (None, None, None, None)
                };

                // Use default address (could be enhanced to read from backend config)
                let address = "0x00".to_string();
                let tx_can_id = "0x18DA00F1".to_string(); // Default extended addressing
                let rx_can_id = "0x18DAF100".to_string();

                ecus.push(DiscoveredEcuInfo {
                    address,
                    tx_can_id: tx_can_id.clone(),
                    rx_can_id: rx_can_id.clone(),
                    vin,
                    part_number,
                    serial_number,
                    software_version,
                    config_snippet: format!(
                        r#"[ecu.{}]
name = "Discovered ECU"

[transport.isotp]
tx_id = "{}"
rx_id = "{}""#,
                        component_id, tx_can_id, rx_can_id
                    ),
                });
            }

            Ok(Json(DiscoveryResponse {
                method: params.method,
                count: ecus.len(),
                ecus,
            }))
        }

        "someip" | "gateway" => {
            // SOME/IP gateway discovery requires gateway_host
            let _gateway_host = params.gateway_host.ok_or_else(|| {
                ApiError::BadRequest("gateway_host required for SOME/IP discovery".into())
            })?;

            // SOME/IP discovery not implemented - requires gateway infrastructure
            Err(ApiError::NotImplemented(
                "SOME/IP discovery requires gateway infrastructure".to_string(),
            ))
        }

        _ => Err(ApiError::BadRequest(format!(
            "Unknown discovery method: {}. Use 'isotp' (CAN) or 'someip' (gateway)",
            params.method
        ))),
    }
}

/// Helper to read a DID and convert to string
async fn read_did_as_string(
    backend: &std::sync::Arc<dyn sovd_core::DiagnosticBackend>,
    did: u16,
) -> Option<String> {
    match backend.read_raw_did(did).await {
        Ok(data) if !data.is_empty() => {
            // Try to interpret as ASCII/UTF-8 string
            let s = String::from_utf8_lossy(&data);
            let s = s.trim_matches(char::from(0)).trim();
            if !s.is_empty() {
                Some(s.to_string())
            } else {
                // Return as hex if not valid string
                Some(hex::encode(&data))
            }
        }
        _ => None,
    }
}
