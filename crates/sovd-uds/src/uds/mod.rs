//! UDS (Unified Diagnostic Services) protocol implementation
//!
//! This module provides the UDS protocol layer for communicating with ECUs.

pub mod dtc;
mod error;
mod nrc;
mod services;

pub use dtc::{
    dtc_group, status_bit as dtc_status_bit, sub_function as dtc_sub_function, Dtc, DtcCategory,
    DtcCountResult, DtcExtendedDataRecord, DtcSnapshotRecord, DtcStatus,
};
pub use error::UdsError;
pub use nrc::NegativeResponseCode;
pub use services::UdsService;

/// RoutineControl (0x31) sub-functions
pub mod routine_sub_function {
    /// Start routine
    pub const START_ROUTINE: u8 = 0x01;
    /// Stop routine
    pub const STOP_ROUTINE: u8 = 0x02;
    /// Request routine results
    pub const REQUEST_ROUTINE_RESULTS: u8 = 0x03;
}

/// DynamicallyDefineDataIdentifier (0x2C) sub-functions
pub mod ddid_sub_function {
    /// Define by identifier - compose DDID from source DIDs
    pub const DEFINE_BY_IDENTIFIER: u8 = 0x01;
    /// Define by memory address
    pub const DEFINE_BY_MEMORY_ADDRESS: u8 = 0x02;
    /// Clear dynamically defined data identifier
    pub const CLEAR_DYNAMICALLY_DEFINED_DATA_IDENTIFIER: u8 = 0x03;
}

/// InputOutputControlById (0x2F) sub-functions
pub mod io_control_option {
    /// Return control to ECU - release tester control
    pub const RETURN_CONTROL_TO_ECU: u8 = 0x00;
    /// Reset output to default value
    pub const RESET_TO_DEFAULT: u8 = 0x01;
    /// Freeze current state
    pub const FREEZE_CURRENT_STATE: u8 = 0x02;
    /// Short-term adjustment - set specific value
    pub const SHORT_TERM_ADJUSTMENT: u8 = 0x03;
}

/// LinkControl (0x87) sub-functions
pub mod link_control_sub_function {
    /// Verify fixed baud rate
    pub const VERIFY_FIXED_BAUD_RATE: u8 = 0x01;
    /// Verify specific baud rate
    pub const VERIFY_SPECIFIC_BAUD_RATE: u8 = 0x02;
    /// Transition baud rate
    pub const TRANSITION_BAUD_RATE: u8 = 0x03;
}

/// LinkControl (0x87) baud rate identifiers
pub mod link_baud_rate {
    /// CAN 125 kbps
    pub const CAN_125K: u8 = 0x10;
    /// CAN 250 kbps
    pub const CAN_250K: u8 = 0x11;
    /// CAN 500 kbps
    pub const CAN_500K: u8 = 0x12;
    /// CAN 1 Mbps
    pub const CAN_1M: u8 = 0x13;
}

/// ECUReset (0x11) sub-functions
pub mod reset_type {
    /// Hard reset - complete shutdown and restart of ECU
    pub const HARD_RESET: u8 = 0x01;
    /// Key off/on reset - simulate ignition cycle
    pub const KEY_OFF_ON_RESET: u8 = 0x02;
    /// Soft reset - application-level restart
    pub const SOFT_RESET: u8 = 0x03;
}

/// Standard UDS service ID constants
pub mod service_id {
    pub const DIAGNOSTIC_SESSION_CONTROL: u8 = 0x10;
    pub const ECU_RESET: u8 = 0x11;
    pub const CLEAR_DIAGNOSTIC_INFO: u8 = 0x14;
    pub const READ_DTC_INFO: u8 = 0x19;
    pub const READ_DATA_BY_ID: u8 = 0x22;
    pub const READ_MEMORY_BY_ADDRESS: u8 = 0x23;
    pub const SECURITY_ACCESS: u8 = 0x27;
    pub const COMMUNICATION_CONTROL: u8 = 0x28;
    pub const READ_DATA_BY_PERIODIC_ID: u8 = 0x2A;
    pub const DYNAMICALLY_DEFINE_DATA_ID: u8 = 0x2C;
    pub const WRITE_DATA_BY_ID: u8 = 0x2E;
    pub const IO_CONTROL_BY_ID: u8 = 0x2F;
    pub const ROUTINE_CONTROL: u8 = 0x31;
    pub const REQUEST_DOWNLOAD: u8 = 0x34;
    pub const REQUEST_UPLOAD: u8 = 0x35;
    pub const TRANSFER_DATA: u8 = 0x36;
    pub const REQUEST_TRANSFER_EXIT: u8 = 0x37;
    pub const TESTER_PRESENT: u8 = 0x3E;
    pub const CONTROL_DTC_SETTING: u8 = 0x85;
    pub const RESPONSE_ON_EVENT: u8 = 0x86;
    pub const LINK_CONTROL: u8 = 0x87;
    pub const NEGATIVE_RESPONSE: u8 = 0x7F;
}

