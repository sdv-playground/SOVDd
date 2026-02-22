//! Flash client implementation with async two-phase transfer support

use std::time::Duration;

use reqwest::{Client, StatusCode};
use tracing::{debug, info, instrument};
use url::Url;

use super::config::FlashConfig;
use super::types::*;

/// Flash client for OTA operations
///
/// Supports async two-phase transfers:
/// 1. Upload: Package upload to server with progress tracking
/// 2. Flash: ECU flash with block-level progress tracking
#[derive(Debug, Clone)]
pub struct FlashClient {
    client: Client,
    base_url: Url,
    config: FlashConfig,
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
}

pub type Result<T> = std::result::Result<T, FlashError>;

impl FlashClient {
    /// Create a new flash client from configuration
    pub fn new(config: FlashConfig) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_millis(config.timeouts.request_ms))
            .connect_timeout(Duration::from_millis(config.timeouts.connect_ms))
            .build()?;

        let base_url = Url::parse(&config.connection.base_url)?;

        info!("Flash client created for {}", base_url);

        Ok(Self {
            client,
            base_url,
            config,
        })
    }

    /// Create a flash client for an SOVD server
    ///
    /// This configures the client to use SOVD-style paths:
    /// - Files: `/vehicle/v1/components/{component_id}/files`
    /// - Flash: `/vehicle/v1/components/{component_id}/flash`
    ///
    /// # Example
    /// ```rust,ignore
    /// let client = FlashClient::for_sovd("http://localhost:18080", "vtx_ecm")?;
    /// ```
    pub fn for_sovd(base_url: &str, component_id: &str) -> Result<Self> {
        let config = FlashConfig::builder(base_url)
            .component_id(component_id)
            .build();
        Self::new(config)
    }

    /// Create a flash client for an SOVD sub-entity (ECU behind a gateway)
    ///
    /// This configures the client to use sub-entity paths (SOVD §6.5):
    /// - Files: `/vehicle/v1/components/{gateway_id}/apps/{app_id}/files`
    /// - Flash: `/vehicle/v1/components/{gateway_id}/apps/{app_id}/flash`
    /// - Reset: `/vehicle/v1/components/{gateway_id}/apps/{app_id}/reset`
    ///
    /// # Example
    /// ```rust,ignore
    /// let client = FlashClient::for_sovd_sub_entity("http://localhost:18080", "uds_gw", "engine_ecu")?;
    /// ```
    pub fn for_sovd_sub_entity(base_url: &str, gateway_id: &str, app_id: &str) -> Result<Self> {
        let config = FlashConfig::builder(base_url)
            .gateway_id(gateway_id)
            .component_id(app_id)
            .build();
        Self::new(config)
    }

    /// Create a flash client from a YAML config file
    pub fn from_yaml_file(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let config =
            FlashConfig::from_yaml_file(path).map_err(|e| FlashError::Parse(e.to_string()))?;
        Self::new(config)
    }

    /// Create a flash client from a YAML string
    pub fn from_yaml(yaml: &str) -> Result<Self> {
        let config = FlashConfig::from_yaml(yaml).map_err(|e| FlashError::Parse(e.to_string()))?;
        Self::new(config)
    }

    /// Fetch configuration from server discovery endpoint
    pub async fn from_discovery(base_url: &str) -> Result<Self> {
        let temp_client = Client::new();
        let discovery_url = format!(
            "{}/.well-known/flash-client",
            base_url.trim_end_matches('/')
        );

        let response = temp_client.get(&discovery_url).send().await?;

        if !response.status().is_success() {
            return Err(FlashError::Server {
                status: response.status().as_u16(),
                message: "Discovery endpoint not available".into(),
            });
        }

        let discovery: DiscoveryResponse = response
            .json()
            .await
            .map_err(|e| FlashError::Parse(e.to_string()))?;

        // Build config from discovery response
        let mut builder = FlashConfig::builder(base_url);

        if let Some(auth) = &discovery.auth {
            if auth.auth_type == "api_key" {
                if let Some(header) = &auth.header {
                    builder = builder.api_key_header(header.clone());
                }
            }
        }

        // Extract paths from discovery
        if let Some(list) = &discovery.endpoints.files.list {
            builder = builder.files_path(list.path.clone());
        }
        if let Some(create) = &discovery.endpoints.flash.create {
            builder = builder.flash_path(create.path.replace("/transfer", ""));
        }

        Self::new(builder.build())
    }

    /// Get the configuration
    pub fn config(&self) -> &FlashConfig {
        &self.config
    }

    // =========================================================================
    // Phase 1: File Upload
    // =========================================================================

    /// List available files on the server
    #[instrument(skip(self))]
    pub async fn list_files(&self) -> Result<FileListResponse> {
        let url = self.build_url(&self.config.files_list_path())?;
        debug!("Listing files from {}", url);

        let response = self.request_get(url).await?;
        self.handle_response(response).await
    }

    /// Upload a file (async - returns immediately with upload_id)
    ///
    /// The upload happens in the background. Poll with `get_upload_status()`
    /// or use `poll_upload_complete()` to wait for completion.
    #[instrument(skip(self, data))]
    pub async fn upload_file(&self, data: &[u8]) -> Result<UploadResponse> {
        self.upload_file_with_name(data, None).await
    }

    /// Upload a file with a filename
    #[instrument(skip(self, data))]
    pub async fn upload_file_with_name(
        &self,
        data: &[u8],
        filename: Option<&str>,
    ) -> Result<UploadResponse> {
        let url = self.build_url(&self.config.files_upload_path())?;
        info!("Uploading {} bytes to {}", data.len(), url);

        let mut request = self
            .client
            .post(url)
            .header("Content-Type", "application/octet-stream")
            .header("Content-Length", data.len());

        if let Some(name) = filename {
            request = request.header("X-Filename", name);
        }

        request = self.add_auth_header(request);

        let response = request.body(data.to_vec()).send().await?;
        self.handle_response(response).await
    }

    /// Get the status of an upload
    #[instrument(skip(self))]
    pub async fn get_upload_status(&self, upload_id: &str) -> Result<FileStatus> {
        let url = self.build_url(&self.config.files_status_path(upload_id))?;
        debug!("Getting upload status from {}", url);

        let response = self.request_get(url).await?;
        self.handle_response(response).await
    }

    /// Delete an uploaded file
    #[instrument(skip(self))]
    pub async fn delete_file(&self, file_id: &str) -> Result<()> {
        let url = self.build_url(&self.config.files_status_path(file_id))?;
        info!("Deleting file {} at {}", file_id, url);

        let mut request = self.client.delete(url);
        request = self.add_auth_header(request);

        let response = request.send().await?;
        if response.status().is_success() {
            Ok(())
        } else {
            let status = response.status();
            let body: serde_json::Value = response.json().await.unwrap_or_default();
            Err(FlashError::Server {
                status: status.as_u16(),
                message: body["message"].as_str().unwrap_or("Delete failed").into(),
            })
        }
    }

    /// Poll until upload completes (or fails)
    #[instrument(skip(self))]
    pub async fn poll_upload_complete(&self, upload_id: &str) -> Result<FileStatus> {
        let poll_interval = Duration::from_millis(self.config.timeouts.flash_poll_ms);
        let timeout = Duration::from_millis(self.config.timeouts.upload_ms);
        let start = std::time::Instant::now();

        loop {
            let status = self.get_upload_status(upload_id).await?;

            if status.state.is_success() {
                info!("Upload {} completed", upload_id);
                return Ok(status);
            }

            if status.state.is_failed() {
                let msg = status
                    .error
                    .map(|e| e.message)
                    .unwrap_or_else(|| "Unknown error".into());
                return Err(FlashError::TransferFailed(msg));
            }

            // Still in progress
            if start.elapsed() > timeout {
                return Err(FlashError::Timeout {
                    operation: "upload".into(),
                });
            }
            if let Some(progress) = &status.progress {
                debug!(
                    "Upload progress: {}/{}",
                    progress.bytes_received,
                    progress.bytes_total.unwrap_or(0)
                );
            }
            tokio::time::sleep(poll_interval).await;
        }
    }

    /// Verify an uploaded file
    #[instrument(skip(self))]
    pub async fn verify_file(&self, file_id: &str) -> Result<VerifyResponse> {
        self.verify_file_with_checksum(file_id, None).await
    }

    /// Verify an uploaded file with expected checksum
    #[instrument(skip(self))]
    pub async fn verify_file_with_checksum(
        &self,
        file_id: &str,
        expected_checksum: Option<&str>,
    ) -> Result<VerifyResponse> {
        let url = self.build_url(&self.config.files_verify_path(file_id))?;
        info!("Verifying file {} at {}", file_id, url);

        // SOVD server doesn't require a body for verify
        // Container-style servers may expect a body with checksum params
        let mut request = if expected_checksum.is_some() {
            let request_body = VerifyRequest {
                expected_checksum: expected_checksum.map(String::from),
                algorithm: "sha256".into(),
            };
            self.client.post(url).json(&request_body)
        } else {
            // Send empty JSON body for compatibility with both server types
            self.client.post(url).json(&serde_json::json!({}))
        };
        request = self.add_auth_header(request);

        let response = request.send().await?;
        let verify_response: VerifyResponse = self.handle_response(response).await?;

        if !verify_response.valid {
            return Err(FlashError::VerificationFailed(
                verify_response
                    .error
                    .unwrap_or_else(|| "Checksum mismatch".into()),
            ));
        }

        Ok(verify_response)
    }

    // =========================================================================
    // Phase 2: Flash Transfer
    // =========================================================================

    /// Start a flash transfer to ECU (async)
    ///
    /// The flash happens in the background. Poll with `get_flash_status()`
    /// or use `poll_flash_complete()` to wait for completion.
    #[instrument(skip(self))]
    pub async fn start_flash(&self, file_id: &str) -> Result<StartFlashResponse> {
        self.start_flash_with_options(file_id, None, None).await
    }

    /// Start a flash transfer with options
    #[instrument(skip(self))]
    pub async fn start_flash_with_options(
        &self,
        file_id: &str,
        memory_address: Option<u32>,
        block_size: Option<usize>,
    ) -> Result<StartFlashResponse> {
        let url = self.build_url(&self.config.flash_transfer_path())?;
        info!("Starting flash for file {} at {}", file_id, url);

        let request_body = StartFlashRequest {
            file_id: file_id.to_string(),
            memory_address,
            block_size,
        };

        let mut request = self.client.post(url).json(&request_body);
        request = self.add_auth_header(request);

        let response = request.send().await?;
        self.handle_response(response).await
    }

    /// List all flash transfers
    #[instrument(skip(self))]
    pub async fn list_transfers(&self) -> Result<TransferListResponse> {
        let url = self.build_url(&self.config.flash_transfer_path())?;
        debug!("Listing transfers from {}", url);

        let response = self.request_get(url).await?;
        self.handle_response(response).await
    }

    /// Get the status of a flash transfer
    #[instrument(skip(self))]
    pub async fn get_flash_status(&self, transfer_id: &str) -> Result<FlashTransferStatus> {
        let url = self.build_url(&self.config.flash_transfer_status_path(transfer_id))?;
        debug!("Getting flash status from {}", url);

        let response = self.request_get(url).await?;
        self.handle_response(response).await
    }

    /// Abort a flash transfer
    #[instrument(skip(self))]
    pub async fn abort_flash(&self, transfer_id: &str) -> Result<()> {
        let url = self.build_url(&self.config.flash_transfer_status_path(transfer_id))?;
        info!("Aborting flash transfer {} at {}", transfer_id, url);

        let mut request = self.client.delete(url);
        request = self.add_auth_header(request);

        let response = request.send().await?;
        if response.status().is_success() {
            Ok(())
        } else {
            let status = response.status();
            let body: serde_json::Value = response.json().await.unwrap_or_default();
            Err(FlashError::Server {
                status: status.as_u16(),
                message: body["message"].as_str().unwrap_or("Abort failed").into(),
            })
        }
    }

    /// Poll until flash completes (or fails)
    ///
    /// Returns when state becomes `Finished`, `AwaitingExit`, or an error state.
    #[instrument(skip(self, progress_callback))]
    pub async fn poll_flash_complete<F>(
        &self,
        transfer_id: &str,
        mut progress_callback: Option<F>,
    ) -> Result<FlashTransferStatus>
    where
        F: FnMut(&FlashProgress),
    {
        let poll_interval = Duration::from_millis(self.config.timeouts.flash_poll_ms);

        loop {
            let status = self.get_flash_status(transfer_id).await?;

            // Call progress callback if provided
            if let (Some(ref mut callback), Some(ref progress)) =
                (&mut progress_callback, &status.progress)
            {
                callback(progress);
            }

            if status.state.is_success() {
                info!(
                    "Flash {} completed (state: {:?})",
                    transfer_id, status.state
                );
                return Ok(status);
            }

            if status.state.is_failed() {
                let msg = status
                    .error
                    .map(|e| e.message)
                    .unwrap_or_else(|| "Unknown error".into());
                return Err(FlashError::TransferFailed(msg));
            }

            // Still in progress
            if let Some(progress) = &status.progress {
                debug!(
                    "Flash progress: {}/{} blocks",
                    progress.blocks_transferred, progress.blocks_total
                );
            }
            tokio::time::sleep(poll_interval).await;
        }
    }

    /// Simple poll without callback
    pub async fn poll_flash_complete_simple(
        &self,
        transfer_id: &str,
    ) -> Result<FlashTransferStatus> {
        self.poll_flash_complete::<fn(&FlashProgress)>(transfer_id, None)
            .await
    }

    // =========================================================================
    // Phase 3: Finalization
    // =========================================================================

    /// Send transfer exit command (UDS 0x37)
    #[instrument(skip(self))]
    pub async fn transfer_exit(&self) -> Result<TransferExitResponse> {
        let url = self.build_url(&self.config.flash_transfer_exit_path())?;
        info!("Sending transfer exit to {}", url);

        let mut request = self.client.put(url);
        request = self.add_auth_header(request);

        let response = request.send().await?;
        self.handle_response(response).await
    }

    /// Reset the ECU (UDS 0x11)
    #[instrument(skip(self))]
    pub async fn ecu_reset(&self) -> Result<ResetResponse> {
        self.ecu_reset_with_type("hard").await
    }

    /// Reset the ECU with specific reset type
    #[instrument(skip(self))]
    pub async fn ecu_reset_with_type(&self, reset_type: &str) -> Result<ResetResponse> {
        let url = self.build_url(&self.config.flash_reset_path())?;
        info!("Resetting ECU ({}) via {}", reset_type, url);

        let request_body = ResetRequest {
            reset_type: reset_type.to_string(),
        };

        let mut request = self.client.post(url).json(&request_body);
        request = self.add_auth_header(request);

        let response = request.send().await?;
        self.handle_response(response).await
    }

    // =========================================================================
    // Phase 4: Commit / Rollback
    // =========================================================================

    /// Commit activated firmware (makes it permanent)
    #[instrument(skip(self))]
    pub async fn commit_flash(&self) -> Result<CommitRollbackResponse> {
        let url = self.build_url(&self.config.flash_commit_path())?;
        info!("Committing firmware at {}", url);

        let mut request = self.client.post(url);
        request = self.add_auth_header(request);

        let response = request.send().await?;
        self.handle_response(response).await
    }

    /// Rollback activated firmware to previous version
    #[instrument(skip(self))]
    pub async fn rollback_flash(&self) -> Result<CommitRollbackResponse> {
        let url = self.build_url(&self.config.flash_rollback_path())?;
        info!("Rolling back firmware at {}", url);

        let mut request = self.client.post(url);
        request = self.add_auth_header(request);

        let response = request.send().await?;
        self.handle_response(response).await
    }

    /// Get firmware activation state
    #[instrument(skip(self))]
    pub async fn get_activation_state(&self) -> Result<ActivationStateResponse> {
        let url = self.build_url(&self.config.flash_activation_path())?;
        debug!("Getting activation state from {}", url);

        let response = self.request_get(url).await?;
        self.handle_response(response).await
    }

    // =========================================================================
    // High-Level Operations
    // =========================================================================

    /// Perform a complete flash update (all phases)
    ///
    /// 1. Upload package
    /// 2. Wait for upload complete
    /// 3. Verify package
    /// 4. Start flash
    /// 5. Wait for flash complete
    /// 6. Transfer exit
    /// 7. ECU reset
    #[instrument(skip(self, package_data, progress_callback))]
    pub async fn flash_update<F>(
        &self,
        package_data: &[u8],
        mut progress_callback: Option<F>,
    ) -> Result<()>
    where
        F: FnMut(FlashUpdatePhase, Option<f64>),
    {
        // Phase 1: Upload
        if let Some(ref mut cb) = progress_callback {
            cb(FlashUpdatePhase::Uploading, Some(0.0));
        }

        let upload = self.upload_file(package_data).await?;
        let upload_status = self.poll_upload_complete(&upload.upload_id).await?;

        let file_id = upload_status
            .file_id
            .ok_or_else(|| FlashError::TransferFailed("No file_id after upload".into()))?;

        if let Some(ref mut cb) = progress_callback {
            cb(FlashUpdatePhase::Uploading, Some(100.0));
        }

        // Phase 2: Verify
        if let Some(ref mut cb) = progress_callback {
            cb(FlashUpdatePhase::Verifying, None);
        }

        self.verify_file(&file_id).await?;

        // Phase 3: Flash
        if let Some(ref mut cb) = progress_callback {
            cb(FlashUpdatePhase::Flashing, Some(0.0));
        }

        let flash = self.start_flash(&file_id).await?;

        // Poll with progress updates
        let flash_progress_cb = progress_callback.as_mut().map(|cb| {
            move |progress: &FlashProgress| {
                let percent = progress.percent.unwrap_or_else(|| {
                    if progress.blocks_total > 0 {
                        (progress.blocks_transferred as f64 / progress.blocks_total as f64) * 100.0
                    } else {
                        0.0
                    }
                });
                cb(FlashUpdatePhase::Flashing, Some(percent));
            }
        });

        self.poll_flash_complete(&flash.transfer_id, flash_progress_cb)
            .await?;

        // Phase 4: Finalize
        if let Some(ref mut cb) = progress_callback {
            cb(FlashUpdatePhase::Finalizing, None);
        }

        self.transfer_exit().await?;
        self.ecu_reset().await?;

        if let Some(ref mut cb) = progress_callback {
            cb(FlashUpdatePhase::Complete, Some(100.0));
        }

        Ok(())
    }

    // =========================================================================
    // Helper Methods
    // =========================================================================

    fn build_url(&self, path: &str) -> Result<Url> {
        self.base_url.join(path).map_err(Into::into)
    }

    fn add_auth_header(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(ref api_key) = self.config.connection.api_key {
            request.header(&self.config.connection.api_key_header, api_key)
        } else {
            request
        }
    }

    async fn request_get(&self, url: Url) -> Result<reqwest::Response> {
        let mut request = self.client.get(url);
        request = self.add_auth_header(request);
        request.send().await.map_err(Into::into)
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

/// Flash update phases for progress reporting
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashUpdatePhase {
    /// Uploading package to server
    Uploading,
    /// Verifying package integrity
    Verifying,
    /// Flashing to ECU
    Flashing,
    /// Finalizing (transfer exit, reset)
    Finalizing,
    /// Complete
    Complete,
}

impl std::fmt::Display for FlashUpdatePhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Uploading => write!(f, "Uploading"),
            Self::Verifying => write!(f, "Verifying"),
            Self::Flashing => write!(f, "Flashing"),
            Self::Finalizing => write!(f, "Finalizing"),
            Self::Complete => write!(f, "Complete"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_creation() {
        let config = FlashConfig::builder("http://localhost:8080").build();
        let client = FlashClient::new(config);
        assert!(client.is_ok());
    }

    #[test]
    fn test_client_from_yaml() {
        let yaml = r#"
connection:
  base_url: "http://localhost:8080"

endpoints:
  files: "/files"
  flash: "/flash"
"#;

        let client = FlashClient::from_yaml(yaml);
        assert!(client.is_ok());
    }
}
