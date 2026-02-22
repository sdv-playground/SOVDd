//! Actuate command - control I/O outputs

use anyhow::{Context, Result};
use sovd_client::SovdClient;

use crate::output::OutputContext;

/// Control an I/O output
pub async fn actuate(
    client: &SovdClient,
    ecu: &str,
    output_id: &str,
    action: &str,
    value: Option<&str>,
    ctx: &OutputContext,
) -> Result<()> {
    // Validate action
    let valid_actions = [
        "short_term_adjust",
        "return_to_ecu",
        "reset_to_default",
        "freeze",
    ];
    if !valid_actions.contains(&action) {
        anyhow::bail!(
            "Invalid action: {}. Valid actions: {}",
            action,
            valid_actions.join(", ")
        );
    }

    // Value should be hex string for short_term_adjust
    let json_value = value.map(|v| serde_json::Value::String(v.to_string()));

    let result = client
        .control_output(ecu, output_id, action, json_value)
        .await
        .context("Failed to control output")?;

    if let Some(error) = result.error {
        ctx.error(&format!("Control failed: {}", error));
    } else if result.success {
        ctx.success(&format!(
            "Output {} {} successfully",
            output_id,
            action.replace('_', " ")
        ));
        if let Some(new_val) = result.new_value {
            ctx.info(&format!("New value: {}", new_val));
        }
    } else {
        ctx.error("Control failed");
    }

    Ok(())
}
