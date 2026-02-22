//! List command - show all ECU components

use anyhow::Result;
use sovd_client::SovdClient;

use crate::output::{ComponentRow, OutputContext};

/// List all ECU components
pub async fn list(client: &SovdClient, ctx: &OutputContext) -> Result<()> {
    let components = client.list_components().await?;

    let rows: Vec<ComponentRow> = components
        .into_iter()
        .map(|c| ComponentRow {
            id: c.id,
            name: c.name,
            status: c.status.unwrap_or_else(|| "unknown".to_string()),
        })
        .collect();

    ctx.print(&rows);
    Ok(())
}
