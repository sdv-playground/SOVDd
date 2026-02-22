//! Outputs command - list I/O outputs

use anyhow::Result;
use sovd_client::SovdClient;

use crate::output::{OutputContext, OutputRow};

/// List available I/O outputs for an ECU
pub async fn outputs(client: &SovdClient, ecu: &str, ctx: &OutputContext) -> Result<()> {
    let outputs = client.list_outputs(ecu).await?;

    if outputs.is_empty() {
        ctx.info("No outputs available");
        return Ok(());
    }

    let rows: Vec<OutputRow> = outputs
        .into_iter()
        .map(|o| OutputRow {
            id: o.id,
            name: o.name.unwrap_or_default(),
            data_type: o.data_type.unwrap_or_default(),
            controls: o.control_types.join(", "),
        })
        .collect();

    ctx.print(&rows);
    Ok(())
}
