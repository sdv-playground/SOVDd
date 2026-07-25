//! The tester harness — the payoff of the tests-as-scripts work.
//!
//! A run:
//!   1. DISCOVERS a component's registered tests (`GET /scripts`, optional
//!      `--tag`), or runs one named test,
//!   2. EXECUTES each over SOVD (`POST /scripts/{id}/executions`) — the guest
//!      test-agent run is synchronous, so the returned execution is already
//!      terminal (verdict + output tails + the log-cursor bracket),
//!   3. CAPTURES exactly that run's log window by paging `logs` from the
//!      execution's `x-sumo-log-from` bracket (reboot-safe — the cursor crosses
//!      a reboot the test may have triggered),
//!   4. JUDGES both the framework's own verdict AND the captured logs (a
//!      conservative crash/severe-priority anomaly scan),
//!   5. REPORTS per-test verdict + its log window; exits non-zero if anything
//!      failed, and optionally writes a JSON report artifact.
//!
//! It owns NO test semantics — the developer's framework (inside the guest
//! entry's `cmd`) produces the verdict; this only runs it and reads the result +
//! the log window. Living in its OWN binary (not the generic sovd-cli) keeps this
//! policy layer — anomaly heuristics, verdict+logs pairing, exit-code-as-result —
//! out of the protocol client. See `tasks/sovd-tests-as-operations-design.md`.

use anyhow::Result;
use serde::Serialize;
use sovd_client::{LogEntry, LogFilter, ScriptExecution, ScriptVerdict, SovdClient};
use tabled::Tabled;

use crate::output::OutputContext;

/// Knobs for a tester run.
#[derive(Debug, Clone, Default)]
pub struct TestArgs {
    /// Run only this test id (else all registered on the component).
    pub only: Option<String>,
    /// Discovery filter — only tests carrying this tag (§6.2.7 `tags`).
    pub tag: Option<String>,
    /// Extra case-insensitive substrings that mark a log line anomalous, on top
    /// of the built-in crash set.
    pub grep: Vec<String>,
    /// Skip the log-window anomaly scan — judge on the framework verdict alone.
    pub no_log_check: bool,
    /// Print anomalous log lines from each test's window.
    pub show_logs: bool,
    /// Write the full machine-readable report (JSON) to this path.
    pub report: Option<String>,
}

/// Built-in anomaly substrings (lower-case) — deliberately CONSERVATIVE: crashes
/// and hard failures, not the ubiquitous word "error". A test that legitimately
/// logs "error" in its own output won't false-positive; a panic/segfault will.
/// Extend per-run with `--grep`.
const CRASH_MARKERS: &[&str] = &[
    "panic",
    "segfault",
    "segmentation fault",
    "core dumped",
    "assertion failed",
    "fatal error",
    "kernel bug",
    "oom-kill",
    "aborted (core dumped)",
];

/// Log priorities that count as anomalous regardless of message (syslog
/// emergency/alert/critical — below `error`, which is too common to flag).
const SEVERE_PRIORITIES: &[&str] = &["emergency", "emerg", "alert", "critical", "crit"];

/// One test's outcome — the report row (also the JSON artifact element).
#[derive(Debug, Clone, Serialize)]
pub struct TestOutcome {
    pub id: String,
    pub verdict: String,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<u64>,
    /// Entries captured in the run's log window (`None` = bracket unavailable).
    pub log_count: Option<usize>,
    /// Anomalous log lines found in the window (crash markers / severe priority).
    pub anomalies: Vec<String>,
    /// The log-cursor bracket the run reported (for the report artifact).
    pub log_from: Option<String>,
    pub log_to: Option<String>,
    /// Final call: `pass` only when the verdict passed AND (unless skipped) the
    /// log window is clean. Otherwise `fail`.
    pub ok: bool,
    /// Why it's not ok (verdict / anomalies / execution error), for the report.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Compact table row for human output.
#[derive(Debug, Tabled, Serialize)]
pub struct TestRow {
    #[tabled(rename = "Test")]
    pub id: String,
    #[tabled(rename = "Verdict")]
    pub verdict: String,
    #[tabled(rename = "Exit")]
    pub exit: String,
    #[tabled(rename = "ms")]
    pub duration: String,
    #[tabled(rename = "Logs")]
    pub logs: String,
    #[tabled(rename = "Anomalies")]
    pub anomalies: String,
    #[tabled(rename = "Result")]
    pub result: String,
}

impl From<&TestOutcome> for TestRow {
    fn from(o: &TestOutcome) -> Self {
        TestRow {
            id: o.id.clone(),
            verdict: o.verdict.clone(),
            exit: o
                .exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "-".into()),
            duration: o
                .duration_ms
                .map(|d| d.to_string())
                .unwrap_or_else(|| "-".into()),
            logs: o
                .log_count
                .map(|n| n.to_string())
                .unwrap_or_else(|| "n/a".into()),
            anomalies: if o.anomalies.is_empty() {
                "-".into()
            } else {
                o.anomalies.len().to_string()
            },
            result: if o.ok { "PASS".into() } else { "FAIL".into() },
        }
    }
}

