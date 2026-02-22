//! Faults command - fault/DTC management

use anyhow::Result;
use sovd_client::SovdClient;

use crate::output::{FaultRow, OutputContext};

/// List and manage faults
pub async fn faults(
    client: &SovdClient,
    ecu: &str,
    active_only: bool,
    clear: bool,
    ctx: &OutputContext,
) -> Result<()> {
    if clear {
        // Clear all faults
        let result = client.clear_faults(ecu).await?;
        if result.success {
            ctx.success(&format!(
                "Cleared {} fault(s)",
                result.cleared_count.unwrap_or(0)
            ));
        } else {
            ctx.error(&format!(
                "Failed to clear faults: {}",
                result
                    .message
                    .unwrap_or_else(|| "Unknown error".to_string())
            ));
        }
        return Ok(());
    }

    // List faults
    let faults = if active_only {
        client.get_faults_filtered(ecu, Some("active")).await?
    } else {
        client.get_faults(ecu).await?
    };

    if faults.is_empty() {
        ctx.info("No faults found");
        return Ok(());
    }

    let rows: Vec<FaultRow> = faults
        .into_iter()
        .map(|f| FaultRow {
            code: f.code,
            message: f.message,
            severity: f.severity,
            active: if f.active {
                "Yes".to_string()
            } else {
                "No".to_string()
            },
            category: f.category.unwrap_or_else(|| "-".to_string()),
        })
        .collect();

    ctx.print(&rows);
    Ok(())
}
