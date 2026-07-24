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
