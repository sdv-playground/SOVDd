//! Log entry models (primarily for HPC backends)
//!
//! This module supports both traditional text logs (journald-style) and
//! binary data dumps for the message passing pattern (container → cloud).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A log entry - supports both text logs and binary dumps
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    /// Unique identifier for this entry
    pub id: String,
    /// Timestamp of the log entry
    pub timestamp: DateTime<Utc>,
    /// Log priority/level
    pub priority: LogPriority,
    /// Log message content (for text logs)
    pub message: String,
    /// Source of the log (service name, container, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Process/PID that generated the log
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// Additional fields (journald fields, labels, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<serde_json::Value>,
    /// Log type for categorization (e.g., "engine_dump", "diagnostic", "system")
    #[serde(skip_serializing_if = "Option::is_none", rename = "type")]
    pub log_type: Option<String>,
    /// Size of content in bytes (for binary logs)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Retrieval status (pending, retrieved, processed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<LogStatus>,
    /// URL to download content (for large binary data)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
    /// Additional metadata (trigger, fault codes, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Monotonic runtime at which this entry was logged — seconds since the
    /// producer's boot (CLOCK_MONOTONIC on the host slog2 path,
    /// `__MONOTONIC_TIMESTAMP` on guest journald). Unlike `timestamp` (wall clock,
    /// unreliable on the CVC — boots at epoch, jumps when NTP sets it), this is
    /// jump-proof and is the axis the `x-sumo-runtime` window filters on. `None`
    /// when the source doesn't carry a monotonic clock.
    #[serde(skip_serializing_if = "Option::is_none", rename = "x-sumo-uptime-secs")]
    pub uptime_secs: Option<u64>,
}

/// Status of a log entry for message passing pattern
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogStatus {
    /// Log is available for retrieval
    #[default]
    Pending,
    /// Log content has been downloaded at least once
    Retrieved,
    /// Log has been processed (kept for audit)
    Processed,
}

/// What KIND of log source an entry in the source catalog is — the retrieval
/// model differs by kind, so a client knows how to read it before it asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogSourceKind {
    /// A live line stream you PAGE (a journal): the QNX slog2 ring, journald.
    /// Read via `GET /logs/sources/{name}` (entries + cursor). No file to
    /// download.
    Journal,
    /// A discrete text FILE you download whole via bulk-data
    /// (`GET /bulk-data/logs/{id}`). The `href` on the catalog entry points there.
    File,
    /// A dump artifact (crash dump / trace) — the §7.21 message-passing pattern
    /// (retrieve then acknowledge). Also downloaded, distinguished from `File`
    /// so a client can treat it as a discrete event.
    Dump,
}

/// One entry in a component's log-source CATALOG (`GET /logs/sources`). A source
/// is a thing you ENUMERATE then ADDRESS — not a filter value you must know in
/// advance. Distinct sources are NEVER merged/time-sorted with each other (their
/// clocks are independent — a live journal at real time vs. a boot file stamped
/// 1970), so each is read on its own via `GET /logs/sources/{name}` (journals) or
/// its bulk-data `href` (files/dumps). Vendor extension (`x-sumo`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogSourceInfo {
    /// The source name — the `{name}` path segment. For slog2 this is the fixed
    /// physical source `"slog2"`; for a host file it is the file stem; for a
    /// guest it is the guest's own source label.
    pub name: String,
    /// How to retrieve this source (see [`LogSourceKind`]).
    pub kind: LogSourceKind,
    /// Whether this source supports reboot-safe cursor paging on
    /// `GET /logs/sources/{name}` (`x-sumo-after` + response cursors). `false`
    /// ⇒ a single terminal page (e.g. the slog2 ring, until its segment cursor
    /// lands) — a client must NOT loop expecting a cursor.
    pub cursor: bool,
    /// For a `Journal` source that multiplexes sub-sources (the slog2 ring's
    /// per-buffer emitters: `snova`/`vhsm`/`devb_sdmmc_mx8x`/…), the known emitter
    /// names — narrowable via `x-sumo-emitter[-exclude]`. Empty when the source
    /// has no sub-dimension OR when enumeration is deferred (a client can still
    /// discover emitters from each entry's `fields.emitter`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub emitters: Vec<String>,
}

/// Log priority levels (aligned with syslog priorities)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogPriority {
    /// Emergency: system is unusable
    Emergency = 0,
    /// Alert: action must be taken immediately
    Alert = 1,
    /// Critical: critical conditions
    Critical = 2,
    /// Error: error conditions
    Error = 3,
    /// Warning: warning conditions
    Warning = 4,
    /// Notice: normal but significant condition
    Notice = 5,
    /// Info: informational messages
    #[default]
    Info = 6,
    /// Debug: debug-level messages
    Debug = 7,
}

