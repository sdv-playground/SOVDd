//! UDS service layer for diagnostic communication

use std::sync::Arc;
use std::time::Duration;

use super::{service_id, NegativeResponseCode, PeriodicRate, ServiceIds, UdsError};
use crate::transport::TransportAdapter;

const DEFAULT_TIMEOUT: Duration = Duration::from_millis(5000);
const RESPONSE_PENDING_TIMEOUT: Duration = Duration::from_millis(30000);

/// UDS Service layer for diagnostic communication
#[derive(Clone)]
pub struct UdsService {
    transport: Arc<dyn TransportAdapter>,
    timeout: Duration,
    /// Service IDs to use (may include OEM overrides)
    svc: ServiceIds,
}

impl UdsService {
    pub fn new(transport: Arc<dyn TransportAdapter>) -> Self {
        Self {
            transport,
            timeout: DEFAULT_TIMEOUT,
            svc: ServiceIds::default(),
        }
    }

    /// Create a UDS service with custom service IDs (for OEM-specific implementations)
    pub fn with_service_ids(transport: Arc<dyn TransportAdapter>, service_ids: ServiceIds) -> Self {
        Self {
            transport,
            timeout: DEFAULT_TIMEOUT,
            svc: service_ids,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Get the service IDs being used
    pub fn service_ids(&self) -> &ServiceIds {
        &self.svc
    }

    /// Send a UDS request and handle response pending
    async fn send_request(&self, request: &[u8]) -> Result<Vec<u8>, UdsError> {
        let start = std::time::Instant::now();

        loop {
            let response = self
                .transport
                .send_receive(request, self.timeout)
                .await
                .map_err(|e| UdsError::Transport(e.to_string()))?;

            // Check for negative response
            if response.first() == Some(&service_id::NEGATIVE_RESPONSE) {
                if response.len() < 3 {
                    return Err(UdsError::InvalidResponse(
                        "Negative response too short".to_string(),
                    ));
                }

                let service_id = response[1];
                let nrc = NegativeResponseCode::from(response[2]);

                // Handle response pending
                if nrc == NegativeResponseCode::ResponsePending {
                    if start.elapsed() > RESPONSE_PENDING_TIMEOUT {
                        return Err(UdsError::Timeout);
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }

                return Err(UdsError::NegativeResponse { service_id, nrc });
            }

            return Ok(response);
        }
    }

    /// Diagnostic Session Control (0x10)
    pub async fn diagnostic_session_control(&self, session: u8) -> Result<Vec<u8>, UdsError> {
        let request = vec![self.svc.diagnostic_session_control, session];
        self.send_request(&request).await
    }

    /// Tester Present (0x3E)
    pub async fn tester_present(&self, suppress_response: bool) -> Result<(), UdsError> {
        let sub_function = if suppress_response { 0x80 } else { 0x00 };
        let request = vec![self.svc.tester_present, sub_function];

        if suppress_response {
            self.transport
                .send(&request)
                .await
                .map_err(|e| UdsError::Transport(e.to_string()))?;
            Ok(())
        } else {
            self.send_request(&request).await?;
            Ok(())
        }
    }

    /// Security Access - Request Seed (0x27 odd)
    pub async fn security_access_request_seed(&self, level: u8) -> Result<Vec<u8>, UdsError> {
        // Security level for seed request is odd (0x01, 0x03, 0x05, etc.)
        let sub_function = (level * 2) - 1;
        let request = vec![self.svc.security_access, sub_function];
        let response = self.send_request(&request).await?;

        // Response: 0x67 [sub_function] [seed...]
        if response.len() < 2 {
            return Err(UdsError::InvalidResponse(
                "Seed response too short".to_string(),
            ));
        }

        Ok(response[2..].to_vec())
    }

    /// Security Access - Send Key (0x27 even)
    pub async fn security_access_send_key(&self, level: u8, key: &[u8]) -> Result<(), UdsError> {
        // Security level for key is even (0x02, 0x04, 0x06, etc.)
        let sub_function = level * 2;
        let mut request = vec![self.svc.security_access, sub_function];
        request.extend_from_slice(key);

        self.send_request(&request).await?;
        Ok(())
    }

    /// Read Data By Identifier (0x22)
    pub async fn read_data_by_id(&self, dids: &[u16]) -> Result<Vec<u8>, UdsError> {
        let mut request = vec![self.svc.read_data_by_id];
        for did in dids {
            request.extend_from_slice(&did.to_be_bytes());
        }

        self.send_request(&request).await
    }

    /// Write Data By Identifier (0x2E)
    pub async fn write_data_by_id(&self, did: u16, data: &[u8]) -> Result<(), UdsError> {
        let mut request = vec![self.svc.write_data_by_id];
        request.extend_from_slice(&did.to_be_bytes());
        request.extend_from_slice(data);

        self.send_request(&request).await?;
        Ok(())
    }

    /// Read Data By Periodic Identifier (0x2A) - Start periodic transmission
    pub async fn start_periodic(&self, rate: PeriodicRate, pids: &[u8]) -> Result<(), UdsError> {
        let mut request = vec![self.svc.read_data_by_periodic_id, rate as u8];
        request.extend_from_slice(pids);

        self.send_request(&request).await?;
        Ok(())
    }

    /// Read Data By Periodic Identifier (0x2A) - Stop periodic transmission
    pub async fn stop_periodic(&self, pids: &[u8]) -> Result<(), UdsError> {
        let mut request = vec![self.svc.read_data_by_periodic_id, PeriodicRate::Stop as u8];
        request.extend_from_slice(pids);

        self.send_request(&request).await?;
        Ok(())
    }

    /// Response On Event (0x86) - Setup event-triggered data
    pub async fn response_on_event_setup(
        &self,
        event_type: u8,
        event_window: u8,
        service_to_respond: &[u8],
    ) -> Result<(), UdsError> {
        let mut request = vec![service_id::RESPONSE_ON_EVENT, event_type, event_window];
        request.extend_from_slice(service_to_respond);

        self.send_request(&request).await?;
        Ok(())
    }

    /// Response On Event (0x86) - Stop
    pub async fn response_on_event_stop(&self) -> Result<(), UdsError> {
        let request = vec![service_id::RESPONSE_ON_EVENT, 0x00]; // stopResponseOnEvent
        self.send_request(&request).await?;
        Ok(())
    }

    /// Parse a ReadDataByIdentifier (0x62) response into DID-value pairs
    pub fn parse_read_response(response: &[u8]) -> Result<Vec<(u16, Vec<u8>)>, UdsError> {
        if response.is_empty() || response[0] != 0x62 {
            return Err(UdsError::InvalidResponse(
                "Not a ReadDataByIdentifier response".to_string(),
            ));
        }

        // For now, assume single DID response
        // Format: 0x62 [DID_HI] [DID_LO] [DATA...]
        if response.len() < 3 {
            return Err(UdsError::InvalidResponse("Response too short".to_string()));
        }

        let did = u16::from_be_bytes([response[1], response[2]]);
        let data = response[3..].to_vec();

        Ok(vec![(did, data)])
    }

    // =========================================================================
    // DTC Services (0x19 ReadDTCInformation, 0x14 ClearDiagnosticInformation)
    // =========================================================================

    /// Read DTC count matching a status mask (sub-function 0x01)
    pub async fn read_dtc_count(&self, status_mask: u8) -> Result<Vec<u8>, UdsError> {
        let request = vec![
            self.svc.read_dtc_info,
            super::dtc::sub_function::REPORT_NUMBER_OF_DTC_BY_STATUS_MASK,
            status_mask,
        ];
        self.send_request(&request).await
    }

    /// Read DTCs matching a status mask (sub-function 0x02)
    pub async fn read_dtc_by_status_mask(&self, status_mask: u8) -> Result<Vec<u8>, UdsError> {
        let request = vec![
            self.svc.read_dtc_info,
            super::dtc::sub_function::REPORT_DTC_BY_STATUS_MASK,
            status_mask,
        ];
        self.send_request(&request).await
    }

    /// Read DTC snapshot record by DTC number (sub-function 0x04)
    pub async fn read_dtc_snapshot(
        &self,
        dtc_high: u8,
        dtc_mid: u8,
        dtc_low: u8,
        record_number: u8,
    ) -> Result<Vec<u8>, UdsError> {
        let request = vec![
            self.svc.read_dtc_info,
            super::dtc::sub_function::REPORT_DTC_SNAPSHOT_RECORD_BY_DTC_NUMBER,
            dtc_high,
            dtc_mid,
            dtc_low,
            record_number,
        ];
        self.send_request(&request).await
    }

    /// Read DTC extended data record by DTC number (sub-function 0x06)
    pub async fn read_dtc_extended_data(
        &self,
        dtc_high: u8,
        dtc_mid: u8,
        dtc_low: u8,
        record_number: u8,
    ) -> Result<Vec<u8>, UdsError> {
        let request = vec![
            self.svc.read_dtc_info,
            super::dtc::sub_function::REPORT_DTC_EXTENDED_DATA_RECORD_BY_DTC_NUMBER,
            dtc_high,
            dtc_mid,
            dtc_low,
            record_number,
        ];
        self.send_request(&request).await
    }

    /// Clear Diagnostic Information (0x14)
    pub async fn clear_dtc(&self, group: u32) -> Result<(), UdsError> {
        // Group is 3 bytes
        let group_bytes = group.to_be_bytes();
        let request = vec![
            self.svc.clear_diagnostic_info,
            group_bytes[1], // High byte
            group_bytes[2], // Mid byte
            group_bytes[3], // Low byte
        ];
        self.send_request(&request).await?;
        Ok(())
    }

    // =========================================================================
    // Routine Control (0x31)
    // =========================================================================

    /// Start a routine (sub-function 0x01)
    pub async fn routine_control_start(
        &self,
        routine_id: u16,
        params: &[u8],
    ) -> Result<Vec<u8>, UdsError> {
        let mut request = vec![
            self.svc.routine_control,
            super::routine_sub_function::START_ROUTINE,
        ];
        request.extend_from_slice(&routine_id.to_be_bytes());
        request.extend_from_slice(params);

        let response = self.send_request(&request).await?;

        // Response: 0x71 [sub-function] [routineIdHi] [routineIdLo] [routineInfo...]
        if response.len() < 4 {
            return Err(UdsError::InvalidResponse(
                "Routine response too short".to_string(),
            ));
        }

        Ok(response[4..].to_vec())
    }

    /// Stop a routine (sub-function 0x02)
    pub async fn routine_control_stop(&self, routine_id: u16) -> Result<Vec<u8>, UdsError> {
        let mut request = vec![
            self.svc.routine_control,
            super::routine_sub_function::STOP_ROUTINE,
        ];
        request.extend_from_slice(&routine_id.to_be_bytes());

        let response = self.send_request(&request).await?;

        if response.len() < 4 {
            return Err(UdsError::InvalidResponse(
                "Routine response too short".to_string(),
            ));
        }

        Ok(response[4..].to_vec())
    }

    /// Request routine results (sub-function 0x03)
    pub async fn routine_control_result(&self, routine_id: u16) -> Result<Vec<u8>, UdsError> {
        let mut request = vec![
            self.svc.routine_control,
            super::routine_sub_function::REQUEST_ROUTINE_RESULTS,
        ];
        request.extend_from_slice(&routine_id.to_be_bytes());

        let response = self.send_request(&request).await?;

        if response.len() < 4 {
            return Err(UdsError::InvalidResponse(
                "Routine response too short".to_string(),
            ));
        }

        Ok(response[4..].to_vec())
    }

    // =========================================================================
    // DynamicallyDefineDataIdentifier (0x2C)
    // =========================================================================

    /// Define a DDID by composing it from source DIDs (sub-function 0x01)
    pub async fn define_data_identifier(
        &self,
        ddid: u16,
        source_definitions: &[(u16, u8, u8)],
    ) -> Result<(), UdsError> {
        let mut request = vec![
            self.svc.dynamically_define_data_id,
            super::ddid_sub_function::DEFINE_BY_IDENTIFIER,
        ];
        request.extend_from_slice(&ddid.to_be_bytes());

        for (source_did, position, size) in source_definitions {
            request.extend_from_slice(&source_did.to_be_bytes());
            request.push(*position);
            request.push(*size);
        }

        self.send_request(&request).await?;
        Ok(())
    }

    /// Clear a dynamically defined data identifier (sub-function 0x03)
    pub async fn clear_data_identifier(&self, ddid: u16) -> Result<(), UdsError> {
        let mut request = vec![
            self.svc.dynamically_define_data_id,
            super::ddid_sub_function::CLEAR_DYNAMICALLY_DEFINED_DATA_IDENTIFIER,
        ];
        request.extend_from_slice(&ddid.to_be_bytes());

        self.send_request(&request).await?;
        Ok(())
    }

    // =========================================================================
    // Programming Services (0x34, 0x35, 0x36, 0x37, 0x11)
    // =========================================================================

    /// Request Download (0x34) - Initiate download session
    pub async fn request_download(
        &self,
        data_format: u8,
        addr_len_format: u8,
        memory_address: &[u8],
        memory_size: &[u8],
    ) -> Result<u32, UdsError> {
        let mut request = vec![self.svc.request_download, data_format, addr_len_format];
        request.extend_from_slice(memory_address);
        request.extend_from_slice(memory_size);

        let response = self.send_request(&request).await?;

        if response.len() < 2 {
            return Err(UdsError::InvalidResponse(
                "RequestDownload response too short".to_string(),
            ));
        }

        let length_format = response[1];
        let num_bytes = (length_format >> 4) as usize;

        if response.len() < 2 + num_bytes {
            return Err(UdsError::InvalidResponse(
                "RequestDownload response missing maxBlockLength".to_string(),
            ));
        }

        let mut max_block_length: u32 = 0;
        for i in 0..num_bytes {
            max_block_length = (max_block_length << 8) | response[2 + i] as u32;
        }

        Ok(max_block_length.saturating_sub(2))
    }

    /// Transfer Data (0x36) - Transfer data block
    pub async fn transfer_data(&self, block_counter: u8, data: &[u8]) -> Result<u8, UdsError> {
        let mut request = vec![self.svc.transfer_data, block_counter];
        request.extend_from_slice(data);

        let response = self.send_request(&request).await?;

        if response.len() < 2 {
            return Err(UdsError::InvalidResponse(
                "TransferData response too short".to_string(),
            ));
        }

        Ok(response[1])
    }

    /// Request Transfer Exit (0x37) - Complete transfer session
    pub async fn request_transfer_exit(&self, params: &[u8]) -> Result<Vec<u8>, UdsError> {
        let mut request = vec![self.svc.request_transfer_exit];
        request.extend_from_slice(params);

        let response = self.send_request(&request).await?;

        if response.is_empty() {
            return Err(UdsError::InvalidResponse(
                "RequestTransferExit response empty".to_string(),
            ));
        }

        Ok(response[1..].to_vec())
    }

    /// ECU Reset (0x11) - Reset ECU
    pub async fn ecu_reset(&self, reset_type: u8) -> Result<Option<u8>, UdsError> {
        let request = vec![self.svc.ecu_reset, reset_type];

        let response = self.send_request(&request).await?;

        if response.len() < 2 {
            return Err(UdsError::InvalidResponse(
                "ECUReset response too short".to_string(),
            ));
        }

        let power_down_time = if response.len() > 2 {
            Some(response[2])
        } else {
            None
        };

        Ok(power_down_time)
    }

    /// Request Upload (0x35) - Initiate upload session
    pub async fn request_upload(
        &self,
        data_format: u8,
        addr_len_format: u8,
        memory_address: &[u8],
        memory_size: &[u8],
    ) -> Result<u32, UdsError> {
        let mut request = vec![self.svc.request_upload, data_format, addr_len_format];
        request.extend_from_slice(memory_address);
        request.extend_from_slice(memory_size);

        let response = self.send_request(&request).await?;

        if response.len() < 2 {
            return Err(UdsError::InvalidResponse(
                "RequestUpload response too short".to_string(),
            ));
        }

        let length_format = response[1];
        let num_bytes = (length_format >> 4) as usize;

        if response.len() < 2 + num_bytes {
            return Err(UdsError::InvalidResponse(
                "RequestUpload response missing maxBlockLength".to_string(),
            ));
        }

        let mut max_block_length: u32 = 0;
        for i in 0..num_bytes {
            max_block_length = (max_block_length << 8) | response[2 + i] as u32;
        }

        Ok(max_block_length.saturating_sub(2))
    }

    /// Transfer Data Upload (0x36) - Request data block from ECU
    pub async fn transfer_data_upload(&self, block_counter: u8) -> Result<(u8, Vec<u8>), UdsError> {
        let request = vec![self.svc.transfer_data, block_counter];

        let response = self.send_request(&request).await?;

        if response.len() < 2 {
            return Err(UdsError::InvalidResponse(
                "TransferData response too short".to_string(),
            ));
        }

        let counter_echo = response[1];
        let data = response[2..].to_vec();

        Ok((counter_echo, data))
    }

    // =========================================================================
    // InputOutputControlById (0x2F)
    // =========================================================================

    /// I/O Control - Return control to ECU (sub-function 0x00)
    pub async fn io_control_return_to_ecu(&self, output_id: u16) -> Result<Vec<u8>, UdsError> {
        let mut request = vec![self.svc.io_control_by_id];
        request.extend_from_slice(&output_id.to_be_bytes());
        request.push(super::io_control_option::RETURN_CONTROL_TO_ECU);

        let response = self.send_request(&request).await?;

        if response.len() < 4 {
            return Err(UdsError::InvalidResponse(
                "IOControlById response too short".to_string(),
            ));
        }

        Ok(response[4..].to_vec())
    }

    /// I/O Control - Reset to default (sub-function 0x01)
    pub async fn io_control_reset_to_default(&self, output_id: u16) -> Result<Vec<u8>, UdsError> {
        let mut request = vec![self.svc.io_control_by_id];
        request.extend_from_slice(&output_id.to_be_bytes());
        request.push(super::io_control_option::RESET_TO_DEFAULT);

        let response = self.send_request(&request).await?;

        if response.len() < 4 {
            return Err(UdsError::InvalidResponse(
                "IOControlById response too short".to_string(),
            ));
        }

        Ok(response[4..].to_vec())
    }

    /// I/O Control - Freeze current state (sub-function 0x02)
    pub async fn io_control_freeze(&self, output_id: u16) -> Result<Vec<u8>, UdsError> {
        let mut request = vec![self.svc.io_control_by_id];
        request.extend_from_slice(&output_id.to_be_bytes());
        request.push(super::io_control_option::FREEZE_CURRENT_STATE);

        let response = self.send_request(&request).await?;

        if response.len() < 4 {
            return Err(UdsError::InvalidResponse(
                "IOControlById response too short".to_string(),
            ));
        }

        Ok(response[4..].to_vec())
    }

    /// I/O Control - Short-term adjustment (sub-function 0x03)
    pub async fn io_control_short_term_adjustment(
        &self,
        output_id: u16,
        control_state: &[u8],
        control_mask: Option<&[u8]>,
    ) -> Result<Vec<u8>, UdsError> {
        let mut request = vec![self.svc.io_control_by_id];
        request.extend_from_slice(&output_id.to_be_bytes());
        request.push(super::io_control_option::SHORT_TERM_ADJUSTMENT);
        request.extend_from_slice(control_state);

        if let Some(mask) = control_mask {
            request.extend_from_slice(mask);
        }

        let response = self.send_request(&request).await?;

        if response.len() < 4 {
            return Err(UdsError::InvalidResponse(
                "IOControlById response too short".to_string(),
            ));
        }

        Ok(response[4..].to_vec())
    }

    // =========================================================================
    // LinkControl (0x87)
    // =========================================================================

    /// Link Control - Verify fixed baud rate (sub-function 0x01)
    pub async fn link_control_verify_fixed(&self, baud_rate_id: u8) -> Result<(), UdsError> {
        let request = vec![
            self.svc.link_control,
            super::link_control_sub_function::VERIFY_FIXED_BAUD_RATE,
            baud_rate_id,
        ];

        self.send_request(&request).await?;
        Ok(())
    }

    /// Link Control - Verify specific baud rate (sub-function 0x02)
    pub async fn link_control_verify_specific(&self, baud_rate: u32) -> Result<(), UdsError> {
        let baud_bytes = baud_rate.to_be_bytes();
        let request = vec![
            self.svc.link_control,
            super::link_control_sub_function::VERIFY_SPECIFIC_BAUD_RATE,
            baud_bytes[1],
            baud_bytes[2],
            baud_bytes[3],
        ];

        self.send_request(&request).await?;
        Ok(())
    }

    /// Link Control - Transition baud rate (sub-function 0x03)
    pub async fn link_control_transition(&self) -> Result<(), UdsError> {
        let request = vec![
            self.svc.link_control,
            super::link_control_sub_function::TRANSITION_BAUD_RATE,
        ];

        self.send_request(&request).await?;
        Ok(())
    }
}
