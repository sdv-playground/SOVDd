//! Reset command - ECU reset

use anyhow::Result;
use sovd_client::SovdClient;

use crate::output::OutputContext;

/// Reset an ECU
pub async fn reset(
    client: &SovdClient,
    ecu: &str,
    reset_type: Option<&str>,
    ctx: &OutputContext,
) -> Result<()> {
    let rtype = reset_type.unwrap_or("hard");

    ctx.info(&format!("Performing {} reset on {}...", rtype, ecu));

    let result = client.ecu_reset(ecu, rtype).await?;

    // Legacy-tree body: `completed` once the reset has taken effect,
    // `running` when it was triggered and is still in progress.
    if result.status == "completed" || result.status == "running" {
        ctx.success(&format!("ECU {} reset accepted", result.reset_type));
        if !result.message.is_empty() {
            ctx.info(&result.message);
        }
    } else {
        ctx.error(&format!(
            "Reset status: {} ({})",
            result.status, result.message
        ));
    }

    Ok(())
}
