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

/// Execute an operation/routine (UDS RoutineControl 0x31 0x01 start).
///
/// Spec §7.14 executions sub-resource — the CLI starts one execution
/// per run; the resulting execution-id is printed for further polling
/// or stopping with `sovd-cli` follow-up commands.
pub async fn run(
    client: &SovdClient,
    ecu: &str,
    operation_id: &str,
    _action: Option<&str>,
    params_json: Option<&str>,
    ctx: &OutputContext,
) -> Result<()> {
    ctx.info(&format!(
        "Starting execution of operation '{}'...",
        operation_id
    ));

    let result = client
        .start_operation_execution(ecu, operation_id, params_json)
        .await?;

    if let Some(error) = result.error {
        ctx.error(&format!("Operation failed: {}", error));
    } else {
        ctx.success(&format!(
            "Operation {} (exec_id {})",
            result.status, result.execution_id
        ));

        if let Some(ref data) = result.result {
            ctx.info(&format!("Result: {}", data));
        }
    }

    Ok(())
}
