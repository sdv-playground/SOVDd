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
    /// Page the whole log via the server cursor (oldest→newest) until exhausted.
    pub all: bool,
    /// Lower time bound (RFC 3339), server-side.
    pub since: Option<String>,
    /// Upper time bound (RFC 3339), server-side.
    pub until: Option<String>,
}

impl LogArgs {
    fn to_filter(&self) -> LogFilter {
        LogFilter {
            log_type: self.log_type.clone(),
            status: self.status.clone(),
            priority: self.priority.clone(),
            source: self.source.clone(),
            limit: self.limit,
            // `after` is set by the --all paging loop per-request; None here.
            after: None,
            since: self.since.clone(),
            until: self.until.clone(),
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

    // --all: page the whole log via the server cursor. Each response's
    // `next_cursor` feeds the next request's `after`; stop when it is None (the
    // server has no more — see the backend `get_logs_paged` contract). A server
    // that doesn't paginate returns next_cursor=None on the first page, so this
    // degrades to a single fetch. Client-side `pattern` still filters; `tail`
    // is ignored here (it contradicts "all") — warn if both were given.
    if args.all {
        if args.follow {
            anyhow::bail!("--all and --follow are mutually exclusive (one drains history, the other tails live)");
        }
        if args.tail.is_some() {
            ctx.info("--tail ignored with --all (paging the whole log)");
        }
        let mut after: Option<String> = None;
        let mut printed = 0usize;
        // Bound the loop defensively so a misbehaving server (a cursor that never
        // clears) can't spin forever; 100k pages × server page size is far beyond
        // any real log.
        for _ in 0..100_000 {
            let mut f = filter.clone();
            f.after = after.clone();
            let resp = client.get_logs_filtered(ecu, &f).await?;
            for e in &resp.items {
                if let Some(pat) = &args.pattern {
                    if !e.message.to_lowercase().contains(&pat.to_lowercase()) {
                        continue;
                    }
                }
                ctx.print_one(&LogRow::from(e));
                printed += 1;
            }
            match resp.next_cursor {
                Some(c) => after = Some(c),
                None => break,
            }
        }
        if printed == 0 {
            ctx.info("No log entries");
        }
        return Ok(());
    }

    if !args.follow {
        let entries = fetch(client, ecu, &filter, args).await?;
        if entries.is_empty() {
            ctx.info("No log entries");
            return Ok(());
        }
        ctx.print(&entries.iter().map(LogRow::from).collect::<Vec<_>>());
        return Ok(());
    }

    // --follow: prefer the CURSOR. First call establishes a resume point — the
    // server's tip_cursor ("now"), unless --since anchored an earlier start —
    // then each poll requests `after=<last cursor>` and prints what arrived. The
    // cursor is reboot-safe, so a follow survives a device reboot (unlike an
    // id-dedup window). If the server returns no cursors at all (a non-paging
    // backend), fall back to id-dedup polling.
    ctx.info(&format!(
        "Following {ecu} logs (poll every {:.1}s, Ctrl-C to stop)…",
        args.interval_secs
    ));
    let poll = Duration::from_secs_f64(args.interval_secs.max(0.1));

    // Seed: one fetch to learn the starting cursor. With --since we START from
    // that bound (print the matching backlog, then follow past it); without, we
    // jump to the tip and follow only NEW entries.
    let seed = client.get_logs_filtered(ecu, &filter).await?;
    let cursor_mode = seed.next_cursor.is_some() || seed.tip_cursor.is_some();

    if !cursor_mode {
        // No server cursor → legacy id-dedup poll (bounded seen-set).
        return follow_by_id_dedup(client, ecu, &filter, args, poll, ctx).await;
    }

    // If the user anchored with --since, emit the seed backlog first; otherwise
    // start silent at the tip. Resume cursor = wherever the seed left off.
    let mut after: Option<String> = if args.since.is_some() {
        for e in &seed.items {
            if pattern_ok(&e.message, args) {
                ctx.print_one(&LogRow::from(e));
            }
        }
        seed.next_cursor.clone().or_else(|| seed.tip_cursor.clone())
    } else {
        seed.tip_cursor.clone().or_else(|| seed.next_cursor.clone())
    };

    loop {
        tokio::time::sleep(poll).await;
        let mut f = filter.clone();
        f.after = after.clone();
        match client.get_logs_filtered(ecu, &f).await {
            Ok(resp) => {
                for e in &resp.items {
                    if pattern_ok(&e.message, args) {
                        ctx.print_one(&LogRow::from(e));
                    }
                }
                // Advance only when the server moved us forward; keep the last
                // cursor otherwise (an empty poll shouldn't reset the position).
                if let Some(c) = resp.next_cursor.or(resp.tip_cursor) {
                    after = Some(c);
                }
            }
            Err(e) => ctx.error(&format!("poll failed (retrying): {e}")),
        }
    }
}

/// Client-side substring filter (the server `LogFilter` has no pattern field).
fn pattern_ok(message: &str, args: &LogArgs) -> bool {
    match &args.pattern {
        Some(p) => message.to_lowercase().contains(&p.to_lowercase()),
        None => true,
    }
}

/// Legacy follow for a backend that returns no cursor: re-fetch the list and
/// print only ids not seen in the bounded window. Reboot-unsafe (id-based), but
/// the only option when the server can't paginate.
async fn follow_by_id_dedup(
    client: &SovdClient,
    ecu: &str,
    filter: &LogFilter,
    args: &LogArgs,
    poll: Duration,
    ctx: &OutputContext,
) -> Result<()> {
    let mut seen: HashSet<String> = HashSet::new();
    loop {
        match fetch(client, ecu, filter, args).await {
            Ok(entries) => {
                for e in entries.iter().rev() {
                    if seen.insert(e.id.clone()) {
                        ctx.print_one(&LogRow::from(e));
                    }
                }
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
