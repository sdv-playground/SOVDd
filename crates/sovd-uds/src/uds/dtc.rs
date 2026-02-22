//! DTC (Diagnostic Trouble Code) handling for UDS service 0x19 (ReadDTCInformation)
//!
//! This module provides types and utilities for working with DTCs according to ISO 14229-1.

use serde::Serialize;

/// Sub-function codes for ReadDTCInformation (0x19)
pub mod sub_function {
    /// Report number of DTCs matching a status mask
    pub const REPORT_NUMBER_OF_DTC_BY_STATUS_MASK: u8 = 0x01;
    /// Report DTCs matching a status mask
    pub const REPORT_DTC_BY_STATUS_MASK: u8 = 0x02;
    /// Report DTC snapshot identification (list of available snapshots)
    pub const REPORT_DTC_SNAPSHOT_IDENTIFICATION: u8 = 0x03;
    /// Report DTC snapshot record by DTC number
    pub const REPORT_DTC_SNAPSHOT_RECORD_BY_DTC_NUMBER: u8 = 0x04;
    /// Report DTC stored data by record number
    pub const REPORT_DTC_STORED_DATA_BY_RECORD_NUMBER: u8 = 0x05;
    /// Report DTC extended data record by DTC number
    pub const REPORT_DTC_EXTENDED_DATA_RECORD_BY_DTC_NUMBER: u8 = 0x06;
    /// Report supported DTCs
    pub const REPORT_SUPPORTED_DTC: u8 = 0x0A;
}

/// DTC group addresses for ClearDiagnosticInformation (0x14)
pub mod dtc_group {
    /// All DTC groups (clear all)
    pub const ALL: u32 = 0xFFFFFF;
    /// Powertrain group (P codes)
    pub const POWERTRAIN: u32 = 0x000000;
    /// Chassis group (C codes)
    pub const CHASSIS: u32 = 0x400000;
    /// Body group (B codes)
    pub const BODY: u32 = 0x800000;
    /// Network group (U codes)
    pub const NETWORK: u32 = 0xC00000;
}

/// DTC status byte bit definitions per ISO 14229-1
pub mod status_bit {
    /// Bit 0: Test Failed - DTC test failed this operation cycle
    pub const TEST_FAILED: u8 = 0x01;
    /// Bit 1: Test Failed This Operation Cycle
    pub const TEST_FAILED_THIS_OPERATION_CYCLE: u8 = 0x02;
    /// Bit 2: Pending DTC - Test failed but not yet confirmed
    pub const PENDING_DTC: u8 = 0x04;
    /// Bit 3: Confirmed DTC - Malfunction confirmed and stored
    pub const CONFIRMED_DTC: u8 = 0x08;
    /// Bit 4: Test Not Completed Since Last Clear
    pub const TEST_NOT_COMPLETED_SINCE_LAST_CLEAR: u8 = 0x10;
    /// Bit 5: Test Failed Since Last Clear
    pub const TEST_FAILED_SINCE_LAST_CLEAR: u8 = 0x20;
    /// Bit 6: Test Not Completed This Operation Cycle
    pub const TEST_NOT_COMPLETED_THIS_OPERATION_CYCLE: u8 = 0x40;
    /// Bit 7: Warning Indicator Requested
    pub const WARNING_INDICATOR_REQUESTED: u8 = 0x80;

    /// Common mask for active faults (test failed + confirmed)
    pub const ACTIVE_MASK: u8 = TEST_FAILED | CONFIRMED_DTC;
    /// Common mask for all confirmed faults
    pub const CONFIRMED_MASK: u8 = CONFIRMED_DTC;
    /// Common mask for pending faults
    pub const PENDING_MASK: u8 = PENDING_DTC;
}

/// DTC category based on the first character of the DTC code
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DtcCategory {
    /// P codes - Powertrain (engine, transmission)
    Powertrain,
    /// C codes - Chassis (ABS, suspension)
    Chassis,
    /// B codes - Body (airbags, climate control)
    Body,
    /// U codes - Network (communication)
    Network,
}

