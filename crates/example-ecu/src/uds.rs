//! UDS Protocol handling for the test ECU

/// UDS Service IDs
#[allow(dead_code)]
pub mod service_id {
    pub const DIAGNOSTIC_SESSION_CONTROL: u8 = 0x10;
    pub const ECU_RESET: u8 = 0x11;
    pub const CLEAR_DIAGNOSTIC_INFO: u8 = 0x14;
    pub const READ_DTC_INFO: u8 = 0x19;
    pub const READ_DATA_BY_ID: u8 = 0x22;
    pub const SECURITY_ACCESS: u8 = 0x27;
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
    pub const RESPONSE_ON_EVENT: u8 = 0x86;
    pub const LINK_CONTROL: u8 = 0x87;
    pub const NEGATIVE_RESPONSE: u8 = 0x7F;
}

/// RoutineControl sub-functions
pub mod routine_sub_function {
    pub const START_ROUTINE: u8 = 0x01;
    pub const STOP_ROUTINE: u8 = 0x02;
    pub const REQUEST_ROUTINE_RESULTS: u8 = 0x03;
}

/// DynamicallyDefineDataIdentifier sub-functions
#[allow(dead_code)]
pub mod ddid_sub_function {
    pub const DEFINE_BY_IDENTIFIER: u8 = 0x01;
    pub const DEFINE_BY_MEMORY_ADDRESS: u8 = 0x02;
    pub const CLEAR_DYNAMICALLY_DEFINED_DATA_IDENTIFIER: u8 = 0x03;
}

/// ReadDTCInformation sub-functions
pub mod dtc_sub_function {
    pub const REPORT_NUMBER_OF_DTC_BY_STATUS_MASK: u8 = 0x01;
    pub const REPORT_DTC_BY_STATUS_MASK: u8 = 0x02;
    pub const REPORT_DTC_SNAPSHOT_RECORD_BY_DTC_NUMBER: u8 = 0x04;
    pub const REPORT_DTC_EXTENDED_DATA_RECORD_BY_DTC_NUMBER: u8 = 0x06;
}

/// InputOutputControlById sub-functions
pub mod io_control_option {
    pub const RETURN_CONTROL_TO_ECU: u8 = 0x00;
    pub const RESET_TO_DEFAULT: u8 = 0x01;
    pub const FREEZE_CURRENT_STATE: u8 = 0x02;
    pub const SHORT_TERM_ADJUSTMENT: u8 = 0x03;
}

/// LinkControl sub-functions
pub mod link_control_sub_function {
    pub const VERIFY_FIXED_BAUD_RATE: u8 = 0x01;
    pub const VERIFY_SPECIFIC_BAUD_RATE: u8 = 0x02;
    pub const TRANSITION_BAUD_RATE: u8 = 0x03;
}

/// LinkControl baud rate identifiers
pub mod link_baud_rate {
    pub const CAN_125K: u8 = 0x10;
    pub const CAN_250K: u8 = 0x11;
    pub const CAN_500K: u8 = 0x12;
    pub const CAN_1M: u8 = 0x13;
}

/// UDS Negative Response Codes
#[allow(dead_code)]
pub mod nrc {
    pub const GENERAL_REJECT: u8 = 0x10;
    pub const SERVICE_NOT_SUPPORTED: u8 = 0x11;
    pub const SUB_FUNCTION_NOT_SUPPORTED: u8 = 0x12;
    pub const INCORRECT_MESSAGE_LENGTH: u8 = 0x13;
    pub const CONDITIONS_NOT_CORRECT: u8 = 0x22;
    pub const REQUEST_SEQUENCE_ERROR: u8 = 0x24;
    pub const REQUEST_OUT_OF_RANGE: u8 = 0x31;
    pub const SECURITY_ACCESS_DENIED: u8 = 0x33;
    pub const INVALID_KEY: u8 = 0x35;
    pub const UPLOAD_DOWNLOAD_NOT_ACCEPTED: u8 = 0x70;
    pub const GENERAL_PROGRAMMING_FAILURE: u8 = 0x72;
    pub const WRONG_BLOCK_SEQUENCE_COUNTER: u8 = 0x73;
}

/// Create a positive response for a service
pub fn positive_response(service_id: u8, data: &[u8]) -> Vec<u8> {
    let mut response = Vec::with_capacity(1 + data.len());
    response.push(service_id + 0x40); // Positive response = service + 0x40
    response.extend_from_slice(data);
    response
}

/// Create a negative response
pub fn negative_response(service_id: u8, nrc: u8) -> Vec<u8> {
    vec![service_id::NEGATIVE_RESPONSE, service_id, nrc]
}