/// Standard UDS Data Identifiers (ISO 14229-1 Annex C)
pub mod standard_did {
    // Boot / Software Identification
    pub const BOOT_SOFTWARE_ID: u16 = 0xF180;
    pub const APPLICATION_SOFTWARE_ID: u16 = 0xF181;
    pub const APPLICATION_DATA_ID: u16 = 0xF182;
    pub const BOOT_SOFTWARE_FINGERPRINT: u16 = 0xF183;
    pub const APP_SOFTWARE_FINGERPRINT: u16 = 0xF184;
    pub const APP_DATA_FINGERPRINT: u16 = 0xF185;

    // Session
    pub const ACTIVE_DIAGNOSTIC_SESSION: u16 = 0xF186;

    // Identification
    pub const SPARE_PART_NUMBER: u16 = 0xF187;
    pub const ECU_SOFTWARE_NUMBER: u16 = 0xF188;
    pub const ECU_SOFTWARE_VERSION: u16 = 0xF189;
    pub const SYSTEM_SUPPLIER_ID: u16 = 0xF18A;
    pub const ECU_MANUFACTURING_DATE: u16 = 0xF18B;
    pub const ECU_SERIAL_NUMBER: u16 = 0xF18C;

    // Vehicle / Hardware
    pub const VIN: u16 = 0xF190;
    pub const ECU_HARDWARE_NUMBER: u16 = 0xF191;
    pub const SUPPLIER_HW_NUMBER: u16 = 0xF192;
    pub const SUPPLIER_HW_VERSION: u16 = 0xF193;
    pub const SUPPLIER_SW_NUMBER: u16 = 0xF194;
    pub const SUPPLIER_SW_VERSION: u16 = 0xF195;
    pub const SYSTEM_NAME: u16 = 0xF197;
    pub const PROGRAMMING_DATE: u16 = 0xF199;
    pub const TESTER_SERIAL_NUMBER: u16 = 0xF19E;

    /// Standard identification DIDs for enumeration: (did, key, label)
    pub const IDENTIFICATION_DIDS: &[(u16, &str, &str)] = &[
        (VIN, "vin", "VIN"),
        (ECU_SERIAL_NUMBER, "ecu_serial", "ECU Serial Number"),
        (ECU_SOFTWARE_NUMBER, "sw_number", "ECU Software Number"),
        (
            ECU_SOFTWARE_VERSION,
            "ecu_sw_version",
            "ECU Software Version",
        ),
        (ECU_HARDWARE_NUMBER, "hw_number", "ECU Hardware Number"),
        (SUPPLIER_HW_VERSION, "hw_version", "Hardware Version"),
        (SPARE_PART_NUMBER, "part_number", "Spare Part Number"),
        (SYSTEM_SUPPLIER_ID, "supplier", "System Supplier"),
        (ECU_MANUFACTURING_DATE, "mfg_date", "Manufacturing Date"),
        (
            SUPPLIER_HW_NUMBER,
            "supplier_hw_number",
            "Supplier HW Number",
        ),
        (
            SUPPLIER_SW_NUMBER,
            "supplier_sw_number",
            "Supplier SW Number",
        ),
        (
            SUPPLIER_SW_VERSION,
            "supplier_sw_version",
            "Supplier SW Version",
        ),
        (SYSTEM_NAME, "system_name", "System Name"),
        (PROGRAMMING_DATE, "programming_date", "Programming Date"),
        (BOOT_SOFTWARE_ID, "boot_sw_id", "Boot Software ID"),
        (
            APPLICATION_SOFTWARE_ID,
            "app_sw_id",
            "Application Software ID",
        ),
        (APPLICATION_DATA_ID, "app_data_id", "Application Data ID"),
        (
            BOOT_SOFTWARE_FINGERPRINT,
            "boot_sw_fingerprint",
            "Boot Software Fingerprint",
        ),
        (
            APP_SOFTWARE_FINGERPRINT,
            "app_sw_fingerprint",
            "App Software Fingerprint",
        ),
        (
            APP_DATA_FINGERPRINT,
            "app_data_fingerprint",
            "App Data Fingerprint",
        ),
        (
            TESTER_SERIAL_NUMBER,
            "tester_serial",
            "Tester Serial Number",
        ),
    ];
}

use crate::config::ServiceOverrides;

