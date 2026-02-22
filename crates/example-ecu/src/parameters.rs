//! ECU Parameters and simulation
//!
//! Data-driven ECU simulation using config definitions.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU8, Ordering};

use crate::sw_package::FirmwareImage;
use crc::{Crc, CRC_32_ISO_HDLC};
use parking_lot::RwLock;
use rand::Rng;
use sovd_uds::uds::standard_did;
use tracing::{debug, info, warn};

/// CRC-32 calculator (ISO HDLC / CRC-32)
const CRC32: Crc<u32> = Crc::<u32>::new(&CRC_32_ISO_HDLC);

use crate::config::{
    AccessLevel as ConfigAccessLevel, DtcDef, EcuConfig, OutputDef, ParameterDef, RoutineDef,
    ServiceIdConfig,
};
use crate::uds::{
    ddid_sub_function, dtc_sub_function, io_control_option, link_baud_rate,
    link_control_sub_function, negative_response, nrc, positive_response, routine_sub_function,
    service_id,
};
use crate::PeriodicRate;

/// Access level required to read a parameter
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AccessLevel {
    /// Readable in default session (0x01), no security needed
    Public,
    /// Requires extended diagnostic session (0x03)
    Extended,
    /// Requires security access (0x27) to be unlocked
    Protected,
}

impl From<ConfigAccessLevel> for AccessLevel {
    fn from(level: ConfigAccessLevel) -> Self {
        match level {
            ConfigAccessLevel::Public => AccessLevel::Public,
            ConfigAccessLevel::Extended => AccessLevel::Extended,
            ConfigAccessLevel::Protected => AccessLevel::Protected,
        }
    }
}

/// A simulated parameter
#[derive(Debug, Clone)]
pub struct Parameter {
    #[allow(dead_code)]
    pub id: String,
    #[allow(dead_code)]
    pub did: u16,
    pub value: Vec<u8>,
    pub min_raw: u32,
    pub max_raw: u32,
    pub variation: u32,
    pub access_level: AccessLevel,
    pub writable: bool,
    pub varies: bool,
}

impl Parameter {
    /// Create from a parameter definition
    pub fn from_def(def: &ParameterDef) -> Self {
        let value = def.value.to_bytes(&def.data_type);
        let min_raw = def.min.unwrap_or(0);
        let max_raw = def.max.unwrap_or(0);

        // Calculate variation based on percentage
        let range = max_raw.saturating_sub(min_raw);
        let variation = if def.varies && range > 0 {
            (range as u64 * def.variation_percent as u64 / 100) as u32
        } else {
            0
        };

        Self {
            id: def.id.clone(),
            did: def.did,
            value,
            min_raw,
            max_raw,
            variation,
            access_level: def.access.into(),
            writable: def.writable,
            varies: def.varies,
        }
    }

    pub fn update(&mut self) {
        if !self.varies || self.variation == 0 {
            return;
        }

        let mut rng = rand::thread_rng();
        let current = self.get_raw_value();

        // Random walk with bounds
        let delta = rng.gen_range(0..=self.variation * 2) as i32 - self.variation as i32;
        let new_value = (current as i32 + delta)
            .max(self.min_raw as i32)
            .min(self.max_raw as i32) as u32;

        self.set_raw_value(new_value);
    }

    fn get_raw_value(&self) -> u32 {
        match self.value.len() {
            1 => self.value[0] as u32,
            2 => u16::from_be_bytes([self.value[0], self.value[1]]) as u32,
            4 => u32::from_be_bytes([self.value[0], self.value[1], self.value[2], self.value[3]]),
            _ => 0,
        }
    }

    fn set_raw_value(&mut self, value: u32) {
        match self.value.len() {
            1 => self.value[0] = value as u8,
            2 => {
                let bytes = (value as u16).to_be_bytes();
                self.value[0] = bytes[0];
                self.value[1] = bytes[1];
            }
            4 => {
                let bytes = value.to_be_bytes();
                self.value.copy_from_slice(&bytes);
            }
            _ => {}
        }
    }
}

/// A simulated DTC (Diagnostic Trouble Code)
#[derive(Debug, Clone)]
pub struct SimulatedDtc {
    /// 3-byte DTC number (high, mid, low)
    pub dtc_bytes: [u8; 3],
    /// DTC status byte
    pub status: u8,
    /// Snapshot data (optional)
    pub snapshot: Option<Vec<u8>>,
    /// Extended data (optional)
    pub extended_data: Option<Vec<u8>>,
}

impl SimulatedDtc {
    /// Create from a DTC definition
    pub fn from_def(def: &DtcDef) -> Self {
        Self {
            dtc_bytes: def.bytes,
            status: def.status,
            snapshot: def.snapshot.clone(),
            extended_data: def.extended_data.clone(),
        }
    }

    /// Check if status matches mask
    pub fn matches_mask(&self, mask: u8) -> bool {
        (self.status & mask) != 0
    }
}

/// State of a simulated routine
#[derive(Debug, Clone)]
pub struct RoutineState {
    /// Whether the routine is currently running
    pub running: bool,
    /// Result data (if available)
    pub result: Option<Vec<u8>>,
}

/// Routine definition with runtime info
#[derive(Debug, Clone)]
pub struct SimulatedRoutine {
    #[allow(dead_code)]
    pub id: u16,
    #[allow(dead_code)]
    pub name: String,
    pub requires_security: bool,
    /// Minimum session required (0x01=default, 0x02=programming, 0x03=extended)
    pub required_session: u8,
    pub default_result: Vec<u8>,
    pub instant: bool,
}

impl SimulatedRoutine {
    /// Create from a routine definition
    pub fn from_def(def: &RoutineDef) -> Self {
        Self {
            id: def.id,
            name: def.name.clone(),
            requires_security: def.requires_security,
            required_session: def.required_session,
            default_result: def.result.clone(),
            instant: def.instant,
        }
    }
}

/// Definition for a dynamic data identifier (DDID)
#[derive(Debug, Clone)]
pub struct DdidDefinition {
    /// Source DID
    pub source_did: u16,
    /// Byte position (1-based)
    pub position: u8,
    /// Size in bytes
    pub size: u8,
}

/// State for an active download transfer
#[derive(Debug)]
pub struct DownloadState {
    /// Target memory address
    pub address: u32,
    /// Total size of data to be downloaded
    pub total_size: u32,
    /// Bytes received so far
    pub received: u32,
    /// Buffer holding received data
    pub buffer: Vec<u8>,
    /// Expected next block sequence counter
    pub expected_block: u8,
}

/// State for an active upload transfer (ECU to tester)
#[derive(Debug)]
pub struct UploadState {
    /// Source memory address
    pub address: u32,
    /// Total size of data to be uploaded
    pub total_size: u32,
    /// Bytes sent so far
    pub sent: u32,
    /// Buffer holding data to upload (from simulated memory)
    pub buffer: Vec<u8>,
    /// Expected next block sequence counter
    pub next_block: u8,
}

/// A simulated I/O output for actuator testing
#[derive(Debug, Clone)]
pub struct SimulatedOutput {
    /// Output identifier
    #[allow(dead_code)]
    pub id: u16,
    /// Human-readable name
    #[allow(dead_code)]
    pub name: String,
    /// Current value
    pub current_value: Vec<u8>,
    /// Default value
    pub default_value: Vec<u8>,
    /// Whether tester has control
    pub controlled_by_tester: bool,
    /// Whether the value is frozen
    pub frozen: bool,
    /// Whether security access is required
    pub requires_security: bool,
}

impl SimulatedOutput {
    /// Create from an output definition
    pub fn from_def(def: &OutputDef) -> Self {
        let default_value = if def.default.is_empty() {
            vec![0u8; def.size]
        } else {
            def.default.clone()
        };

        Self {
            id: def.id,
            name: def.name.clone(),
            current_value: default_value.clone(),
            default_value,
            controlled_by_tester: false,
            frozen: false,
            requires_security: def.requires_security,
        }
    }
}

