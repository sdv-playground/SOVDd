//! Example ECU Simulator
//!
//! Simulates a VTX ECM for testing the SOVD server.
//! Supports UDS services with configurable service IDs for OEM variants.
//!
//! # Usage
//!
//! Standard UDS mode:
//! ```bash
//! ./example-ecu --interface vcan0
//! ```
//!
//! With config file (e.g., Vortex Motors mode):
//! ```bash
//! ./example-ecu --config config/example-ecu-vortex.toml
//! ```

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use socketcan::{CanFrame, CanSocket, EmbeddedFrame, ExtendedId, Frame, Socket};
use socketcan_isotp::IsoTpSocket;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

mod config;
mod parameters;
mod sw_package;
mod uds;

use config::EcuConfig;
use parameters::SimulatedEcu;

/// UDS Functional broadcast CAN IDs (ISO 15765-2)
const FUNCTIONAL_CAN_ID_29BIT: u32 = 0x18DB33F1; // All ECUs, tester address F1
const FUNCTIONAL_CAN_ID_11BIT: u32 = 0x7DF; // OBD-II broadcast

#[derive(Parser, Debug)]
#[command(name = "example-ecu")]
#[command(about = "Example ECU simulator for SOVD server development")]
struct Args {
    /// Configuration file path (TOML format)
    /// If provided, overrides command-line options
    #[arg(short, long)]
    config: Option<String>,

    /// CAN interface name
    #[arg(short, long, default_value = "vcan0")]
    interface: String,

    /// ECU's receive CAN ID (tester sends to this)
    #[arg(long, default_value = "0x18DA00F1")]
    rx_id: String,

    /// ECU's transmit CAN ID (ECU sends from this)
    #[arg(long, default_value = "0x18DAF100")]
    tx_id: String,

    /// Enable verbose output
    #[arg(short, long)]
    verbose: bool,

    /// Security access shared secret (hex string, e.g., "deadbeef")
    /// Both server and ECU must use the same secret for auth to succeed
    #[arg(long, default_value = "ff")]
    security_secret: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize tracing
    let filter = if args.verbose {
        "example_ecu=debug"
    } else {
        "example_ecu=info"
    };

    tracing_subscriber::fmt().with_env_filter(filter).init();

    // Load configuration
    let config = if let Some(config_path) = &args.config {
        info!("Loading config from: {}", config_path);
        EcuConfig::load(config_path).map_err(|e| anyhow::anyhow!("Failed to load config: {}", e))?
    } else {
        // Build config from command-line args
        let mut config = EcuConfig::default();
        config.transport.interface = args.interface.clone();
        config.transport.rx_id = args.rx_id.clone();
        config.transport.tx_id = args.tx_id.clone();
        config.security.secret = args.security_secret.clone();
        config
    };

    info!("Starting Test ECU Simulator");
    info!(
        interface = %config.transport.interface,
        rx_id = %config.transport.rx_id,
        tx_id = %config.transport.tx_id
    );

    // Log service ID configuration
    if config.service_ids.is_non_standard() {
        warn!("Using non-standard service IDs:");
        if config.service_ids.read_data_by_periodic_id != 0x2A {
            warn!(
                "  ReadDataByPeriodicId: 0x{:02X} (standard: 0x2A)",
                config.service_ids.read_data_by_periodic_id
            );
        }
        if config.service_ids.dynamically_define_data_id != 0x2C {
            warn!(
                "  DynamicallyDefineDataId: 0x{:02X} (standard: 0x2C)",
                config.service_ids.dynamically_define_data_id
            );
        }
        if config.service_ids.write_data_by_id != 0x2E {
            warn!(
                "  WriteDataById: 0x{:02X} (standard: 0x2E)",
                config.service_ids.write_data_by_id
            );
        }
    }

    // Log transfer configuration if non-default
    if config.transfer.block_counter_start != 0 || config.transfer.block_counter_wrap != 0 {
        info!(
            "Transfer config: block_counter_start={}, block_counter_wrap={}",
            config.transfer.block_counter_start, config.transfer.block_counter_wrap
        );
    }

    // Parse CAN IDs
    let rx_id = parse_can_id(&config.transport.rx_id)?;
    let tx_id = parse_can_id(&config.transport.tx_id)?;

    // Parse security secret from hex
    let security_secret = parse_hex_string(&config.security.secret)
        .map_err(|e| anyhow::anyhow!("Invalid security secret: {}", e))?;

    // Create the simulated ECU with full configuration
    let ecu = Arc::new(SimulatedEcu::from_config(&config, security_secret));

    // Create and run the ECU simulator
    let simulator = EcuSimulator::new(&config.transport.interface, rx_id, tx_id, ecu)?;

    info!("ECU Simulator ready - waiting for requests");
    info!("Press Ctrl+C to stop");

    // Run until interrupted
    simulator.run().await?;

    info!("ECU Simulator stopped");
    Ok(())
}