/// Resolved service IDs for a specific ECU
///
/// This struct holds the actual service IDs to use when communicating with an ECU.
/// It starts with standard UDS service IDs and applies any OEM-specific overrides.
#[derive(Debug, Clone, Copy)]
pub struct ServiceIds {
    pub diagnostic_session_control: u8,
    pub ecu_reset: u8,
    pub clear_diagnostic_info: u8,
    pub read_dtc_info: u8,
    pub read_data_by_id: u8,
    pub security_access: u8,
    pub read_data_by_periodic_id: u8,
    pub dynamically_define_data_id: u8,
    pub write_data_by_id: u8,
    pub io_control_by_id: u8,
    pub routine_control: u8,
    pub request_download: u8,
    pub request_upload: u8,
    pub transfer_data: u8,
    pub request_transfer_exit: u8,
    pub tester_present: u8,
    pub link_control: u8,
    pub negative_response: u8,
}

impl Default for ServiceIds {
    fn default() -> Self {
        Self {
            diagnostic_session_control: service_id::DIAGNOSTIC_SESSION_CONTROL,
            ecu_reset: service_id::ECU_RESET,
            clear_diagnostic_info: service_id::CLEAR_DIAGNOSTIC_INFO,
            read_dtc_info: service_id::READ_DTC_INFO,
            read_data_by_id: service_id::READ_DATA_BY_ID,
            security_access: service_id::SECURITY_ACCESS,
            read_data_by_periodic_id: service_id::READ_DATA_BY_PERIODIC_ID,
            dynamically_define_data_id: service_id::DYNAMICALLY_DEFINE_DATA_ID,
            write_data_by_id: service_id::WRITE_DATA_BY_ID,
            io_control_by_id: service_id::IO_CONTROL_BY_ID,
            routine_control: service_id::ROUTINE_CONTROL,
            request_download: service_id::REQUEST_DOWNLOAD,
            request_upload: service_id::REQUEST_UPLOAD,
            transfer_data: service_id::TRANSFER_DATA,
            request_transfer_exit: service_id::REQUEST_TRANSFER_EXIT,
            tester_present: service_id::TESTER_PRESENT,
            link_control: service_id::LINK_CONTROL,
            negative_response: service_id::NEGATIVE_RESPONSE,
        }
    }
}

impl ServiceIds {
    /// Create ServiceIds with OEM-specific overrides applied
    pub fn from_overrides(overrides: &ServiceOverrides) -> Self {
        let mut ids = Self::default();

        if let Some(v) = overrides.diagnostic_session_control {
            ids.diagnostic_session_control = v;
        }
        if let Some(v) = overrides.ecu_reset {
            ids.ecu_reset = v;
        }
        if let Some(v) = overrides.clear_diagnostic_info {
            ids.clear_diagnostic_info = v;
        }
        if let Some(v) = overrides.read_dtc_info {
            ids.read_dtc_info = v;
        }
        if let Some(v) = overrides.read_data_by_id {
            ids.read_data_by_id = v;
        }
        if let Some(v) = overrides.security_access {
            ids.security_access = v;
        }
        if let Some(v) = overrides.read_data_by_periodic_id {
            ids.read_data_by_periodic_id = v;
        }
        if let Some(v) = overrides.dynamically_define_data_id {
            ids.dynamically_define_data_id = v;
        }
        if let Some(v) = overrides.write_data_by_id {
            ids.write_data_by_id = v;
        }
        if let Some(v) = overrides.io_control_by_id {
            ids.io_control_by_id = v;
        }
        if let Some(v) = overrides.routine_control {
            ids.routine_control = v;
        }
        if let Some(v) = overrides.request_download {
            ids.request_download = v;
        }
        if let Some(v) = overrides.request_upload {
            ids.request_upload = v;
        }
        if let Some(v) = overrides.transfer_data {
            ids.transfer_data = v;
        }
        if let Some(v) = overrides.request_transfer_exit {
            ids.request_transfer_exit = v;
        }
        if let Some(v) = overrides.tester_present {
            ids.tester_present = v;
        }
        if let Some(v) = overrides.link_control {
            ids.link_control = v;
        }

        ids
    }
}

/// Periodic transmission rates for 0x2A
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeriodicRate {
    /// Send at slow rate (configurable, typically 1Hz)
    Slow = 0x01,
    /// Send at medium rate (configurable, typically 2-5Hz)
    Medium = 0x02,
    /// Send at fast rate (configurable, typically 10Hz+)
    Fast = 0x03,
    /// Stop sending periodic data
    Stop = 0x04,
}

impl From<u32> for PeriodicRate {
    fn from(hz: u32) -> Self {
        match hz {
            0 => PeriodicRate::Stop,
            1 => PeriodicRate::Slow,
            2..=5 => PeriodicRate::Medium,
            _ => PeriodicRate::Fast,
        }
    }
}
