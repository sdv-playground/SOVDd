//! DoIP Vehicle Discovery (VIR/VAM)

use std::net::SocketAddr;
use std::time::Duration;

use doip_definitions::payload::{
    DoipPayload, VehicleAnnouncementMessage, VehicleIdentificationRequest,
};
use doip_sockets::udp::UdpSocket;
use tracing::{debug, info};

use crate::transport::TransportError;

/// Discovered DoIP gateway
#[derive(Debug, Clone)]
pub struct DiscoveredGateway {
    pub ip: String,
    pub logical_address: u16,
    pub vin: [u8; 17],
    pub eid: [u8; 6],
}

impl DiscoveredGateway {
    pub fn vin_string(&self) -> String {
        String::from_utf8_lossy(&self.vin)
            .trim_matches(char::from(0))
            .to_string()
    }
}

impl From<(VehicleAnnouncementMessage, SocketAddr)> for DiscoveredGateway {
    fn from((vam, addr): (VehicleAnnouncementMessage, SocketAddr)) -> Self {
        Self {
            ip: addr.ip().to_string(),
            logical_address: u16::from_be_bytes(vam.logical_address),
            vin: vam.vin,
            eid: vam.eid,
        }
    }
}

/// Discover DoIP gateways via UDP broadcast
pub async fn discover_gateways(timeout_ms: u64) -> Result<Vec<DiscoveredGateway>, TransportError> {
    // Bind to any address
    let mut socket = UdpSocket::bind("0.0.0.0:0")
        .await
        .map_err(|e| TransportError::ConnectionFailed(e.to_string()))?;

    // Broadcast VIR
    let broadcast: SocketAddr = "255.255.255.255:13400".parse().unwrap();
    socket
        .send(
            DoipPayload::VehicleIdentificationRequest(VehicleIdentificationRequest {}),
            broadcast,
        )
        .await
        .map_err(|e| TransportError::SendFailed(e.to_string()))?;

    info!("Sent VIR broadcast");

    // Collect VAM responses
    let mut gateways = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }

        match tokio::time::timeout(remaining, socket.recv()).await {
            Ok(Some(Ok((msg, addr)))) => {
                if let DoipPayload::VehicleAnnouncementMessage(vam) = msg.payload {
                    debug!(ip = %addr.ip(), "Received VAM");
                    gateways.push(DiscoveredGateway::from((vam, addr)));
                }
            }
            _ => break,
        }
    }

    info!(count = gateways.len(), "Discovery complete");
    Ok(gateways)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vin_string() {
        let gw = DiscoveredGateway {
            ip: "192.168.1.1".into(),
            logical_address: 0x0010,
            vin: *b"WVWZZZ3CZWE123456",
            eid: [0; 6],
        };
        assert_eq!(gw.vin_string(), "WVWZZZ3CZWE123456");
    }
}
