//! Info command - show ECU component details

use anyhow::Result;
use sovd_client::SovdClient;

use crate::output::OutputContext;

/// Show detailed information about an ECU component
pub async fn info(client: &SovdClient, ecu: &str, ctx: &OutputContext) -> Result<()> {
    let component = client.get_component(ecu).await?;

    let mut pairs = vec![
        ("ID", component.id),
        ("Name", component.name),
        (
            "Description",
            component.description.unwrap_or_else(|| "-".to_string()),
        ),
        (
            "Type",
            component.component_type.unwrap_or_else(|| "-".to_string()),
        ),
        (
            "Status",
            component.status.unwrap_or_else(|| "-".to_string()),
        ),
        ("Href", component.href.unwrap_or_else(|| "-".to_string())),
    ];

    // Capabilities the client cares about for triage/tooling — a compact list
    // of the ones that gate a whole command surface (logs/scripts/diagnostics/
    // bulk-data), so `info` answers "what can I ask this entity?". Only the
    // enabled ones are listed; absent = not wired.
    if let Some(c) = &component.capabilities {
        let mut on: Vec<&str> = Vec::new();
        if c.logs {
            on.push("logs");
        }
        if c.diagnostics {
            on.push("diagnostics");
        }
        if c.bulk_data {
            on.push("bulk-data");
        }
        if c.faults {
            on.push("faults");
        }
        if c.operations {
            on.push("operations");
        }
        if c.software_update {
            on.push("software-update");
        }
        let caps = if on.is_empty() {
            "-".to_string()
        } else {
            on.join(", ")
        };
        pairs.push(("Capabilities", caps));
    }

    ctx.print_kv(&pairs);
    Ok(())
}
