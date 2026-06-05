//! UDS backend errors

use sovd_core::BackendError;
use thiserror::Error;

use crate::uds::UdsError;

/// UDS-specific backend errors
#[derive(Debug, Error)]
pub enum UdsBackendError {
    /// Transport error (CAN bus issues)
    #[error("Transport error: {0}")]
    Transport(String),

    /// UDS protocol error (negative response)
    #[error("UDS error: service 0x{service:02X}, NRC 0x{nrc:02X} - {message}")]
    Protocol {
        service: u8,
        nrc: u8,
        message: String,
    },

    /// Configuration error
    #[error("Configuration error: {0}")]
    Config(String),

    /// Parameter not found
    #[error("Parameter not found: {0}")]
    ParameterNotFound(String),

    /// Timeout waiting for response
    #[error("Timeout waiting for ECU response")]
    Timeout,
}

impl From<UdsBackendError> for BackendError {
    fn from(err: UdsBackendError) -> Self {
        match err {
            UdsBackendError::Transport(msg) => BackendError::Transport(msg),
            UdsBackendError::Protocol {
                service,
                nrc,
                message,
            } => map_nrc_to_backend_error(service, nrc, &message),
            UdsBackendError::Config(msg) => BackendError::Internal(msg),
            UdsBackendError::ParameterNotFound(id) => BackendError::ParameterNotFound(id),
            UdsBackendError::Timeout => BackendError::Timeout,
        }
    }
}

/// Convert UdsError to BackendError, preserving NRC information
impl From<UdsError> for BackendError {
    fn from(err: UdsError) -> Self {
        match err {
            UdsError::NegativeResponse { service_id, nrc } => {
                let nrc_byte: u8 = nrc.into();
                map_nrc_to_backend_error(service_id, nrc_byte, &nrc.to_string())
            }
            UdsError::Timeout => BackendError::Timeout,
            UdsError::Transport(msg) => BackendError::Transport(msg),
            UdsError::InvalidResponse(msg) => {
                BackendError::Protocol(format!("Invalid response: {}", msg))
            }
            UdsError::SecurityAccessFailed(_) => BackendError::SecurityRequired(1),
            UdsError::SessionTransitionFailed(msg) => {
                BackendError::Protocol(format!("Session transition failed: {}", msg))
            }
        }
    }
}

/// Convert UdsError to BackendError (public helper for explicit conversions)
pub fn convert_uds_error(err: UdsError) -> BackendError {
    err.into()
}

