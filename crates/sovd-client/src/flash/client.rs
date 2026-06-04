//! SOVD update client — ISO 17978-3 §7.18 `/updates` lifecycle.
//!
//! Thin wrapper over the spec-compliant
//! `/vehicle/v1/components/{id}/updates` wire.  Lifecycle:
//!
//! ```text
//! open_update                                 (POST /updates)
//! upload_part × N                             (PUT  /updates/{id}/bulk-data/{part_id})
//! prepare                                     (PUT  /updates/{id}/prepare)  — async 202+poll
//! execute(orchestrated: bool)                 (PUT  /updates/{id}/execute)  — async 202+poll
//! ecu_reset                                   (PUT  /components/{id}/status/restart)
//! spec_commit | spec_rollback                 (PUT  /updates/{id}/x-sumo-{commit|rollback})
//! force_rollback (trial-recovery)             (PUT  /components/{id}/x-sumo-force-rollback)
//! automated  (server-driven prepare+execute)  (PUT  /updates/{id}/automated)
//! ```
//!
//! Each `FlashClient` instance is bound to one component (top-level via
//! `for_sovd`, sub-entity via `for_sovd_sub_entity`) and keeps a single
//! in-flight update_id.  Multiple cloned handles share state.
//!
//! Post-reset callers (orchestrators) that lose the in-process state but
//! must drive an already-registered update re-bind with [`attach`] using
//! the stable update_id they captured at registration.
//!
//! [`attach`]: FlashClient::attach

use std::sync::Arc;
use std::time::Duration;

use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use tokio::sync::Mutex;
use tracing::{info, instrument};
use url::Url;

