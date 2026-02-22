//! Info command - show ECU component details

use anyhow::Result;
use sovd_client::SovdClient;

use crate::output::OutputContext;

/// Show detailed information about an ECU component
pub async fn info(client: &SovdClient, ecu: &str, ctx: &OutputContext) -> Result<()> {
    let component = client.get_component(ecu).await?;

    let pairs = vec![
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

    ctx.print_kv(&pairs);
    Ok(())
}