/// Simulated ECU state
pub struct SimulatedEcu {
    /// ECU identifier (for firmware target validation)
    ecu_id: String,
    /// Configurable service IDs (for OEM-specific implementations)
    svc: ServiceIdConfig,
    /// Current diagnostic session
    session: AtomicU8,
    /// Security unlocked
    security_unlocked: RwLock<bool>,
    /// Current seed for security access
    current_seed: RwLock<Vec<u8>>,
    /// Shared secret for seed-key algorithm (must match tester's secret)
    security_secret: Vec<u8>,
    /// Parameters by DID
    parameters: RwLock<HashMap<u16, Parameter>>,
    /// Periodic identifiers (PIDs) by rate
    periodic_slow: RwLock<HashSet<u8>>,
    periodic_medium: RwLock<HashSet<u8>>,
    periodic_fast: RwLock<HashSet<u8>>,
    /// Stored DTCs
    dtcs: RwLock<Vec<SimulatedDtc>>,
    /// Status availability mask (which status bits this ECU supports)
    dtc_status_availability_mask: u8,
    /// Routine definitions by ID
    routines: RwLock<HashMap<u16, SimulatedRoutine>>,
    /// Routine states by routine ID
    routine_states: RwLock<HashMap<u16, RoutineState>>,
    /// Dynamic data identifier definitions
    ddid_definitions: RwLock<HashMap<u16, Vec<DdidDefinition>>>,
    /// Current download transfer state (if active)
    download_state: RwLock<Option<DownloadState>>,
    /// Current upload transfer state (if active)
    upload_state: RwLock<Option<UploadState>>,
    /// Simulated memory for upload/download (256KB)
    simulated_memory: RwLock<Vec<u8>>,
    /// Simulated I/O outputs
    outputs: RwLock<HashMap<u16, SimulatedOutput>>,
    /// Current baud rate (bps)
    current_baud_rate: RwLock<u32>,
    /// Pending baud rate (verified but not yet transitioned)
    pending_baud_rate: RwLock<Option<u32>>,
    /// Pending firmware version (to be applied after reset)
    pending_firmware_version: RwLock<Option<String>>,
    /// Previous firmware version (for A/B bank rollback simulation)
    previous_firmware_version: RwLock<Option<String>>,
    /// Transfer block counter start value
    block_counter_start: u8,
    /// Transfer block counter wrap value (after 255)
    block_counter_wrap: u8,
}

impl SimulatedEcu {
    /// Create a new simulated ECU from configuration
    pub fn from_config(config: &EcuConfig, security_secret: Vec<u8>) -> Self {
        // Use custom definitions if provided, otherwise use defaults
        let mut effective_config = if config.has_custom_definitions() {
            config.clone()
        } else {
            EcuConfig::default_vtx_ecm()
        };

        // Ensure all standard identification DIDs are present
        // (auto-injects any missing ones with sensible defaults)
        effective_config.ensure_standard_dids();

        // Build parameters map
        let mut parameters = HashMap::new();
        for def in &effective_config.parameters {
            let param = Parameter::from_def(def);
            parameters.insert(def.did, param);
        }

        info!(
            "Created ECU simulator with {} parameters ({} public, {} extended, {} protected)",
            parameters.len(),
            parameters
                .values()
                .filter(|p| p.access_level == AccessLevel::Public)
                .count(),
            parameters
                .values()
                .filter(|p| p.access_level == AccessLevel::Extended)
                .count(),
            parameters
                .values()
                .filter(|p| p.access_level == AccessLevel::Protected)
                .count(),
        );

        // Build DTCs
        let dtcs: Vec<SimulatedDtc> = effective_config
            .dtcs
            .iter()
            .map(SimulatedDtc::from_def)
            .collect();

        info!(
            "Created ECU simulator with {} DTCs ({} active, {} pending, {} confirmed)",
            dtcs.len(),
            dtcs.iter().filter(|d| (d.status & 0x09) == 0x09).count(),
            dtcs.iter().filter(|d| (d.status & 0x04) != 0).count(),
            dtcs.iter().filter(|d| (d.status & 0x08) != 0).count(),
        );

        // Build outputs
        let mut outputs = HashMap::new();
        for def in &effective_config.outputs {
            let output = SimulatedOutput::from_def(def);
            outputs.insert(def.id, output);
        }

        info!("Created ECU simulator with {} I/O outputs", outputs.len());

        // Build routines
        let mut routines = HashMap::new();
        for def in &effective_config.routines {
            let routine = SimulatedRoutine::from_def(def);
            routines.insert(def.id, routine);
        }

        info!("Created ECU simulator with {} routines", routines.len());

        // Initialize simulated memory with test pattern (256KB)
        let mut simulated_memory = vec![0u8; 256 * 1024];
        for (i, byte) in simulated_memory.iter_mut().enumerate() {
            *byte = (i % 256) as u8;
        }

        Self {
            ecu_id: config.id.clone(),
            svc: config.service_ids.clone(),
            session: AtomicU8::new(0x01),
            security_unlocked: RwLock::new(false),
            current_seed: RwLock::new(Vec::new()),
            security_secret,
            parameters: RwLock::new(parameters),
            periodic_slow: RwLock::new(HashSet::new()),
            periodic_medium: RwLock::new(HashSet::new()),
            periodic_fast: RwLock::new(HashSet::new()),
            dtcs: RwLock::new(dtcs),
            dtc_status_availability_mask: 0xFF,
            routines: RwLock::new(routines),
            routine_states: RwLock::new(HashMap::new()),
            ddid_definitions: RwLock::new(HashMap::new()),
            download_state: RwLock::new(None),
            upload_state: RwLock::new(None),
            simulated_memory: RwLock::new(simulated_memory),
            outputs: RwLock::new(outputs),
            current_baud_rate: RwLock::new(500000),
            pending_baud_rate: RwLock::new(None),
            pending_firmware_version: RwLock::new(None),
            previous_firmware_version: RwLock::new(None),
            block_counter_start: config.transfer.block_counter_start,
            block_counter_wrap: config.transfer.block_counter_wrap,
        }
    }

    /// Create a new VTX ECM simulator (backwards compatibility)
    #[allow(dead_code)]
    pub fn new_vtx_ecm(security_secret: Vec<u8>) -> Self {
        let config = EcuConfig::default_vtx_ecm();
        Self::from_config(&config, security_secret)
    }

    /// Create a new VTX ECM simulator with custom service IDs (backwards compatibility)
    #[allow(dead_code)]
    pub fn new_vtx_ecm_with_config(security_secret: Vec<u8>, service_ids: ServiceIdConfig) -> Self {
        let mut config = EcuConfig::default_vtx_ecm();
        config.service_ids = service_ids;
        Self::from_config(&config, security_secret)
    }

    /// Update all parameter values (called periodically)
    pub fn update_values(&self) {
        let mut params = self.parameters.write();
        for param in params.values_mut() {
            param.update();
        }
    }

    /// Process a UDS request and return the response
    pub fn process_request(&self, request: &[u8]) -> Vec<u8> {
        if request.is_empty() {
            return negative_response(0x00, nrc::INCORRECT_MESSAGE_LENGTH);
        }

        let sid = request[0];

        // Match against configurable service IDs
        if sid == self.svc.diagnostic_session_control {
            self.handle_session_control(request)
        } else if sid == self.svc.tester_present {
            self.handle_tester_present(request)
        } else if sid == self.svc.security_access {
            self.handle_security_access(request)
        } else if sid == self.svc.read_data_by_id {
            self.handle_read_data_by_id(request)
        } else if sid == self.svc.write_data_by_id {
            self.handle_write_data_by_id(request)
        } else if sid == self.svc.dynamically_define_data_id {
            self.handle_ddid(request)
        } else if sid == self.svc.routine_control {
            self.handle_routine_control(request)
        } else if sid == self.svc.read_data_by_periodic_id {
            self.handle_read_periodic(request)
        } else if sid == self.svc.ecu_reset {
            self.handle_ecu_reset(request)
        } else if sid == self.svc.read_dtc_info {
            self.handle_read_dtc_info(request)
        } else if sid == self.svc.clear_diagnostic_info {
            self.handle_clear_dtc(request)
        } else if sid == self.svc.request_download {
            self.handle_request_download(request)
        } else if sid == self.svc.request_upload {
            self.handle_request_upload(request)
        } else if sid == self.svc.transfer_data {
            self.handle_transfer_data(request)
        } else if sid == self.svc.request_transfer_exit {
            self.handle_request_transfer_exit(request)
        } else if sid == self.svc.io_control_by_id {
            self.handle_io_control(request)
        } else if sid == self.svc.link_control {
            self.handle_link_control(request)
        } else {
            debug!(service_id = sid, "Unsupported service");
            negative_response(sid, nrc::SERVICE_NOT_SUPPORTED)
        }
    }

    fn handle_session_control(&self, request: &[u8]) -> Vec<u8> {
        if request.len() < 2 {
            return negative_response(
                service_id::DIAGNOSTIC_SESSION_CONTROL,
                nrc::INCORRECT_MESSAGE_LENGTH,
            );
        }

        let session = request[1];
        info!(
            session = format!("0x{:02X}", session),
            "Session control request"
        );

        match session {
            // 0x01 = default, 0x02 = programming, 0x03 = extended, 0x60 = engineering
            0x01 | 0x02 | 0x03 | 0x60 => {
                let previous_session = self.session.swap(session, Ordering::SeqCst);

                // Per ISO 14229: session change resets security access to locked
                if session != previous_session {
                    let was_unlocked =
                        std::mem::replace(&mut *self.security_unlocked.write(), false);
                    if was_unlocked {
                        info!("Session change: security access reset to locked");
                    }
                }

                // Per ISO 14229: session change resets active transfer state
                if session == 0x01 {
                    let had_download = self.download_state.write().take().is_some();
                    let had_upload = self.upload_state.write().take().is_some();
                    if had_download || had_upload {
                        info!("Session reset to default: cleared active transfer state");
                    }
                }
                info!(session = format!("0x{:02X}", session), "Session changed");
                // Response format: [session, P2_hi, P2_lo, P2*_hi, P2*_lo]
                // P2 = 25ms (0x0019), P2* = 500ms (0x01F4)
                positive_response(
                    service_id::DIAGNOSTIC_SESSION_CONTROL,
                    &[session, 0x00, 0x19, 0x01, 0xF4],
                )
            }
            _ => {
                debug!(
                    session = format!("0x{:02X}", session),
                    "Unsupported session type"
                );
                negative_response(
                    service_id::DIAGNOSTIC_SESSION_CONTROL,
                    nrc::SUB_FUNCTION_NOT_SUPPORTED,
                )
            }
        }
    }