/// Run the tester. Returns the number of FAILED tests (0 = all passed) so the
/// binary can map it to an exit code — the tester's result IS its exit status.
pub async fn run(
    client: &SovdClient,
    ecu: &str,
    args: &TestArgs,
    ctx: &OutputContext,
) -> Result<usize> {
    // 1. Discover (or take the single named test).
    let script_ids: Vec<String> = match &args.only {
        Some(id) => vec![id.clone()],
        None => {
            let scripts = client.list_scripts(ecu, args.tag.as_deref()).await?;
            if scripts.is_empty() {
                ctx.info(&format!(
                    "No registered tests on {ecu}{}",
                    args.tag
                        .as_deref()
                        .map(|t| format!(" (tag `{t}`)"))
                        .unwrap_or_default()
                ));
                return Ok(0);
            }
            scripts.into_iter().map(|s| s.id).collect()
        }
    };

    ctx.info(&format!("Running {} test(s) on {ecu}…", script_ids.len()));

    // 2–4. Run each test, capture its log window, judge.
    let mut outcomes = Vec::with_capacity(script_ids.len());
    for id in &script_ids {
        outcomes.push(run_one(client, ecu, id, args, ctx).await);
    }

    // 5. Report.
    let rows: Vec<TestRow> = outcomes.iter().map(TestRow::from).collect();
    ctx.print(&rows);

    if let Some(path) = &args.report {
        let json = serde_json::to_string_pretty(&outcomes)?;
        std::fs::write(path, json)?;
        ctx.success(&format!("wrote report to {path}"));
    }

    let failed = outcomes.iter().filter(|o| !o.ok).count();
    if failed > 0 {
        ctx.error(&format!("{failed}/{} test(s) failed", outcomes.len()));
    } else {
        ctx.success(&format!("all {} test(s) passed", outcomes.len()));
    }
    Ok(failed)
}

/// Execute one test, capture its log window, and judge it. Never bails — an
/// execution error becomes a `TestOutcome` with `ok = false` so one broken test
/// doesn't abort the run.
async fn run_one(
    client: &SovdClient,
    ecu: &str,
    id: &str,
    args: &TestArgs,
    ctx: &OutputContext,
) -> TestOutcome {
    let exec = match client.execute_script(ecu, id).await {
        Ok(e) => e,
        Err(e) => {
            return TestOutcome {
                id: id.to_string(),
                verdict: "error".into(),
                exit_code: None,
                duration_ms: None,
                log_count: None,
                anomalies: Vec::new(),
                log_from: None,
                log_to: None,
                ok: false,
                reason: Some(format!("execution failed: {e}")),
            };
        }
    };

    // Capture the run's log window from the bracket (best-effort). `None` count
    // means we didn't scan — either the check was disabled or the bracket was
    // unavailable (a source that can't name a tip; see ScriptExecution.log_from).
    let (log_count, anomalies) = match (args.no_log_check, exec.log_from.as_deref()) {
        (false, Some(from)) => match capture_window(client, ecu, from, args).await {
            Ok((n, an)) => {
                if args.show_logs && !an.is_empty() {
                    for line in &an {
                        ctx.print_one(&LogLine { line: line.clone() });
                    }
                }
                (Some(n), an)
            }
            Err(e) => {
                ctx.warn(&format!("{id}: log capture failed: {e}"));
                (None, Vec::new())
            }
        },
        _ => (None, Vec::new()),
    };

    judge(id, exec, log_count, anomalies)
}

/// Page the log window `after = log_from` to the current tip (drains
/// `next_cursor` until exhausted — the design's `GET /logs?x-sumo-after=`).
/// Returns (entries seen, anomalous lines). Bounded against a runaway cursor.
async fn capture_window(
    client: &SovdClient,
    ecu: &str,
    log_from: &str,
    args: &TestArgs,
) -> Result<(usize, Vec<String>)> {
    let mut after = Some(log_from.to_string());
    let mut count = 0usize;
    let mut anomalies = Vec::new();
    for _ in 0..100_000 {
        let filter = LogFilter {
            after: after.clone(),
            ..Default::default()
        };
        let resp = client.get_logs_filtered(ecu, &filter).await?;
        for e in &resp.items {
            count += 1;
            if let Some(line) = anomaly(e, args) {
                anomalies.push(line);
            }
        }
        match resp.next_cursor {
            Some(c) => after = Some(c),
            None => break,
        }
    }
    Ok((count, anomalies))
}

/// Is this entry anomalous? A crash marker in the message, or a severe priority.
/// Returns the formatted line if so.
fn anomaly(e: &LogEntry, args: &TestArgs) -> Option<String> {
    let msg = e.message.to_lowercase();
    let prio = e.priority.to_lowercase();
    let crash = CRASH_MARKERS.iter().any(|m| msg.contains(m))
        || args.grep.iter().any(|g| msg.contains(&g.to_lowercase()));
    let severe = SEVERE_PRIORITIES.iter().any(|p| prio == *p);
    (crash || severe).then(|| {
        format!(
            "[{}] {} {}",
            e.priority,
            e.source.as_deref().unwrap_or("-"),
            e.message
        )
    })
}

