//! SOVD HTTP Client implementation

use std::time::Duration;

use reqwest::{Client, StatusCode};
use tracing::{debug, instrument};
use url::Url;

use crate::error::{Result, SovdClientError};
use crate::types::*;

/// URL-encode a resource ID for use in path segments.
///
/// Gateway-prefixed IDs like `"vtx_ecm/vin"` must be encoded to
/// `"vtx_ecm%2Fvin"` so they form a single path segment rather than
/// being split across two segments by the literal `/`.
fn encode_path_segment(id: &str) -> String {
    id.replace('/', "%2F")
}

/// Convert an `/operations` entry (IO control flavor) into the
/// legacy `OutputInfo` shape used by callers.
fn operation_to_output(op: OperationInfo) -> OutputInfo {
    let href = Some(format!("operations/{}", op.id));
    OutputInfo {
        id: op.id,
        name: Some(op.name),
        description: op.description,
        data_type: op.data_type,
        control_types: op.control_types,
        href,
        current_value: op.current_value,
        default_value: None,
        value: op.value,
        default: op.default,
        allowed: if op.allowed.is_empty() {
            None
        } else {
            Some(
                op.allowed
                    .into_iter()
                    .map(serde_json::Value::String)
                    .collect(),
            )
        },
        controlled_by_tester: op.controlled_by_tester,
        frozen: op.frozen,
        requires_security: Some(op.requires_security),
        security_level: Some(op.security_level),
    }
}

/// Default request timeout
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Default connection timeout
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// SOVD REST API client
///
/// Apply the device-TLS verification mode to a reqwest builder — the single
/// place this crate decides server-cert trust. When `ca_cert_pem` is set, pin
/// that CA root (verify the device leaf chains to it — the tower identity root,
/// for dialling `<id>.local`); otherwise `insecure` chooses skip-verify (raw-IP
/// dialling, the `curl -k` equivalent) vs the system roots.
pub(crate) fn apply_tls(
    builder: reqwest::ClientBuilder,
    insecure: bool,
    ca_cert_pem: Option<&[u8]>,
) -> reqwest::Result<reqwest::ClientBuilder> {
    Ok(match ca_cert_pem {
        Some(pem) => builder.add_root_certificate(reqwest::Certificate::from_pem(pem)?),
        None => builder.danger_accept_invalid_certs(insecure),
    })
}

/// Provides methods to communicate with SOVD-compliant servers.
#[derive(Debug, Clone)]
pub struct SovdClient {
    client: Client,
    base_url: Url,
}

impl SovdClient {
    /// Create a new SOVD client
    ///
    /// # Arguments
    /// * `base_url` - Base URL of the SOVD server (e.g., "http://localhost:9080")
    pub fn new(base_url: &str) -> Result<Self> {
        Self::new_insecure(base_url, false)
    }

    /// Like [`new`](Self::new), but optionally disable TLS certificate
    /// verification — the `curl -k` equivalent. `insecure == false` is the
    /// default and byte-identical to [`new`](Self::new) (full verification);
    /// `insecure == true` accepts an invalid/mismatched server cert, for a dev
    /// device whose leaf SAN won't match the dialled host (e.g. `127.0.0.1`).
    pub fn new_insecure(base_url: &str, insecure: bool) -> Result<Self> {
        Self::with_config_verifying(
            base_url,
            DEFAULT_TIMEOUT,
            DEFAULT_CONNECT_TIMEOUT,
            insecure,
            None,
        )
    }

    /// Like [`new`](Self::new), but pin a CA root (PEM) for verification — dial
    /// `<id>.local` and verify the device leaf chains to the tower identity root.
    /// `ca_cert_pem == None` falls back to the [`new_insecure`](Self::new_insecure)
    /// behaviour (skip-verify when `insecure`, else system roots).
    pub fn new_verifying(
        base_url: &str,
        insecure: bool,
        ca_cert_pem: Option<&[u8]>,
    ) -> Result<Self> {
        Self::with_config_verifying(
            base_url,
            DEFAULT_TIMEOUT,
            DEFAULT_CONNECT_TIMEOUT,
            insecure,
            ca_cert_pem,
        )
    }

    /// Create a new SOVD client with custom configuration
    pub fn with_config(
        base_url: &str,
        timeout: Duration,
        connect_timeout: Duration,
    ) -> Result<Self> {
        Self::with_config_verifying(base_url, timeout, connect_timeout, false, None)
    }

    /// The single non-bearer client-building site. `ca_cert_pem` (when set) pins
    /// that CA root; otherwise `insecure` decides skip-verify vs system roots.
    /// See [`apply_tls`].
    fn with_config_verifying(
        base_url: &str,
        timeout: Duration,
        connect_timeout: Duration,
        insecure: bool,
        ca_cert_pem: Option<&[u8]>,
    ) -> Result<Self> {
        let client = apply_tls(
            Client::builder()
                .timeout(timeout)
                .connect_timeout(connect_timeout),
            insecure,
            ca_cert_pem,
        )?
        .build()?;

        let base_url = Url::parse(base_url)?;

        Ok(Self { client, base_url })
    }

    /// Create a new SOVD client that sends a bearer token with every request.
    ///
    /// The token is set as a default `Authorization: Bearer <token>` header.
    pub fn with_bearer_token(base_url: &str, token: &str) -> Result<Self> {
        Self::with_bearer_token_insecure(base_url, token, false)
    }

    /// Like [`with_bearer_token`](Self::with_bearer_token), with the insecure-TLS
    /// toggle. `insecure == false` ⇒ full verification (byte-identical default).
    pub fn with_bearer_token_insecure(base_url: &str, token: &str, insecure: bool) -> Result<Self> {
        Self::with_bearer_token_verifying(base_url, token, insecure, None)
    }

    /// Like [`with_bearer_token`](Self::with_bearer_token), pinning a CA root
    /// (PEM) for verification — the tower identity root, for dialling
    /// `<id>.local`. `ca_cert_pem == None` falls back to the `insecure` behaviour.
    pub fn with_bearer_token_verifying(
        base_url: &str,
        token: &str,
        insecure: bool,
        ca_cert_pem: Option<&[u8]>,
    ) -> Result<Self> {
        let mut headers = reqwest::header::HeaderMap::new();
        let header_value = reqwest::header::HeaderValue::from_str(&format!("Bearer {}", token))
            .map_err(|e| SovdClientError::ParseError(format!("Invalid auth token: {}", e)))?;
        headers.insert(reqwest::header::AUTHORIZATION, header_value);

        let client = apply_tls(
            Client::builder()
                .timeout(DEFAULT_TIMEOUT)
                .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
                .default_headers(headers),
            insecure,
            ca_cert_pem,
        )?
        .build()?;

        let base_url = Url::parse(base_url)?;

        Ok(Self { client, base_url })
    }