    fn handle_tester_present(&self, request: &[u8]) -> Vec<u8> {
        if request.len() < 2 {
            return negative_response(service_id::TESTER_PRESENT, nrc::INCORRECT_MESSAGE_LENGTH);
        }

        let sub_function = request[1];

        if sub_function & 0x80 != 0 {
            debug!("Tester present (suppressed response)");
            return Vec::new();
        }

        debug!("Tester present");
        positive_response(service_id::TESTER_PRESENT, &[sub_function & 0x7F])
    }

    fn handle_security_access(&self, request: &[u8]) -> Vec<u8> {
        if request.len() < 2 {
            return negative_response(service_id::SECURITY_ACCESS, nrc::INCORRECT_MESSAGE_LENGTH);
        }

        let sub_function = request[1];

        if sub_function % 2 == 1 {
            // Request seed
            let mut rng = rand::thread_rng();
            let seed: Vec<u8> = (0..4).map(|_| rng.gen()).collect();

            info!(seed = ?seed, "Security access: providing seed");
            *self.current_seed.write() = seed.clone();

            let mut response_data = vec![sub_function];
            response_data.extend_from_slice(&seed);
            positive_response(service_id::SECURITY_ACCESS, &response_data)
        } else {
            // Send key
            let key = &request[2..];
            let seed = self.current_seed.read();

            let expected_key: Vec<u8> = seed
                .iter()
                .enumerate()
                .map(|(i, b)| b ^ self.security_secret[i % self.security_secret.len()])
                .collect();

            if key == expected_key.as_slice() {
                info!("Security access: key accepted");
                *self.security_unlocked.write() = true;
                positive_response(service_id::SECURITY_ACCESS, &[sub_function])
            } else {
                info!(expected = ?expected_key, received = ?key, "Security access: invalid key (NRC 0x35)");
                negative_response(service_id::SECURITY_ACCESS, nrc::INVALID_KEY)
            }
        }
    }

    fn handle_read_data_by_id(&self, request: &[u8]) -> Vec<u8> {
        if request.len() < 3 {
            return negative_response(service_id::READ_DATA_BY_ID, nrc::INCORRECT_MESSAGE_LENGTH);
        }

        let mut response_data = Vec::new();
        let current_session = self.session.load(Ordering::SeqCst);
        let security_unlocked = *self.security_unlocked.read();

        let mut i = 1;
        while i + 1 < request.len() {
            let did = u16::from_be_bytes([request[i], request[i + 1]]);

            // Check if this is a DDID (0xF200-0xF3FF)
            if (0xF200..=0xF3FF).contains(&did) {
                if let Some(ddid_value) = self.read_ddid(did) {
                    debug!(did = format!("0x{:04X}", did), value = ?ddid_value, "Reading DDID");
                    response_data.extend_from_slice(&did.to_be_bytes());
                    response_data.extend_from_slice(&ddid_value);
                } else {
                    debug!(did = format!("0x{:04X}", did), "DDID not defined");
                    return negative_response(
                        service_id::READ_DATA_BY_ID,
                        nrc::REQUEST_OUT_OF_RANGE,
                    );
                }
            } else {
                let params = self.parameters.read();
                if let Some(param) = params.get(&did) {
                    match param.access_level {
                        AccessLevel::Public => {}
                        AccessLevel::Extended => {
                            if current_session < 0x03 {
                                debug!(
                                    did = format!("0x{:04X}", did),
                                    "Access denied: requires extended session"
                                );
                                return negative_response(
                                    service_id::READ_DATA_BY_ID,
                                    nrc::CONDITIONS_NOT_CORRECT,
                                );
                            }
                        }
                        AccessLevel::Protected => {
                            if !security_unlocked {
                                debug!(
                                    did = format!("0x{:04X}", did),
                                    "Access denied: requires security access"
                                );
                                return negative_response(
                                    service_id::READ_DATA_BY_ID,
                                    nrc::SECURITY_ACCESS_DENIED,
                                );
                            }
                        }
                    }

                    debug!(did = format!("0x{:04X}", did), value = ?param.value, "Reading parameter");
                    response_data.extend_from_slice(&did.to_be_bytes());
                    response_data.extend_from_slice(&param.value);
                } else {
                    // Also check I/O outputs — their current value is readable via 0x22
                    let outputs = self.outputs.read();
                    if let Some(output) = outputs.get(&did) {
                        if output.requires_security && !security_unlocked {
                            debug!(
                                did = format!("0x{:04X}", did),
                                "Access denied: I/O output requires security access"
                            );
                            return negative_response(
                                service_id::READ_DATA_BY_ID,
                                nrc::SECURITY_ACCESS_DENIED,
                            );
                        }
                        debug!(did = format!("0x{:04X}", did), value = ?output.current_value, "Reading I/O output via 0x22");
                        response_data.extend_from_slice(&did.to_be_bytes());
                        response_data.extend_from_slice(&output.current_value);
                    } else {
                        debug!(did = format!("0x{:04X}", did), "Unknown DID");
                        return negative_response(
                            service_id::READ_DATA_BY_ID,
                            nrc::REQUEST_OUT_OF_RANGE,
                        );
                    }
                }
            }

            i += 2;
        }

        positive_response(service_id::READ_DATA_BY_ID, &response_data)
    }

    fn handle_write_data_by_id(&self, request: &[u8]) -> Vec<u8> {
        if request.len() < 4 {
            return negative_response(service_id::WRITE_DATA_BY_ID, nrc::INCORRECT_MESSAGE_LENGTH);
        }

        let did = u16::from_be_bytes([request[1], request[2]]);
        let data = &request[3..];

        let mut params = self.parameters.write();
        let current_session = self.session.load(Ordering::SeqCst);
        let security_unlocked = *self.security_unlocked.read();

        if let Some(param) = params.get_mut(&did) {
            if !param.writable {
                debug!(
                    did = format!("0x{:04X}", did),
                    "Write denied: parameter is read-only"
                );
                return negative_response(
                    service_id::WRITE_DATA_BY_ID,
                    nrc::GENERAL_PROGRAMMING_FAILURE,
                );
            }

            match param.access_level {
                AccessLevel::Public => {}
                AccessLevel::Extended => {
                    if current_session < 0x03 {
                        debug!(
                            did = format!("0x{:04X}", did),
                            "Write denied: requires extended session"
                        );
                        return negative_response(
                            service_id::WRITE_DATA_BY_ID,
                            nrc::CONDITIONS_NOT_CORRECT,
                        );
                    }
                }
                AccessLevel::Protected => {
                    if !security_unlocked {
                        debug!(
                            did = format!("0x{:04X}", did),
                            "Write denied: requires security access"
                        );
                        return negative_response(
                            service_id::WRITE_DATA_BY_ID,
                            nrc::SECURITY_ACCESS_DENIED,
                        );
                    }
                }
            }

            info!(did = format!("0x{:04X}", did), data = ?data, "Writing parameter");
            param.value = data.to_vec();
            positive_response(service_id::WRITE_DATA_BY_ID, &did.to_be_bytes())
        } else {
            debug!(did = format!("0x{:04X}", did), "Unknown DID");
            negative_response(service_id::WRITE_DATA_BY_ID, nrc::REQUEST_OUT_OF_RANGE)
        }
    }

    fn handle_read_periodic(&self, request: &[u8]) -> Vec<u8> {
        let sid = self.svc.read_data_by_periodic_id;

        if request.len() < 3 {
            return negative_response(sid, nrc::INCORRECT_MESSAGE_LENGTH);
        }

        let transmission_mode = request[1];
        let pids: Vec<u8> = request[2..].to_vec();

        info!(mode = transmission_mode, pids = ?pids, "Periodic identifier request");

        match transmission_mode {
            0x01 => {
                let mut slow = self.periodic_slow.write();
                for pid in &pids {
                    slow.insert(*pid);
                }
                info!(pids = ?pids, "Added to slow periodic");
            }
            0x02 => {
                let mut medium = self.periodic_medium.write();
                for pid in &pids {
                    medium.insert(*pid);
                }
                info!(pids = ?pids, "Added to medium periodic");
            }
            0x03 => {
                let mut fast = self.periodic_fast.write();
                for pid in &pids {
                    fast.insert(*pid);
                }
                info!(pids = ?pids, "Added to fast periodic");
            }
            0x04 => {
                let mut slow = self.periodic_slow.write();
                let mut medium = self.periodic_medium.write();
                let mut fast = self.periodic_fast.write();
                for pid in &pids {
                    slow.remove(pid);
                    medium.remove(pid);
                    fast.remove(pid);
                }
                info!(pids = ?pids, "Stopped periodic");
            }
            _ => {
                return negative_response(sid, nrc::SUB_FUNCTION_NOT_SUPPORTED);
            }
        }

        positive_response(sid, &[transmission_mode])
    }

