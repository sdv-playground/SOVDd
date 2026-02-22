//! ECU Reset handlers (UDS 0x11 ECUReset)

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::state::AppState;

/// Request for ECU reset
#[derive(Debug, Deserialize)]
pub struct EcuResetRequest {
    /// Reset type: "hard", "soft", "key_off_on", or hex value like "0x01"
    pub reset_type: String,
}

/// Response for ECU reset
#[derive(Debug, Serialize)]
pub struct EcuResetResponse {
    pub success: bool,
    pub reset_type: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub power_down_time: Option<u8>,
}

/// Parse reset type string to UDS reset type byte
fn parse_reset_type(s: &str) -> Result<(u8, &'static str), ApiError> {
    match s.to_lowercase().as_str() {
        "hard" | "hardreset" => Ok((0x01, "hard")),
        "key_off_on" | "keyoffonreset" => Ok((0x02, "key_off_on")),
        "soft" | "softreset" => Ok((0x03, "soft")),
        "0x01" | "0x1" | "1" => Ok((0x01, "hard")),
        "0x02" | "0x2" | "2" => Ok((0x02, "key_off_on")),
        "0x03" | "0x3" | "3" => Ok((0x03, "soft")),
        _ => {
            // Try parsing as hex
            let cleaned = s.trim_start_matches("0x").trim_start_matches("0X");
            u8::from_str_radix(cleaned, 16)
                .map(|v| {
                    (
                        v,
                        match v {
                            0x01 => "hard",
                            0x02 => "key_off_on",
                            0x03 => "soft",
                            _ => "custom",
                        },
                    )
                })
                .map_err(|_| {
                    ApiError::BadRequest(format!(
                        "Invalid reset type: {}. Use 'hard', 'soft', 'key_off_on', or hex value",
                        s
                    ))
                })
        }
    }
}

/// POST /vehicle/v1/components/:component_id/reset
/// Request ECU reset
pub async fn ecu_reset(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
    Json(request): Json<EcuResetRequest>,
) -> Result<Json<EcuResetResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;

    let (reset_type_byte, reset_type_name) = parse_reset_type(&request.reset_type)?;

    let power_down_time = backend.ecu_reset(reset_type_byte).await?;

    let message = match reset_type_name {
        "hard" => "hard reset initiated".to_string(),
        "soft" => "soft reset initiated".to_string(),
        "key_off_on" => "key_off_on reset initiated".to_string(),
        _ => format!("Reset type 0x{:02X} initiated", reset_type_byte),
    };

    Ok(Json(EcuResetResponse {
        success: true,
        reset_type: reset_type_name.to_string(),
        message,
        power_down_time,
    }))
}
