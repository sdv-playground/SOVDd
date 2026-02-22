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

/// Default request timeout
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Default connection timeout
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// SOVD REST API client
///
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
        Self::with_config(base_url, DEFAULT_TIMEOUT, DEFAULT_CONNECT_TIMEOUT)
    }

    /// Create a new SOVD client with custom configuration
    pub fn with_config(
        base_url: &str,
        timeout: Duration,
        connect_timeout: Duration,
    ) -> Result<Self> {
        let client = Client::builder()
            .timeout(timeout)
            .connect_timeout(connect_timeout)
            .build()?;

        let base_url = Url::parse(base_url)?;

        Ok(Self { client, base_url })
    }

    /// Create a new SOVD client that sends a bearer token with every request.
    ///
    /// The token is set as a default `Authorization: Bearer <token>` header.
    pub fn with_bearer_token(base_url: &str, token: &str) -> Result<Self> {
        let mut headers = reqwest::header::HeaderMap::new();
        let header_value = reqwest::header::HeaderValue::from_str(&format!("Bearer {}", token))
            .map_err(|e| SovdClientError::ParseError(format!("Invalid auth token: {}", e)))?;
        headers.insert(reqwest::header::AUTHORIZATION, header_value);

        let client = Client::builder()
            .timeout(DEFAULT_TIMEOUT)
            .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
            .default_headers(headers)
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

        // Send as hex string with explicit format hint
        let body = serde_json::json!({
            "value": hex::encode(data),
            "format": "hex"
        });

        let response = self.client.put(url).json(&body).send().await?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(self.extract_error(response).await)
        }
    }

    /// Read raw DID by hex identifier
    #[instrument(skip(self))]
    pub async fn read_did(&self, component_id: &str, did: u16) -> Result<DataResponse> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/did/{:04X}",
            component_id, did
        ))?;

        let response = self.client.get(url).send().await?;
        self.handle_response(response).await
    }

    /// Write raw DID by hex identifier
    #[instrument(skip(self, value))]
    pub async fn write_did(
        &self,
        component_id: &str,
        did: u16,
        value: serde_json::Value,
    ) -> Result<()> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/did/{:04X}",
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

    /// Clear all faults/DTCs from a component
    #[instrument(skip(self))]
    pub async fn clear_faults(&self, component_id: &str) -> Result<ClearFaultsResponse> {
        let url = self
            .base_url
            .join(&format!("/vehicle/v1/components/{}/faults", component_id))?;

        let response = self.client.delete(url).send().await?;
        self.handle_response(response).await
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

    /// Execute an operation on a component
    #[instrument(skip(self))]
    pub async fn execute_operation(
        &self,
        component_id: &str,
        operation_id: &str,
        action: &str,
        parameters: Option<&str>,
    ) -> Result<OperationResponse> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/operations/{}",
            component_id,
            encode_path_segment(operation_id)
        ))?;

        let request = OperationRequest {
            action: action.to_string(),
            parameters: parameters.map(|s| s.to_string()),
        };
        let response = self.client.post(url).json(&request).send().await?;
        self.handle_response(response).await
    }

    /// Execute an operation with "start" action and no parameters
    #[instrument(skip(self))]
    pub async fn execute_operation_simple(
        &self,
        component_id: &str,
        operation_id: &str,
    ) -> Result<OperationResponse> {
        self.execute_operation(component_id, operation_id, "start", None)
            .await
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

    /// Control an output/actuator
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
            "/vehicle/v1/components/{}/outputs/{}",
            component_id,
            encode_path_segment(output_id)
        ))?;

        let mut body = serde_json::json!({
            "action": action
        });
        if let Some(v) = value {
            body["value"] = v;
        }

        let response = self.client.post(url).json(&body).send().await?;
        self.handle_response(response).await
    }

    /// List available outputs for a component
    #[instrument(skip(self))]
    pub async fn list_outputs(&self, component_id: &str) -> Result<Vec<OutputInfo>> {
        let url = self
            .base_url
            .join(&format!("/vehicle/v1/components/{}/outputs", component_id))?;

        let response = self.client.get(url).send().await?;
        self.handle_response::<OutputsResponse>(response)
            .await
            .map(|r| r.items)
    }

    /// Get detailed information about a specific output
    #[instrument(skip(self))]
    pub async fn get_output(&self, component_id: &str, output_id: &str) -> Result<OutputInfo> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/outputs/{}",
            component_id,
            encode_path_segment(output_id)
        ))?;

        let response = self.client.get(url).send().await?;
        self.handle_response(response).await
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

    /// Request security access (send seed request, receive seed)
    ///
    /// SOVD standard response format:
    /// ```json
    /// {"id": "security", "seed": {"Request_Seed": "0xaa 0xbb 0xcc 0xdd"}}
    /// ```
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

        // SOVD standard: {"id": "security", "seed": {"Request_Seed": "0xaa 0xbb 0xcc 0xdd"}}
        if let Some(seed_obj) = result.get("seed") {
            // Try SOVD standard format first: seed.Request_Seed
            if let Some(seed_str) = seed_obj.get("Request_Seed").and_then(|v| v.as_str()) {
                // Parse space-separated "0xaa 0xbb" format
                let bytes: Vec<u8> = seed_str
                    .split_whitespace()
                    .filter_map(|s| {
                        let s = s.trim_start_matches("0x").trim_start_matches("0X");
                        u8::from_str_radix(s, 16).ok()
                    })
                    .collect();
                if !bytes.is_empty() {
                    return Ok(bytes);
                }
            }
            // Fallback: seed as direct hex string
            if let Some(seed_str) = seed_obj.as_str() {
                return hex::decode(seed_str)
                    .map_err(|e| SovdClientError::ParseError(e.to_string()));
            }
        }

        Err(SovdClientError::ParseError(
            "No seed in security access response".into(),
        ))
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

    /// Create a subscription for periodic data
    #[instrument(skip(self))]
    pub async fn create_subscription(
        &self,
        component_id: &str,
        parameters: Vec<String>,
        rate_hz: u32,
    ) -> Result<SubscriptionResponse> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/subscriptions",
            component_id
        ))?;

        let request = SubscriptionRequest {
            parameters,
            rate_hz,
            mode: Some("periodic".into()),
            duration_secs: None,
        };

        let response = self.client.post(url).json(&request).send().await?;
        self.handle_response(response).await
    }

    /// List all subscriptions for a component
    #[instrument(skip(self))]
    pub async fn list_subscriptions(&self, component_id: &str) -> Result<SubscriptionListResponse> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/subscriptions",
            component_id
        ))?;

        let response = self.client.get(url).send().await?;
        self.handle_response(response).await
    }

    /// Get a specific subscription
    #[instrument(skip(self))]
    pub async fn get_subscription(
        &self,
        component_id: &str,
        subscription_id: &str,
    ) -> Result<SubscriptionResponse> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/subscriptions/{}",
            component_id, subscription_id
        ))?;

        let response = self.client.get(url).send().await?;
        self.handle_response(response).await
    }

    /// Delete a subscription
    #[instrument(skip(self))]
    pub async fn delete_subscription(
        &self,
        component_id: &str,
        subscription_id: &str,
    ) -> Result<()> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/subscriptions/{}",
            component_id, subscription_id
        ))?;

        let response = self.client.delete(url).send().await?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(self.extract_error(response).await)
        }
    }

    /// Get the stream URL for a component (for SSE connection)
    pub fn stream_url(&self, component_id: &str) -> Result<Url> {
        self.base_url
            .join(&format!("/vehicle/v1/components/{}/streams", component_id))
            .map_err(Into::into)
    }

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
    #[instrument(skip(self, parameters))]
    pub async fn subscribe(
        &self,
        component_id: &str,
        parameters: Vec<String>,
        rate_hz: u32,
    ) -> Result<crate::streaming::Subscription> {
        use crate::streaming::Subscription;

        // Create the subscription
        let response = self
            .create_subscription(component_id, parameters, rate_hz)
            .await?;

        // Connect to the stream
        let subscription = Subscription::connect(
            self.base_url.clone(),
            self.client.clone(),
            response.subscription_id,
            Some(component_id.to_string()),
            &response.stream_url,
        )
        .await
        .map_err(|e| SovdClientError::StreamError(e.to_string()))?;

        Ok(subscription)
    }

    /// Subscribe using inline parameters (no subscription ID, direct stream)
    ///
    /// Connects directly to the stream endpoint with query parameters.
    /// Simpler but doesn't create a trackable subscription on the server.
    ///
    /// # Arguments
    /// * `component_id` - ECU/component identifier
    /// * `parameters` - List of parameter names to subscribe to
    /// * `rate_hz` - Desired update rate in Hz
    #[instrument(skip(self, parameters))]
    pub async fn subscribe_inline(
        &self,
        component_id: &str,
        parameters: Vec<String>,
        rate_hz: u32,
    ) -> Result<crate::streaming::Subscription> {
        use crate::streaming::Subscription;

        // Build stream URL with query parameters
        let params_str = parameters.join(",");
        let stream_path = format!(
            "/vehicle/v1/components/{}/streams?parameters={}&rate_hz={}",
            component_id, params_str, rate_hz
        );

        // Generate a pseudo subscription ID for tracking
        let subscription_id = format!("inline-{}", uuid::Uuid::new_v4());

        // Connect directly to the stream
        let subscription = Subscription::connect(
            self.base_url.clone(),
            self.client.clone(),
            subscription_id,
            Some(component_id.to_string()),
            &stream_path,
        )
        .await
        .map_err(|e| SovdClientError::StreamError(e.to_string()))?;

        Ok(subscription)
    }

    // =========================================================================
    // Global Subscription Management (flat namespace)
    // =========================================================================

    /// Create a global subscription (component_id in body)
    #[instrument(skip(self))]
    pub async fn create_global_subscription(
        &self,
        component_id: &str,
        parameters: Vec<String>,
        rate_hz: u32,
    ) -> Result<GlobalSubscriptionResponse> {
        let url = self.base_url.join("/vehicle/v1/subscriptions")?;

        let body = serde_json::json!({
            "component_id": component_id,
            "parameters": parameters,
            "rate_hz": rate_hz,
            "mode": "periodic"
        });

        let response = self.client.post(url).json(&body).send().await?;
        self.handle_response(response).await
    }

    /// List all global subscriptions
    #[instrument(skip(self))]
    pub async fn list_global_subscriptions(&self) -> Result<GlobalSubscriptionListResponse> {
        let url = self.base_url.join("/vehicle/v1/subscriptions")?;
        let response = self.client.get(url).send().await?;
        self.handle_response(response).await
    }

    /// Get a global subscription by ID
    #[instrument(skip(self))]
    pub async fn get_global_subscription(
        &self,
        subscription_id: &str,
    ) -> Result<GlobalSubscriptionResponse> {
        let url = self
            .base_url
            .join(&format!("/vehicle/v1/subscriptions/{}", subscription_id))?;
        let response = self.client.get(url).send().await?;
        self.handle_response(response).await
    }

    /// Delete a global subscription
    #[instrument(skip(self))]
    pub async fn delete_global_subscription(&self, subscription_id: &str) -> Result<()> {
        let url = self
            .base_url
            .join(&format!("/vehicle/v1/subscriptions/{}", subscription_id))?;
        let response = self.client.delete(url).send().await?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(self.extract_error(response).await)
        }
    }

    // =========================================================================
    // Dynamic Data Identifiers (DDID)
    // =========================================================================

    /// Create a dynamic data identifier (UDS 0x2C)
    #[instrument(skip(self))]
    pub async fn create_data_definition(
        &self,
        component_id: &str,
        ddid: &str,
        source_dids: Vec<DataDefinitionSource>,
    ) -> Result<DataDefinitionResponse> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/data-definitions",
            component_id
        ))?;

        let body = serde_json::json!({
            "ddid": ddid,
            "source_dids": source_dids
        });

        let response = self.client.post(url).json(&body).send().await?;
        self.handle_response(response).await
    }

    /// Delete a dynamic data identifier
    #[instrument(skip(self))]
    pub async fn delete_data_definition(&self, component_id: &str, ddid: &str) -> Result<()> {
        let url = self.base_url.join(&format!(
            "/vehicle/v1/components/{}/data-definitions/{}",
            component_id, ddid
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
        let url = self
            .base_url
            .join(&format!("/vehicle/v1/components/{}/reset", component_id))?;

        let body = serde_json::json!({
            "reset_type": reset_type
        });

        let response = self.client.post(url).json(&body).send().await?;
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
        // Try to parse error response body
        let message = match response.json::<ErrorResponse>().await {
            Ok(err) => err.error,
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

    #[test]
    fn test_stream_url() {
        let client = SovdClient::new("http://localhost:9080").unwrap();
        let url = client.stream_url("engine_ecu").unwrap();
        assert_eq!(
            url.as_str(),
            "http://localhost:9080/vehicle/v1/components/engine_ecu/streams"
        );
    }
}