    fn handle_ecu_reset(&self, request: &[u8]) -> Vec<u8> {
        if request.len() < 2 {
            return negative_response(service_id::ECU_RESET, nrc::INCORRECT_MESSAGE_LENGTH);
        }

        let reset_type = request[1];
        info!(reset_type, "ECU reset request");

        // Reset session and security state
        self.session.store(0x01, Ordering::SeqCst);
        *self.security_unlocked.write() = false;

        // Clear periodic transmission
        self.periodic_slow.write().clear();
        self.periodic_medium.write().clear();
        self.periodic_fast.write().clear();

        // Apply pending firmware version (simulates "new firmware boots up")
        if let Some(new_version) = self.pending_firmware_version.write().take() {
            info!(new_version = %new_version, "Applying firmware update after reset");

            // Save current version as previous (for A/B bank rollback)
            {
                let params = self.parameters.read();
                if let Some(param) = params.get(&standard_did::ECU_SOFTWARE_VERSION) {
                    let current_version = String::from_utf8_lossy(&param.value).to_string();
                    info!(previous_version = %current_version, "Saving current version for rollback");
                    *self.previous_firmware_version.write() = Some(current_version);
                }
            }

            // Update ecu_sw_version parameter (DID 0xF189 — ECU Software Version per ISO 14229-1)
            let mut params = self.parameters.write();
            if let Some(param) = params.get_mut(&standard_did::ECU_SOFTWARE_VERSION) {
                param.value = new_version.as_bytes().to_vec();
                info!(did = "0xF189", version = %new_version, "Software version updated");
            } else {
                // Create the parameter if it doesn't exist
                let param = Parameter {
                    id: "ecu_sw_version".to_string(),
                    did: standard_did::ECU_SOFTWARE_VERSION,
                    value: new_version.as_bytes().to_vec(),
                    min_raw: 0,
                    max_raw: 0,
                    variation: 0,
                    access_level: AccessLevel::Public,
                    writable: false,
                    varies: false,
                };
                params.insert(standard_did::ECU_SOFTWARE_VERSION, param);
                info!(did = "0xF189", version = %new_version, "Software version parameter created");
            }
        }

        positive_response(service_id::ECU_RESET, &[reset_type])
    }

    /// Get PIDs configured for a specific periodic rate
    pub fn get_periodic_pids(&self, rate: PeriodicRate) -> Vec<u8> {
        match rate {
            PeriodicRate::Slow => self.periodic_slow.read().iter().cloned().collect(),
            PeriodicRate::Medium => self.periodic_medium.read().iter().cloned().collect(),
            PeriodicRate::Fast => self.periodic_fast.read().iter().cloned().collect(),
        }
    }

    /// Get the periodic response data for a PID
    pub fn get_periodic_response(&self, pid: u8) -> Option<Vec<u8>> {
        let params = self.parameters.read();

        for (did, param) in params.iter() {
            if (*did & 0xFF) as u8 == pid {
                let mut data = vec![pid];
                data.extend_from_slice(&param.value);
                return Some(data);
            }
        }

        None
    }

    // =========================================================================
    // DTC Handlers (0x19 ReadDTCInformation, 0x14 ClearDiagnosticInformation)
    // =========================================================================

    fn handle_read_dtc_info(&self, request: &[u8]) -> Vec<u8> {
        if request.len() < 2 {
            return negative_response(service_id::READ_DTC_INFO, nrc::INCORRECT_MESSAGE_LENGTH);
        }

        let sub_function = request[1];

        match sub_function {
            dtc_sub_function::REPORT_NUMBER_OF_DTC_BY_STATUS_MASK => {
                self.handle_report_dtc_count(request)
            }
            dtc_sub_function::REPORT_DTC_BY_STATUS_MASK => {
                self.handle_report_dtc_by_status_mask(request)
            }
            dtc_sub_function::REPORT_DTC_SNAPSHOT_RECORD_BY_DTC_NUMBER => {
                self.handle_report_dtc_snapshot(request)
            }
            dtc_sub_function::REPORT_DTC_EXTENDED_DATA_RECORD_BY_DTC_NUMBER => {
                self.handle_report_dtc_extended_data(request)
            }
            _ => {
                debug!(
                    sub_function = format!("0x{:02X}", sub_function),
                    "Unsupported DTC sub-function"
                );
                negative_response(service_id::READ_DTC_INFO, nrc::SUB_FUNCTION_NOT_SUPPORTED)
            }
        }
    }

    fn handle_report_dtc_count(&self, request: &[u8]) -> Vec<u8> {
        if request.len() < 3 {
            return negative_response(service_id::READ_DTC_INFO, nrc::INCORRECT_MESSAGE_LENGTH);
        }

        let status_mask = request[2];
        let dtcs = self.dtcs.read();
        let count = dtcs.iter().filter(|d| d.matches_mask(status_mask)).count() as u16;

        info!(
            status_mask = format!("0x{:02X}", status_mask),
            count, "Report DTC count"
        );

        let count_bytes = count.to_be_bytes();
        positive_response(
            service_id::READ_DTC_INFO,
            &[
                dtc_sub_function::REPORT_NUMBER_OF_DTC_BY_STATUS_MASK,
                self.dtc_status_availability_mask,
                0x01,
                count_bytes[0],
                count_bytes[1],
            ],
        )
    }

    fn handle_report_dtc_by_status_mask(&self, request: &[u8]) -> Vec<u8> {
        if request.len() < 3 {
            return negative_response(service_id::READ_DTC_INFO, nrc::INCORRECT_MESSAGE_LENGTH);
        }

        let status_mask = request[2];
        let dtcs = self.dtcs.read();
        let matching_dtcs: Vec<&SimulatedDtc> = dtcs
            .iter()
            .filter(|d| d.matches_mask(status_mask))
            .collect();

        info!(
            status_mask = format!("0x{:02X}", status_mask),
            count = matching_dtcs.len(),
            "Report DTCs by status mask"
        );

        let mut response_data = vec![
            dtc_sub_function::REPORT_DTC_BY_STATUS_MASK,
            self.dtc_status_availability_mask,
        ];

        for dtc in matching_dtcs {
            response_data.push(dtc.dtc_bytes[0]);
            response_data.push(dtc.dtc_bytes[1]);
            response_data.push(dtc.dtc_bytes[2]);
            response_data.push(dtc.status);
        }

        positive_response(service_id::READ_DTC_INFO, &response_data)
    }

    fn handle_report_dtc_snapshot(&self, request: &[u8]) -> Vec<u8> {
        if request.len() < 6 {
            return negative_response(service_id::READ_DTC_INFO, nrc::INCORRECT_MESSAGE_LENGTH);
        }

        let dtc_high = request[2];
        let dtc_mid = request[3];
        let dtc_low = request[4];
        let record_number = request[5];

        let dtcs = self.dtcs.read();
        let dtc = dtcs.iter().find(|d| {
            d.dtc_bytes[0] == dtc_high && d.dtc_bytes[1] == dtc_mid && d.dtc_bytes[2] == dtc_low
        });

        match dtc {
            Some(dtc) => {
                info!(
                    dtc = format!("{:02X}{:02X}{:02X}", dtc_high, dtc_mid, dtc_low),
                    record_number, "Report DTC snapshot"
                );

                let mut response_data = vec![
                    dtc_sub_function::REPORT_DTC_SNAPSHOT_RECORD_BY_DTC_NUMBER,
                    dtc.dtc_bytes[0],
                    dtc.dtc_bytes[1],
                    dtc.dtc_bytes[2],
                    dtc.status,
                ];

                if let Some(ref snapshot) = dtc.snapshot {
                    if record_number == 0xFF || record_number == 0x01 {
                        response_data.push(0x01);
                        response_data.extend_from_slice(snapshot);
                    }
                }

                positive_response(service_id::READ_DTC_INFO, &response_data)
            }
            None => {
                debug!(
                    dtc = format!("{:02X}{:02X}{:02X}", dtc_high, dtc_mid, dtc_low),
                    "DTC not found"
                );
                negative_response(service_id::READ_DTC_INFO, nrc::REQUEST_OUT_OF_RANGE)
            }
        }
    }

