//! Script models — SOVD §7.15 (ISO 17978-3, UC14 "Manage & execute scripts",
//! "Mainly development/production").
//!
//! We use the `scripts` collection to expose DEVELOPER-REGISTERED TESTS: each
//! registered test is a script resource, a test run is a script execution. The
//! mechanism is framework-agnostic — a script here wraps whatever the developer's
//! own test framework (cargo test / pytest / gtest / a shell script) does. See
//! `tasks/sovd-tests-as-operations-design.md`.

use serde::{Deserialize, Serialize};

/// One registered script (test) exposed on an entity. Metadata only — the
/// command that runs it is a backend/guest-agent internal, never on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptInfo {
    /// Script id, unique within the entity — for tests, `<layer>.<id>`.
    pub id: String,
    /// Human-readable title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Selection tags (e.g. `smoke`, `hsm`) — filterable via the `tags` query.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tags: Vec<String>,
}

/// Lifecycle status of a script (test) execution — §7.15 execution.status.
///
/// A guest test-agent `POST /tests/{id}/run` is SYNCHRONOUS: the run has
/// already finished by the time the backend's `start_script` returns, so an
/// execution is created directly in `Done`. `Running` exists for symmetry with
/// the async 202+poll shape (and for any future backend that runs a test
/// out-of-band), but the guest-proxy path never dwells there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScriptStatus {
    /// The test is still running (async backend); poll again.
    Running,
    /// The test has finished — `verdict` + the output tails are populated.
    Done,
}

/// A finished test's verdict — passed through the developer's framework outcome
/// (verbatim, per the `result` interpreter the entry declared). `Error` is
/// distinct from `Fail`: the agent couldn't run it / read the result (spawn
/// failure, timeout, unparseable output), not a clean test failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScriptVerdict {
    Pass,
    Fail,
    Error,
}

/// One execution of a script (test) — §7.15 execution resource. Mirrors the
/// framework-agnostic result envelope the guest test-agent produces (see
/// `test-agent::RunResult`), plus the SOVD log-cursor BRACKET (`log_from` /
/// `log_to`) the host snapshots around the run so a tester can page exactly the
/// window a run produced (`GET /logs?x-sumo-after=<log_from>`). See
/// `tasks/sovd-tests-as-operations-design.md`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptExecution {
    /// Execution id, unique within the script — the `{exec-id}` path segment.
    pub exec_id: String,
    /// The script (test) this execution belongs to.
    pub script_id: String,
    /// Lifecycle status (`running` / `done`).
    pub status: ScriptStatus,
    /// Terminal verdict (`pass` / `fail` / `error`) — present once `Done`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<ScriptVerdict>,
    /// Process exit code, if the test exited cleanly (`None` = killed / spawn
    /// failed / not yet finished).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Wall-clock run duration in milliseconds (present once `Done`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Human summary — e.g. why it errored (spawn failure, timeout, bad rule).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Bounded tail of the framework's stdout / stderr (the developer's native
    /// output — TAP/JSON/whatever — rides here for a framework-aware consumer).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout_tail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr_tail: Option<String>,
    /// When the run started (RFC 3339).
    pub started: String,
    /// When the run finished (RFC 3339) — present once `Done`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended: Option<String>,
    /// x-sumo log-cursor BRACKET: the component's log tip snapshotted just
    /// BEFORE the run (`log_from`) and just AFTER (`log_to`). A tester pages
    /// `GET /logs?x-sumo-after=<log_from>` to capture exactly this run's window.
    /// `None` means the tip was unavailable for this component's log source
    /// (e.g. a guest journald cursor the agent didn't report) — the bracket is
    /// best-effort, not a hard requirement.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_to: Option<String>,
}
