//! CAN bus ECU auto-discovery via UDS functional addressing
//!
//! Broadcasts a TesterPresent request using UDS functional addressing
//! (`0x18DB33F1`) on a raw CAN socket. ECUs respond on their physical
//! address (`0x18DAF1xx`). For each discovered ECU, opens a temporary
//! ISO-TP connection to read standard identification DIDs.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use socketcan::{CanFrame, CanSocket, EmbeddedFrame, ExtendedId, Frame, Socket};
use socketcan_isotp::IsoTpSocket;
use tracing::{debug, info, warn};

use crate::transport::TransportError;

/// An ECU discovered via CAN bus scan
#[derive(Debug, Clone)]
pub struct DiscoveredEcu {
    /// Physical ECU address (low byte, 0x00–0xFF)
    pub address: u8,
    /// CAN interface the ECU was found on
    pub interface: String,
    /// Tester→ECU CAN ID (e.g., `0x18DA00F1`)
    pub tx_can_id: u32,
    /// ECU→Tester CAN ID (e.g., `0x18DAF100`)
    pub rx_can_id: u32,
    /// VIN (DID 0xF190)
    pub vin: Option<String>,
    /// ECU Part Number (DID 0xF187)
    pub part_number: Option<String>,
    /// ECU Serial Number (DID 0xF18C)
    pub serial_number: Option<String>,
    /// ECU Software Version (DID 0xF195)
    pub software_version: Option<String>,
}

/// Configuration for a CAN bus scan
#[derive(Debug, Clone)]
pub struct ScanConfig {
    /// CAN interface to scan (e.g., "vcan0", "can0")
    pub interface: String,
    /// How long to listen for broadcast responses (ms)
    pub timeout_ms: u64,
}

/// UDS functional broadcast CAN ID (29-bit, ISO 15765-2)
const FUNCTIONAL_CAN_ID: u32 = 0x18DB33F1;

/// ECU→Tester response prefix: `0x18DAF1xx`
const RESPONSE_PREFIX: u32 = 0x18DAF100;
const RESPONSE_MASK: u32 = 0xFFFFFF00;

/// Timeout for individual DID reads during identification
const DID_READ_TIMEOUT: Duration = Duration::from_millis(500);

/// Scan a CAN bus interface for ECUs using UDS functional addressing.
///
/// Sends a TesterPresent broadcast and collects responses. For each
/// responding ECU, reads standard identification DIDs over ISO-TP.
pub async fn scan_can_bus(config: &ScanConfig) -> Result<Vec<DiscoveredEcu>, TransportError> {
    let interface = config.interface.clone();
    let timeout = Duration::from_millis(config.timeout_ms);

    info!(
        interface = %interface,
        timeout_ms = config.timeout_ms,
        "Starting CAN bus ECU discovery scan"
    );

    // Phase 1: broadcast TesterPresent and collect responding addresses
    let addresses = {
        let iface = interface.clone();
        tokio::task::spawn_blocking(move || broadcast_tester_present(&iface, timeout))
            .await
            .map_err(|e| TransportError::SendFailed(format!("Scan task join error: {}", e)))??
    };

    if addresses.is_empty() {
        info!("No ECUs responded to functional broadcast");
        return Ok(Vec::new());
    }

    info!(count = addresses.len(), "ECUs responded to broadcast");

    // Phase 2: read identification DIDs from each discovered ECU
    let mut ecus = Vec::new();
    for &addr in &addresses {
        let iface = interface.clone();
        let iface_for_fallback = interface.clone();
        let ecu = tokio::task::spawn_blocking(move || read_ecu_identification(&iface, addr))
            .await
            .map_err(|e| TransportError::SendFailed(format!("DID read task join error: {}", e)))?;

        match ecu {
            Ok(ecu) => {
                info!(
                    address = format!("0x{:02X}", ecu.address),
                    vin = ?ecu.vin,
                    part_number = ?ecu.part_number,
                    sw_version = ?ecu.software_version,
                    "Identified ECU"
                );
                ecus.push(ecu);
            }
            Err(e) => {
                warn!(
                    address = format!("0x{:02X}", addr),
                    error = %e,
                    "Failed to read identification from ECU, registering with address only"
                );
                // Still register the ECU even without identification data
                ecus.push(DiscoveredEcu {
                    address: addr,
                    interface: iface_for_fallback,
                    tx_can_id: 0x18DA0000 | ((addr as u32) << 8) | 0xF1,
                    rx_can_id: 0x18DA0000 | (0xF1 << 8) | (addr as u32),
                    vin: None,
                    part_number: None,
                    serial_number: None,
                    software_version: None,
                });
            }
        }
    }

    info!(discovered = ecus.len(), "CAN bus scan complete");
    Ok(ecus)
}

