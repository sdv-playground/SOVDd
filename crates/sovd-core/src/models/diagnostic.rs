//! Diagnostic models — SOVD §7.9 (read-only system introspection).
//!
//! Exposes a guest's READ-ONLY system probes (memory / disk / log usage /
//! processes / boot log …) as a discoverable `system` data group. A
//! machine-manager guest backend proxies its in-guest diag-agent: `list_*`
//! mirrors the agent's `GET /probes`, `read_*` its `GET /probes/{id}`. The
//! mechanism is layer-contributed — a probe is whatever a layer's `diag.d/*.toml`
//! declares. See `tasks/diag-agent-design.md`.
//!
//! These mirror the diag-agent's wire types (ProbeSummary / ProbeResult) — the
//! host must not git-dep the guest tree, so field names are the CONTRACT, kept
//! in lockstep like the log/test mirrors.

use serde::{Deserialize, Serialize};

/// One registered diagnostic probe — metadata only (the gather command is a
/// guest-agent internal, never on the wire), `GET /{entity}/diagnostics`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticInfo {
    /// Probe id, unique within the entity — for guests, `<layer>.<id>`.
    pub id: String,
    /// Human-readable title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Selection tags (e.g. `health`, `storage`) — filterable via `tags`.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tags: Vec<String>,
}

/// A gathered probe's result — `GET /{entity}/diagnostics/{id}`. `ok`
/// distinguishes a clean read from a gather failure (bad descriptor, source
/// unavailable, spawn error) so a consumer tells "0 is the real answer" from
/// "couldn't read it".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticResult {
    /// The probe id this result is for.
    pub id: String,
    /// True if the probe gathered cleanly; false on any failure (see `message`).
    pub ok: bool,
    /// The captured output (text — `/proc/meminfo` lines, `df` table, …), head
    /// bounded by the agent. Empty on failure.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub output: String,
    /// Why it failed — present only when `ok == false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}