/// Characters that must be percent-encoded inside a URL path segment.
/// Matches RFC 3986 §3.3 `pchar` exclusions plus `/`, `?`, `#` which
/// are reserved as path/query/fragment delimiters.  SUIT component
/// URIs frequently start with `#` (fragment-style identifiers like
/// `#kernel`) — without encoding, `Url::join` chops the path at the
/// `#` and the server sees an empty part_id.
const PART_SEGMENT_ENCODE: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`')
    .add(b'{')
    .add(b'}')
    .add(b'/')
    .add(b'%');

use super::config::FlashConfig;
use super::types::*;

/// SOVD update client.
#[derive(Debug, Clone)]
pub struct FlashClient {
    client: Client,
    base_url: Url,
    config: FlashConfig,
    /// In-flight `/updates` session id.  Allocated by `open_update`;
    /// cleared on `commit`/`rollback`/`abort` so a new cycle can
    /// open a fresh session through the same client handle.
    update_id: Arc<Mutex<Option<String>>>,
}

/// Flash client errors
#[derive(Debug, thiserror::Error)]
pub enum FlashError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("URL parse error: {0}")]
    Url(#[from] url::ParseError),

    #[error("Server error ({status}): {message}")]
    Server { status: u16, message: String },

    #[error("Transfer failed: {0}")]
    TransferFailed(String),

    #[error("Verification failed: {0}")]
    VerificationFailed(String),

    #[error("Timeout waiting for {operation}")]
    Timeout { operation: String },

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Parse error: {0}")]
    Parse(String),

    #[error("No /updates session open — call open_update first")]
    NoSession,
}

pub type Result<T> = std::result::Result<T, FlashError>;

// ---------------------------------------------------------------------------
// Public wire shapes (matching the /updates server responses).
// ---------------------------------------------------------------------------

/// Reply from `POST /updates`.
#[derive(Debug, Clone, Deserialize)]
pub struct OpenUpdateResponse {
    pub update_id: String,
    pub href: String,
    pub bulk_data_href: String,
    pub executions_href: String,
}

/// Reply from `PUT /updates/{id}/bulk-data/{part_id}`.
#[derive(Debug, Clone, Deserialize)]
pub struct PartUploadResponse {
    pub part_id: String,
    pub size: u64,
    pub sha256: String,
    pub href: String,
}

/// ISO 17978-3 §7.18.7 Table 270 — body of `GET /updates/{id}/status`.
/// Returned by the spec verbs (`prepare` / `execute` / `automated`).
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateStatusBody {
    pub phase: String,
    pub status: String,
    #[serde(default)]
    pub progress: Option<u8>,
    #[serde(default)]
    pub step: Option<String>,
    #[serde(default)]
    pub error: Option<UpdateStatusError>,
    /// Vendor extension (Phase B): present when execute is running
    /// in orchestrated mode.  Values: `awaiting-verdict`, `committing`,
    /// `rolling-back`.
    #[serde(default, rename = "x-sumo-substate")]
    pub substate: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateStatusError {
    pub error_code: String,
    pub message: String,
    #[serde(default)]
    pub parameters: Option<serde_json::Value>,
}

impl UpdateStatusBody {
    pub fn is_terminal(&self) -> bool {
        matches!(self.status.as_str(), "completed" | "failed")
    }
    pub fn is_awaiting_verdict(&self) -> bool {
        self.substate.as_deref() == Some("awaiting-verdict")
    }
}

/// Reply from `GET /updates` — ISO 17978-3 Table 257: a bare list of
/// package-id strings (origin-filtered server-side).
#[derive(Debug, Clone, Deserialize)]
pub struct UpdatesList {
    #[serde(default)]
    pub items: Vec<String>,
}

// ---------------------------------------------------------------------------
// Constructors
// ---------------------------------------------------------------------------

impl FlashClient {
    pub fn new(config: FlashConfig) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_millis(config.timeouts.request_ms))
            .connect_timeout(Duration::from_millis(config.timeouts.connect_ms))
            .build()?;
        let base_url = Url::parse(&config.connection.base_url)?;
        info!("flash client created for {}", base_url);
        Ok(Self {
            client,
            base_url,
            config,
            update_id: Arc::new(Mutex::new(None)),
        })
    }

    pub fn for_sovd(base_url: &str, component_id: &str) -> Result<Self> {
        Self::new(
            FlashConfig::builder(base_url)
                .component_id(component_id)
                .build(),
        )
    }

    pub fn for_sovd_sub_entity(base_url: &str, gateway_id: &str, app_id: &str) -> Result<Self> {
        Self::new(
            FlashConfig::builder(base_url)
                .gateway_id(gateway_id)
                .component_id(app_id)
                .build(),
        )
    }

    pub fn from_yaml_file(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let config =
            FlashConfig::from_yaml_file(path).map_err(|e| FlashError::Parse(e.to_string()))?;
        Self::new(config)
    }

    pub fn from_yaml(yaml: &str) -> Result<Self> {
        let config = FlashConfig::from_yaml(yaml).map_err(|e| FlashError::Parse(e.to_string()))?;
        Self::new(config)
    }

    pub async fn from_discovery(base_url: &str) -> Result<Self> {
        let temp = Client::new();
        let url = format!(
            "{}/.well-known/flash-client",
            base_url.trim_end_matches('/')
        );
        let r = temp.get(&url).send().await?;
        if !r.status().is_success() {
            return Err(FlashError::Server {
                status: r.status().as_u16(),
                message: "Discovery endpoint not available".into(),
            });
        }
        let d: DiscoveryResponse = r
            .json()
            .await
            .map_err(|e| FlashError::Parse(e.to_string()))?;
        let mut b = FlashConfig::builder(base_url);
        if let Some(auth) = &d.auth {
            if auth.auth_type == "api_key" {
                if let Some(h) = &auth.header {
                    b = b.api_key_header(h.clone());
                }
            }
        }
        Self::new(b.build())
    }

    pub fn config(&self) -> &FlashConfig {
        &self.config
    }

    /// The currently-open update_id, if any.
    pub async fn current_update_id(&self) -> Option<String> {
        self.update_id.lock().await.clone()
    }
}

// ---------------------------------------------------------------------------
// /updates lifecycle
// ---------------------------------------------------------------------------

impl FlashClient {
    /// `POST /vehicle/v1/components/{id}/updates` with an empty body —
    /// the server mints a fresh update_id.  Subsequent lifecycle calls
    /// operate on it.  Errors if a session is already open — call
    /// `commit` / `rollback` / `abort` first.
    #[instrument(skip(self))]
    pub async fn open_update(&self) -> Result<OpenUpdateResponse> {
        self.post_open(&serde_json::json!({})).await
    }

    /// `POST /vehicle/v1/components/{id}/updates` with `{"id": id}` —
    /// registers the update under a caller-chosen stable catalog key
    /// instead of a server-minted UUID.  Returns the registered id (the
    /// server echoes it back).  Same single-session guard as
    /// [`open_update`](Self::open_update).
    #[instrument(skip(self))]
    pub async fn open_update_with_id(&self, id: &str) -> Result<String> {
        let body = self.post_open(&serde_json::json!({ "id": id })).await?;
        Ok(body.update_id)
    }

    /// Shared `POST /updates` issuance: enforces the single-session
    /// guard, posts `body`, and latches the returned update_id.
    async fn post_open(&self, body: &serde_json::Value) -> Result<OpenUpdateResponse> {
        {
            let g = self.update_id.lock().await;
            if let Some(id) = &*g {
                return Err(FlashError::Server {
                    status: 409,
                    message: format!("update session {id} already open"),
                });
            }
        }
        let url = self.build_url(&self.config.updates_collection_path())?;
        let mut req = self.client.post(url).json(body);
        req = self.add_auth(req);
        let resp = req.send().await?;
        let body: OpenUpdateResponse = self.handle_response(resp).await?;
        *self.update_id.lock().await = Some(body.update_id.clone());
        Ok(body)
    }

    /// `PUT /vehicle/v1/components/{id}/updates/{update_id}/bulk-data/{part_id}`.
    /// Streams `data` into the named part.  Lazily opens an update
    /// session if none is currently open.
    #[instrument(skip(self, data))]
    pub async fn upload_part(&self, part_id: &str, data: &[u8]) -> Result<PartUploadResponse> {
        let update_id = self.ensure_session().await?;
        let encoded_part = utf8_percent_encode(part_id, PART_SEGMENT_ENCODE).to_string();
        let url = self.build_url(&self.config.updates_part_path(&update_id, &encoded_part))?;
        let bytes = data.len();
        info!("PUT {} ({bytes} bytes)", url);
        let started = std::time::Instant::now();
        let mut req = self
            .client
            .put(url)
            .header("content-type", "application/octet-stream")
            .header("content-length", bytes);
        req = self.add_auth(req);
        let resp = req
            .timeout(Duration::from_millis(self.config.timeouts.upload_ms))
            .body(data.to_vec())
            .send()
            .await?;
        let body: PartUploadResponse = self.handle_response(resp).await?;
        let elapsed = started.elapsed();
        let mb = bytes as f64 / 1_048_576.0;
        let secs = elapsed.as_secs_f64();
        let mb_per_sec = if secs > 0.0 { mb / secs } else { 0.0 };
        info!(
            bytes,
            elapsed_ms = elapsed.as_millis() as u64,
            "part {part_id} uploaded: {:.2} MB at {:.2} MB/s",
            mb,
            mb_per_sec
        );
        Ok(body)
    }

    /// `POST /executions {action: "verify"}`.  Legacy
    /// vendor-extension wire.
    ///
    /// **Deprecated:** use [`prepare`](Self::prepare) — the spec
    /// verb (ISO 17978-3 §7.18.5) is async (202+poll) and uses
    /// `PUT /components/{id}/x-sumo-force-rollback` — trial-recovery
    /// vendor verb.  Unconditionally calls `backend.rollback_flash`,
    /// regardless of whether any execute task is paused or any
    /// `/updates` entry exists.  Used by orchestrators to unstick a
    /// previous flash that left the backend in trial state across
    /// process restart / abandoned session.  Idempotent; returns 204.
    ///
    /// Doesn't need an open session_id (the trial that needs clearing
    /// by definition isn't tracked by an in-flight FlashClient
    /// session).
    #[instrument(skip(self))]
    pub async fn force_rollback(&self) -> Result<()> {
        let url = self.build_url(&self.config.x_sumo_force_rollback_path())?;
        let mut req = self.client.put(url);
        req = self.add_auth(req);
        let resp = req.send().await?;
        if resp.status() != StatusCode::NO_CONTENT {
            return Err(FlashError::Server {
                status: resp.status().as_u16(),
                message: resp.text().await.unwrap_or_default(),
            });
        }
        Ok(())
    }

    /// Bind this client to a known `update_id`.  Local and infallible —
    /// just latches the held id so the lifecycle/status methods address
    /// the right `/updates` entry.
    ///
    /// Used by post-reset callers (orchestrators) that construct a fresh
    /// FlashClient after the device reboots: the in-process `update_id`
    /// is gone, but the server-side entry survives the reset and the
    /// caller still holds the stable id it captured at registration.
    #[instrument(skip(self))]
    pub async fn attach(&self, update_id: &str) -> Result<()> {
        *self.update_id.lock().await = Some(update_id.to_string());
        Ok(())
    }

    /// `GET /updates` — ISO 17978-3 Table 257 catalog: the list of
    /// package-id strings on this component (origin-filtered server-side).
    /// Retained for diagnostics / recovery; lifecycle callers address a
    /// specific id via [`attach`](Self::attach).
    #[instrument(skip(self))]
    pub async fn list_updates(&self) -> Result<Vec<String>> {
        let url = self.build_url(&self.config.updates_collection_path())?;
        let resp = self.request_get(url).await?;
        let list: UpdatesList = self.handle_response(resp).await?;
        Ok(list.items)
    }

    /// Reset the ECU (PUT `status/restart`) — unchanged by the
    /// /updates migration since it lives at the entity root, not
    /// under /updates.
    #[instrument(skip(self))]
    pub async fn ecu_reset(&self, reset_type: &str) -> Result<ResetResponse> {
        let url = self.build_url(&self.config.flash_status_restart_path())?;
        let mut req = self.client.put(url).json(&ResetRequest {
            reset_type: reset_type.to_string(),
        });
        req = self.add_auth(req);
        let resp = req.send().await?;
        self.handle_response(resp).await
    }
}

// ---------------------------------------------------------------------------
// ISO 17978-3 §7.18 spec lifecycle — PUT prepare / execute / automated +
// GET /status.  Async on the wire (PUT returns 202; client polls /status
// until terminal).  Replaces the F.D8b /executions{action} verb-bag.
// See `tasks/spec-aligned-updates-wire.md` UPDATE-WIRE-001.
// ---------------------------------------------------------------------------

impl FlashClient {
    /// `GET /vehicle/v1/components/{id}/updates/{update_id}/status` — returns
    /// the ISO 17978-3 §7.18.7 Table 270 `UpdateStatusBody`
    /// (`{phase, status, progress?, step?, error?, x-sumo-substate?}`).
    /// This is the lifecycle-state source of truth; `GET /updates/{id}`
    /// (Table 261) is the package *catalog* descriptor, not state.
    #[instrument(skip(self))]
    pub async fn spec_status(&self) -> Result<UpdateStatusBody> {
        let update_id = self
            .current_update_id()
            .await
            .ok_or(FlashError::NoSession)?;
        let url = self.build_url(&self.config.updates_spec_status_path(&update_id))?;
        let resp = self.request_get(url).await?;
        self.handle_response(resp).await
    }

    /// `PUT /vehicle/v1/components/{id}/updates/{update_id}/prepare`.
    ///
    /// Issues the async PUT (server returns 202 + `Location: .../status`),
    /// then polls `/status` until `phase=prepare, status ∈ {completed,
    /// failed}` or the prepare budget elapses.  Returns the final
    /// `UpdateStatusBody`.
    #[instrument(skip(self))]
    pub async fn prepare(&self) -> Result<UpdateStatusBody> {
        let update_id = self
            .current_update_id()
            .await
            .ok_or(FlashError::NoSession)?;
        let url = self.build_url(&self.config.updates_prepare_path(&update_id))?;
        let mut req = self.client.put(url);
        req = self.add_auth(req);
        let resp = req.send().await?;
        if resp.status() != StatusCode::ACCEPTED {
            return Err(FlashError::Server {
                status: resp.status().as_u16(),
                message: resp.text().await.unwrap_or_default(),
            });
        }
        self.poll_status_until(
            "prepare",
            Duration::from_millis(self.config.timeouts.prepare_budget_ms),
        )
        .await
    }

    /// `PUT /vehicle/v1/components/{id}/updates/{update_id}/execute`.
    ///
    /// When `orchestrated == true`, sends
    /// `?x-sumo-control=orchestrated` and returns once the entry hits
    /// `substate=awaiting-verdict` — the caller is expected to follow
    /// up with [`commit`](Self::commit) or [`rollback`](Self::rollback)
    /// (Phase B).  When `false`, polls until the standard terminal
    /// (`status=completed|failed`).
    #[instrument(skip(self))]
    pub async fn execute(&self, orchestrated: bool) -> Result<UpdateStatusBody> {
        let update_id = self
            .current_update_id()
            .await
            .ok_or(FlashError::NoSession)?;
        let mut url = self.build_url(&self.config.updates_execute_path(&update_id))?;
        if orchestrated {
            url.query_pairs_mut()
                .append_pair("x-sumo-control", "orchestrated");
        }
        let mut req = self.client.put(url);
        req = self.add_auth(req);
        let resp = req.send().await?;
        if resp.status() != StatusCode::ACCEPTED {
            return Err(FlashError::Server {
                status: resp.status().as_u16(),
                message: resp.text().await.unwrap_or_default(),
            });
        }
        let budget = Duration::from_millis(self.config.timeouts.execute_budget_ms);
        if orchestrated {
            self.poll_status_until_awaiting_verdict(budget).await
        } else {
            self.poll_status_until("execute", budget).await
        }
    }

    /// `PUT /vehicle/v1/components/{id}/updates/{update_id}/automated`.
    /// Server-driven prepare → execute chain.  Polls until terminal.
    #[instrument(skip(self))]
    pub async fn automated(&self) -> Result<UpdateStatusBody> {
        let update_id = self
            .current_update_id()
            .await
            .ok_or(FlashError::NoSession)?;
        let url = self.build_url(&self.config.updates_automated_path(&update_id))?;
        let mut req = self.client.put(url);
        req = self.add_auth(req);
        let resp = req.send().await?;
        if resp.status() != StatusCode::ACCEPTED {
            return Err(FlashError::Server {
                status: resp.status().as_u16(),
                message: resp.text().await.unwrap_or_default(),
            });
        }
        let budget = Duration::from_millis(
            self.config.timeouts.prepare_budget_ms + self.config.timeouts.execute_budget_ms,
        );
        // We don't filter on phase here — automated runs both, terminal
        // status is what counts.
        self.poll_status_until("execute", budget).await
    }

    /// `PUT /updates/{update_id}/x-sumo-commit` — Phase B vendor verb.
    /// Posts the `Commit` verdict, then polls until terminal.
    #[instrument(skip(self))]
    pub async fn spec_commit(&self) -> Result<UpdateStatusBody> {
        self.post_verdict_and_wait("x-sumo-commit").await
    }

    /// `PUT /updates/{update_id}/x-sumo-rollback` — Phase B vendor verb.
    /// Posts the `Rollback` verdict, then polls until terminal.
    #[instrument(skip(self))]
    pub async fn spec_rollback(&self) -> Result<UpdateStatusBody> {
        self.post_verdict_and_wait("x-sumo-rollback").await
    }

    async fn post_verdict_and_wait(&self, verb: &str) -> Result<UpdateStatusBody> {
        let update_id = self
            .current_update_id()
            .await
            .ok_or(FlashError::NoSession)?;
        let path = match verb {
            "x-sumo-commit" => self.config.updates_x_sumo_commit_path(&update_id),
            "x-sumo-rollback" => self.config.updates_x_sumo_rollback_path(&update_id),
            _ => unreachable!("post_verdict_and_wait called with non-verdict verb"),
        };
        let url = self.build_url(&path)?;
        let mut req = self.client.put(url);
        req = self.add_auth(req);
        let resp = req.send().await?;
        if resp.status() != StatusCode::ACCEPTED {
            return Err(FlashError::Server {
                status: resp.status().as_u16(),
                message: resp.text().await.unwrap_or_default(),
            });
        }
        let final_status = self
            .poll_status_until(
                "execute",
                Duration::from_millis(self.config.timeouts.execute_budget_ms),
            )
            .await?;
        // The verdict landed; clear our in-process session id so a
        // subsequent open_update can allocate a fresh one.
        *self.update_id.lock().await = None;
        Ok(final_status)
    }

    /// Poll `GET /status` until the body's `(phase, status)` matches
    /// `(expected_phase, terminal)` or `budget` elapses.  Returns the
    /// final status body.
    async fn poll_status_until(
        &self,
        expected_phase: &str,
        budget: Duration,
    ) -> Result<UpdateStatusBody> {
        let interval = Duration::from_millis(self.config.timeouts.flash_poll_ms);
        let deadline = std::time::Instant::now() + budget;
        loop {
            let body = self.spec_status().await?;
            if body.phase == expected_phase && body.is_terminal() {
                return Ok(body);
            }
            if std::time::Instant::now() > deadline {
                return Err(FlashError::Timeout {
                    operation: format!(
                        "{} phase: still {}/{} after {:?}",
                        expected_phase, body.phase, body.status, budget
                    ),
                });
            }
            tokio::time::sleep(interval).await;
        }
    }

    /// Orchestrated-mode helper: poll until execute is paused at
    /// `awaiting-verdict` (or terminal, if the server rejected the
    /// flow before getting there).
    async fn poll_status_until_awaiting_verdict(
        &self,
        budget: Duration,
    ) -> Result<UpdateStatusBody> {
        let interval = Duration::from_millis(self.config.timeouts.flash_poll_ms);
        let deadline = std::time::Instant::now() + budget;
        loop {
            let body = self.spec_status().await?;
            if body.phase == "execute" && (body.is_awaiting_verdict() || body.is_terminal()) {
                return Ok(body);
            }
            if std::time::Instant::now() > deadline {
                return Err(FlashError::Timeout {
                    operation: format!(
                        "execute orchestrated: still {}/{} substate={:?} after {:?}",
                        body.phase, body.status, body.substate, budget
                    ),
                });
            }
            tokio::time::sleep(interval).await;
        }
    }
}

