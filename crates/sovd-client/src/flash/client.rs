//! SOVD update client — ISO 17978-3 `/updates` collection.
//!
//! Thin wrapper over the spec-compliant
//! `/vehicle/v1/components/{id}/updates` and `/campaigns` wire
//! (F.D2 + F.D4).  No legacy `/flash` + `/files` semantics, no shape
//! synthesis.  Just the /updates lifecycle:
//!
//! ```text
//! open_update            (POST /updates)
//! upload_part × N        (PUT  /updates/{id}/bulk-data/{part_id})
//! verify                 (POST /executions{verify})
//! finalize               (POST /executions{finalize})
//! ecu_reset              (PUT  /components/{id}/status/restart)
//! commit | rollback      (POST /executions{commit|rollback})
//! ```
//!
//! Each `FlashClient` instance is bound to one component (top-level via
//! `for_sovd`, sub-entity via `for_sovd_sub_entity`) and keeps a single
//! in-flight update_id.  Multiple cloned handles share state.

use std::sync::Arc;
use std::time::Duration;

use reqwest::{Client, StatusCode};
use serde::Deserialize;
use tokio::sync::Mutex;
use tracing::{debug, info, instrument};
use url::Url;

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

/// Reply from `POST /updates/{id}/executions {action}`.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateExecution {
    pub execution_id: String,
    pub update_id: String,
    pub action: String,
    pub status: String, // "completed" | "failed" | "running" | "stopped"
    #[serde(default)]
    pub message: Option<String>,
    pub started_at: String,
    pub completed_at: String,
}

/// Reply from `GET /updates/{id}`.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateStatus {
    pub update_id: String,
    pub state: String,
    #[serde(default)]
    pub parts_uploaded: usize,
    #[serde(default)]
    pub parts: Vec<PartStatusEntry>,
    #[serde(default)]
    pub manifest: Option<serde_json::Value>,
    #[serde(default)]
    pub transfer_id: Option<String>,
    pub href: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PartStatusEntry {
    pub part_id: String,
    pub size: u64,
    pub sha256: String,
    pub href: String,
}