impl DtcCategory {
    /// Get category from DTC high byte
    pub fn from_dtc_high_byte(high_byte: u8) -> Self {
        match (high_byte >> 6) & 0x03 {
            0 => DtcCategory::Powertrain,
            1 => DtcCategory::Chassis,
            2 => DtcCategory::Body,
            _ => DtcCategory::Network,
        }
    }

    /// Get category prefix character
    pub fn prefix(&self) -> char {
        match self {
            DtcCategory::Powertrain => 'P',
            DtcCategory::Chassis => 'C',
            DtcCategory::Body => 'B',
            DtcCategory::Network => 'U',
        }
    }
}

impl std::fmt::Display for DtcCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            DtcCategory::Powertrain => "powertrain",
            DtcCategory::Chassis => "chassis",
            DtcCategory::Body => "body",
            DtcCategory::Network => "network",
        };
        f.write_str(s)
    }
}

/// Parsed DTC status byte
#[derive(Debug, Clone, Serialize)]
pub struct DtcStatus {
    /// Bit 0: Test failed at time of request
    pub test_failed: bool,
    /// Bit 1: Test failed during current operation cycle
    pub test_failed_this_operation_cycle: bool,
    /// Bit 2: DTC is pending (failed but not yet confirmed)
    pub pending_dtc: bool,
    /// Bit 3: DTC is confirmed (malfunction confirmed and stored)
    pub confirmed_dtc: bool,
    /// Bit 4: Test not completed since last clear
    pub test_not_completed_since_last_clear: bool,
    /// Bit 5: Test failed since last clear
    pub test_failed_since_last_clear: bool,
    /// Bit 6: Test not completed this operation cycle
    pub test_not_completed_this_operation_cycle: bool,
    /// Bit 7: Warning indicator (MIL) requested
    pub warning_indicator_requested: bool,
    /// Raw status byte value
    pub raw: u8,
}

impl DtcStatus {
    /// Parse a status byte into structured status
    pub fn from_byte(status: u8) -> Self {
        Self {
            test_failed: (status & status_bit::TEST_FAILED) != 0,
            test_failed_this_operation_cycle: (status
                & status_bit::TEST_FAILED_THIS_OPERATION_CYCLE)
                != 0,
            pending_dtc: (status & status_bit::PENDING_DTC) != 0,
            confirmed_dtc: (status & status_bit::CONFIRMED_DTC) != 0,
            test_not_completed_since_last_clear: (status
                & status_bit::TEST_NOT_COMPLETED_SINCE_LAST_CLEAR)
                != 0,
            test_failed_since_last_clear: (status & status_bit::TEST_FAILED_SINCE_LAST_CLEAR) != 0,
            test_not_completed_this_operation_cycle: (status
                & status_bit::TEST_NOT_COMPLETED_THIS_OPERATION_CYCLE)
                != 0,
            warning_indicator_requested: (status & status_bit::WARNING_INDICATOR_REQUESTED) != 0,
            raw: status,
        }
    }

    /// Check if this DTC is currently active (test failed + confirmed)
    pub fn is_active(&self) -> bool {
        self.test_failed && self.confirmed_dtc
    }

    /// Check if the raw status matches a given mask
    pub fn matches_mask(&self, mask: u8) -> bool {
        (self.raw & mask) != 0
    }
}

/// A parsed DTC with its status
#[derive(Debug, Clone)]
pub struct Dtc {
    /// 3-byte DTC number (high, mid, low)
    pub dtc_number: [u8; 3],
    /// DTC status byte
    pub status: DtcStatus,
}

impl Dtc {
    /// Create from raw bytes
    pub fn new(dtc_high: u8, dtc_mid: u8, dtc_low: u8, status: u8) -> Self {
        Self {
            dtc_number: [dtc_high, dtc_mid, dtc_low],
            status: DtcStatus::from_byte(status),
        }
    }