fn parse_can_id(s: &str) -> Result<u32> {
    let s = s.trim();
    let s = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    u32::from_str_radix(s, 16).map_err(|e| anyhow::anyhow!("Invalid CAN ID: {}", e))
}

fn parse_hex_string(s: &str) -> Result<Vec<u8>> {
    let s = s.trim();
    let s = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);

    if !s.len().is_multiple_of(2) {
        return Err(anyhow::anyhow!(
            "Hex string must have even number of characters"
        ));
    }

    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16))
        .collect::<Result<Vec<u8>, _>>()
        .map_err(|e| anyhow::anyhow!("Invalid hex: {}", e))
}

struct EcuSimulator {
    interface: String,
    rx_id: u32,
    tx_id: u32,
    /// ECU address derived from CAN IDs (used for functional response)
    ecu_address: u8,
    ecu: Arc<SimulatedEcu>,
    running: Arc<AtomicBool>,
}

impl EcuSimulator {
    fn new(interface: &str, rx_id: u32, tx_id: u32, ecu: Arc<SimulatedEcu>) -> Result<Self> {
        // Extract ECU address from tx_id
        // Format: 0x18DA<target><source> where source is the ECU address
        // tx_id 0x18DAF100 means ECU (source=0x00) -> tester (target=0xF1)
        let ecu_address = (tx_id & 0xFF) as u8;

        Ok(Self {
            interface: interface.to_string(),
            rx_id,
            tx_id,
            ecu_address,
            ecu,
            running: Arc::new(AtomicBool::new(true)),
        })
    }

