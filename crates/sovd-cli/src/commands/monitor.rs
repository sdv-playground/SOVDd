//! Monitor command - real-time parameter streaming

use anyhow::Result;
use sovd_client::SovdClient;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::output::{OutputContext, OutputFormat, StreamRow};

/// Monitor parameters in real-time via SSE streaming
pub async fn monitor(
    client: &SovdClient,
    ecu: &str,
    params: Vec<String>,
    rate: u32,
    ctx: &OutputContext,
) -> Result<()> {
    ctx.info(&format!(
        "Subscribing to {} parameter(s) at {}Hz...",
        params.len(),
        rate
    ));
    ctx.info("Press Ctrl+C to stop");

    let mut subscription = client.subscribe(ecu, params.clone(), rate).await?;

    // Set up Ctrl+C handler
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })?;

    // Print header for table format
    if ctx.format == OutputFormat::Table && !ctx.quiet {
        println!();
    }

    // For CSV, print header once
    if ctx.format == OutputFormat::Csv {
        let headers: Vec<&str> = std::iter::once("timestamp")
            .chain(std::iter::once("sequence"))
            .chain(params.iter().map(|s| s.as_str()))
            .collect();
        println!("{}", headers.join(","));
    }

    while running.load(Ordering::SeqCst) {
        tokio::select! {
            event = subscription.next() => {
                match event {
                    Some(Ok(data)) => {
                        print_stream_event(&data, &params, ctx);
                    }
                    Some(Err(e)) => {
                        ctx.error(&format!("Stream error: {}", e));
                        break;
                    }
                    None => {
                        ctx.info("Stream ended");
                        break;
                    }
                }
            }
            _ = tokio::time::sleep(tokio::time::Duration::from_millis(100)) => {
                // Check running flag periodically
                if !running.load(Ordering::SeqCst) {
                    break;
                }
            }
        }
    }

    ctx.info("\nStopping subscription...");
    subscription.cancel().await?;
    ctx.success("Subscription cancelled");

    Ok(())
}

/// Print a stream event in the appropriate format
fn print_stream_event(event: &sovd_client::StreamEvent, params: &[String], ctx: &OutputContext) {
    match ctx.format {
        OutputFormat::Table => {
            // Print each parameter value on its own line
            for param in params {
                if let Some(value) = event.values.get(param) {
                    let row = StreamRow {
                        timestamp: event.timestamp.to_string(),
                        sequence: event.sequence.to_string(),
                        parameter: param.clone(),
                        value: format_json_value(value),
                    };
                    // Simple inline display for streaming
                    println!(
                        "[{}] #{}: {} = {}",
                        row.timestamp, row.sequence, row.parameter, row.value
                    );
                }
            }
        }
        OutputFormat::Json => {
            // Print each event as JSON
            if let Ok(json) = serde_json::to_string(event) {
                println!("{}", json);
            }
        }
        OutputFormat::Csv => {
            // Print as CSV row
            let values: Vec<String> = std::iter::once(event.timestamp.to_string())
                .chain(std::iter::once(event.sequence.to_string()))
                .chain(params.iter().map(|p| {
                    event
                        .values
                        .get(p)
                        .map(|v| format_json_value(v))
                        .unwrap_or_default()
                }))
                .collect();
            println!("{}", values.join(","));
        }
    }
}

fn format_json_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}