/// Reply from `GET /updates`.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdatesList {
    #[serde(default)]
    pub items: Vec<UpdateSummary>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateSummary {
    pub update_id: String,
    pub state: String,
    #[serde(default)]
    pub href: String,
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
    /// `POST /vehicle/v1/components/{id}/updates`.
    /// Allocates a fresh update_id; subsequent lifecycle calls
    /// operate on it.  Errors if a session is already open — call
    /// `commit` / `rollback` / `abort` first.
    #[instrument(skip(self))]
    pub async fn open_update(&self) -> Result<OpenUpdateResponse> {
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
        let mut req = self.client.post(url).json(&serde_json::json!({}));
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
        let url = self.build_url(&self.config.updates_part_path(&update_id, part_id))?;
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

    /// `POST /executions {action: "verify"}`.  Server-side: runs
    /// `verify_package` per part, opens the backend flash session,
    /// and waits for it to settle.
    #[instrument(skip(self))]
    pub async fn verify(&self) -> Result<UpdateExecution> {
        self.run_execution("verify").await
    }

    /// `POST /executions {action: "finalize"}`.  Server-side:
    /// `finalize_flash` + `validate` + `activate`.  Requires
    /// `verify` to have succeeded first.
    #[instrument(skip(self))]
    pub async fn finalize(&self) -> Result<UpdateExecution> {
        self.run_execution("finalize").await
    }

    /// `POST /executions {action: "commit"}`.  Clears the local
    /// session id on success.
    #[instrument(skip(self))]
    pub async fn commit(&self) -> Result<UpdateExecution> {
        let exec = self.run_execution("commit").await?;
        *self.update_id.lock().await = None;
        Ok(exec)
    }

    /// `POST /executions {action: "rollback"}`.  Clears the local
    /// session id on success.
    #[instrument(skip(self))]
    pub async fn rollback(&self) -> Result<UpdateExecution> {
        let exec = self.run_execution("rollback").await?;
        *self.update_id.lock().await = None;
        Ok(exec)
    }

    /// `POST /executions {action: "abort"}`.  Clears the local
    /// session id; idempotent if no session is open.
    #[instrument(skip(self))]
    pub async fn abort(&self) -> Result<UpdateExecution> {
        let exec = self.run_execution("abort").await?;
        *self.update_id.lock().await = None;
        Ok(exec)
    }

    /// Attach this client to the most-recent /updates entry on the
    /// server.  Used by post-reset callers (orchestrators) that
    /// construct a fresh FlashClient after the device reboots — the
    /// in-process `update_id` is gone but the server-side entry
    /// survives across the reset.
    #[instrument(skip(self))]
    pub async fn attach_to_latest(&self) -> Result<String> {
        let list = self.list_updates().await?;
        let summary = list
            .items
            .into_iter()
            .last()
            .ok_or(FlashError::NoSession)?;
        *self.update_id.lock().await = Some(summary.update_id.clone());
        Ok(summary.update_id)
    }

    /// `GET /updates/{id}`.
    #[instrument(skip(self))]
    pub async fn status(&self) -> Result<UpdateStatus> {
        let update_id = self
            .current_update_id()
            .await
            .ok_or(FlashError::NoSession)?;
        let url = self.build_url(&self.config.updates_status_path(&update_id))?;
        let resp = self.request_get(url).await?;
        self.handle_response(resp).await
    }

    /// `GET /updates` — list all /updates entries on this component.
    /// Used by post-reset callers that don't carry the original
    /// FlashClient instance and need to rediscover the latest update_id.
    #[instrument(skip(self))]
    pub async fn list_updates(&self) -> Result<UpdatesList> {
        let url = self.build_url(&self.config.updates_collection_path())?;
        let resp = self.request_get(url).await?;
        self.handle_response(resp).await
    }

    /// `GET /updates/{id}` for the *last* update on this component,
    /// without requiring a locally-held update_id. Useful after an ECU
    /// reset where the original FlashClient has gone out of scope but
    /// the server-side entry still carries the post-finalize state.
    #[instrument(skip(self))]
    pub async fn latest_status(&self) -> Result<UpdateStatus> {
        let list = self.list_updates().await?;
        let summary = list.items.into_iter().last().ok_or(FlashError::NoSession)?;
        let url = self.build_url(&self.config.updates_status_path(&summary.update_id))?;
        let resp = self.request_get(url).await?;
        self.handle_response(resp).await
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
    /// One-shot single-part flash + reset + commit.  Useful for
    /// simple binary-blob flashes (sovd-cli).  Multi-part flows
    /// (manifest + payloads) compose the primitives directly.
    #[instrument(skip(self, data, progress))]
    pub async fn flash_update<F>(
        &self,
        part_id: &str,
        data: &[u8],
        reset_type: &str,
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
        self.verify().await?;

        if let Some(ref mut p) = progress {
            p(FlashUpdatePhase::Finalizing);
        }
        self.finalize().await?;

        if let Some(ref mut p) = progress {
            p(FlashUpdatePhase::Resetting);
        }
        self.ecu_reset(reset_type).await?;

        if let Some(ref mut p) = progress {
            p(FlashUpdatePhase::Committing);
        }
        self.commit().await?;

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

    async fn run_execution(&self, action: &str) -> Result<UpdateExecution> {
        let update_id = self
            .current_update_id()
            .await
            .ok_or(FlashError::NoSession)?;
        let url = self.build_url(&self.config.updates_executions_path(&update_id))?;
        debug!("POST {action} at {url}");
        let body = serde_json::json!({ "action": action });
        let mut req = self
            .client
            .post(url)
            .json(&body)
            .timeout(Duration::from_millis(self.config.timeouts.execution_ms));
        req = self.add_auth(req);
        let resp = req.send().await?;
        let exec: UpdateExecution = self.handle_response(resp).await?;
        if exec.status != "completed" {
            return Err(FlashError::TransferFailed(format!(
                "/executions{{{}}} status={}: {}",
                action,
                exec.status,
                exec.message.clone().unwrap_or_default()
            )));
        }
        Ok(exec)
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