    fn handle_report_dtc_extended_data(&self, request: &[u8]) -> Vec<u8> {
        if request.len() < 6 {
            return negative_response(service_id::READ_DTC_INFO, nrc::INCORRECT_MESSAGE_LENGTH);
        }

        let dtc_high = request[2];
        let dtc_mid = request[3];
        let dtc_low = request[4];
        let record_number = request[5];

        let dtcs = self.dtcs.read();
        let dtc = dtcs.iter().find(|d| {
            d.dtc_bytes[0] == dtc_high && d.dtc_bytes[1] == dtc_mid && d.dtc_bytes[2] == dtc_low
        });

        match dtc {
            Some(dtc) => {
                info!(
                    dtc = format!("{:02X}{:02X}{:02X}", dtc_high, dtc_mid, dtc_low),
                    record_number, "Report DTC extended data"
                );

                let mut response_data = vec![
                    dtc_sub_function::REPORT_DTC_EXTENDED_DATA_RECORD_BY_DTC_NUMBER,
                    dtc.dtc_bytes[0],
                    dtc.dtc_bytes[1],
                    dtc.dtc_bytes[2],
                    dtc.status,
                ];

                if let Some(ref ext_data) = dtc.extended_data {
                    if record_number == 0xFF || record_number == 0x01 {
                        response_data.push(0x01);
                        response_data.extend_from_slice(ext_data);
                    }
                }

                positive_response(service_id::READ_DTC_INFO, &response_data)
            }
            None => {
                debug!(
                    dtc = format!("{:02X}{:02X}{:02X}", dtc_high, dtc_mid, dtc_low),
                    "DTC not found"
                );
                negative_response(service_id::READ_DTC_INFO, nrc::REQUEST_OUT_OF_RANGE)
            }
        }
    }

    fn handle_clear_dtc(&self, request: &[u8]) -> Vec<u8> {
        if request.len() < 4 {
            return negative_response(
                service_id::CLEAR_DIAGNOSTIC_INFO,
                nrc::INCORRECT_MESSAGE_LENGTH,
            );
        }

        let current_session = self.session.load(Ordering::SeqCst);
        // Accept programming (0x02) or extended (0x03) session
        if current_session != 0x02 && current_session < 0x03 {
            debug!(
                session = format!("0x{:02X}", current_session),
                "Clear DTC denied: requires programming or extended session"
            );
            return negative_response(
                service_id::CLEAR_DIAGNOSTIC_INFO,
                nrc::CONDITIONS_NOT_CORRECT,
            );
        }

        let group_high = request[1];
        let group_mid = request[2];
        let group_low = request[3];
        let group = ((group_high as u32) << 16) | ((group_mid as u32) << 8) | (group_low as u32);

        info!(group = format!("0x{:06X}", group), "Clear DTCs");

        let mut dtcs = self.dtcs.write();

        match group {
            0xFFFFFF => {
                dtcs.clear();
            }
            0x000000..=0x3FFFFF => {
                dtcs.retain(|d| (d.dtc_bytes[0] >> 6) != 0);
            }
            0x400000..=0x7FFFFF => {
                dtcs.retain(|d| (d.dtc_bytes[0] >> 6) != 1);
            }
            0x800000..=0xBFFFFF => {
                dtcs.retain(|d| (d.dtc_bytes[0] >> 6) != 2);
            }
            0xC00000..=0xFFFFFF => {
                dtcs.retain(|d| (d.dtc_bytes[0] >> 6) != 3);
            }
            _ => {}
        }

        positive_response(service_id::CLEAR_DIAGNOSTIC_INFO, &[])
    }

    // =========================================================================
    // Routine Control Handler (0x31)
    // =========================================================================

    fn handle_routine_control(&self, request: &[u8]) -> Vec<u8> {
        if request.len() < 4 {
            return negative_response(service_id::ROUTINE_CONTROL, nrc::INCORRECT_MESSAGE_LENGTH);
        }

        let sub_function = request[1];
        let routine_id = u16::from_be_bytes([request[2], request[3]]);
        let params = &request[4..];

        match sub_function {
            routine_sub_function::START_ROUTINE => self.handle_start_routine(routine_id, params),
            routine_sub_function::STOP_ROUTINE => self.handle_stop_routine(routine_id),
            routine_sub_function::REQUEST_ROUTINE_RESULTS => {
                self.handle_request_routine_results(routine_id)
            }
            _ => {
                debug!(
                    sub_function = format!("0x{:02X}", sub_function),
                    "Unsupported routine sub-function"
                );
                negative_response(service_id::ROUTINE_CONTROL, nrc::SUB_FUNCTION_NOT_SUPPORTED)
            }
        }
    }

    fn handle_start_routine(&self, routine_id: u16, _params: &[u8]) -> Vec<u8> {
        let current_session = self.session.load(Ordering::SeqCst);
        let security_unlocked = *self.security_unlocked.read();
        let routines = self.routines.read();

        if let Some(routine) = routines.get(&routine_id) {
            // Check session requirement (0x02=programming, 0x03=extended; both >= required)
            if current_session < routine.required_session
                && !(routine.required_session == 0x03 && current_session == 0x02)
            {
                debug!(
                    routine_id = format!("0x{:04X}", routine_id),
                    current_session = format!("0x{:02X}", current_session),
                    required_session = format!("0x{:02X}", routine.required_session),
                    "Routine denied: requires higher session"
                );
                return negative_response(service_id::ROUTINE_CONTROL, nrc::CONDITIONS_NOT_CORRECT);
            }

            if routine.requires_security && !security_unlocked {
                debug!(
                    routine_id = format!("0x{:04X}", routine_id),
                    "Routine denied: requires security access"
                );
                return negative_response(service_id::ROUTINE_CONTROL, nrc::SECURITY_ACCESS_DENIED);
            }

            info!(routine_id = format!("0x{:04X}", routine_id), name = %routine.name, "Starting routine");

            // Special handling for firmware commit (0xFF01)
            if routine_id == 0xFF01 {
                return self.handle_firmware_commit();
            }

            // Special handling for firmware rollback (0xFF02)
            if routine_id == 0xFF02 {
                return self.handle_firmware_rollback();
            }

            let result = routine.default_result.clone();
            let running = !routine.instant;

            {
                let mut states = self.routine_states.write();
                states.insert(
                    routine_id,
                    RoutineState {
                        running,
                        result: Some(result.clone()),
                    },
                );
            }

            let routine_id_bytes = routine_id.to_be_bytes();
            let mut response = vec![
                routine_sub_function::START_ROUTINE,
                routine_id_bytes[0],
                routine_id_bytes[1],
            ];
            if !result.is_empty() {
                response.push(result[0]);
            } else {
                response.push(0x00);
            }
            positive_response(service_id::ROUTINE_CONTROL, &response)
        } else {
            debug!(
                routine_id = format!("0x{:04X}", routine_id),
                "Unknown routine ID"
            );
            negative_response(service_id::ROUTINE_CONTROL, nrc::REQUEST_OUT_OF_RANGE)
        }
    }

    /// Handle firmware commit routine (RID 0xFF01)
    /// Clears previous_firmware_version, making the current firmware permanent
    fn handle_firmware_commit(&self) -> Vec<u8> {
        let had_previous = self.previous_firmware_version.write().take().is_some();

        if had_previous {
            info!("Firmware commit: cleared previous version (current firmware is now permanent)");
        } else {
            info!("Firmware commit: no previous version to clear");
        }

        let routine_id_bytes = 0xFF01_u16.to_be_bytes();
        positive_response(
            service_id::ROUTINE_CONTROL,
            &[
                routine_sub_function::START_ROUTINE,
                routine_id_bytes[0],
                routine_id_bytes[1],
                0x00,
            ],
        )
    }

    /// Handle firmware rollback routine (RID 0xFF02)
    /// Restores previous_firmware_version to DID 0xF189
    fn handle_firmware_rollback(&self) -> Vec<u8> {
        let previous = self.previous_firmware_version.write().take();

        match previous {
            Some(prev_version) => {
                info!(version = %prev_version, "Firmware rollback: restoring previous version");

                let mut params = self.parameters.write();
                if let Some(param) = params.get_mut(&standard_did::ECU_SOFTWARE_VERSION) {
                    param.value = prev_version.as_bytes().to_vec();
                }

                let routine_id_bytes = 0xFF02_u16.to_be_bytes();
                positive_response(
                    service_id::ROUTINE_CONTROL,
                    &[
                        routine_sub_function::START_ROUTINE,
                        routine_id_bytes[0],
                        routine_id_bytes[1],
                        0x00,
                    ],
                )
            }
            None => {
                warn!("Firmware rollback: no previous version available");
                negative_response(service_id::ROUTINE_CONTROL, nrc::CONDITIONS_NOT_CORRECT)
            }
        }
    }

    fn handle_stop_routine(&self, routine_id: u16) -> Vec<u8> {
        let routines = self.routines.read();

        if routines.contains_key(&routine_id) {
            info!(
                routine_id = format!("0x{:04X}", routine_id),
                "Stopping routine"
            );

            {
                let mut states = self.routine_states.write();
                if let Some(state) = states.get_mut(&routine_id) {
                    state.running = false;
                }
            }

            let routine_id_bytes = routine_id.to_be_bytes();
            positive_response(
                service_id::ROUTINE_CONTROL,
                &[
                    routine_sub_function::STOP_ROUTINE,
                    routine_id_bytes[0],
                    routine_id_bytes[1],
                ],
            )
        } else {
            debug!(
                routine_id = format!("0x{:04X}", routine_id),
                "Unknown routine ID"
            );
            negative_response(service_id::ROUTINE_CONTROL, nrc::REQUEST_OUT_OF_RANGE)
        }
    }