    /// Get the DTC category
    pub fn category(&self) -> DtcCategory {
        DtcCategory::from_dtc_high_byte(self.dtc_number[0])
    }

    /// Convert to standard DTC string format (e.g., P0101, C0420, B1234, U0100)
    pub fn to_code_string(&self) -> String {
        let prefix = self.category().prefix();

        // Extract the numeric portion
        // High byte bits 5-4 = second digit (0-3)
        // High byte bits 3-0 = third digit (0-F)
        // Mid byte = fourth and fifth digits
        let second_digit = (self.dtc_number[0] >> 4) & 0x03;
        let third_digit = self.dtc_number[0] & 0x0F;
        let fourth_digit = (self.dtc_number[1] >> 4) & 0x0F;
        let fifth_digit = self.dtc_number[1] & 0x0F;

        format!(
            "{}{:01X}{:01X}{:01X}{:01X}",
            prefix, second_digit, third_digit, fourth_digit, fifth_digit
        )
    }

    /// Convert to unique ID for API (hex representation of 3-byte DTC)
    pub fn to_id(&self) -> String {
        format!(
            "{:02X}{:02X}{:02X}",
            self.dtc_number[0], self.dtc_number[1], self.dtc_number[2]
        )
    }

    /// Parse DTC ID back to bytes
    pub fn parse_id(id: &str) -> Option<[u8; 3]> {
        if id.len() != 6 {
            return None;
        }
        let bytes = hex::decode(id).ok()?;
        if bytes.len() != 3 {
            return None;
        }
        Some([bytes[0], bytes[1], bytes[2]])
    }

    /// Get the 24-bit DTC number as u32
    pub fn dtc_number_u32(&self) -> u32 {
        ((self.dtc_number[0] as u32) << 16)
            | ((self.dtc_number[1] as u32) << 8)
            | (self.dtc_number[2] as u32)
    }
}

/// DTC snapshot record
#[derive(Debug, Clone, Serialize)]
pub struct DtcSnapshotRecord {
    /// Record number
    pub record_number: u8,
    /// Number of identifiers in this record
    pub number_of_identifiers: u8,
    /// DID-value pairs in the snapshot
    pub data: Vec<SnapshotDataItem>,
}

/// A single data item in a snapshot
#[derive(Debug, Clone, Serialize)]
pub struct SnapshotDataItem {
    /// Data identifier (DID)
    pub did: u16,
    /// Raw data bytes
    pub data: Vec<u8>,
}

/// DTC extended data record
#[derive(Debug, Clone, Serialize)]
pub struct DtcExtendedDataRecord {
    /// Record number
    pub record_number: u8,
    /// Raw extended data bytes
    pub data: Vec<u8>,
}

/// Result of reading DTC count
#[derive(Debug, Clone)]
pub struct DtcCountResult {
    /// Status availability mask (which status bits the ECU supports)
    pub status_availability_mask: u8,
    /// DTC format identifier (typically 0x01 for ISO 14229-1)
    pub dtc_format_identifier: u8,
    /// Number of DTCs matching the mask
    pub dtc_count: u16,
}

/// Parse response from sub-function 0x01 (reportNumberOfDTCByStatusMask)
pub fn parse_dtc_count_response(response: &[u8]) -> Result<DtcCountResult, String> {
    // Response: 0x59 0x01 [statusAvailabilityMask] [DTCFormatIdentifier] [DTCCount_HI] [DTCCount_LO]
    if response.len() < 6 {
        return Err(format!("Response too short: {} bytes", response.len()));
    }

    if response[0] != 0x59 {
        return Err(format!("Invalid response SID: 0x{:02X}", response[0]));
    }

    if response[1] != sub_function::REPORT_NUMBER_OF_DTC_BY_STATUS_MASK {
        return Err(format!("Invalid sub-function: 0x{:02X}", response[1]));
    }

    Ok(DtcCountResult {
        status_availability_mask: response[2],
        dtc_format_identifier: response[3],
        dtc_count: u16::from_be_bytes([response[4], response[5]]),
    })
}