    /// Get the base URL
    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    /// Get a reference to the underlying HTTP client.
    ///
    /// Useful for making custom requests while reusing the client's
    /// connection pool and default headers (e.g., bearer token).
    pub fn http_client(&self) -> &Client {
        &self.client
    }

    // =========================================================================
    // Health Check
    // =========================================================================

    /// Check server health
    #[instrument(skip(self))]
    pub async fn health(&self) -> Result<String> {
        let url = self.base_url.join("/health")?;
        let response = self.client.get(url).send().await?;

        if response.status().is_success() {
            Ok(response.text().await?)
        } else {
            Err(self.extract_error(response).await)
        }
    }

    // =========================================================================
    // Component Operations
    // =========================================================================

    /// List all available components
    #[instrument(skip(self))]
    pub async fn list_components(&self) -> Result<Vec<Component>> {
        let url = self.base_url.join("/vehicle/v1/components")?;
        debug!("Listing components from {}", url);

        let response = self.client.get(url).send().await?;
        self.handle_response::<ComponentList>(response)
            .await
            .map(|r| r.items)
    }

    /// Get information about a specific component
    #[instrument(skip(self))]
    pub async fn get_component(&self, component_id: &str) -> Result<Component> {
        let url = self
            .base_url
            .join(&format!("/vehicle/v1/components/{}", component_id))?;

        let response = self.client.get(url).send().await?;
        self.handle_response(response).await
    }

    /// Read an entity's runtime status (ISO 17978-3 §7.19.2):
    /// `GET /vehicle/v1/components/{id}/status` → `EntityStatusBody` —
    /// `status: ready|notReady` + control links + vendor `x-sumo-*` runtime fields.
    /// An orchestrator reads the vendor boot counter here to verify a reset took
    /// effect (baseline → restart → wait until incremented + `ready`).
    #[instrument(skip(self))]
    pub async fn read_status(&self, component_id: &str) -> Result<sovd_core::EntityStatusBody> {
        let url = self
            .base_url
            .join(&format!("/vehicle/v1/components/{}/status", component_id))?;
        let response = self.client.get(url).send().await?;
        self.handle_response(response).await
    }

