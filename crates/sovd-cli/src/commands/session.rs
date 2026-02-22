//! Session command - diagnostic session control

use anyhow::{bail, Result};
use sovd_client::{SessionType, SovdClient};

use crate::output::OutputContext;

/// Change diagnostic session
pub async fn session(
    client: &SovdClient,
    ecu: &str,
    session_type: &str,
    ctx: &OutputContext,
) -> Result<()> {
    let session = match session_type.to_lowercase().as_str() {
        "default" | "1" | "0x01" => SessionType::Default,
        "programming" | "2" | "0x02" => SessionType::Programming,
        "extended" | "3" | "0x03" => SessionType::Extended,
        "engineering" | "0x60" | "96" => SessionType::Engineering,
        _ => bail!(
            "Unknown session type: {}. Valid types: default, programming, extended, engineering",
            session_type
        ),
    };

    client.set_session(ecu, session).await?;

    let session_name = match session {
        SessionType::Default => "Default (0x01)",
        SessionType::Programming => "Programming (0x02)",
        SessionType::Extended => "Extended (0x03)",
        SessionType::Engineering => "Engineering (0x60)",
    };

    ctx.success(&format!("Session changed to {}", session_name));
    Ok(())
}
