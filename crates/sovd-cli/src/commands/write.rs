//! Write command - write data parameters

use anyhow::{Context, Result};
use sovd_client::SovdClient;

use crate::output::OutputContext;

/// Write a parameter value
pub async fn write(
    client: &SovdClient,
    ecu: &str,
    param: &str,
    value: &str,
    ctx: &OutputContext,
) -> Result<()> {
    // Try to parse the value as JSON, fall back to string
    let json_value: serde_json::Value = if value.starts_with('{')
        || value.starts_with('[')
        || value == "true"
        || value == "false"
        || value == "null"
    {
        serde_json::from_str(value).context("Failed to parse value as JSON")?
    } else if let Ok(num) = value.parse::<i64>() {
        serde_json::Value::Number(num.into())
    } else if let Ok(num) = value.parse::<f64>() {
        serde_json::Number::from_f64(num)
            .map(serde_json::Value::Number)
            .unwrap_or_else(|| serde_json::Value::String(value.to_string()))
    } else {
        serde_json::Value::String(value.to_string())
    };

    client
        .write_data(ecu, param, json_value)
        .await
        .context("Failed to write parameter")?;

    ctx.success(&format!("Successfully wrote {} = {}", param, value));
    Ok(())
}