// ---------------------------------------------------------------------------
// High-level helper: one-shot flash through the full lifecycle.
// ---------------------------------------------------------------------------

/// Phases reported by [`FlashClient::flash_update`] for progress UX.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashUpdatePhase {
    Uploading,
    Verifying,
    Finalizing,
    Resetting,
    Committing,
    Complete,
}

impl std::fmt::Display for FlashUpdatePhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Uploading => write!(f, "Uploading"),
            Self::Verifying => write!(f, "Verifying"),
            Self::Finalizing => write!(f, "Finalizing"),
            Self::Resetting => write!(f, "Resetting"),
            Self::Committing => write!(f, "Committing"),
            Self::Complete => write!(f, "Complete"),
        }
    }
}

impl FlashClient {
    /// One-shot single-part flash on the spec wire.  Useful for
    /// simple binary-blob flashes (sovd-cli) where the caller doesn't
    /// want to compose the lifecycle primitives directly.
    ///
    /// Drives the unorchestrated path (singleshot auto-commits;
    /// banked auto-commits via server-side standard flow).  Callers
    /// that need orchestrator-driven commit/rollback over a banked
    /// trial use the typed primitives (open_update + upload_part +
    /// prepare + execute(true) + spec_commit / spec_rollback).
    #[instrument(skip(self, data, progress))]
    pub async fn flash_update<F>(
        &self,
        part_id: &str,
        data: &[u8],
        _reset_type: &str,
        mut progress: Option<F>,
    ) -> Result<()>
    where
        F: FnMut(FlashUpdatePhase),
    {
        if let Some(ref mut p) = progress {
            p(FlashUpdatePhase::Uploading);
        }
        self.open_update().await?;
        self.upload_part(part_id, data).await?;

        if let Some(ref mut p) = progress {
            p(FlashUpdatePhase::Verifying);
        }
        let prepared = self.prepare().await?;
        if prepared.status != "completed" {
            return Err(FlashError::TransferFailed(format!(
                "prepare ended at {}/{}",
                prepared.phase, prepared.status
            )));
        }

        if let Some(ref mut p) = progress {
            p(FlashUpdatePhase::Finalizing);
        }
        let executed = self.execute(false).await?;
        if executed.status != "completed" {
            return Err(FlashError::TransferFailed(format!(
                "execute ended at {}/{}",
                executed.phase, executed.status
            )));
        }

        // execute (unorchestrated) already drove the server-side
        // commit on the standard flow.  No separate reset/commit
        // dance from the client side.
        if let Some(ref mut p) = progress {
            p(FlashUpdatePhase::Complete);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

impl FlashClient {
    async fn ensure_session(&self) -> Result<String> {
        if let Some(id) = self.current_update_id().await {
            return Ok(id);
        }
        let body = self.open_update().await?;
        Ok(body.update_id)
    }

    fn build_url(&self, path: &str) -> Result<Url> {
        self.base_url.join(path).map_err(Into::into)
    }

    fn add_auth(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(ref k) = self.config.connection.api_key {
            request.header(&self.config.connection.api_key_header, k)
        } else {
            request
        }
    }

    async fn request_get(&self, url: Url) -> Result<reqwest::Response> {
        let mut req = self.client.get(url);
        req = self.add_auth(req);
        req.send().await.map_err(Into::into)
    }

    async fn handle_response<T: serde::de::DeserializeOwned>(
        &self,
        response: reqwest::Response,
    ) -> Result<T> {
        let status = response.status();
        if status.is_success() {
            response
                .json()
                .await
                .map_err(|e| FlashError::Parse(e.to_string()))
        } else {
            let message = response
                .text()
                .await
                .unwrap_or_else(|_| format!("HTTP {}", status));
            match status {
                StatusCode::NOT_FOUND => Err(FlashError::NotFound(message)),
                _ => Err(FlashError::Server {
                    status: status.as_u16(),
                    message,
                }),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Off-band: entity-root ECU restart (no /updates context needed)
// ---------------------------------------------------------------------------

/// Issue an ECU-level reset at the SOVD entity root (ISO 17978-3 §7.19).
/// Standalone — does not require a flash client.
pub async fn system_restart(
    server_url: &str,
    gateway_id: Option<&str>,
    reset_type: &str,
) -> Result<()> {
    let base = Url::parse(server_url)?;
    let path = match gateway_id {
        Some(gw) => format!("/vehicle/v1/components/{gw}/status/restart"),
        None => "/vehicle/v1/status/restart".to_string(),
    };
    let url = base.join(&path)?;
    info!("ECU restart at {url} (reset_type={reset_type})");
    let body = ResetRequest {
        reset_type: reset_type.to_string(),
    };
    let resp = Client::new().put(url).json(&body).send().await?;
    let status = resp.status();
    if status.is_success() {
        Ok(())
    } else {
        Err(FlashError::Server {
            status: status.as_u16(),
            message: resp.text().await.unwrap_or_default(),
        })
    }
}