/// Send a TesterPresent functional broadcast and collect unique ECU addresses.
fn broadcast_tester_present(interface: &str, timeout: Duration) -> Result<Vec<u8>, TransportError> {
    let socket = CanSocket::open(interface).map_err(|e| {
        TransportError::ConnectionFailed(format!(
            "Failed to open raw CAN socket on {}: {}",
            interface, e
        ))
    })?;

    socket
        .set_nonblocking(true)
        .map_err(|e| TransportError::InvalidConfig(format!("Failed to set non-blocking: {}", e)))?;

    // Build TesterPresent single-frame: [PCI=0x02] [SID=0x3E] [sub=0x00] [pad...]
    let request_data: [u8; 8] = [0x02, 0x3E, 0x00, 0xCC, 0xCC, 0xCC, 0xCC, 0xCC];

    let can_id = ExtendedId::new(FUNCTIONAL_CAN_ID)
        .ok_or_else(|| TransportError::InvalidConfig("Invalid functional CAN ID".to_string()))?;

    let frame = CanFrame::new(can_id, &request_data).expect("Valid CAN frame for TesterPresent");

    // Send the broadcast
    socket.write_frame(&frame).map_err(|e| {
        TransportError::SendFailed(format!("Failed to send TesterPresent broadcast: {}", e))
    })?;

    debug!("Sent TesterPresent broadcast on {}", interface);

    // Collect responses
    let mut seen_addresses = HashSet::new();
    let deadline = Instant::now() + timeout;

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }

        match socket.read_frame() {
            Ok(frame) => {
                let raw_id = frame.raw_id();

                // Check if this is a response from an ECU (0x18DAF1xx)
                if raw_id & RESPONSE_MASK == RESPONSE_PREFIX {
                    let ecu_addr = (raw_id & 0xFF) as u8;
                    let data = frame.data();

                    // Verify it's a positive TesterPresent response: [PCI] [0x7E] ...
                    if data.len() >= 2 && data[1] == 0x7E && seen_addresses.insert(ecu_addr) {
                        debug!(
                            address = format!("0x{:02X}", ecu_addr),
                            can_id = format!("0x{:08X}", raw_id),
                            "Discovered ECU"
                        );
                    }
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(_) => {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    let mut addresses: Vec<u8> = seen_addresses.into_iter().collect();
    addresses.sort();
    Ok(addresses)
}

/// Read identification DIDs from a single ECU via ISO-TP.
fn read_ecu_identification(interface: &str, ecu_addr: u8) -> Result<DiscoveredEcu, TransportError> {
    // Tester→ECU: 0x18DA{addr}F1, ECU→Tester: 0x18DAF1{addr}
    let tx_can_id = 0x18DA0000 | ((ecu_addr as u32) << 8) | 0xF1;
    let rx_can_id = 0x18DA0000 | (0xF1 << 8) | (ecu_addr as u32);

    let ext_tx = ExtendedId::new(tx_can_id).ok_or_else(|| {
        TransportError::InvalidConfig(format!("Invalid TX CAN ID: 0x{:08X}", tx_can_id))
    })?;
    let ext_rx = ExtendedId::new(rx_can_id).ok_or_else(|| {
        TransportError::InvalidConfig(format!("Invalid RX CAN ID: 0x{:08X}", rx_can_id))
    })?;

    // Note: IsoTpSocket::open(iface, rx_id, tx_id) — rx_id is what we listen on
    let mut socket = IsoTpSocket::open(interface, ext_rx, ext_tx).map_err(|e| {
        TransportError::ConnectionFailed(format!(
            "Failed to open ISO-TP to ECU 0x{:02X}: {}",
            ecu_addr, e
        ))
    })?;

    socket
        .set_nonblocking(true)
        .map_err(|e| TransportError::InvalidConfig(format!("Failed to set non-blocking: {}", e)))?;

    let vin = read_did_string(&mut socket, 0xF190);
    let part_number = read_did_string(&mut socket, 0xF187);
    let serial_number = read_did_string(&mut socket, 0xF18C);
    let software_version = read_did_string(&mut socket, 0xF195);

    Ok(DiscoveredEcu {
        address: ecu_addr,
        interface: interface.to_string(),
        tx_can_id,
        rx_can_id,
        vin,
        part_number,
        serial_number,
        software_version,
    })
}

/// Read a single DID and decode as a UTF-8 string. Returns None on any error.
fn read_did_string(socket: &mut IsoTpSocket, did: u16) -> Option<String> {
    let did_hi = (did >> 8) as u8;
    let did_lo = (did & 0xFF) as u8;

    // UDS ReadDataByIdentifier: [0x22, DID_HI, DID_LO]
    let request = [0x22, did_hi, did_lo];

    if let Err(e) = socket.write(&request) {
        debug!(did = format!("0x{:04X}", did), error = %e, "Failed to send DID read");
        return None;
    }

    // Wait for response with timeout
    let deadline = Instant::now() + DID_READ_TIMEOUT;

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            debug!(did = format!("0x{:04X}", did), "DID read timed out");
            return None;
        }

        match socket.read() {
            Ok(data) if !data.is_empty() => {
                // Positive response: [0x62, DID_HI, DID_LO, DATA...]
                if data.len() >= 4 && data[0] == 0x62 && data[1] == did_hi && data[2] == did_lo {
                    let value_bytes = &data[3..];
                    let value = String::from_utf8_lossy(value_bytes).trim().to_string();
                    if !value.is_empty() {
                        return Some(value);
                    }
                    return None;
                }
                // Negative response or wrong DID — skip
                if data[0] == 0x7F {
                    debug!(
                        did = format!("0x{:04X}", did),
                        nrc = format!("0x{:02X}", data.get(2).copied().unwrap_or(0)),
                        "DID not supported by ECU"
                    );
                    return None;
                }
                // Unexpected response, keep waiting
            }
            Ok(_) => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(e) => {
                debug!(did = format!("0x{:04X}", did), error = %e, "DID read error");
                return None;
            }
        }
    }
}