    fn handle_request_routine_results(&self, routine_id: u16) -> Vec<u8> {
        let routines = self.routines.read();

        if let Some(routine) = routines.get(&routine_id) {
            info!(
                routine_id = format!("0x{:04X}", routine_id),
                "Requesting routine results"
            );

            let states = self.routine_states.read();
            let result = states
                .get(&routine_id)
                .and_then(|s| s.result.clone())
                .unwrap_or_else(|| routine.default_result.clone());

            let routine_id_bytes = routine_id.to_be_bytes();
            let mut response_data = vec![
                routine_sub_function::REQUEST_ROUTINE_RESULTS,
                routine_id_bytes[0],
                routine_id_bytes[1],
            ];
            response_data.extend_from_slice(&result);

            positive_response(service_id::ROUTINE_CONTROL, &response_data)
        } else {
            debug!(
                routine_id = format!("0x{:04X}", routine_id),
                "Unknown routine ID"
            );
            negative_response(service_id::ROUTINE_CONTROL, nrc::REQUEST_OUT_OF_RANGE)
        }
    }

    // =========================================================================
    // DynamicallyDefineDataIdentifier Handler (0x2C)
    // =========================================================================

    fn handle_ddid(&self, request: &[u8]) -> Vec<u8> {
        if request.len() < 4 {
            return negative_response(
                service_id::DYNAMICALLY_DEFINE_DATA_ID,
                nrc::INCORRECT_MESSAGE_LENGTH,
            );
        }

        let sub_function = request[1];
        let ddid = u16::from_be_bytes([request[2], request[3]]);

        match sub_function {
            ddid_sub_function::DEFINE_BY_IDENTIFIER => {
                self.handle_define_by_identifier(ddid, &request[4..])
            }
            ddid_sub_function::CLEAR_DYNAMICALLY_DEFINED_DATA_IDENTIFIER => {
                self.handle_clear_ddid(ddid)
            }
            _ => {
                debug!(
                    sub_function = format!("0x{:02X}", sub_function),
                    "Unsupported DDID sub-function"
                );
                negative_response(
                    service_id::DYNAMICALLY_DEFINE_DATA_ID,
                    nrc::SUB_FUNCTION_NOT_SUPPORTED,
                )
            }
        }
    }

    fn handle_define_by_identifier(&self, ddid: u16, data: &[u8]) -> Vec<u8> {
        if !(0xF200..=0xF3FF).contains(&ddid) {
            debug!(ddid = format!("0x{:04X}", ddid), "DDID out of range");
            return negative_response(
                service_id::DYNAMICALLY_DEFINE_DATA_ID,
                nrc::REQUEST_OUT_OF_RANGE,
            );
        }

        if data.is_empty() || data.len() % 4 != 0 {
            return negative_response(
                service_id::DYNAMICALLY_DEFINE_DATA_ID,
                nrc::INCORRECT_MESSAGE_LENGTH,
            );
        }

        let mut definitions = Vec::new();
        let mut i = 0;
        while i + 3 < data.len() {
            let source_did = u16::from_be_bytes([data[i], data[i + 1]]);
            let position = data[i + 2];
            let size = data[i + 3];

            let params = self.parameters.read();
            if !params.contains_key(&source_did) {
                debug!(
                    source_did = format!("0x{:04X}", source_did),
                    "Source DID not found"
                );
                return negative_response(
                    service_id::DYNAMICALLY_DEFINE_DATA_ID,
                    nrc::REQUEST_OUT_OF_RANGE,
                );
            }

            definitions.push(DdidDefinition {
                source_did,
                position,
                size,
            });
            i += 4;
        }

        info!(
            ddid = format!("0x{:04X}", ddid),
            source_count = definitions.len(),
            "Defining DDID"
        );

        {
            let mut ddids = self.ddid_definitions.write();
            ddids.insert(ddid, definitions);
        }

        let ddid_bytes = ddid.to_be_bytes();
        positive_response(
            service_id::DYNAMICALLY_DEFINE_DATA_ID,
            &[
                ddid_sub_function::DEFINE_BY_IDENTIFIER,
                ddid_bytes[0],
                ddid_bytes[1],
            ],
        )
    }

    fn handle_clear_ddid(&self, ddid: u16) -> Vec<u8> {
        info!(ddid = format!("0x{:04X}", ddid), "Clearing DDID");

        let mut ddids = self.ddid_definitions.write();
        if ddids.remove(&ddid).is_none() {
            debug!(ddid = format!("0x{:04X}", ddid), "DDID not found");
            return negative_response(
                service_id::DYNAMICALLY_DEFINE_DATA_ID,
                nrc::REQUEST_OUT_OF_RANGE,
            );
        }

        let ddid_bytes = ddid.to_be_bytes();
        positive_response(
            service_id::DYNAMICALLY_DEFINE_DATA_ID,
            &[
                ddid_sub_function::CLEAR_DYNAMICALLY_DEFINED_DATA_IDENTIFIER,
                ddid_bytes[0],
                ddid_bytes[1],
            ],
        )
    }

    pub fn read_ddid(&self, ddid: u16) -> Option<Vec<u8>> {
        let ddids = self.ddid_definitions.read();
        let definitions = ddids.get(&ddid)?;

        let params = self.parameters.read();
        let mut result = Vec::new();

        for def in definitions {
            if let Some(param) = params.get(&def.source_did) {
                let start = (def.position as usize).saturating_sub(1);
                let end = (start + def.size as usize).min(param.value.len());

                if start < param.value.len() {
                    result.extend_from_slice(&param.value[start..end]);
                }
            }
        }

        Some(result)
    }

    // =========================================================================
    // Programming Services Handlers (0x34, 0x35, 0x36, 0x37)
    // =========================================================================

    fn handle_request_download(&self, request: &[u8]) -> Vec<u8> {
        if request.len() < 3 {
            return negative_response(service_id::REQUEST_DOWNLOAD, nrc::INCORRECT_MESSAGE_LENGTH);
        }

        let current_session = self.session.load(Ordering::SeqCst);
        // Accept programming (0x02) or extended (0x03) session
        if current_session != 0x02 && current_session < 0x03 {
            debug!(
                session = format!("0x{:02X}", current_session),
                "Download denied: requires programming or extended session"
            );
            return negative_response(service_id::REQUEST_DOWNLOAD, nrc::CONDITIONS_NOT_CORRECT);
        }

        // Per ISO 14229: RequestDownload requires security access to be unlocked
        if !*self.security_unlocked.read() {
            debug!("Download denied: requires security access (0x27)");
            return negative_response(service_id::REQUEST_DOWNLOAD, nrc::SECURITY_ACCESS_DENIED);
        }

        if self.download_state.read().is_some() {
            debug!("Download denied: transfer already in progress");
            return negative_response(service_id::REQUEST_DOWNLOAD, nrc::CONDITIONS_NOT_CORRECT);
        }

        let _data_format = request[1];
        let addr_len_format = request[2];

        let memory_size_len = ((addr_len_format >> 4) & 0x0F) as usize;
        let memory_addr_len = (addr_len_format & 0x0F) as usize;

        let expected_len = 3 + memory_addr_len + memory_size_len;
        if request.len() < expected_len {
            return negative_response(service_id::REQUEST_DOWNLOAD, nrc::INCORRECT_MESSAGE_LENGTH);
        }

        let addr_start = 3;
        let addr_end = addr_start + memory_addr_len;
        let mut memory_address: u32 = 0;
        for byte in &request[addr_start..addr_end] {
            memory_address = (memory_address << 8) | *byte as u32;
        }

        let size_start = addr_end;
        let size_end = size_start + memory_size_len;
        let mut memory_size: u32 = 0;
        for byte in &request[size_start..size_end] {
            memory_size = (memory_size << 8) | *byte as u32;
        }

        info!(
            address = format!("0x{:08X}", memory_address),
            size = memory_size,
            "RequestDownload: initiating download"
        );

        let download_state = DownloadState {
            address: memory_address,
            total_size: memory_size,
            received: 0,
            buffer: Vec::with_capacity(memory_size as usize),
            expected_block: self.block_counter_start,
        };
        *self.download_state.write() = Some(download_state);

        let max_block_length: u16 = 4096;
        let length_format_id: u8 = 0x20;

        positive_response(
            service_id::REQUEST_DOWNLOAD,
            &[
                length_format_id,
                (max_block_length >> 8) as u8,
                (max_block_length & 0xFF) as u8,
            ],
        )
    }