impl LogPriority {
    /// Convert from syslog priority number
    pub fn from_syslog(priority: u8) -> Self {
        match priority {
            0 => Self::Emergency,
            1 => Self::Alert,
            2 => Self::Critical,
            3 => Self::Error,
            4 => Self::Warning,
            5 => Self::Notice,
            6 => Self::Info,
            _ => Self::Debug,
        }
    }
}

/// Filter for querying logs
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LogFilter {
    /// Filter by priority (this level and above)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<LogPriority>,
    /// Filter by source (service/unit name)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Filter by EMITTER (sub-source): the buffer/daemon name carried in
    /// `LogEntry.fields.emitter`, distinct from `source`. Where one physical
    /// `source` multiplexes many emitters (the slog2 ring: `source="slog2"`,
    /// emitters `snova`/`vhsm`/`devb_sdmmc_mx8x`/…), this narrows to the named
    /// ones — INCLUDE semantics, comma-separated, prefix-matched (so `devb`
    /// selects `devb_sdmmc_mx8x`). A backend whose source has no emitter
    /// dimension ignores it. Vendor extension (wire name `x-sumo-emitter`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emitter: Option<String>,
    /// EXCLUDE emitters: the inverse of `emitter`, applied after it. The device
    /// still SERVES every emitter; this drops the named ones from the response
    /// (comma-separated, prefix-matched). The intended use is muting a
    /// high-volume sub-source — e.g. `emitter_exclude="devb_,CAM"` drops the
    /// eMMC/CAM driver firehose so real records aren't crowded out of a tail.
    /// Vendor extension (wire name `x-sumo-emitter-exclude`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emitter_exclude: Option<String>,
    /// Logs since this time
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<DateTime<Utc>>,
    /// Logs until this time
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<DateTime<Utc>>,
    /// Text pattern to search for
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    /// Maximum number of entries to return
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// Return last N entries (tail)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tail: Option<usize>,
    /// Filter by log type (e.g., "engine_dump", "diagnostic")
    #[serde(skip_serializing_if = "Option::is_none", rename = "type")]
    pub log_type: Option<String>,
    /// Filter by retrieval status (pending, retrieved)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<LogStatus>,
    /// Opaque pagination cursor: return entries STRICTLY AFTER this position.
    /// A backend that supports paging returns [`LogPage::next_cursor`]; the
    /// client feeds it back here to get the next batch, looping until
    /// `next_cursor` is `None`. Clients never parse the token. `None` starts at
    /// the oldest available entry. The cursor encodes the backend's monotonic
    /// ordering key (journald `__CURSOR`, or a host `(boot,gen,offset)`), so it
    /// is reboot-safe where wall-clock time is not — see
    /// `tasks/log-retrieval-design.md`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    /// RUNTIME window in seconds — keep only entries within the last N seconds of
    /// the producer's runtime (monotonic uptime), measured back from the newest
    /// record (the "tip"). Unlike `since`/`until` (absolute wall-clock times, which
    /// the CVC's unreliable clock makes useless — especially offboard, where the
    /// server clock is the workstation's), this is jump-proof and resolved by the
    /// BACKEND against each source's tip uptime (the only party that knows it), not
    /// by wall-clock in the API layer. Wire name `x-sumo-runtime` (a duration like
    /// `3h`/`90s`, parsed to seconds). Backends without a monotonic axis ignore it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_secs: Option<u64>,
}

impl LogFilter {
    /// Decide whether an entry with the given `emitter` passes the
    /// `emitter` (include) / `emitter_exclude` filters. Both are
    /// comma-separated PREFIX lists (so `devb` matches `devb_sdmmc_mx8x`);
    /// include is applied first (empty ⇒ all pass), then exclude removes.
    /// Backends whose source has no emitter dimension needn't call this.
    pub fn emitter_allows(&self, emitter: &str) -> bool {
        let matches_any = |list: &str| {
            list.split(',')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .any(|p| emitter.starts_with(p))
        };
        if let Some(inc) = self.emitter.as_deref() {
            if !inc.trim().is_empty() && !matches_any(inc) {
                return false;
            }
        }
        if let Some(exc) = self.emitter_exclude.as_deref() {
            if matches_any(exc) {
                return false;
            }
        }
        true
    }
}

