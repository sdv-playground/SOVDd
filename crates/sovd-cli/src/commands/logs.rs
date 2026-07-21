//! Logs command — SOVD §7.21 log access.
//!
//! `list` fetches the log list for a component (or, when pointed at a vehicle
//! gateway, the whole vehicle's logs merged + timestamp-sorted server-side by
//! `GatewayBackend::get_logs`). `get`/`content`/`delete` operate on one entry.
//! `--follow` polls the same list on an interval, printing only entries not seen
//! before — a tail against the one implemented endpoint (`get_logs`). A true SSE
//! log stream is a separate server-side feature (cyclic-subscriptions are for
//! periodic data, not event logs); see the streaming design note.

use std::collections::HashSet;
use std::time::Duration;

use anyhow::Result;
use sovd_client::{LogEntry, LogFilter, SovdClient};

use crate::output::{LogRow, OutputContext};

/// Filters accepted on the CLI, mapped onto the client `LogFilter` (server-side)
/// plus the client-side refinements the server filter can't express (`pattern`,
/// `tail`).
#[derive(Debug, Default, Clone)]
pub struct LogArgs {
    pub priority: Option<String>,
    pub source: Option<String>,
    pub log_type: Option<String>,
    pub status: Option<String>,
    pub limit: Option<u32>,
    /// Client-side substring match (the client `LogFilter` has no pattern field).
    pub pattern: Option<String>,
    /// Keep only the last N after fetch+sort (client-side tail).
    pub tail: Option<usize>,
    /// Poll the list every `interval`, printing only new entries.
    pub follow: bool,
    pub interval_secs: f64,
}

impl LogArgs {
    fn to_filter(&self) -> LogFilter {
        LogFilter {
            log_type: self.log_type.clone(),
            status: self.status.clone(),
            priority: self.priority.clone(),
            source: self.source.clone(),
            limit: self.limit,
        }
    }
}

/// Entry point for `sovd-cli logs list ...` / the follow loop.
pub async fn list(
    client: &SovdClient,
    ecu: &str,
    args: &LogArgs,
    ctx: &OutputContext,
) -> Result<()> {
    let filter = args.to_filter();

    if !args.follow {
        let entries = fetch(client, ecu, &filter, args).await?;
        if entries.is_empty() {
            ctx.info("No log entries");
            return Ok(());
        }
        ctx.print(&entries.iter().map(LogRow::from).collect::<Vec<_>>());
        return Ok(());
    }

    // --follow: poll, printing only entries whose id we haven't emitted. The
    // seen-set is bounded to the last window so it can't grow without bound on a
    // long-lived tail (ids are re-checked against the most recent fetch only).
    ctx.info(&format!(
        "Following {ecu} logs (poll every {:.1}s, Ctrl-C to stop)…",
        args.interval_secs
    ));
    let mut seen: HashSet<String> = HashSet::new();
    let poll = Duration::from_secs_f64(args.interval_secs.max(0.1));
    loop {
        match fetch(client, ecu, &filter, args).await {
            Ok(entries) => {
                // Oldest-first for a natural tail; the list arrives newest-first.
                for e in entries.iter().rev() {
                    if seen.insert(e.id.clone()) {
                        ctx.print_one(&LogRow::from(e));
                    }
                }
                // Bound the seen-set: retain only ids present this round, so it
                // tracks the live window rather than all history ever seen.
                if seen.len() > 4096 {
                    let current: HashSet<String> = entries.iter().map(|e| e.id.clone()).collect();
                    seen.retain(|id| current.contains(id));
                }
            }
            Err(e) => ctx.error(&format!("poll failed (retrying): {e}")),
        }
        tokio::time::sleep(poll).await;
    }
}

/// Fetch + apply the client-side refinements (pattern, tail) the server filter
/// can't. Returns newest-first (as the server sorts).
async fn fetch(
    client: &SovdClient,
    ecu: &str,
    filter: &LogFilter,
    args: &LogArgs,
) -> Result<Vec<LogEntry>> {
    let mut entries = client.get_logs_filtered(ecu, filter).await?.items;

    if let Some(pat) = &args.pattern {
        let pat = pat.to_lowercase();
        entries.retain(|e| e.message.to_lowercase().contains(&pat));
    }
    if let Some(n) = args.tail {
        // Newest-first, so the first N are the most recent.
        entries.truncate(n);
    }
    Ok(entries)
}

/// `sovd-cli logs get <ecu> <id>` — one entry's metadata.
pub async fn get(client: &SovdClient, ecu: &str, id: &str, ctx: &OutputContext) -> Result<()> {
    let entry = client.get_log(ecu, id).await?;
    ctx.print_one(&LogRow::from(&entry));
    Ok(())
}

/// `sovd-cli logs content <ecu> <id> [-o file]` — the entry's raw bytes
/// (a dump); to stdout by default, or a file.
pub async fn content(
    client: &SovdClient,
    ecu: &str,
    id: &str,
    out: Option<&str>,
    ctx: &OutputContext,
) -> Result<()> {
    let bytes = client.get_log_content(ecu, id).await?;
    match out {
        Some(path) => {
            std::fs::write(path, &bytes)?;
            ctx.success(&format!("wrote {} byte(s) to {path}", bytes.len()));
        }
        None => {
            use std::io::Write;
            std::io::stdout().write_all(&bytes)?;
        }
    }
    Ok(())
}

/// `sovd-cli logs delete <ecu> <id>` — acknowledge/remove an entry.
pub async fn delete(client: &SovdClient, ecu: &str, id: &str, ctx: &OutputContext) -> Result<()> {
    client.delete_log(ecu, id).await?;
    ctx.success(&format!("deleted log {id}"));
    Ok(())
}