    fn handle_request_upload(&self, request: &[u8]) -> Vec<u8> {
        if request.len() < 3 {
            return negative_response(service_id::REQUEST_UPLOAD, nrc::INCORRECT_MESSAGE_LENGTH);
        }

        let current_session = self.session.load(Ordering::SeqCst);
        // Accept programming (0x02) or extended (0x03) session
        if current_session != 0x02 && current_session < 0x03 {
            debug!(
                session = format!("0x{:02X}", current_session),
                "Upload denied: requires programming or extended session"
            );
            return negative_response(service_id::REQUEST_UPLOAD, nrc::CONDITIONS_NOT_CORRECT);
        }

        // Per ISO 14229: RequestUpload requires security access to be unlocked
        if !*self.security_unlocked.read() {
            debug!("Upload denied: requires security access (0x27)");
            return negative_response(service_id::REQUEST_UPLOAD, nrc::SECURITY_ACCESS_DENIED);
        }

        if self.upload_state.read().is_some() || self.download_state.read().is_some() {
            debug!("Upload denied: transfer already in progress");
            return negative_response(service_id::REQUEST_UPLOAD, nrc::CONDITIONS_NOT_CORRECT);
        }

        let _data_format = request[1];
        let addr_len_format = request[2];

        let memory_size_len = ((addr_len_format >> 4) & 0x0F) as usize;
        let memory_addr_len = (addr_len_format & 0x0F) as usize;

        let expected_len = 3 + memory_addr_len + memory_size_len;
        if request.len() < expected_len {
            return negative_response(service_id::REQUEST_UPLOAD, nrc::INCORRECT_MESSAGE_LENGTH);
        }

        let addr_start = 3;
        let addr_end = addr_start + memory_addr_len;
        let mut memory_address: u32 = 0;
        for byte in &request[addr_start..addr_end] {
            memory_address = (memory_address << 8) | *byte as u32;
        }

        let size_start = addr_end;
        let size_end = size_start + memory_size_len;
        let mut memory_size: u32 = 0;
        for byte in &request[size_start..size_end] {
            memory_size = (memory_size << 8) | *byte as u32;
        }

        let sim_memory = self.simulated_memory.read();
        let start = memory_address as usize;
        let end = (memory_address + memory_size) as usize;

        if end > sim_memory.len() {
            debug!(
                address = format!("0x{:08X}", memory_address),
                size = memory_size,
                "Upload denied: address range out of bounds"
            );
            return negative_response(service_id::REQUEST_UPLOAD, nrc::REQUEST_OUT_OF_RANGE);
        }

        let buffer = sim_memory[start..end].to_vec();

        info!(
            address = format!("0x{:08X}", memory_address),
            size = memory_size,
            "RequestUpload: initiating upload"
        );

        let upload_state = UploadState {
            address: memory_address,
            total_size: memory_size,
            sent: 0,
            buffer,
            next_block: self.block_counter_start,
        };
        *self.upload_state.write() = Some(upload_state);

        let max_block_length: u16 = 4096;
        let length_format_id: u8 = 0x20;

        positive_response(
            service_id::REQUEST_UPLOAD,
            &[
                length_format_id,
                (max_block_length >> 8) as u8,
                (max_block_length & 0xFF) as u8,
            ],
        )
    }

    fn handle_transfer_data(&self, request: &[u8]) -> Vec<u8> {
        let upload_active = self.upload_state.read().is_some();
        let download_active = self.download_state.read().is_some();

        if upload_active {
            self.handle_transfer_data_upload(request)
        } else if download_active {
            self.handle_transfer_data_download(request)
        } else {
            debug!("TransferData denied: no active transfer");
            negative_response(service_id::TRANSFER_DATA, nrc::REQUEST_SEQUENCE_ERROR)
        }
    }

    fn handle_transfer_data_upload(&self, request: &[u8]) -> Vec<u8> {
        if request.len() < 2 {
            return negative_response(service_id::TRANSFER_DATA, nrc::INCORRECT_MESSAGE_LENGTH);
        }

        let block_counter = request[1];

        let mut upload_state_guard = self.upload_state.write();
        let upload_state = match upload_state_guard.as_mut() {
            Some(state) => state,
            None => {
                return negative_response(service_id::TRANSFER_DATA, nrc::REQUEST_SEQUENCE_ERROR)
            }
        };

        if block_counter != upload_state.next_block {
            debug!(
                expected = upload_state.next_block,
                received = block_counter,
                "TransferData upload: wrong block sequence counter"
            );
            return negative_response(service_id::TRANSFER_DATA, nrc::WRONG_BLOCK_SEQUENCE_COUNTER);
        }

        let remaining = upload_state.total_size - upload_state.sent;
        let max_block_data = 4094_u32;
        let data_size = remaining.min(max_block_data) as usize;

        let start = upload_state.sent as usize;
        let end = start + data_size;
        let data = &upload_state.buffer[start..end];

        upload_state.sent += data_size as u32;
        upload_state.next_block = upload_state.next_block.wrapping_add(1);
        if upload_state.next_block == 0 && self.block_counter_wrap > 0 {
            upload_state.next_block = self.block_counter_wrap;
        }

        info!(
            block = block_counter,
            bytes = data_size,
            total_sent = upload_state.sent,
            total_expected = upload_state.total_size,
            "TransferData upload: block sent"
        );

        let mut response_data = vec![block_counter];
        response_data.extend_from_slice(data);
        positive_response(service_id::TRANSFER_DATA, &response_data)
    }

    fn handle_transfer_data_download(&self, request: &[u8]) -> Vec<u8> {
        if request.len() < 3 {
            return negative_response(service_id::TRANSFER_DATA, nrc::INCORRECT_MESSAGE_LENGTH);
        }

        let block_counter = request[1];
        let data = &request[2..];

        let mut download_state_guard = self.download_state.write();
        let download_state = match download_state_guard.as_mut() {
            Some(state) => state,
            None => {
                return negative_response(service_id::TRANSFER_DATA, nrc::REQUEST_SEQUENCE_ERROR)
            }
        };

        if block_counter != download_state.expected_block {
            debug!(
                expected = download_state.expected_block,
                received = block_counter,
                "TransferData download: wrong block sequence counter"
            );
            return negative_response(service_id::TRANSFER_DATA, nrc::WRONG_BLOCK_SEQUENCE_COUNTER);
        }

        if download_state.received + data.len() as u32 > download_state.total_size {
            debug!(
                received = download_state.received,
                data_len = data.len(),
                total = download_state.total_size,
                "TransferData download: data exceeds expected size"
            );
            return negative_response(service_id::TRANSFER_DATA, nrc::UPLOAD_DOWNLOAD_NOT_ACCEPTED);
        }

        download_state.buffer.extend_from_slice(data);
        download_state.received += data.len() as u32;

        download_state.expected_block = download_state.expected_block.wrapping_add(1);
        if download_state.expected_block == 0 && self.block_counter_wrap > 0 {
            download_state.expected_block = self.block_counter_wrap;
        }

        info!(
            block = block_counter,
            bytes = data.len(),
            total_received = download_state.received,
            total_expected = download_state.total_size,
            "TransferData download: block received"
        );

        positive_response(service_id::TRANSFER_DATA, &[block_counter])
    }

    fn handle_request_transfer_exit(&self, request: &[u8]) -> Vec<u8> {
        if request.is_empty() {
            return negative_response(
                service_id::REQUEST_TRANSFER_EXIT,
                nrc::INCORRECT_MESSAGE_LENGTH,
            );
        }

        let upload_active = self.upload_state.read().is_some();
        let download_active = self.download_state.read().is_some();

        if upload_active {
            self.handle_request_transfer_exit_upload()
        } else if download_active {
            self.handle_request_transfer_exit_download(request)
        } else {
            debug!("RequestTransferExit denied: no active transfer");
            negative_response(
                service_id::REQUEST_TRANSFER_EXIT,
                nrc::REQUEST_SEQUENCE_ERROR,
            )
        }
    }

    fn handle_request_transfer_exit_upload(&self) -> Vec<u8> {
        let upload_state = match self.upload_state.write().take() {
            Some(state) => state,
            None => {
                return negative_response(
                    service_id::REQUEST_TRANSFER_EXIT,
                    nrc::REQUEST_SEQUENCE_ERROR,
                )
            }
        };

        let crc32 = CRC32.checksum(&upload_state.buffer);

        info!(
            address = format!("0x{:08X}", upload_state.address),
            total_sent = upload_state.sent,
            crc32 = format!("0x{:08X}", crc32),
            "RequestTransferExit upload: transfer completed"
        );

        let crc_bytes = crc32.to_be_bytes();
        positive_response(service_id::REQUEST_TRANSFER_EXIT, &crc_bytes)
    }