/// Parse response from sub-function 0x02 (reportDTCByStatusMask)
pub fn parse_dtc_by_status_mask_response(response: &[u8]) -> Result<(u8, Vec<Dtc>), String> {
    // Response: 0x59 0x02 [statusAvailabilityMask] {[DTCHighByte] [DTCMiddleByte] [DTCLowByte] [statusOfDTC]}*
    if response.len() < 3 {
        return Err(format!("Response too short: {} bytes", response.len()));
    }

    if response[0] != 0x59 {
        return Err(format!("Invalid response SID: 0x{:02X}", response[0]));
    }

    if response[1] != sub_function::REPORT_DTC_BY_STATUS_MASK {
        return Err(format!("Invalid sub-function: 0x{:02X}", response[1]));
    }

    let status_availability_mask = response[2];
    let mut dtcs = Vec::new();

    // Each DTC record is 4 bytes: 3 bytes DTC + 1 byte status
    let dtc_data = &response[3..];
    for chunk in dtc_data.chunks(4) {
        if chunk.len() == 4 {
            dtcs.push(Dtc::new(chunk[0], chunk[1], chunk[2], chunk[3]));
        }
    }

    Ok((status_availability_mask, dtcs))
}

/// Parse response from sub-function 0x04 (reportDTCSnapshotRecordByDTCNumber)
pub fn parse_dtc_snapshot_response(
    response: &[u8],
) -> Result<(Dtc, Vec<DtcSnapshotRecord>), String> {
    // Response: 0x59 0x04 [DTCHigh] [DTCMid] [DTCLow] [statusOfDTC] {[SnapshotRecordNumber] [NumberOfIdentifiers] {[DID_HI] [DID_LO] [data...]}*}*
    if response.len() < 7 {
        return Err(format!("Response too short: {} bytes", response.len()));
    }

    if response[0] != 0x59 {
        return Err(format!("Invalid response SID: 0x{:02X}", response[0]));
    }

    if response[1] != sub_function::REPORT_DTC_SNAPSHOT_RECORD_BY_DTC_NUMBER {
        return Err(format!("Invalid sub-function: 0x{:02X}", response[1]));
    }

    let dtc = Dtc::new(response[2], response[3], response[4], response[5]);

    // Parse snapshot records - this is complex as it depends on DID lengths
    // For simplicity, we return the raw remaining data as a single record
    let mut records = Vec::new();

    if response.len() > 6 {
        let remaining = &response[6..];
        if !remaining.is_empty() {
            let record_number = remaining[0];
            let data = remaining[1..].to_vec();

            records.push(DtcSnapshotRecord {
                record_number,
                number_of_identifiers: 0, // Unknown without DID length info
                data: vec![SnapshotDataItem { did: 0, data }],
            });
        }
    }

    Ok((dtc, records))
}