/// One page of logs plus the cursors that make "get all logs" a terminating
/// loop. Returned by [`crate::DiagnosticBackend::get_logs_paged`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LogPage {
    /// The entries in this page, oldest-first within the page.
    pub items: Vec<LogEntry>,
    /// Feed back as [`LogFilter::after`] for the next batch. `None` means the
    /// caller has reached the head (all currently-available logs consumed) — a
    /// paging loop stops here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// The oldest position the backend can still serve. If a caller's `after`
    /// predates this, history in between was rotated/dropped — the caller can
    /// detect the gap rather than silently missing entries. `None` if unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_cursor: Option<String>,
    /// The cursor at the current HEAD ("now") — a resume point for FOLLOWING:
    /// poll `after = tip_cursor` to get only entries that arrive after this call.
    /// Set even when `next_cursor` is `None` (head reached), so `--since END`
    /// / a follower has a starting handle. `None` when the backend can't name
    /// its tip. Reboot-safe like the other cursors.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tip_cursor: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::LogFilter;

    fn f(inc: Option<&str>, exc: Option<&str>) -> LogFilter {
        LogFilter {
            emitter: inc.map(str::to_string),
            emitter_exclude: exc.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn emitter_allows_default_passes_all() {
        assert!(f(None, None).emitter_allows("devb_sdmmc_mx8x"));
        assert!(f(None, None).emitter_allows("snova"));
    }

    #[test]
    fn emitter_include_is_prefix_matched() {
        let inc = f(Some("snova,vhsm"), None);
        assert!(inc.emitter_allows("snova"));
        assert!(inc.emitter_allows("vhsm"));
        assert!(!inc.emitter_allows("devb_sdmmc_mx8x"));
        // prefix: "devb" selects the pid-suffixed buffer name.
        assert!(f(Some("devb"), None).emitter_allows("devb_sdmmc_mx8x"));
    }

    #[test]
    fn emitter_exclude_drops_the_firehose() {
        let exc = f(None, Some("devb_,CAM"));
        assert!(!exc.emitter_allows("devb_sdmmc_mx8x"));
        assert!(exc.emitter_allows("snova"));
        assert!(exc.emitter_allows("vhsm"));
    }

    #[test]
    fn exclude_wins_over_include() {
        // include narrows, then exclude removes from within it.
        let both = f(Some("devb"), Some("devb_sdmmc"));
        assert!(!both.emitter_allows("devb_sdmmc_mx8x"));
    }

    #[test]
    fn blank_and_whitespace_lists_are_noops() {
        assert!(f(Some(""), None).emitter_allows("anything"));
        assert!(f(Some("  "), None).emitter_allows("anything"));
        assert!(f(None, Some("")).emitter_allows("anything"));
    }

    #[test]
    fn log_source_info_serde_round_trips() {
        use super::{LogSourceInfo, LogSourceKind};

        // A journal source with no emitters: `emitters` must be OMITTED (skip if
        // empty), and `kind` must serialize lowercase.
        let slog2 = LogSourceInfo {
            name: "slog2".into(),
            kind: LogSourceKind::Journal,
            cursor: false,
            emitters: vec![],
        };
        let j = serde_json::to_string(&slog2).unwrap();
        assert!(j.contains("\"kind\":\"journal\""), "{j}");
        assert!(
            !j.contains("emitters"),
            "empty emitters must be skipped: {j}"
        );
        let back: LogSourceInfo = serde_json::from_str(&j).unwrap();
        assert_eq!(back.name, "slog2");
        assert_eq!(back.kind, LogSourceKind::Journal);
        assert!(!back.cursor);
        assert!(back.emitters.is_empty());

        // A file source WITH emitters present round-trips them + the kind.
        let f = LogSourceInfo {
            name: "host-boot".into(),
            kind: LogSourceKind::File,
            cursor: true,
            emitters: vec!["a".into(), "b".into()],
        };
        let j = serde_json::to_string(&f).unwrap();
        assert!(j.contains("\"kind\":\"file\""), "{j}");
        let back: LogSourceInfo = serde_json::from_str(&j).unwrap();
        assert_eq!(back.emitters, vec!["a".to_string(), "b".to_string()]);
        assert!(back.cursor);
        assert_eq!(back.kind, LogSourceKind::File);

        // Absent `emitters` in the wire JSON deserializes to empty (serde default).
        let back: LogSourceInfo =
            serde_json::from_str(r#"{"name":"x","kind":"dump","cursor":false}"#).unwrap();
        assert_eq!(back.kind, LogSourceKind::Dump);
        assert!(back.emitters.is_empty());
    }
}