    fn handle_request_transfer_exit_download(&self, _request: &[u8]) -> Vec<u8> {
        let download_state = match self.download_state.write().take() {
            Some(state) => state,
            None => {
                return negative_response(
                    service_id::REQUEST_TRANSFER_EXIT,
                    nrc::REQUEST_SEQUENCE_ERROR,
                )
            }
        };

        let buffer = &download_state.buffer;
        let total_len = buffer.len();

        // Verify firmware payload format
        let verification_result = self.verify_firmware_payload(buffer);

        match verification_result {
            Ok(version) => {
                info!(
                    address = format!("0x{:08X}", download_state.address),
                    total_received = total_len,
                    version = %version,
                    "Firmware verified successfully, pending version update"
                );

                // Store pending version (will be applied after ECU reset)
                *self.pending_firmware_version.write() = Some(version);

                // Return success (0x00 = verification passed)
                positive_response(service_id::REQUEST_TRANSFER_EXIT, &[0x00])
            }
            Err(err) => {
                warn!(
                    address = format!("0x{:08X}", download_state.address),
                    total_received = total_len,
                    error = %err,
                    "Firmware verification failed"
                );

                // Return negative response: GeneralProgrammingFailure (0x72)
                // This properly signals that the firmware verification failed
                negative_response(
                    service_id::REQUEST_TRANSFER_EXIT,
                    nrc::GENERAL_PROGRAMMING_FAILURE,
                )
            }
        }
    }

    /// Verify firmware payload format and checksum.
    /// Returns the version string if valid, or error message if invalid.
    fn verify_firmware_payload(&self, buffer: &[u8]) -> Result<String, String> {
        // Verify structure + checksum via shared FirmwareImage
        let version = FirmwareImage::verify_bytes(buffer)
            .map_err(|e: crate::sw_package::FirmwareImageError| e.to_string())?;

        // Parse to check target ECU
        let image = FirmwareImage::from_bytes(buffer)
            .map_err(|e: crate::sw_package::FirmwareImageError| e.to_string())?;

        if !image.target_ecu.is_empty() && image.target_ecu != self.ecu_id {
            return Err(format!(
                "Firmware target mismatch: package is for '{}', but this is '{}'",
                image.target_ecu, self.ecu_id
            ));
        }

        info!(
            payload_size = buffer.len(),
            version = %version,
            target_ecu = %if image.target_ecu.is_empty() { "(any)" } else { &image.target_ecu },
            "Firmware payload verified"
        );

        Ok(version)
    }

    // =========================================================================
    // InputOutputControlById Handler (0x2F)
    // =========================================================================

    fn handle_io_control(&self, request: &[u8]) -> Vec<u8> {
        if request.len() < 4 {
            return negative_response(service_id::IO_CONTROL_BY_ID, nrc::INCORRECT_MESSAGE_LENGTH);
        }

        let output_id = u16::from_be_bytes([request[1], request[2]]);
        let control_option = request[3];
        let control_data = &request[4..];

        let mut outputs = self.outputs.write();
        let output = match outputs.get_mut(&output_id) {
            Some(o) => o,
            None => {
                debug!(
                    output_id = format!("0x{:04X}", output_id),
                    "Unknown output ID"
                );
                return negative_response(service_id::IO_CONTROL_BY_ID, nrc::REQUEST_OUT_OF_RANGE);
            }
        };

        if output.requires_security && !*self.security_unlocked.read() {
            debug!(
                output_id = format!("0x{:04X}", output_id),
                "I/O control denied: requires security"
            );
            return negative_response(service_id::IO_CONTROL_BY_ID, nrc::SECURITY_ACCESS_DENIED);
        }

        match control_option {
            io_control_option::RETURN_CONTROL_TO_ECU => {
                info!(
                    output_id = format!("0x{:04X}", output_id),
                    "I/O: Return control to ECU"
                );
                output.controlled_by_tester = false;
                output.frozen = false;
            }
            io_control_option::RESET_TO_DEFAULT => {
                info!(
                    output_id = format!("0x{:04X}", output_id),
                    "I/O: Reset to default"
                );
                output.current_value = output.default_value.clone();
                output.controlled_by_tester = false;
                output.frozen = false;
            }
            io_control_option::FREEZE_CURRENT_STATE => {
                info!(
                    output_id = format!("0x{:04X}", output_id),
                    "I/O: Freeze current state"
                );
                output.frozen = true;
                output.controlled_by_tester = true;
            }
            io_control_option::SHORT_TERM_ADJUSTMENT => {
                if control_data.is_empty() {
                    return negative_response(
                        service_id::IO_CONTROL_BY_ID,
                        nrc::INCORRECT_MESSAGE_LENGTH,
                    );
                }
                let control_state_len = output.current_value.len();
                if control_data.len() >= control_state_len {
                    let new_value = &control_data[..control_state_len];
                    if control_data.len() >= control_state_len * 2 {
                        let mask = &control_data[control_state_len..control_state_len * 2];
                        for i in 0..control_state_len {
                            output.current_value[i] =
                                (output.current_value[i] & !mask[i]) | (new_value[i] & mask[i]);
                        }
                    } else {
                        output.current_value = new_value.to_vec();
                    }
                } else {
                    output.current_value = control_data.to_vec();
                }
                output.controlled_by_tester = true;
                output.frozen = false;
                info!(output_id = format!("0x{:04X}", output_id), value = ?output.current_value, "I/O: Short-term adjustment");
            }
            _ => {
                debug!(control_option, "Unsupported I/O control option");
                return negative_response(
                    service_id::IO_CONTROL_BY_ID,
                    nrc::SUB_FUNCTION_NOT_SUPPORTED,
                );
            }
        }

        let mut response_data = vec![
            (output_id >> 8) as u8,
            (output_id & 0xFF) as u8,
            control_option,
        ];
        response_data.extend_from_slice(&output.current_value);
        positive_response(service_id::IO_CONTROL_BY_ID, &response_data)
    }

    // =========================================================================
    // LinkControl Handler (0x87)
    // =========================================================================

    fn handle_link_control(&self, request: &[u8]) -> Vec<u8> {
        if request.len() < 2 {
            return negative_response(service_id::LINK_CONTROL, nrc::INCORRECT_MESSAGE_LENGTH);
        }

        let current_session = self.session.load(Ordering::SeqCst);
        // Accept programming (0x02) or extended (0x03) session
        if current_session != 0x02 && current_session < 0x03 {
            debug!(
                session = format!("0x{:02X}", current_session),
                "Link control denied: requires programming or extended session"
            );
            return negative_response(service_id::LINK_CONTROL, nrc::CONDITIONS_NOT_CORRECT);
        }

        let sub_function = request[1];

        match sub_function {
            link_control_sub_function::VERIFY_FIXED_BAUD_RATE => {
                if request.len() < 3 {
                    return negative_response(
                        service_id::LINK_CONTROL,
                        nrc::INCORRECT_MESSAGE_LENGTH,
                    );
                }
                let baud_rate_id = request[2];
                let baud_rate = match baud_rate_id {
                    link_baud_rate::CAN_125K => 125000,
                    link_baud_rate::CAN_250K => 250000,
                    link_baud_rate::CAN_500K => 500000,
                    link_baud_rate::CAN_1M => 1000000,
                    _ => {
                        debug!(baud_rate_id, "Unsupported baud rate ID");
                        return negative_response(
                            service_id::LINK_CONTROL,
                            nrc::REQUEST_OUT_OF_RANGE,
                        );
                    }
                };

                *self.pending_baud_rate.write() = Some(baud_rate);
                info!(
                    baud_rate_id = format!("0x{:02X}", baud_rate_id),
                    baud_rate, "Link control: verified fixed baud rate"
                );

                positive_response(service_id::LINK_CONTROL, &[sub_function])
            }
            link_control_sub_function::VERIFY_SPECIFIC_BAUD_RATE => {
                if request.len() < 5 {
                    return negative_response(
                        service_id::LINK_CONTROL,
                        nrc::INCORRECT_MESSAGE_LENGTH,
                    );
                }
                let baud_rate =
                    ((request[2] as u32) << 16) | ((request[3] as u32) << 8) | (request[4] as u32);

                if baud_rate < 10000 || baud_rate > 1000000 {
                    debug!(baud_rate, "Baud rate out of range");
                    return negative_response(service_id::LINK_CONTROL, nrc::REQUEST_OUT_OF_RANGE);
                }

                *self.pending_baud_rate.write() = Some(baud_rate);
                info!(baud_rate, "Link control: verified specific baud rate");

                positive_response(service_id::LINK_CONTROL, &[sub_function])
            }
            link_control_sub_function::TRANSITION_BAUD_RATE => {
                let pending = self.pending_baud_rate.read().clone();
                match pending {
                    Some(baud_rate) => {
                        *self.current_baud_rate.write() = baud_rate;
                        *self.pending_baud_rate.write() = None;
                        info!(baud_rate, "Link control: transitioned to new baud rate");
                        positive_response(service_id::LINK_CONTROL, &[sub_function])
                    }
                    None => {
                        debug!("Link control transition denied: no pending baud rate");
                        negative_response(service_id::LINK_CONTROL, nrc::REQUEST_SEQUENCE_ERROR)
                    }
                }
            }
            _ => {
                debug!(
                    sub_function = format!("0x{:02X}", sub_function),
                    "Unsupported link control sub-function"
                );
                negative_response(service_id::LINK_CONTROL, nrc::SUB_FUNCTION_NOT_SUPPORTED)
            }
        }
    }
}