/// Parse response from sub-function 0x06 (reportDTCExtendedDataRecordByDTCNumber)
pub fn parse_dtc_extended_data_response(
    response: &[u8],
) -> Result<(Dtc, Vec<DtcExtendedDataRecord>), String> {
    // Response: 0x59 0x06 [DTCHigh] [DTCMid] [DTCLow] [statusOfDTC] {[ExtendedDataRecordNumber] [data...]}*
    if response.len() < 7 {
        return Err(format!("Response too short: {} bytes", response.len()));
    }

    if response[0] != 0x59 {
        return Err(format!("Invalid response SID: 0x{:02X}", response[0]));
    }

    if response[1] != sub_function::REPORT_DTC_EXTENDED_DATA_RECORD_BY_DTC_NUMBER {
        return Err(format!("Invalid sub-function: 0x{:02X}", response[1]));
    }

    let dtc = Dtc::new(response[2], response[3], response[4], response[5]);

    let mut records = Vec::new();

    if response.len() > 6 {
        let remaining = &response[6..];
        if !remaining.is_empty() {
            let record_number = remaining[0];
            let data = remaining[1..].to_vec();

            records.push(DtcExtendedDataRecord {
                record_number,
                data,
            });
        }
    }

    Ok((dtc, records))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dtc_code_string_powertrain() {
        // P0101 = 0x01 0x01 0x00
        let dtc = Dtc::new(0x01, 0x01, 0x00, 0x00);
        assert_eq!(dtc.to_code_string(), "P0101");
        assert_eq!(dtc.category(), DtcCategory::Powertrain);
    }

    #[test]
    fn test_dtc_code_string_chassis() {
        // C0420 = 0x44 0x20 0x00
        let dtc = Dtc::new(0x44, 0x20, 0x00, 0x00);
        assert_eq!(dtc.to_code_string(), "C0420");
        assert_eq!(dtc.category(), DtcCategory::Chassis);
    }

    #[test]
    fn test_dtc_code_string_body() {
        // B1234 = 0x92 0x34 0x00
        let dtc = Dtc::new(0x92, 0x34, 0x00, 0x00);
        assert_eq!(dtc.to_code_string(), "B1234");
        assert_eq!(dtc.category(), DtcCategory::Body);
    }

    #[test]
    fn test_dtc_code_string_network() {
        // U0100 = 0xC1 0x00 0x00
        let dtc = Dtc::new(0xC1, 0x00, 0x00, 0x00);
        assert_eq!(dtc.to_code_string(), "U0100");
        assert_eq!(dtc.category(), DtcCategory::Network);
    }

    #[test]
    fn test_dtc_status_parsing() {
        // Active fault: test_failed + confirmed_dtc = 0x09
        let status = DtcStatus::from_byte(0x09);
        assert!(status.test_failed);
        assert!(status.confirmed_dtc);
        assert!(!status.pending_dtc);
        assert!(status.is_active());
    }

    #[test]
    fn test_dtc_status_pending() {
        // Pending fault: pending_dtc = 0x04
        let status = DtcStatus::from_byte(0x04);
        assert!(!status.test_failed);
        assert!(!status.confirmed_dtc);
        assert!(status.pending_dtc);
        assert!(!status.is_active());
    }

    #[test]
    fn test_dtc_id_conversion() {
        let dtc = Dtc::new(0x01, 0x01, 0x00, 0x09);
        assert_eq!(dtc.to_id(), "010100");

        let parsed = Dtc::parse_id("010100");
        assert_eq!(parsed, Some([0x01, 0x01, 0x00]));
    }

    #[test]
    fn test_parse_dtc_count_response() {
        // Valid response: 0x59 0x01 [mask] [format] [count_hi] [count_lo]
        let response = vec![0x59, 0x01, 0xFF, 0x01, 0x00, 0x05];
        let result = parse_dtc_count_response(&response).unwrap();
        assert_eq!(result.status_availability_mask, 0xFF);
        assert_eq!(result.dtc_format_identifier, 0x01);
        assert_eq!(result.dtc_count, 5);
    }

    #[test]
    fn test_parse_dtc_by_status_mask_response() {
        // Response with 2 DTCs
        let response = vec![
            0x59, 0x02, 0xFF, // Header + status availability mask
            0x01, 0x01, 0x00, 0x09, // P0101 with active status
            0x44, 0x20, 0x00, 0x04, // C0420 with pending status
        ];
        let (mask, dtcs) = parse_dtc_by_status_mask_response(&response).unwrap();
        assert_eq!(mask, 0xFF);
        assert_eq!(dtcs.len(), 2);
        assert_eq!(dtcs[0].to_code_string(), "P0101");
        assert!(dtcs[0].status.is_active());
        assert_eq!(dtcs[1].to_code_string(), "C0420");
        assert!(dtcs[1].status.pending_dtc);
    }
}
