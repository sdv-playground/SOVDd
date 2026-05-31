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
        .map(|f| {
            // Spec Fault dropped `active` and `category` in F.6;
            // derive `active` from `status.testFailed`.
            let active = f
                .status
                .as_ref()
                .and_then(|s| s.get("testFailed"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            FaultRow {
                code: f.code,
                fault_name: f.fault_name,
                severity: severity_label(f.severity),
                active: if active { "Yes" } else { "No" }.to_string(),
                category: "-".to_string(),
            }
        })
        .collect();

    ctx.print(&rows);
    Ok(())
}

/// Spec §7.8 severity is integer 1..4; render as the well-known label
/// for human-friendly CLI output.
fn severity_label(s: u8) -> String {
    match s {
        1 => "FATAL",
        2 => "ERROR",
        3 => "WARN",
        4 => "INFO",
        _ => "?",
    }
    .to_string()
}
