//! Unlock command - security access

use anyhow::{bail, Result};
use sovd_client::{SecurityLevel, SovdClient};

use crate::output::OutputContext;

/// Perform security access (unlock ECU)
pub async fn unlock(
    client: &SovdClient,
    ecu: &str,
    level: Option<u8>,
    key: Option<&str>,
    ctx: &OutputContext,
) -> Result<()> {
    let security_level = match level.unwrap_or(1) {
        1 => SecurityLevel::LEVEL_1,
        3 => SecurityLevel::LEVEL_3,
        _ => bail!("Unsupported security level. Valid levels: 1, 3"),
    };

    ctx.info(&format!(
        "Requesting seed for security level {}...",
        level.unwrap_or(1)
    ));

    // Request seed
    let seed = client
        .security_access_request_seed(ecu, security_level)
        .await?;

    ctx.info(&format!("Seed received: {}", hex::encode(&seed)));

    // If key is provided, use it; otherwise prompt for key algorithm
    let key_bytes = if let Some(key_hex) = key {
        hex::decode(key_hex.trim_start_matches("0x"))
            .map_err(|_| anyhow::anyhow!("Invalid hex key"))?
    } else {
        // Simple XOR-based key calculation (common algorithm)
        // In production, this would be algorithm-specific
        ctx.info("No key provided. Using simple XOR algorithm (for testing only)");
        seed.iter().map(|b| b ^ 0xFF).collect()
    };

    ctx.info(&format!("Sending key: {}", hex::encode(&key_bytes)));

    // Send key
    client
        .security_access_send_key(ecu, security_level, &key_bytes)
        .await?;

    ctx.success("Security access granted");
    Ok(())
}
