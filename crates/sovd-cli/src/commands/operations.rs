//! Operations command - routine control

use anyhow::Result;
use sovd_client::SovdClient;

use crate::output::{OperationRow, OutputContext};

/// List available operations for an ECU
pub async fn ops(client: &SovdClient, ecu: &str, ctx: &OutputContext) -> Result<()> {
    let operations = client.list_operations(ecu).await?;

    if operations.is_empty() {
        ctx.info("No operations available");
        return Ok(());
    }

    let rows: Vec<OperationRow> = operations
        .into_iter()
        .map(|op| OperationRow {
            id: op.id,
            name: op.name,
            description: op.description.unwrap_or_default(),
            requires_security: if op.requires_security {
                "Yes".to_string()
            } else {
                "No".to_string()
            },
        })
        .collect();

    ctx.print(&rows);
    Ok(())
}

/// Execute an operation/routine
pub async fn run(
    client: &SovdClient,
    ecu: &str,
    operation_id: &str,
    action: Option<&str>,
    params_json: Option<&str>,
    ctx: &OutputContext,
) -> Result<()> {
    let action = action.unwrap_or("start");

    ctx.info(&format!(
        "Executing operation '{}' with action '{}'...",
        operation_id, action
    ));

    let result = client
        .execute_operation(ecu, operation_id, action, params_json)
        .await?;

    if let Some(error) = result.error {
        ctx.error(&format!("Operation failed: {}", error));
    } else {
        ctx.success(&format!("Operation {} completed", result.status));

        // Print result data if present
        if let Some(ref data) = result.result_data {
            if !data.is_empty() {
                ctx.info(&format!("Result: {}", data));
            }
        }
    }

    Ok(())
}