/// Map a UDS Negative Response Code (NRC) to a [`BackendError`].
///
/// **Every** NRC surfaces as [`BackendError::EcuError`] carrying the NRC and
/// rejected service ID. The NRC→HTTP status is owned *solely* by
/// [`sovd_core::error::nrc_to_status`] (consumed by both
/// [`BackendError::status_code`] and the `sovd-api` `EcuErrorResponse`
/// `IntoResponse`), and the resulting `error-response` body carries
/// `service` + `nrc` + `http_code` (ISO 17978-3 §8.4 Table 18, C-131).
///
/// This function deliberately does **not** branch per NRC. An earlier version
/// short-circuited specific NRCs (0x11/0x12→NotSupported, 0x13/0x31→
/// InvalidRequest, 0x22/0x7E/0x7F→SessionRequired, 0x33→SecurityRequired,
/// 0x36/0x37→RateLimited) *before* `nrc_to_status` ran. On the live
/// `UdsBackend` write path that bypassed the single-source table entirely and
/// dropped the Table-18 body — e.g. 0x33 surfaced as 401 (not 403) with no
/// `service`/`nrc`, and 0x36/0x37 as 429 (not an RFC-9110 §15 status). Routing
/// all NRCs through `EcuError` removes that divergent path.
fn map_nrc_to_backend_error(service: u8, nrc: u8, message: &str) -> BackendError {
    BackendError::EcuError {
        nrc,
        sid: service,
        message: format!("Negative response: {} (NRC 0x{:02X})", message, nrc),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MockConfig;
    use crate::transport::mock::MockTransportAdapter;
    use crate::uds::{NegativeResponseCode, UdsService};
    use sovd_core::error::nrc_to_status;
    use std::sync::Arc;

    /// Every NRC — including the ones the old per-NRC `match` short-circuited
    /// (0x11/0x13/0x22/0x33/0x36/0x37/0x7F) — must now surface as
    /// `BackendError::EcuError`, never as NotSupported / InvalidRequest /
    /// SessionRequired / SecurityRequired / RateLimited. That is what lets the
    /// single-source `nrc_to_status` own the status and the Table-18 body carry
    /// `service` + `nrc`.
    #[test]
    fn every_nrc_surfaces_as_ecu_error() {
        // Previously intercepted NRCs + a representative fallthrough (0x10).
        for nrc in [
            0x11u8, 0x12, 0x13, 0x31, 0x22, 0x33, 0x36, 0x37, 0x7E, 0x7F, 0x10,
        ] {
            let err = map_nrc_to_backend_error(0x2E, nrc, "rejected");
            match err {
                BackendError::EcuError {
                    nrc: got_nrc, sid, ..
                } => {
                    assert_eq!(got_nrc, nrc, "EcuError must carry the NRC");
                    assert_eq!(sid, 0x2E, "EcuError must carry the rejected service ID");
                }
                other => panic!(
                    "NRC 0x{nrc:02X} must map to EcuError, got {other:?} \
                     (the per-NRC short-circuit must be gone)"
                ),
            }
        }
    }

    /// `0x33` securityAccessDenied used to become `SecurityRequired(1)` (→401);
    /// it must now be `EcuError { nrc: 0x33 }` so the api layer maps it to 403
    /// via `nrc_to_status` and emits the `error-response` body.
    #[test]
    fn security_access_denied_is_ecu_error_not_security_required() {
        let uds_err = UdsError::NegativeResponse {
            service_id: 0x2E,
            nrc: NegativeResponseCode::SecurityAccessDenied,
        };

        match convert_uds_error(uds_err) {
            BackendError::EcuError { nrc, sid, .. } => {
                assert_eq!(nrc, 0x33);
                assert_eq!(sid, 0x2E);
                // The status the api layer will use comes from the single source.
                assert_eq!(nrc_to_status(nrc), 403);
            }
            other => panic!("Expected EcuError, got {other:?}"),
        }
    }

    /// Integration test through the **real** `UdsBackend` write call chain:
    /// `UdsService::write_data_by_id` (the exact service the backend's
    /// `write_raw_did` invokes at backend.rs) over the Mock transport, with the
    /// ECU answering a `0x2E` WriteDataByIdentifier with `7F 2E <nrc>`. The
    /// resulting `BackendError` must be an `EcuError` whose `status_code()`
    /// (i.e. `nrc_to_status`) matches the table — for 0x33→403 and 0x13→400,
    /// the two NRCs the old short-circuit would have mis-mapped (401 / 400-but-
    /// no-body). This is the test the mock-`EcuError` unit test could not catch.
    async fn write_real_path_nrc(nrc: u8) -> BackendError {
        let transport = Arc::new(MockTransportAdapter::new(&MockConfig::default()));
        // ECU rejects the 0x2E write with a UDS negative response.
        transport.add_response(vec![0x2E], vec![0x7F, 0x2E, nrc]);

        // This is byte-for-byte the chain `UdsBackend::write_raw_did` runs:
        //   self.uds.write_data_by_id(did, data).map_err(convert_uds_error)
        let uds = UdsService::new(transport);
        uds.write_data_by_id(0xF40C, &[0x0F, 0xA0])
            .await
            .map_err(convert_uds_error)
            .expect_err("ECU rejected the write; expected an error")
    }

    #[tokio::test]
    async fn real_write_path_security_denied_maps_to_403() {
        let err = write_real_path_nrc(0x33).await;
        match &err {
            BackendError::EcuError { nrc, sid, .. } => {
                assert_eq!(*nrc, 0x33);
                assert_eq!(*sid, 0x2E);
            }
            other => panic!("Expected EcuError on the real path, got {other:?}"),
        }
        // status_code() routes through nrc_to_status — the single source.
        assert_eq!(
            err.status_code(),
            403,
            "0x33 → 403 on the real UdsBackend path"
        );
    }

    #[tokio::test]
    async fn real_write_path_incorrect_length_maps_to_400() {
        let err = write_real_path_nrc(0x13).await;
        match &err {
            BackendError::EcuError { nrc, .. } => assert_eq!(*nrc, 0x13),
            other => panic!("Expected EcuError on the real path, got {other:?}"),
        }
        assert_eq!(
            err.status_code(),
            400,
            "0x13 → 400 on the real UdsBackend path"
        );
    }
}
