//! Diagnostics command — SOVD §7.9 read-only system probes.
//!
//! `diagnostics <ecu>` lists a component's registered probes (proxied from its
//! guest diag-agent); `diagnostics <ecu> <probe-id>` gathers one now and prints
//! its output. Read-only — no probe mutates anything. See the `guest-triage`
//! skill + tasks/diag-agent-design.md.

use anyhow::Result;
use serde::Serialize;
use sovd_client::SovdClient;
use tabled::Tabled;

use crate::output::OutputContext;

/// Table row for the probe list.
#[derive(Debug, Tabled, Serialize)]
pub struct ProbeRow {
    #[tabled(rename = "Probe")]
    pub id: String,
    #[tabled(rename = "Title")]
    pub title: String,
    #[tabled(rename = "Tags")]
    pub tags: String,
}

/// `sovd-cli diagnostics <ecu> [probe-id] [--tag <t>]`.
pub async fn diagnostics(
    client: &SovdClient,
    ecu: &str,
    probe_id: Option<&str>,
    tag: Option<&str>,
    ctx: &OutputContext,
) -> Result<()> {
    match probe_id {
        // Gather one probe → print its output (or the failure message).
        Some(id) => {
            let r = client.read_diagnostic(ecu, id).await?;
            if r.ok {
                // The output IS the payload (a df table, meminfo lines, …) —
                // print it raw so it reads like the tool it wraps.
                if r.output.is_empty() {
                    ctx.info(&format!("{id}: ok (no output)"));
                } else {
                    ctx.info(&format!("=== {id} ==="));
                    print!("{}", r.output);
                    if !r.output.ends_with('\n') {
                        println!();
                    }
                }
            } else {
                ctx.error(&format!(
                    "{id}: gather failed — {}",
                    r.message.as_deref().unwrap_or("(no detail)")
                ));
                // A failed gather is a real signal, not a CLI error; exit
                // non-zero so a script can branch on it.
                std::process::exit(1);
            }
        }
        // List registered probes.
        None => {
            let probes = client.list_diagnostics(ecu, tag).await?;
            if probes.is_empty() {
                ctx.info(&format!(
                    "No diagnostic probes on {ecu}{}",
                    tag.map(|t| format!(" (tag `{t}`)")).unwrap_or_default()
                ));
                return Ok(());
            }
            let rows: Vec<ProbeRow> = probes
                .into_iter()
                .map(|p| ProbeRow {
                    id: p.id,
                    title: p.title.unwrap_or_default(),
                    tags: p.tags.join(","),
                })
                .collect();
            ctx.print(&rows);
        }
    }
    Ok(())
}