    /// Issue an ECU-level reset at the SOVD entity root or a gateway component
    /// (ISO 17978-3 §7.19): `PUT /vehicle/v1/status/restart` (the whole node)
    /// when `gateway_id` is `None`, else
    /// `PUT /vehicle/v1/components/{gateway}/status/restart`. Carries this
    /// client's auth (e.g. a bearer token from [`with_bearer_token`](Self::with_bearer_token)).
    /// The flash engine uses this to coalesce `RequiresEcuReset` components into
    /// a single node reboot.
    #[instrument(skip(self))]
    pub async fn system_restart(&self, gateway_id: Option<&str>, reset_type: &str) -> Result<()> {
        let path = match gateway_id {
            Some(gw) => format!("/vehicle/v1/components/{}/status/restart", gw),
            None => "/vehicle/v1/status/restart".to_string(),
        };
        let url = self.base_url.join(&path)?;
        debug!("ECU restart at {url} (reset_type={reset_type})");
        let response = self
            .client
            .put(url)
            .json(&serde_json::json!({ "reset_type": reset_type }))
            .send()
            .await?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(self.extract_error(response).await)
        }
    }

    // =========================================================================
    // Data/Parameter Operations
    // =========================================================================

    /// List available parameters for a component
    ///
    /// Returns parameters with both semantic IDs (for SOVD-compliant access)
    /// and raw DIDs (for UDS debugging). Use [`find_parameter`] to look up
    /// a specific parameter by semantic name.
    #[instrument(skip(self))]
    pub async fn list_parameters(&self, component_id: &str) -> Result<ParametersResponse> {
        let url = self
            .base_url
            .join(&format!("/vehicle/v1/components/{}/data", component_id))?;

        let response = self.client.get(url).send().await?;
        self.handle_response(response).await
    }

    /// List parameters for a sub-entity (accessed via gateway's apps route)
    ///
    /// Routes through: `GET /vehicle/v1/components/{component_id}/apps/{app_path}/data`
    #[instrument(skip(self))]
    pub async fn list_sub_entity_parameters(
        &self,
        component_id: &str,
        app_path: &str,
    ) -> Result<ParametersResponse> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/apps/{}/data",
            component_id,
            encode_path_segment(app_path)
        ))?;

        let response = self.client.get(url).send().await?;
        self.handle_response(response).await
    }

    /// Read a sub-entity parameter value
    ///
    /// Routes through: `GET /vehicle/v1/components/{component_id}/apps/{app_path}/data/{param_id}`
    #[instrument(skip(self))]
    pub async fn read_sub_entity_data(
        &self,
        component_id: &str,
        app_path: &str,
        param_id: &str,
    ) -> Result<DataResponse> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/apps/{}/data/{}",
            component_id,
            encode_path_segment(app_path),
            encode_path_segment(param_id)
        ))?;

        let response = self.client.get(url).send().await?;
        self.handle_response(response).await
    }

    /// Read a sub-entity parameter value without server-side conversion
    #[instrument(skip(self))]
    pub async fn read_sub_entity_data_raw(
        &self,
        component_id: &str,
        app_path: &str,
        param_id: &str,
    ) -> Result<DataResponse> {
        let mut url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/apps/{}/data/{}",
            component_id,
            encode_path_segment(app_path),
            encode_path_segment(param_id)
        ))?;
        url.set_query(Some("raw=true"));

        let response = self.client.get(url).send().await?;
        self.handle_response(response).await
    }

    /// Find a parameter by semantic ID or DID from a parameters list
    ///
    /// Searches both the `id` field (semantic name) and `did` field (hex).
    pub fn find_parameter<'a>(
        params: &'a ParametersResponse,
        identifier: &str,
    ) -> Option<&'a ParameterInfo> {
        params.items.iter().find(|p| {
            p.id.eq_ignore_ascii_case(identifier) || p.did.eq_ignore_ascii_case(identifier)
        })
    }

    /// Read a single parameter value
    ///
    /// The `param_id` can be either:
    /// - A **semantic name** like `"coolant_temperature"` (SOVD-compliant)
    /// - A **raw DID** in hex format like `"F405"` or `"0xF405"`
    ///
    /// If the server has a DID definition with conversion, the value is
    /// automatically converted (e.g., raw `132` → physical `92` for temp).
    /// Otherwise, raw hex bytes are returned.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// // Read by semantic name (SOVD-compliant)
    /// let temp = client.read_data("engine_ecu", "coolant_temperature").await?;
    ///
    /// // Read by raw DID (fallback for private data)
    /// let temp = client.read_data("engine_ecu", "F405").await?;
    /// ```
    #[instrument(skip(self))]
    pub async fn read_data(&self, component_id: &str, param_id: &str) -> Result<DataResponse> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/data/{}",
            component_id,
            encode_path_segment(param_id)
        ))?;

        let response = self.client.get(url).send().await?;
        self.handle_response(response).await
    }

    /// Read a single parameter value without server-side conversion
    ///
    /// Always returns raw bytes as hex, bypassing any registered DID conversions.
    /// Use this when:
    /// - You need the raw ECU bytes for debugging
    /// - The server's conversion is wrong
    /// - You have private data and will apply client-side conversion
    ///
    /// For client-side conversion of private data, see [`DataResponse::raw_bytes`].
    #[instrument(skip(self))]
    pub async fn read_data_raw(&self, component_id: &str, param_id: &str) -> Result<DataResponse> {
        let mut url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/data/{}",
            component_id,
            encode_path_segment(param_id)
        ))?;
        url.set_query(Some("raw=true"));

        let response = self.client.get(url).send().await?;
        self.handle_response(response).await
    }

    /// Read multiple parameter values
    #[instrument(skip(self))]
    pub async fn read_data_batch(
        &self,
        component_id: &str,
        param_ids: &[&str],
    ) -> Result<Vec<DataResponse>> {
        let params = param_ids.join(",");
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/data?ids={}",
            component_id, params
        ))?;

        let response = self.client.get(url).send().await?;
        self.handle_response::<DataListResponse>(response)
            .await
            .map(|r| r.data)
    }

    /// Write a parameter value
    #[instrument(skip(self, value))]
    pub async fn write_data(
        &self,
        component_id: &str,
        param_id: &str,
        value: serde_json::Value,
    ) -> Result<()> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/data/{}",
            component_id,
            encode_path_segment(param_id)
        ))?;

        let request = WriteDataRequest { value };
        let response = self.client.put(url).json(&request).send().await?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(self.extract_error(response).await)
        }
    }

    /// Write a sub-entity parameter value
    #[instrument(skip(self, value))]
    pub async fn write_sub_entity_data(
        &self,
        component_id: &str,
        app_path: &str,
        param_id: &str,
        value: serde_json::Value,
    ) -> Result<()> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/apps/{}/data/{}",
            component_id,
            encode_path_segment(app_path),
            encode_path_segment(param_id)
        ))?;

        let request = WriteDataRequest { value };
        let response = self.client.put(url).json(&request).send().await?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(self.extract_error(response).await)
        }
    }

    /// Write binary data to a parameter (for message passing pattern)
    ///
    /// Writes raw bytes to the specified parameter. The data is sent as a hex string.
    /// Use this for the inbound message passing pattern (cloud → container).
    ///
    /// # Arguments
    /// * `component_id` - Component ID
    /// * `param_id` - Parameter ID (e.g., "manufacturer_inbox")
    /// * `data` - Raw binary data to write
    #[instrument(skip(self, data))]
    pub async fn write_binary_data(
        &self,
        component_id: &str,
        param_id: &str,
        data: &[u8],
    ) -> Result<()> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/data/{}",
            component_id,
            encode_path_segment(param_id)
        ))?;

        // Send the spec `{value}` body (C-131). The bytes go as a hex
        // string; the server infers the raw-hex encoding from the value
        // shape (no non-spec `format` hint).
        let request = WriteDataRequest {
            value: serde_json::json!(hex::encode(data)),
        };

        let response = self.client.put(url).json(&request).send().await?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(self.extract_error(response).await)
        }
    }

    /// Read raw DID by hex identifier.
    ///
    /// Routes through the standard `data/{param-id}?raw=true` (ISO 17978-3
    /// §7.10). The dedicated `/did/{did}` route was removed in the spec
    /// migration; the DID hex (e.g. "F405") is itself a valid param-id.
    #[instrument(skip(self))]
    pub async fn read_did(&self, component_id: &str, did: u16) -> Result<DataResponse> {
        self.read_data_raw(component_id, &format!("{:04X}", did))
            .await
    }

    /// Write raw DID by hex identifier.
    ///
    /// Routes through the standard `data/{param-id}` (ISO 17978-3 §7.10).
    #[instrument(skip(self, value))]
    pub async fn write_did(
        &self,
        component_id: &str,
        did: u16,
        value: serde_json::Value,
    ) -> Result<()> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/data/{:04X}",
            component_id, did
        ))?;

        let request = WriteDataRequest { value };
        let response = self.client.put(url).json(&request).send().await?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(self.extract_error(response).await)
        }
    }

    // =========================================================================
    // Fault Operations
    // =========================================================================

    /// Get all faults/DTCs from a component
    #[instrument(skip(self))]
    pub async fn get_faults(&self, component_id: &str) -> Result<Vec<FaultInfo>> {
        let url = self
            .base_url
            .join(&format!("/vehicle/v1/components/{}/faults", component_id))?;

        let response = self.client.get(url).send().await?;
        self.handle_response::<FaultsResponse>(response)
            .await
            .map(|r| r.items)
    }

    /// Get faults/DTCs filtered by category
    #[instrument(skip(self))]
    pub async fn get_faults_filtered(
        &self,
        component_id: &str,
        category: Option<&str>,
    ) -> Result<Vec<FaultInfo>> {
        let mut url = self
            .base_url
            .join(&format!("/vehicle/v1/components/{}/faults", component_id))?;

        if let Some(cat) = category {
            url.set_query(Some(&format!("category={}", cat)));
        }

        let response = self.client.get(url).send().await?;
        self.handle_response::<FaultsResponse>(response)
            .await
            .map(|r| r.items)
    }

    /// Get a specific fault by ID
    #[instrument(skip(self))]
    pub async fn get_fault(&self, component_id: &str, fault_id: &str) -> Result<FaultInfo> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/faults/{}",
            component_id,
            encode_path_segment(fault_id)
        ))?;

        let response = self.client.get(url).send().await?;
        self.handle_response(response).await
    }

    /// Clear all faults/DTCs from a component.
    ///
    /// Wire: `DELETE /components/{id}/faults` → **204 No Content** per
    /// spec.  The returned `ClearFaultsResponse` is a courtesy
    /// success-shape derived from the status code; the server no
    /// longer emits a body for collection deletes.
    #[instrument(skip(self))]
    pub async fn clear_faults(&self, component_id: &str) -> Result<ClearFaultsResponse> {
        let url = self
            .base_url
            .join(&format!("/vehicle/v1/components/{}/faults", component_id))?;

        let response = self.client.delete(url).send().await?;
        if response.status().is_success() {
            Ok(ClearFaultsResponse {
                success: true,
                cleared_count: None,
                message: None,
            })
        } else {
            Err(self.extract_error(response).await)
        }
    }

    // =========================================================================
    // Log Operations (for HPC backends and message passing pattern)
    // =========================================================================

    /// Get logs from a component (primarily for HPC backends)
    #[instrument(skip(self))]
    pub async fn get_logs(&self, component_id: &str) -> Result<LogsResponse> {
        let url = self
            .base_url
            .join(&format!("/vehicle/v1/components/{}/logs", component_id))?;

        let response = self.client.get(url).send().await?;
        self.handle_response(response).await
    }

    /// Get logs with filters (extended for message passing pattern)
    ///
    /// # Arguments
    /// * `component_id` - Component ID
    /// * `filter` - Filter options (type, status, priority, limit)
    #[instrument(skip(self))]
    pub async fn get_logs_filtered(
        &self,
        component_id: &str,
        filter: &LogFilter,
    ) -> Result<LogsResponse> {
        let mut url = self
            .base_url
            .join(&format!("/vehicle/v1/components/{}/logs", component_id))?;

        let mut query_parts = Vec::new();
        if let Some(ref t) = filter.log_type {
            query_parts.push(format!("type={}", t));
        }
        if let Some(ref s) = filter.status {
            query_parts.push(format!("status={}", s));
        }
        if let Some(ref p) = filter.priority {
            query_parts.push(format!("priority={}", p));
        }
        if let Some(ref src) = filter.source {
            query_parts.push(format!("source={}", src));
        }
        if let Some(n) = filter.limit {
            query_parts.push(format!("limit={}", n));
        }
        if !query_parts.is_empty() {
            url.set_query(Some(&query_parts.join("&")));
        }

        let response = self.client.get(url).send().await?;
        self.handle_response(response).await
    }

    /// List pending logs/dumps for message passing pattern
    ///
    /// Convenience method for retrieving pending dumps ready for pickup.
    #[instrument(skip(self))]
    pub async fn list_pending_logs(
        &self,
        component_id: &str,
        log_type: Option<&str>,
    ) -> Result<LogsResponse> {
        let filter = LogFilter {
            status: Some("pending".to_string()),
            log_type: log_type.map(|s| s.to_string()),
            ..Default::default()
        };
        self.get_logs_filtered(component_id, &filter).await
    }

    /// Get a single log entry by ID (metadata only)
    #[instrument(skip(self))]
    pub async fn get_log(&self, component_id: &str, log_id: &str) -> Result<LogEntry> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/logs/{}",
            component_id,
            encode_path_segment(log_id)
        ))?;

        let response = self.client.get(url).send().await?;
        self.handle_response(response).await
    }

    /// Get binary content of a log entry (for large dumps)
    ///
    /// Returns raw bytes of the log content using content negotiation.
    /// Use this for message passing pattern to download dump files.
    #[instrument(skip(self))]
    pub async fn get_log_content(&self, component_id: &str, log_id: &str) -> Result<Vec<u8>> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/logs/{}",
            component_id,
            encode_path_segment(log_id)
        ))?;

        let response = self
            .client
            .get(url)
            .header("Accept", "application/octet-stream")
            .send()
            .await?;

        if response.status().is_success() {
            Ok(response.bytes().await?.to_vec())
        } else {
            Err(self.extract_error(response).await)
        }
    }

    /// Delete/acknowledge a log entry
    ///
    /// Use this after successfully retrieving a log in the message passing pattern.
    #[instrument(skip(self))]
    pub async fn delete_log(&self, component_id: &str, log_id: &str) -> Result<()> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/logs/{}",
            component_id,
            encode_path_segment(log_id)
        ))?;

        let response = self.client.delete(url).send().await?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(self.extract_error(response).await)
        }
    }

    /// Retrieve and acknowledge a log in one operation
    ///
    /// Convenience method for message passing: downloads content, then deletes.
    /// Returns the binary content of the log.
    #[instrument(skip(self))]
    pub async fn retrieve_and_ack_log(&self, component_id: &str, log_id: &str) -> Result<Vec<u8>> {
        let content = self.get_log_content(component_id, log_id).await?;
        self.delete_log(component_id, log_id).await?;
        Ok(content)
    }

    // =========================================================================
    // Operation Execution
    // =========================================================================

    /// List available operations for a component
    #[instrument(skip(self))]
    pub async fn list_operations(&self, component_id: &str) -> Result<Vec<OperationInfo>> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/operations",
            component_id
        ))?;

        let response = self.client.get(url).send().await?;
        self.handle_response::<OperationsResponse>(response)
            .await
            .map(|r| r.items)
    }

    /// Start an operation execution (UDS RoutineControl 0x31 0x01).
    ///
    /// Wire: `POST /components/{id}/operations/{op_id}/executions`
    /// → 200/202 + `Location` header to the created executions resource.
    #[instrument(skip(self))]
    pub async fn start_operation_execution(
        &self,
        component_id: &str,
        operation_id: &str,
        parameters: Option<&str>,
    ) -> Result<OperationExecution> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/operations/{}/executions",
            component_id,
            encode_path_segment(operation_id)
        ))?;

        let request = StartExecutionRequest {
            parameters: parameters.map(|s| serde_json::Value::String(s.to_string())),
        };
        let response = self.client.post(url).json(&request).send().await?;
        self.handle_response(response).await
    }

    /// Start an operation execution with no parameters.
    #[instrument(skip(self))]
    pub async fn execute_operation_simple(
        &self,
        component_id: &str,
        operation_id: &str,
    ) -> Result<OperationExecution> {
        self.start_operation_execution(component_id, operation_id, None)
            .await
    }

    /// Poll an in-flight execution.
    ///
    /// Wire: `GET /components/{id}/operations/{op_id}/executions/{exec_id}`.
    #[instrument(skip(self))]
    pub async fn get_operation_execution(
        &self,
        component_id: &str,
        operation_id: &str,
        exec_id: &str,
    ) -> Result<OperationExecution> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/operations/{}/executions/{}",
            component_id,
            encode_path_segment(operation_id),
            exec_id,
        ))?;
        let response = self.client.get(url).send().await?;
        self.handle_response(response).await
    }

    /// Stop an in-flight execution (UDS RoutineControl 0x31 0x02).
    ///
    /// Wire: `DELETE /components/{id}/operations/{op_id}/executions/{exec_id}`.
    #[instrument(skip(self))]
    pub async fn stop_operation_execution(
        &self,
        component_id: &str,
        operation_id: &str,
        exec_id: &str,
    ) -> Result<()> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/operations/{}/executions/{}",
            component_id,
            encode_path_segment(operation_id),
            exec_id,
        ))?;
        let response = self.client.delete(url).send().await?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(self.extract_error(response).await)
        }
    }

    // =========================================================================
    // Sub-entity (App) Operations
    // =========================================================================

    /// List sub-entities/apps for a component (HPC backends)
    #[instrument(skip(self))]
    pub async fn list_apps(&self, component_id: &str) -> Result<Vec<AppInfo>> {
        let url = self
            .base_url
            .join(&format!("/vehicle/v1/components/{}/apps", component_id))?;

        let response = self.client.get(url).send().await?;
        self.handle_response::<AppsResponse>(response)
            .await
            .map(|r| r.items)
    }

    /// List sub-entities of a sub-entity (nested gateway discovery)
    #[instrument(skip(self))]
    pub async fn list_sub_entity_apps(
        &self,
        component_id: &str,
        app_id: &str,
    ) -> Result<Vec<AppInfo>> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/apps/{}/apps",
            component_id,
            encode_path_segment(app_id)
        ))?;

        let response = self.client.get(url).send().await?;
        self.handle_response::<AppsResponse>(response)
            .await
            .map(|r| r.items)
    }

    /// Get a specific app/sub-entity
    #[instrument(skip(self))]
    pub async fn get_app(&self, component_id: &str, app_id: &str) -> Result<AppInfo> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/apps/{}",
            component_id, app_id
        ))?;

        let response = self.client.get(url).send().await?;
        self.handle_response(response).await
    }

    // =========================================================================
    // I/O Control / Outputs
    // =========================================================================

    /// Control an output/actuator.
    ///
    /// Outputs are exposed under `/operations` per ISO 17978-3 C-133
    /// (UDS InputOutputControl folds into the operations collection).
    /// This wrapper posts to the operation's executions sub-resource with
    /// an `{action, value?}` parameters object.
    ///
    /// # Arguments
    /// * `component_id` - Component ID
    /// * `output_id` - Output identifier (e.g., "led_status", "fan_speed")
    /// * `action` - Control action: "short_term_adjust", "return_to_ecu", "reset_to_default", "freeze"
    /// * `value` - Optional hex value for short_term_adjust (e.g., "01", "ff00")
    #[instrument(skip(self))]
    pub async fn control_output(
        &self,
        component_id: &str,
        output_id: &str,
        action: &str,
        value: Option<serde_json::Value>,
    ) -> Result<OutputControlResponse> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/operations/{}/executions",
            component_id,
            encode_path_segment(output_id)
        ))?;

        let mut params = serde_json::json!({ "action": action });
        if let Some(v) = value {
            params["value"] = v;
        }
        let body = serde_json::json!({ "parameters": params });

        let response = self.client.post(url).json(&body).send().await?;
        // Phase E / C-080: the POST returns 202 + a Running placeholder
        // (result: None); the IO control runs in a spawned task and the
        // OutputControlResponse lands on the execution resource.  Poll
        // GET .../executions/{exec_id} until terminal, then unwrap.
        let started: OperationExecution = self.handle_response(response).await?;
        let mut exec = self
            .get_operation_execution(component_id, output_id, &started.execution_id)
            .await?;
        for _ in 0..50 {
            if exec.status != OperationStatus::Running {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            exec = self
                .get_operation_execution(component_id, output_id, &started.execution_id)
                .await?;
        }
        if exec.status == OperationStatus::Failed {
            return Err(SovdClientError::server_error(
                409,
                exec.error
                    .unwrap_or_else(|| "control_output failed".to_string()),
            ));
        }
        let result = exec.result.ok_or_else(|| {
            SovdClientError::ParseError("control_output: missing result in execution".into())
        })?;
        serde_json::from_value(result)
            .map_err(|e| SovdClientError::ParseError(format!("control_output: {}", e)))
    }

    /// List available outputs for a component.
    ///
    /// Filters `/operations` for entries that carry an `output_id`
    /// (the IO control marker set by the server).
    #[instrument(skip(self))]
    pub async fn list_outputs(&self, component_id: &str) -> Result<Vec<OutputInfo>> {
        use crate::types::OperationInfo;
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/operations",
            component_id
        ))?;

        let response = self.client.get(url).send().await?;
        #[derive(serde::Deserialize)]
        struct OperationsList {
            items: Vec<OperationInfo>,
        }
        let resp: OperationsList = self.handle_response(response).await?;
        let outputs: Vec<OutputInfo> = resp
            .items
            .into_iter()
            .filter(|op| op.output_id.is_some())
            .map(operation_to_output)
            .collect();
        Ok(outputs)
    }

    /// Get detailed information about a specific output.
    ///
    /// Routes to `GET /operations/{id}`; the server returns IO control
    /// fields populated for outputs.
    #[instrument(skip(self))]
    pub async fn get_output(&self, component_id: &str, output_id: &str) -> Result<OutputInfo> {
        use crate::types::OperationInfo;
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/operations/{}",
            component_id,
            encode_path_segment(output_id)
        ))?;

        let response = self.client.get(url).send().await?;
        let op: OperationInfo = self.handle_response(response).await?;
        if op.output_id.is_none() {
            return Err(SovdClientError::server_error(
                404,
                format!("operation {} is not an output (no output_id)", output_id),
            ));
        }
        Ok(operation_to_output(op))
    }

    // =========================================================================
    // Mode Operations (session, security, link, etc.)
    // =========================================================================

    /// Get current mode state
    ///
    /// # Arguments
    /// * `component_id` - Component ID
    /// * `mode_type` - Mode type: "session", "security", "link", etc.
    #[instrument(skip(self))]
    pub async fn get_mode(&self, component_id: &str, mode_type: &str) -> Result<ModeResponse> {
        self.get_mode_targeted(component_id, mode_type, None).await
    }

    /// Get mode value with optional target sub-entity routing.
    ///
    /// When `target` is provided, the app_id is extracted and a spec-compliant
    /// sub-entity URL is constructed (SOVD §6.5):
    /// `GET /vehicle/v1/components/{gateway}/apps/{ecu}/modes/{mode_type}`
    #[instrument(skip(self))]
    pub async fn get_mode_targeted(
        &self,
        component_id: &str,
        mode_type: &str,
        target: Option<&str>,
    ) -> Result<ModeResponse> {
        let url = if let Some(t) = target {
            let app_id = t.strip_prefix(&format!("{}/", component_id)).unwrap_or(t);
            self.base_url.join(&format!(
                "/vehicle/v1/components/{}/apps/{}/modes/{}",
                component_id,
                app_id.replace('/', "%2F"),
                mode_type
            ))?
        } else {
            self.base_url.join(&format!(
                "/vehicle/v1/components/{}/modes/{}",
                component_id, mode_type
            ))?
        };

        let response = self.client.get(url).send().await?;
        self.handle_response(response).await
    }

    /// Set mode value (generic)
    ///
    /// # Arguments
    /// * `component_id` - Component ID
    /// * `mode_type` - Mode type: "session", "security", "link", etc.
    /// * `body` - JSON body with mode-specific fields
    #[instrument(skip(self, body))]
    pub async fn set_mode(
        &self,
        component_id: &str,
        mode_type: &str,
        body: serde_json::Value,
    ) -> Result<ModeResponse> {
        self.set_mode_targeted(component_id, mode_type, body, None)
            .await
    }

    /// Set mode value with optional target sub-entity routing.
    ///
    /// When `target` is provided, uses sub-entity URL per SOVD §6.5.
    #[instrument(skip(self, body))]
    pub async fn set_mode_targeted(
        &self,
        component_id: &str,
        mode_type: &str,
        body: serde_json::Value,
        target: Option<&str>,
    ) -> Result<ModeResponse> {
        let url = if let Some(t) = target {
            let app_id = t.strip_prefix(&format!("{}/", component_id)).unwrap_or(t);
            self.base_url.join(&format!(
                "/vehicle/v1/components/{}/apps/{}/modes/{}",
                component_id,
                app_id.replace('/', "%2F"),
                mode_type
            ))?
        } else {
            self.base_url.join(&format!(
                "/vehicle/v1/components/{}/modes/{}",
                component_id, mode_type
            ))?
        };

        let response = self.client.put(url).json(&body).send().await?;
        self.handle_response(response).await
    }

    /// Get current diagnostic session
    #[instrument(skip(self))]
    pub async fn get_session(&self, component_id: &str) -> Result<SessionType> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/modes/session",
            component_id
        ))?;

        let response = self.client.get(url).send().await?;
        let data: serde_json::Value = self.handle_response(response).await?;

        let value = data["value"]
            .as_str()
            .ok_or_else(|| SovdClientError::ParseError("Missing session value".to_string()))?;

        value
            .parse()
            .map_err(|e: String| SovdClientError::ParseError(e))
    }

    /// Change diagnostic session
    #[instrument(skip(self))]
    pub async fn set_session(&self, component_id: &str, session: SessionType) -> Result<()> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/modes/session",
            component_id
        ))?;

        let body = serde_json::json!({
            "value": session.as_name()
        });

        let response = self.client.put(url).json(&body).send().await?;
        let _: serde_json::Value = self.handle_response(response).await?;
        Ok(())
    }

    /// Request security access (send seed request, receive seed).
    ///
    /// Spec wire shape: `{"id": "security", "seed": "aabbccdd"}` where
    /// `seed` is concatenated lowercase hex per spec `string:hex`
    /// primitive (ISO 17978-3 sovd_iso17978_spec.yaml line 192).
    #[instrument(skip(self))]
    pub async fn security_access_request_seed(
        &self,
        component_id: &str,
        level: SecurityLevel,
    ) -> Result<Vec<u8>> {
        self.security_access_request_seed_targeted(component_id, level, None)
            .await
    }

    /// Request security seed with optional target sub-entity routing.
    ///
    /// When `target` is provided, uses sub-entity URL per SOVD §6.5.
    #[instrument(skip(self))]
    pub async fn security_access_request_seed_targeted(
        &self,
        component_id: &str,
        level: SecurityLevel,
        target: Option<&str>,
    ) -> Result<Vec<u8>> {
        let url = if let Some(t) = target {
            let app_id = t.strip_prefix(&format!("{}/", component_id)).unwrap_or(t);
            self.base_url.join(&format!(
                "/vehicle/v1/components/{}/apps/{}/modes/security",
                component_id,
                app_id.replace('/', "%2F"),
            ))?
        } else {
            self.base_url.join(&format!(
                "/vehicle/v1/components/{}/modes/security",
                component_id
            ))?
        };

        let body = serde_json::json!({
            "value": format!("level{}_requestseed", level.as_level_number())
        });

        let response = self.client.put(url).json(&body).send().await?;
        let result: serde_json::Value = self.handle_response(response).await?;

        // Spec shape: `{"id": "security", "seed": "aabbccdd"}` — `seed`
        // is concatenated lowercase hex (string:hex primitive).
        let seed_str = result.get("seed").and_then(|v| v.as_str()).ok_or_else(|| {
            SovdClientError::ParseError("No seed in security access response".into())
        })?;

        hex::decode(seed_str).map_err(|e| SovdClientError::ParseError(e.to_string()))
    }

    /// Send security access key
    ///
    /// SOVD standard response format on success:
    /// ```json
    /// {"id": "security", "value": "level1"}
    /// ```
    #[instrument(skip(self, key))]
    pub async fn security_access_send_key(
        &self,
        component_id: &str,
        level: SecurityLevel,
        key: &[u8],
    ) -> Result<()> {
        self.security_access_send_key_targeted(component_id, level, key, None)
            .await
    }

    /// Send security key with optional target sub-entity routing.
    ///
    /// When `target` is provided, uses sub-entity URL per SOVD §6.5.
    #[instrument(skip(self, key))]
    pub async fn security_access_send_key_targeted(
        &self,
        component_id: &str,
        level: SecurityLevel,
        key: &[u8],
        target: Option<&str>,
    ) -> Result<()> {
        let url = if let Some(t) = target {
            let app_id = t.strip_prefix(&format!("{}/", component_id)).unwrap_or(t);
            self.base_url.join(&format!(
                "/vehicle/v1/components/{}/apps/{}/modes/security",
                component_id,
                app_id.replace('/', "%2F"),
            ))?
        } else {
            self.base_url.join(&format!(
                "/vehicle/v1/components/{}/modes/security",
                component_id
            ))?
        };

        let body = serde_json::json!({
            "value": format!("level{}", level.as_level_number()),
            "key": hex::encode(key)
        });

        let response = self.client.put(url).json(&body).send().await?;
        let result: serde_json::Value = self.handle_response(response).await?;

        // SOVD standard: success returns {"id": "security", "value": "levelN"}
        // The presence of "id" and "value" indicates success (200 OK already verified by handle_response)
        if result.get("id").is_some() && result.get("value").is_some() {
            Ok(())
        } else {
            // Check for error field or return generic error
            Err(SovdClientError::SecurityAccessDenied(
                result
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Key rejected")
                    .to_string(),
            ))
        }
    }

    // =========================================================================
    // Streaming / Subscriptions
    // =========================================================================

    /// Create a cyclic subscription (ISO 17978-3 §7.10).
    ///
    /// Wire: `POST /components/{id}/cyclic-subscriptions`
    /// → 201 + `Location` header + `CyclicSubscription` body.
    ///
    /// Spec model is single-resource-per-subscription.  For
    /// multi-parameter consumers, open N subscriptions and join the
    /// streams client-side.
    #[instrument(skip(self))]
    pub async fn create_cyclic_subscription(
        &self,
        component_id: &str,
        resource: &str,
        interval: SubscriptionInterval,
    ) -> Result<CyclicSubscription> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/cyclic-subscriptions",
            component_id
        ))?;
        let request = CyclicSubscriptionRequest {
            resource: resource.to_string(),
            interval,
            protocol: None,
            duration: None,
        };
        let response = self.client.post(url).json(&request).send().await?;
        self.handle_response(response).await
    }

    /// List cyclic subscriptions for a component.
    #[instrument(skip(self))]
    pub async fn list_cyclic_subscriptions(
        &self,
        component_id: &str,
    ) -> Result<CyclicSubscriptionsResponse> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/cyclic-subscriptions",
            component_id
        ))?;
        let response = self.client.get(url).send().await?;
        self.handle_response(response).await
    }

    /// Get a specific cyclic subscription.
    #[instrument(skip(self))]
    pub async fn get_cyclic_subscription(
        &self,
        component_id: &str,
        subscription_id: &str,
    ) -> Result<CyclicSubscription> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/cyclic-subscriptions/{}",
            component_id, subscription_id
        ))?;
        let response = self.client.get(url).send().await?;
        self.handle_response(response).await
    }

    /// Delete a cyclic subscription.
    #[instrument(skip(self))]
    pub async fn delete_cyclic_subscription(
        &self,
        component_id: &str,
        subscription_id: &str,
    ) -> Result<()> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/cyclic-subscriptions/{}",
            component_id, subscription_id
        ))?;
        let response = self.client.delete(url).send().await?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(self.extract_error(response).await)
        }
    }

    // `stream_url` (the inline `/streams` endpoint helper) was removed
    // for C-025: `streams` is not a standardized resource name. SSE is
    // delivered from the `cyclic-subscriptions/{id}` resource via
    // `subscribe` below.

    /// Subscribe to real-time parameter data with automatic SSE streaming
    ///
    /// Creates a subscription and returns a `Subscription` that streams events.
    /// The subscription is automatically cleaned up when dropped.
    ///
    /// # Arguments
    /// * `component_id` - ECU/component identifier
    /// * `parameters` - List of parameter names to subscribe to
    /// * `rate_hz` - Desired update rate in Hz
    ///
    /// # Example
    /// ```ignore
    /// use futures::StreamExt;
    ///
    /// let mut sub = client.subscribe("engine_ecu", vec!["rpm".into()], 10).await?;
    ///
    /// while let Some(event) = sub.next().await {
    ///     let event = event?;
    ///     if let Some(rpm) = event.get_f64("rpm") {
    ///         println!("RPM: {}", rpm);
    ///     }
    /// }
    /// ```
    #[instrument(skip(self))]
    pub async fn subscribe(
        &self,
        component_id: &str,
        resource: &str,
        interval: SubscriptionInterval,
    ) -> Result<crate::streaming::Subscription> {
        use crate::streaming::Subscription;

        let response = self
            .create_cyclic_subscription(component_id, resource, interval)
            .await?;

        // ISO 17978-3 §7.10.3 / C-025: the cyclic-subscription resource
        // IS the SSE stream. Attach to it with `Accept: text/event-stream`
        // (set by `Subscription::connect`). There is no separate
        // non-standard `streams/{id}` delivery URL.
        let stream_url = format!(
            "/vehicle/v1/components/{}/cyclic-subscriptions/{}",
            component_id, response.subscription_id
        );

        Subscription::connect(
            self.base_url.clone(),
            self.client.clone(),
            response.subscription_id,
            Some(component_id.to_string()),
            &stream_url,
        )
        .await
        .map_err(|e| SovdClientError::StreamError(e.to_string()))
    }

    // `subscribe_inline` (the non-spec inline `?parameters=` streamer) and
    // the global flat-namespace subscriptions were retired for C-025 —
    // `streams` is not a standardized resource name. All streaming goes
    // through `subscribe` / the `cyclic_subscription` methods above per
    // ISO 17978-3 §7.10.

    // =========================================================================
    // Dynamic Data Identifiers (DDID)
    // =========================================================================

    /// Define a dynamic data identifier (UDS 0x2C 0x02).
    ///
    /// Wire shape: `POST /components/{id}/operations/define-data/executions`
    /// (ISO 17978-3 §7.14). Returns 201 with the resulting data-list href.
    #[instrument(skip(self))]
    pub async fn create_data_definition(
        &self,
        component_id: &str,
        ddid: &str,
        source_dids: Vec<DataDefinitionSource>,
    ) -> Result<DataDefinitionResponse> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/operations/define-data/executions",
            component_id
        ))?;

        let body = serde_json::json!({
            "ddid": ddid,
            "source_dids": source_dids
        });

        let response = self.client.post(url).json(&body).send().await?;
        self.handle_response(response).await
    }

    /// Clear a dynamic data identifier (UDS 0x2C 0x03).
    ///
    /// Wire shape: `DELETE /components/{id}/data-lists/{list_id}` where
    /// `list_id` is the DDID hex (e.g. "F200"). The `0x` prefix is stripped
    /// so existing callers passing "0xF200" continue to work.
    #[instrument(skip(self))]
    pub async fn delete_data_definition(&self, component_id: &str, ddid: &str) -> Result<()> {
        let list_id = ddid
            .trim_start_matches("0x")
            .trim_start_matches("0X")
            .to_uppercase();
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/data-lists/{}",
            component_id, list_id
        ))?;

        let response = self.client.delete(url).send().await?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(self.extract_error(response).await)
        }
    }

    // =========================================================================
    // ECU Reset (UDS 0x11)
    // =========================================================================

    /// Perform ECU reset (UDS 0x11)
    ///
    /// # Arguments
    /// * `component_id` - Component to reset
    /// * `reset_type` - Reset type: "hard" (0x01), "key_off_on" (0x02), "soft" (0x03), or hex value
    #[instrument(skip(self))]
    pub async fn ecu_reset(
        &self,
        component_id: &str,
        reset_type: &str,
    ) -> Result<EcuResetResponse> {
        // ISO 17978-3 §7.19: PUT `{entity-path}/status/restart`. CDA §8.7
        // maps UDS ECUReset(0x11) to this path. Server keeps the older
        // POST `/reset` as a deprecated alias for one release cycle.
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/status/restart",
            component_id
        ))?;

        let body = serde_json::json!({
            "reset_type": reset_type
        });

        let response = self.client.put(url).json(&body).send().await?;
        self.handle_response(response).await
    }

    // =========================================================================
    // Software Download (UDS 0x34/0x36/0x37)
    // =========================================================================

    /// Start a download session (UDS 0x34 RequestDownload)
    ///
    /// Returns session_id and max_block_size for subsequent transfers.
    #[instrument(skip(self))]
    pub async fn start_download_session(
        &self,
        component_id: &str,
        memory_address: u32,
        memory_size: u32,
        data_format: u8,
        address_and_length_format: u8,
    ) -> Result<StartDownloadResponse> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/software/download",
            component_id
        ))?;

        let body = serde_json::json!({
            "memory_address": format!("0x{:08X}", memory_address),
            "memory_size": memory_size,
            "data_format": data_format,
            "address_and_length_format": address_and_length_format
        });

        let response = self.client.post(url).json(&body).send().await?;
        self.handle_response(response).await
    }

    /// Transfer a data block (UDS 0x36 TransferData)
    #[instrument(skip(self, data))]
    pub async fn transfer_data_block(
        &self,
        component_id: &str,
        session_id: &str,
        data: &[u8],
    ) -> Result<TransferDataResponse> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/software/download/{}",
            component_id, session_id
        ))?;

        let response = self
            .client
            .put(url)
            .header("Content-Type", "application/octet-stream")
            .body(data.to_vec())
            .send()
            .await?;
        self.handle_response(response).await
    }

    /// Finalize a download session (UDS 0x37 RequestTransferExit)
    #[instrument(skip(self))]
    pub async fn finalize_download(
        &self,
        component_id: &str,
        session_id: &str,
    ) -> Result<FinalizeDownloadResponse> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/software/download/{}",
            component_id, session_id
        ))?;

        let response = self.client.delete(url).send().await?;
        self.handle_response(response).await
    }

    // =========================================================================
    // Admin - DID Definitions
    // =========================================================================

    /// List all DID definitions
    #[instrument(skip(self))]
    pub async fn list_definitions(&self) -> Result<DefinitionsResponse> {
        let url = self.base_url.join("/admin/definitions")?;
        let response = self.client.get(url).send().await?;
        self.handle_response(response).await
    }

    /// Get a specific DID definition
    #[instrument(skip(self))]
    pub async fn get_definition(&self, did: &str) -> Result<DefinitionInfo> {
        let url = self.base_url.join(&format!("/admin/definitions/{}", did))?;
        let response = self.client.get(url).send().await?;
        self.handle_response(response).await
    }

    /// Upload DID definitions (YAML format)
    #[instrument(skip(self, yaml_content))]
    pub async fn upload_definitions(
        &self,
        yaml_content: &str,
    ) -> Result<UploadDefinitionsResponse> {
        let url = self.base_url.join("/admin/definitions")?;
        let response = self
            .client
            .post(url)
            .header("Content-Type", "text/yaml")
            .body(yaml_content.to_string())
            .send()
            .await?;
        self.handle_response(response).await
    }

    /// Delete a specific DID definition
    #[instrument(skip(self))]
    pub async fn delete_definition(&self, did: &str) -> Result<()> {
        let url = self.base_url.join(&format!("/admin/definitions/{}", did))?;
        let response = self.client.delete(url).send().await?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(self.extract_error(response).await)
        }
    }

    /// Clear all DID definitions
    #[instrument(skip(self))]
    pub async fn clear_definitions(&self) -> Result<serde_json::Value> {
        let url = self.base_url.join("/admin/definitions")?;
        let response = self.client.delete(url).send().await?;
        self.handle_response(response).await
    }

    // =========================================================================
    // Helper Methods
    // =========================================================================

    /// Handle response and deserialize JSON
    async fn handle_response<T: serde::de::DeserializeOwned>(
        &self,
        response: reqwest::Response,
    ) -> Result<T> {
        let status = response.status();

        if status.is_success() {
            response
                .json()
                .await
                .map_err(|e| SovdClientError::ParseError(e.to_string()))
        } else {
            Err(self.extract_error_from_status(response, status).await)
        }
    }

    /// Extract error from failed response
    async fn extract_error(&self, response: reqwest::Response) -> SovdClientError {
        let status = response.status();
        self.extract_error_from_status(response, status).await
    }

    async fn extract_error_from_status(
        &self,
        response: reqwest::Response,
        status: StatusCode,
    ) -> SovdClientError {
        // Parse the spec-defined GenericError body (ISO 17978-3 §5.8.3).
        // Older servers that haven't migrated may still send ad-hoc shapes;
        // fall back to the HTTP status line in that case.
        let message = match response.json::<ErrorResponse>().await {
            Ok(err) => err.message,
            Err(_) => format!("HTTP {}", status),
        };

        match status {
            StatusCode::NOT_FOUND => {
                if message.contains("component") {
                    SovdClientError::ComponentNotFound(message)
                } else if message.contains("parameter") || message.contains("DID") {
                    SovdClientError::ParameterNotFound(message)
                } else {
                    SovdClientError::server_error(status.as_u16(), message)
                }
            }
            StatusCode::FORBIDDEN => SovdClientError::SecurityAccessDenied(message),
            StatusCode::REQUEST_TIMEOUT | StatusCode::GATEWAY_TIMEOUT => SovdClientError::Timeout,
            _ => SovdClientError::server_error(status.as_u16(), message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_creation() {
        let client = SovdClient::new("http://localhost:9080");
        assert!(client.is_ok());
    }

    #[test]
    fn test_invalid_url() {
        let client = SovdClient::new("not a url");
        assert!(client.is_err());
    }

    #[test]
    fn test_session_type_bytes() {
        assert_eq!(SessionType::Default.as_uds_byte(), 0x01);
        assert_eq!(SessionType::Programming.as_uds_byte(), 0x02);
        assert_eq!(SessionType::Extended.as_uds_byte(), 0x03);
        assert_eq!(SessionType::Engineering.as_uds_byte(), 0x60);
    }

    #[test]
    fn test_security_level() {
        assert_eq!(SecurityLevel::LEVEL_1.seed_request(), 0x01);
        assert_eq!(SecurityLevel::LEVEL_1.key_send(), 0x02);
        assert_eq!(SecurityLevel::LEVEL_3.seed_request(), 0x03);
        assert_eq!(SecurityLevel::LEVEL_3.key_send(), 0x04);
        assert_eq!(SecurityLevel::PROGRAMMING.seed_request(), 0x11);
        assert_eq!(SecurityLevel::PROGRAMMING.key_send(), 0x12);
    }
}
