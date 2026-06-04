//! Monitor command - real-time parameter streaming

use anyhow::Result;
use futures::stream::{select_all, SelectAll, StreamExt};
use sovd_client::{SovdClient, StreamError, StreamEvent, Subscription, SubscriptionInterval};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::output::{OutputContext, OutputFormat, StreamRow};

/// Active event source for the monitor loop.
///
/// ISO 17978-3 cyclic-subscriptions are single-resource (one parameter
/// per subscription), and the inline multi-param `/streams` endpoint was
/// retired for C-025. So every parameter — direct or gateway-child
/// (`child/param`) — gets its own cyclic subscription; the streams are
/// merged client-side so one event loop drives them all.
struct MonitorStream {
    /// N cyclic subscriptions merged into one.
    streams: SelectAll<Subscription>,
}

impl MonitorStream {
    /// Next event from any underlying subscription.
    async fn next(&mut self) -> Option<Result<StreamEvent, StreamError>> {
        StreamExt::next(&mut self.streams).await
    }

    /// Explicitly cancel every underlying subscription (DELETE on the
    /// server).  Without this, `Subscription::drop` still cleans up, but
    /// Ctrl-C asks for a deterministic teardown.
    async fn cancel(self) -> Result<()> {
        for sub in self.streams.into_iter() {
            sub.cancel().await?;
        }
        Ok(())
    }
}

/// Map the CLI's `--rate` (Hz) to the coarse spec interval.  SOVDd maps
/// fast→20 Hz, normal→5 Hz, slow→1 Hz, so: >=10 Hz → Fast, >=2 Hz →
/// Normal, 0/1 Hz → Slow.
fn rate_to_interval(rate: u32) -> SubscriptionInterval {
    if rate >= 10 {
        SubscriptionInterval::Fast
    } else if rate >= 2 {
        SubscriptionInterval::Normal
    } else {
        SubscriptionInterval::Slow
    }
}

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

    // Every parameter (direct or gateway-child `child/param`) gets its
    // own spec cyclic-subscription; the per-subscription SSE streams are
    // merged client-side. C-025 retired the inline multi-param streamer.
    let interval = rate_to_interval(rate);
    let mut subs = Vec::with_capacity(params.len());
    for param in &params {
        subs.push(client.subscribe(ecu, param, interval).await?);
    }
    let mut stream = MonitorStream {
        streams: select_all(subs),
    };

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
            event = stream.next() => {
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
    stream.cancel().await?;
    ctx.success("Subscription cancelled");

    Ok(())
}

/// Print a stream event in the appropriate format
fn print_stream_event(event: &sovd_client::StreamEvent, params: &[String], ctx: &OutputContext) {
    // EventEnvelope: skip events with no success payload (error-only).
    let Some(values) = event.values() else {
        return;
    };
    let sequence = event.sequence().unwrap_or(0);

    match ctx.format {
        OutputFormat::Table => {
            // Print each parameter value on its own line
            for param in params {
                if let Some(value) = values.get(param) {
                    let row = StreamRow {
                        timestamp: event.timestamp.to_string(),
                        sequence: sequence.to_string(),
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
                .chain(std::iter::once(sequence.to_string()))
                .chain(
                    params
                        .iter()
                        .map(|p| values.get(p).map(format_json_value).unwrap_or_default()),
                )
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
