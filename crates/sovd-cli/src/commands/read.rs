//! Read command - read data parameters

use anyhow::Result;
use sovd_client::SovdClient;

use crate::output::{DataRow, OutputContext, ParameterRow};

/// List available parameters for an ECU
pub async fn data(client: &SovdClient, ecu: &str, ctx: &OutputContext) -> Result<()> {
    let params = client.list_parameters(ecu).await?;

    let rows: Vec<ParameterRow> = params
        .items
        .into_iter()
        .map(|p| ParameterRow {
            id: p.id,
            did: p.did,
            name: p.name.unwrap_or_default(),
            data_type: p.data_type.unwrap_or_default(),
            unit: p.unit.unwrap_or_default(),
        })
        .collect();

    ctx.print(&rows);
    Ok(())
}

/// Read one or more parameters
pub async fn read(
    client: &SovdClient,
    ecu: &str,
    params: &[String],
    all: bool,
    ctx: &OutputContext,
) -> Result<()> {
    let param_ids: Vec<&str> = if all {
        // Get all parameter IDs
        let available = client.list_parameters(ecu).await?;
        let ids: Vec<String> = available.items.into_iter().map(|p| p.id).collect();

        // Read all parameters
        let mut rows = Vec::new();
        for id in &ids {
            match client.read_data(ecu, id).await {
                Ok(data) => {
                    rows.push(DataRow {
                        parameter: id.clone(),
                        value: format_value(&data.value),
                        unit: data.unit.unwrap_or_default(),
                        raw: data.raw.unwrap_or_default(),
                    });
                }
                Err(e) => {
                    rows.push(DataRow {
                        parameter: id.clone(),
                        value: format!("Error: {}", e),
                        unit: String::new(),
                        raw: String::new(),
                    });
                }
            }
        }
        ctx.print(&rows);
        return Ok(());
    } else {
        params.iter().map(|s| s.as_str()).collect()
    };

    if param_ids.len() == 1 {
        // Single parameter read
        let data = client.read_data(ecu, param_ids[0]).await?;
        let row = DataRow {
            parameter: param_ids[0].to_string(),
            value: format_value(&data.value),
            unit: data.unit.unwrap_or_default(),
            raw: data.raw.unwrap_or_default(),
        };
        ctx.print_one(&row);
    } else {
        // Batch read
        let results = client.read_data_batch(ecu, &param_ids).await?;

        let rows: Vec<DataRow> = results
            .into_iter()
            .zip(param_ids.iter())
            .map(|(data, id)| DataRow {
                parameter: id.to_string(),
                value: format_value(&data.value),
                unit: data.unit.unwrap_or_default(),
                raw: data.raw.unwrap_or_default(),
            })
            .collect();

        ctx.print(&rows);
    }

    Ok(())
}

/// Format a JSON value for display
fn format_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "-".to_string(),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Array(arr) => {
            let items: Vec<String> = arr.iter().map(format_value).collect();
            items.join(", ")
        }
        serde_json::Value::Object(_) => serde_json::to_string(value).unwrap_or_default(),
    }
}