/// Combine the framework verdict + the log scan into the final call.
fn judge(
    id: &str,
    exec: ScriptExecution,
    log_count: Option<usize>,
    anomalies: Vec<String>,
) -> TestOutcome {
    let verdict = exec.verdict.unwrap_or(ScriptVerdict::Error);
    let verdict_ok = verdict == ScriptVerdict::Pass;
    let logs_ok = anomalies.is_empty();
    let ok = verdict_ok && logs_ok;

    let reason = if ok {
        None
    } else if !verdict_ok {
        Some(format!(
            "verdict {verdict}{}",
            exec.message
                .as_deref()
                .map(|m| format!(" — {m}"))
                .unwrap_or_default()
        ))
    } else {
        Some(format!("{} log anomaly(ies) in window", anomalies.len()))
    };

    TestOutcome {
        id: id.to_string(),
        verdict: verdict.to_string(),
        exit_code: exec.exit_code,
        duration_ms: exec.duration_ms,
        log_count,
        anomalies,
        log_from: exec.log_from,
        log_to: exec.log_to,
        ok,
        reason,
    }
}

/// Tiny row for `--show-logs` anomaly dump.
#[derive(Debug, Tabled, Serialize)]
struct LogLine {
    #[tabled(rename = "Anomaly")]
    line: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovd_client::ScriptStatus;

    fn exec(verdict: ScriptVerdict) -> ScriptExecution {
        ScriptExecution {
            exec_id: "t-1".into(),
            script_id: "guest-hal.smoke".into(),
            status: ScriptStatus::Done,
            verdict: Some(verdict),
            exit_code: Some(if verdict == ScriptVerdict::Pass { 0 } else { 1 }),
            duration_ms: Some(42),
            message: None,
            stdout_tail: None,
            stderr_tail: None,
            started: "1970-01-01T00:00:00Z".into(),
            ended: Some("1970-01-01T00:00:00Z".into()),
            log_from: Some("cur-a".into()),
            log_to: Some("cur-b".into()),
            href: None,
        }
    }

    #[test]
    fn pass_with_clean_logs_is_ok() {
        let o = judge(
            "guest-hal.smoke",
            exec(ScriptVerdict::Pass),
            Some(3),
            vec![],
        );
        assert!(o.ok);
        assert_eq!(o.verdict, "pass");
        assert!(o.reason.is_none());
        assert_eq!(o.log_from.as_deref(), Some("cur-a"));
    }

    #[test]
    fn fail_verdict_is_not_ok() {
        let o = judge(
            "guest-hal.smoke",
            exec(ScriptVerdict::Fail),
            Some(3),
            vec![],
        );
        assert!(!o.ok);
        assert!(o.reason.unwrap().contains("verdict fail"));
    }

    #[test]
    fn pass_verdict_but_log_anomaly_fails() {
        let o = judge(
            "guest-hal.smoke",
            exec(ScriptVerdict::Pass),
            Some(5),
            vec!["[crit] kernel panic".into()],
        );
        assert!(
            !o.ok,
            "a passing verdict with a log anomaly must FAIL the test"
        );
        assert!(o.reason.unwrap().contains("anomaly"));
    }

    #[test]
    fn crash_marker_in_message_is_anomalous() {
        let e = LogEntry {
            id: "1".into(),
            timestamp: "t".into(),
            priority: "info".into(),
            message: "thread 'main' panicked at src/x.rs".into(),
            source: Some("vm1".into()),
            pid: None,
            log_type: None,
            size: None,
            status: None,
            href: None,
            metadata: None,
        };
        assert!(anomaly(&e, &TestArgs::default()).is_some());
    }

    #[test]
    fn plain_error_word_is_not_anomalous_by_default() {
        // "error" is too common to flag — only crash markers / severe priority do.
        let e = LogEntry {
            id: "1".into(),
            timestamp: "t".into(),
            priority: "error".into(),
            message: "recoverable error, retrying".into(),
            source: None,
            pid: None,
            log_type: None,
            size: None,
            status: None,
            href: None,
            metadata: None,
        };
        assert!(anomaly(&e, &TestArgs::default()).is_none());
    }

    #[test]
    fn severe_priority_is_anomalous() {
        let e = LogEntry {
            id: "1".into(),
            timestamp: "t".into(),
            priority: "critical".into(),
            message: "voltage out of range".into(),
            source: None,
            pid: None,
            log_type: None,
            size: None,
            status: None,
            href: None,
            metadata: None,
        };
        assert!(anomaly(&e, &TestArgs::default()).is_some());
    }

    #[test]
    fn custom_grep_marks_anomalous() {
        let e = LogEntry {
            id: "1".into(),
            timestamp: "t".into(),
            priority: "info".into(),
            message: "watchdog tripped on channel 2".into(),
            source: None,
            pid: None,
            log_type: None,
            size: None,
            status: None,
            href: None,
            metadata: None,
        };
        let args = TestArgs {
            grep: vec!["watchdog tripped".into()],
            ..Default::default()
        };
        assert!(anomaly(&e, &args).is_some());
        assert!(anomaly(&e, &TestArgs::default()).is_none());
    }
}