    async fn run(&self) -> Result<()> {
        // Create ISO-TP socket
        // Note: For ECU, rx_id is what we receive ON (tester's tx), tx_id is what we send FROM
        let rx_id = ExtendedId::new(self.rx_id)
            .ok_or_else(|| anyhow::anyhow!("Invalid extended CAN ID: 0x{:X}", self.rx_id))?;
        let tx_id = ExtendedId::new(self.tx_id)
            .ok_or_else(|| anyhow::anyhow!("Invalid extended CAN ID: 0x{:X}", self.tx_id))?;

        let socket = IsoTpSocket::open(&self.interface, rx_id, tx_id)
            .map_err(|e| anyhow::anyhow!("Failed to open ISO-TP socket: {}", e))?;

        socket.set_nonblocking(true)?;

        let socket = Arc::new(parking_lot::Mutex::new(socket));
        let running = self.running.clone();
        let ecu = self.ecu.clone();

        // Start the value update task
        let ecu_for_update = ecu.clone();
        let running_for_update = running.clone();
        let update_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(100));
            while running_for_update.load(Ordering::SeqCst) {
                interval.tick().await;
                ecu_for_update.update_values();
            }
        });

        // Start periodic transmission task
        let socket_for_periodic = socket.clone();
        let ecu_for_periodic = ecu.clone();
        let running_for_periodic = running.clone();
        let periodic_handle = tokio::spawn(async move {
            Self::periodic_task(socket_for_periodic, ecu_for_periodic, running_for_periodic).await;
        });

        // Start functional broadcast listener (for ECU discovery)
        let interface_for_broadcast = self.interface.clone();
        let ecu_address = self.ecu_address;
        let running_for_broadcast = self.running.clone();
        let broadcast_handle: JoinHandle<Result<()>> = tokio::task::spawn_blocking(move || {
            Self::functional_broadcast_listener(
                &interface_for_broadcast,
                ecu_address,
                running_for_broadcast,
            )
        });

        // Main request handling loop
        let socket_for_main = socket.clone();
        let main_handle: JoinHandle<Result<()>> = tokio::task::spawn_blocking(move || {
            while running.load(Ordering::SeqCst) {
                let mut socket_guard = socket_for_main.lock();

                match socket_guard.read() {
                    Ok(data) if !data.is_empty() => {
                        debug!(request = ?data, "Received UDS request");

                        // Process request and generate response
                        let response = ecu.process_request(data);

                        if !response.is_empty() {
                            debug!(response = ?response, "Sending UDS response");
                            if let Err(e) = socket_guard.write(&response) {
                                error!(?e, "Failed to send response");
                            }
                        }
                    }
                    Ok(_) => {
                        // No data
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(e) => {
                        error!(?e, "Socket read error");
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }

                drop(socket_guard);
            }

            Ok(())
        });

        // Wait for Ctrl+C
        tokio::signal::ctrl_c().await?;
        info!("Shutting down...");

        self.running.store(false, Ordering::SeqCst);

        // Wait for tasks to finish
        let _ = tokio::time::timeout(Duration::from_secs(2), update_handle).await;
        let _ = tokio::time::timeout(Duration::from_secs(2), periodic_handle).await;
        let _ = tokio::time::timeout(Duration::from_secs(2), main_handle).await;
        let _ = tokio::time::timeout(Duration::from_secs(2), broadcast_handle).await;

        Ok(())
    }

    async fn periodic_task(
        socket: Arc<parking_lot::Mutex<IsoTpSocket>>,
        ecu: Arc<SimulatedEcu>,
        running: Arc<AtomicBool>,
    ) {
        let mut fast_interval = tokio::time::interval(Duration::from_millis(100)); // 10Hz
        let mut medium_interval = tokio::time::interval(Duration::from_millis(200)); // 5Hz
        let mut slow_interval = tokio::time::interval(Duration::from_millis(1000)); // 1Hz

        loop {
            tokio::select! {
                _ = fast_interval.tick() => {
                    if !running.load(Ordering::SeqCst) {
                        break;
                    }
                    Self::send_periodic_data(&socket, &ecu, PeriodicRate::Fast);
                }
                _ = medium_interval.tick() => {
                    if !running.load(Ordering::SeqCst) {
                        break;
                    }
                    Self::send_periodic_data(&socket, &ecu, PeriodicRate::Medium);
                }
                _ = slow_interval.tick() => {
                    if !running.load(Ordering::SeqCst) {
                        break;
                    }
                    Self::send_periodic_data(&socket, &ecu, PeriodicRate::Slow);
                }
            }
        }
    }

    fn send_periodic_data(
        socket: &Arc<parking_lot::Mutex<IsoTpSocket>>,
        ecu: &Arc<SimulatedEcu>,
        rate: PeriodicRate,
    ) {
        let periodic_pids = ecu.get_periodic_pids(rate);

        for pid in periodic_pids {
            if let Some(data) = ecu.get_periodic_response(pid) {
                let socket_guard = socket.lock();
                if let Err(e) = socket_guard.write(&data) {
                    debug!(?e, pid, "Failed to send periodic data");
                }
                drop(socket_guard);
            }
        }
    }

    /// Listen for UDS functional broadcast requests (for ECU discovery)
    ///
    /// Real ECUs respond to functional addressing (broadcast) on raw CAN,
    /// which is used for discovery. This is separate from ISO-TP which is
    /// point-to-point.
    fn functional_broadcast_listener(
        interface: &str,
        ecu_address: u8,
        running: Arc<AtomicBool>,
    ) -> Result<()> {
        let socket = CanSocket::open(interface)
            .map_err(|e| anyhow::anyhow!("Failed to open raw CAN socket: {}", e))?;

        socket
            .set_nonblocking(true)
            .map_err(|e| anyhow::anyhow!("Failed to set non-blocking: {}", e))?;

        info!(
            interface = %interface,
            ecu_address = format!("0x{:02X}", ecu_address),
            "Functional broadcast listener started"
        );

        // Response CAN ID: 0x18DAF1xx where xx is ECU address
        let response_can_id = 0x18DA0000 | (0xF1 << 8) | (ecu_address as u32);

        while running.load(Ordering::SeqCst) {
            match socket.read_frame() {
                Ok(frame) => {
                    let can_id = frame.raw_id();

                    // Check for functional broadcast (29-bit or 11-bit)
                    let is_functional =
                        can_id == FUNCTIONAL_CAN_ID_29BIT || can_id == FUNCTIONAL_CAN_ID_11BIT;

                    if is_functional {
                        let data = frame.data();

                        // Parse single-frame format: [PCI] [SID] [sub-function...]
                        if data.len() >= 2 {
                            let pci = data[0];
                            let length = pci & 0x0F;

                            if pci & 0xF0 == 0x00 && length >= 2 {
                                let sid = data[1];

                                // Handle TesterPresent (0x3E)
                                if sid == 0x3E {
                                    let sub_function = if data.len() > 2 { data[2] } else { 0x00 };

                                    info!(
                                        can_id = format!("0x{:08X}", can_id),
                                        sub_function = format!("0x{:02X}", sub_function),
                                        "Functional TesterPresent received"
                                    );

                                    // Check suppress positive response bit
                                    if sub_function & 0x80 == 0 {
                                        // Send positive response on physical address
                                        // Format: [PCI=0x02] [0x7E] [sub_function]
                                        let response_data = [
                                            0x02,                // PCI: single frame, 2 bytes
                                            0x7E, // Positive response for TesterPresent
                                            sub_function & 0x7F, // Echo sub-function
                                            0x00,
                                            0x00,
                                            0x00,
                                            0x00,
                                            0x00, // Padding
                                        ];

                                        let response_id = ExtendedId::new(response_can_id)
                                            .expect("Valid response CAN ID");
                                        let response_frame =
                                            CanFrame::new(response_id, &response_data)
                                                .expect("Valid CAN frame");

                                        if let Err(e) = socket.write_frame(&response_frame) {
                                            error!(?e, "Failed to send functional response");
                                        } else {
                                            info!(
                                                response_id = format!("0x{:08X}", response_can_id),
                                                "Sent TesterPresent response"
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(_) => {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }

        info!("Functional broadcast listener stopped");
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PeriodicRate {
    Slow = 0x01,
    Medium = 0x02,
    Fast = 0x03,
}
