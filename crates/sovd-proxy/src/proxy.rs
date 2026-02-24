//! SovdProxyBackend - DiagnosticBackend that proxies to a remote SOVD server

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use sovd_client::SovdClient;
use sovd_core::models::{FaultSeverity, LogPriority, OperationStatus};
use sovd_core::routing;
use sovd_core::{
    ActivationState, BackendError, BackendResult, Capabilities, ClearFaultsResult, DataValue,
    DiagnosticBackend, EntityInfo, Fault, FaultFilter, FaultsResult, FlashStatus, IoControlAction,
    IoControlResult, LogEntry, LogFilter, OperationExecution, OperationInfo, OutputDetail,
    OutputInfo, PackageInfo, ParameterInfo, SecurityMode, SecurityState, SessionMode, VerifyResult,
};

/// Convert client-side capabilities to core Capabilities.
/// The upstream is authoritative — no local overrides.
fn to_capabilities(rc: sovd_client::ComponentCapabilities) -> Capabilities {
    Capabilities {
        read_data: rc.read_data,
        write_data: rc.write_data,
        faults: rc.faults,
        clear_faults: rc.clear_faults,
        logs: rc.logs,
        operations: rc.operations,
        software_update: rc.software_update,
        io_control: rc.io_control,
        sessions: rc.sessions,
        security: rc.security,
        sub_entities: rc.sub_entities,
        subscriptions: rc.subscriptions,
    }
}

// =========================================================================
// Response types for deserializing upstream SOVD server JSON
// =========================================================================

#[derive(Deserialize)]
struct UploadFileResp {
    file_id: String,
}

#[derive(Deserialize)]
struct ListFilesResp {
    files: Vec<PackageInfo>,
}

#[derive(Deserialize)]
struct StartFlashResp {
    transfer_id: String,
}

#[derive(Deserialize)]
struct ListTransfersResp {
    transfers: Vec<FlashStatus>,
}

#[derive(Deserialize)]
struct UpstreamErrorResp {
    #[serde(default)]
    error: String,
    #[serde(default)]
    message: String,
}

/// A `DiagnosticBackend` implementation that proxies all SOVD operations
/// over HTTP to a remote SOVD server via `SovdClient`.
///
/// Used for tier-1 supplier containers that have no direct CAN access
/// and reach ECUs exclusively through the SOVD HTTP API.
pub struct SovdProxyBackend {
    client: SovdClient,
    component_id: String,
    /// When routing through a gateway, parameter/operation IDs are prefixed
    /// with the sub-entity ID (e.g., "vtx_vx500/boost_pressure").
    sub_entity_prefix: Option<String>,
    entity_info: EntityInfo,
    capabilities: Capabilities,
}

impl SovdProxyBackend {
    /// Return the resolved routing component ID and sub-entity prefix.
    ///
    /// When the proxy discovered a gateway (either via explicit config or
    /// auto-discovery), the first element is the gateway component ID and
    /// the second is `Some(sub_entity_id)`.  When routing directly, the
    /// first element is the target component ID and the second is `None`.
    pub fn routing_info(&self) -> (&str, Option<&str>) {
        (&self.component_id, self.sub_entity_prefix.as_deref())
    }

    /// Create a new proxy backend.
    ///
    /// Connects to `base_url`, fetches entity info and capabilities for
    /// `remote_component_id`, and caches them with `local_id` as the
    /// local entity identifier.
    ///
    /// If `auth_token` is provided, a `Bearer` token will be sent with
    /// every request to the upstream server.
    ///
    /// If `upstream_gateway` is provided, the component is accessed as a
    /// sub-entity of the gateway. Requests are routed through the gateway
    /// and the component info is fetched via the gateway's apps endpoint.
    pub async fn new(
        local_id: &str,
        base_url: &str,
        remote_component_id: &str,
    ) -> Result<Self, String> {
        Self::with_options(local_id, base_url, remote_component_id, None, None).await
    }

    /// Create a new proxy backend with optional bearer token authentication.
    pub async fn with_auth(
        local_id: &str,
        base_url: &str,
        remote_component_id: &str,
        auth_token: Option<&str>,
    ) -> Result<Self, String> {
        Self::with_options(local_id, base_url, remote_component_id, auth_token, None).await
    }

    /// Create a new proxy backend with full options.
    ///
    /// When `upstream_gateway` is `Some`, the component is a sub-entity of
    /// the gateway: component info is fetched from the gateway's apps
    /// endpoint, and all API requests are routed through the gateway component.
    pub async fn with_options(
        local_id: &str,
        base_url: &str,
        remote_component_id: &str,
        auth_token: Option<&str>,
        upstream_gateway: Option<&str>,
    ) -> Result<Self, String> {
        let client = if let Some(token) = auth_token {
            SovdClient::with_bearer_token(base_url, token)
        } else {
            SovdClient::new(base_url)
        }
        .map_err(|e| format!("Failed to create client: {}", e))?;

        // Determine routing component, entity info, and upstream capabilities.
        let (routing_component_id, entity_info, remote_caps) =
            if let Some(gateway_id) = upstream_gateway {
                // Component is a sub-entity of the gateway: fetch detail via apps endpoint
                let app = client
                    .get_app(gateway_id, remote_component_id)
                    .await
                    .map_err(|e| {
                        format!(
                            "Failed to fetch sub-entity '{}' from gateway '{}': {}",
                            remote_component_id, gateway_id, e
                        )
                    })?;

                let info = EntityInfo {
                    id: local_id.to_string(),
                    name: app.name,
                    entity_type: app.app_type.unwrap_or_else(|| "proxy".to_string()),
                    description: app.description,
                    href: format!("/vehicle/v1/components/{}", local_id),
                    status: app.status,
                };

                // Route all requests through the gateway component
                (gateway_id.to_string(), info, app.capabilities)
            } else {
                // Try direct top-level component access first
                match client.get_component(remote_component_id).await {
                    Ok(component) => {
                        let caps = component.capabilities.clone();
                        let info = EntityInfo {
                            id: local_id.to_string(),
                            name: component.name,
                            entity_type: component
                                .component_type
                                .unwrap_or_else(|| "proxy".to_string()),
                            description: component.description,
                            href: format!("/vehicle/v1/components/{}", local_id),
                            status: component.status,
                        };
                        (remote_component_id.to_string(), info, caps)
                    }
                    Err(_) => {
                        // Component not found at top level — search inside gateways
                        tracing::info!(
                            component = %remote_component_id,
                            "Component not found at top level, searching gateway sub-entities..."
                        );
                        let mut found = None;
                        if let Ok(components) = client.list_components().await {
                            for comp in &components {
                                if comp.component_type.as_deref() == Some("gateway") {
                                    if let Ok(apps) = client.list_apps(&comp.id).await {
                                        if let Some(app) =
                                            apps.into_iter().find(|a| a.id == remote_component_id)
                                        {
                                            found = Some((comp.id.clone(), app));
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                        let (gateway_id, app) = found.ok_or_else(|| {
                            format!(
                            "Component '{}' not found at top level or in any gateway sub-entities",
                            remote_component_id
                        )
                        })?;
                        tracing::info!(
                            component = %remote_component_id,
                            gateway = %gateway_id,
                            "Found component as sub-entity of gateway"
                        );
                        // Fetch detail to get capabilities
                        let detail = client.get_app(&gateway_id, remote_component_id).await.ok();
                        let (caps, name, etype, desc, status) = if let Some(d) = detail {
                            (
                                d.capabilities,
                                d.name,
                                d.app_type.unwrap_or_else(|| "proxy".to_string()),
                                d.description,
                                d.status,
                            )
                        } else {
                            (
                                None,
                                app.name,
                                app.app_type.unwrap_or_else(|| "proxy".to_string()),
                                app.description,
                                app.status,
                            )
                        };
                        let info = EntityInfo {
                            id: local_id.to_string(),
                            name,
                            entity_type: etype,
                            description: desc,
                            href: format!("/vehicle/v1/components/{}", local_id),
                            status,
                        };
                        (gateway_id, info, caps)
                    }
                }
            };

        // Use upstream capabilities (per SOVD §6.4).
        // Detail endpoints always return capabilities; if missing, all default to false.
        let capabilities = to_capabilities(remote_caps.unwrap_or_default());

        tracing::info!(
            local_id = %local_id,
            remote = %remote_component_id,
            routing_via = %routing_component_id,
            base_url = %base_url,
            "SovdProxyBackend connected"
        );

        // When routing through a gateway (either explicit or auto-discovered),
        // parameter/operation IDs are prefixed with the sub-entity component ID.
        let sub_entity_prefix = if routing_component_id != remote_component_id {
            Some(remote_component_id.to_string())
        } else {
            None
        };

        Ok(Self {
            client,
            component_id: routing_component_id,
            sub_entity_prefix,
            entity_info,
            capabilities,
        })
    }

    /// Map a SovdClientError to a BackendError
    fn map_err(e: sovd_client::SovdClientError) -> BackendError {
        use sovd_client::SovdClientError;
        match e {
            SovdClientError::ComponentNotFound(m) => BackendError::EntityNotFound(m),
            SovdClientError::ParameterNotFound(m) => BackendError::ParameterNotFound(m),
            SovdClientError::SecurityAccessDenied(_) => BackendError::SecurityRequired(1),
            SovdClientError::Timeout => BackendError::Timeout,
            SovdClientError::HttpError(e) => BackendError::Transport(e.to_string()),
            SovdClientError::ServerError { status, message } => match status {
                404 => BackendError::EntityNotFound(message),
                403 => BackendError::SecurityRequired(1),
                501 => BackendError::NotSupported(message),
                _ => BackendError::Protocol(format!("HTTP {}: {}", status, message)),
            },
            other => BackendError::Transport(other.to_string()),
        }
    }

    /// Parse severity string from client response into FaultSeverity enum
    fn parse_severity(s: &str) -> FaultSeverity {
        match s.to_lowercase().as_str() {
            "info" | "information" => FaultSeverity::Info,
            "warning" | "warn" => FaultSeverity::Warning,
            "error" => FaultSeverity::Error,
            "critical" => FaultSeverity::Critical,
            _ => FaultSeverity::Error,
        }
    }

    /// Parse a timestamp string into DateTime<Utc>, falling back to now
    fn parse_timestamp(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(|_| chrono::Utc::now())
    }

    /// Parse a log priority string into LogPriority enum
    fn parse_log_priority(s: &str) -> LogPriority {
        match s.to_lowercase().as_str() {
            "emergency" | "emerg" => LogPriority::Emergency,
            "alert" => LogPriority::Alert,
            "critical" | "crit" => LogPriority::Critical,
            "error" | "err" => LogPriority::Error,
            "warning" | "warn" => LogPriority::Warning,
            "notice" => LogPriority::Notice,
            "info" | "information" => LogPriority::Info,
            "debug" => LogPriority::Debug,
            _ => LogPriority::Info,
        }
    }

    /// Build the URL path prefix for file/flash operations on the upstream server.
    ///
    /// When `sub_entity_prefix` is set, routes through the sub-entity path
    /// per SOVD §6.5 (e.g., `/vehicle/v1/components/{gw}/apps/{ecu}`).
    fn flash_path_prefix(&self) -> String {
        if let Some(ref prefix) = self.sub_entity_prefix {
            format!(
                "/vehicle/v1/components/{}/apps/{}",
                self.component_id,
                prefix.replace('/', "%2F")
            )
        } else {
            format!("/vehicle/v1/components/{}", self.component_id)
        }
    }

    /// Build a full URL string for a flash/file endpoint on the upstream server.
    fn flash_url(&self, suffix: &str) -> Result<String, BackendError> {
        let base = self.client.base_url().as_str().trim_end_matches('/');
        Ok(format!("{}{}{}", base, self.flash_path_prefix(), suffix))
    }

    /// Map an HTTP error response to BackendError.
    async fn map_response_error(response: reqwest::Response) -> BackendError {
        let status = response.status().as_u16();
        let message = match response.json::<UpstreamErrorResp>().await {
            Ok(err) => {
                if !err.message.is_empty() {
                    err.message
                } else if !err.error.is_empty() {
                    err.error
                } else {
                    format!("HTTP {}", status)
                }
            }
            Err(_) => format!("HTTP {}", status),
        };
        match status {
            404 => BackendError::EntityNotFound(message),
            403 => BackendError::SecurityRequired(1),
            409 => BackendError::Busy(message),
            412 => BackendError::SessionRequired(message),
            501 => BackendError::NotSupported(message),
            _ => BackendError::Protocol(format!("HTTP {}: {}", status, message)),
        }
    }
}

#[async_trait]
impl DiagnosticBackend for SovdProxyBackend {
    // =========================================================================
    // Entity Information (cached)
    // =========================================================================

    fn entity_info(&self) -> &EntityInfo {
        &self.entity_info
    }

    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    // =========================================================================
    // Data Access
    // =========================================================================

    async fn list_parameters(&self) -> BackendResult<Vec<ParameterInfo>> {
        // Use sub-entity route when proxying through a gateway
        let resp = if let Some(ref prefix) = self.sub_entity_prefix {
            self.client
                .list_sub_entity_parameters(&self.component_id, prefix)
                .await
        } else {
            self.client.list_parameters(&self.component_id).await
        }
        .map_err(Self::map_err)?;

        let params = resp
            .items
            .into_iter()
            .map(|p| ParameterInfo {
                id: p.id,
                name: p.name.unwrap_or_default(),
                description: None,
                unit: p.unit,
                data_type: p.data_type,
                read_only: !p.writable,
                href: String::new(),
                did: if p.did.is_empty() { None } else { Some(p.did) },
            })
            .collect();

        Ok(params)
    }

    async fn read_data(&self, param_ids: &[String]) -> BackendResult<Vec<DataValue>> {
        let mut values = Vec::new();
        for param_id in param_ids {
            // Use sub-entity route when proxying through a gateway
            let resp = if let Some(ref prefix) = self.sub_entity_prefix {
                self.client
                    .read_sub_entity_data(&self.component_id, prefix, param_id)
                    .await
            } else {
                self.client.read_data(&self.component_id, param_id).await
            }
            .map_err(Self::map_err)?;

            values.push(DataValue {
                id: param_id.clone(),
                name: param_id.clone(),
                value: resp.value,
                unit: resp.unit,
                timestamp: chrono::Utc::now(),
                raw: resp.raw,
                did: resp.did,
                length: resp.length,
            });
        }
        Ok(values)
    }

    async fn write_data(&self, param_id: &str, value: &[u8]) -> BackendResult<()> {
        let hex_value = hex::encode(value);
        // Use sub-entity route when proxying through a gateway
        if let Some(ref prefix) = self.sub_entity_prefix {
            self.client
                .write_sub_entity_data(
                    &self.component_id,
                    prefix,
                    param_id,
                    serde_json::Value::String(hex_value),
                )
                .await
                .map_err(Self::map_err)
        } else {
            self.client
                .write_data(
                    &self.component_id,
                    param_id,
                    serde_json::Value::String(hex_value),
                )
                .await
                .map_err(Self::map_err)
        }
    }

    async fn read_raw_did(&self, did: u16) -> BackendResult<Vec<u8>> {
        let did_str = format!("{:04X}", did);
        // Use sub-entity route when proxying through a gateway
        let resp = if let Some(ref prefix) = self.sub_entity_prefix {
            self.client
                .read_sub_entity_data(&self.component_id, prefix, &did_str)
                .await
        } else {
            self.client.read_data(&self.component_id, &did_str).await
        }
        .map_err(Self::map_err)?;

        if let Some(raw) = &resp.raw {
            hex::decode(raw)
                .map_err(|e| BackendError::Protocol(format!("Invalid hex in raw field: {}", e)))
        } else if let Some(s) = resp.value.as_str() {
            hex::decode(s)
                .map_err(|e| BackendError::Protocol(format!("Invalid hex in value: {}", e)))
        } else {
            let s = resp.value.to_string();
            Ok(s.into_bytes())
        }
    }

    async fn write_raw_did(&self, did: u16, data: &[u8]) -> BackendResult<()> {
        let hex_value = hex::encode(data);
        let did_str = format!("{:04X}", did);
        let prefixed = routing::prefixed_id(&did_str, self.sub_entity_prefix.as_deref());
        self.client
            .write_data(
                &self.component_id,
                &prefixed,
                serde_json::Value::String(hex_value),
            )
            .await
            .map_err(Self::map_err)
    }

    async fn ecu_reset(&self, reset_type: u8) -> BackendResult<Option<u8>> {
        let type_str = match reset_type {
            0x01 => "hard",
            0x02 => "key_off_on",
            0x03 => "soft",
            _ => "hard",
        };

        if self.sub_entity_prefix.is_some() {
            // Route through sub-entity path on the upstream server
            let url = self.flash_url("/reset")?;
            let body = serde_json::json!({ "reset_type": type_str });

            tracing::info!(url = %url, reset_type = %type_str, "Proxy: sub-entity ECU reset");

            let response = self
                .client
                .http_client()
                .post(&url)
                .json(&body)
                .send()
                .await
                .map_err(|e| BackendError::Transport(e.to_string()))?;

            if !response.status().is_success() {
                return Err(Self::map_response_error(response).await);
            }

            #[derive(Deserialize)]
            struct ResetResp {
                power_down_time: Option<u8>,
            }
            let resp: ResetResp = response.json().await.map_err(|e| {
                BackendError::Protocol(format!("Failed to parse reset response: {}", e))
            })?;

            Ok(resp.power_down_time)
        } else {
            let resp = self
                .client
                .ecu_reset(&self.component_id, type_str)
                .await
                .map_err(Self::map_err)?;

            Ok(resp.power_down_time)
        }
    }

    // =========================================================================
    // Faults
    // =========================================================================

    async fn get_faults(&self, filter: Option<&FaultFilter>) -> BackendResult<FaultsResult> {
        let category = filter.and_then(|f| f.category.as_deref());

        let faults = if let Some(cat) = category {
            self.client
                .get_faults_filtered(&self.component_id, Some(cat))
                .await
                .map_err(Self::map_err)?
        } else {
            self.client
                .get_faults(&self.component_id)
                .await
                .map_err(Self::map_err)?
        };

        let converted: Vec<Fault> = faults
            .into_iter()
            .map(|f| Fault {
                id: f.id,
                code: f.code,
                severity: Self::parse_severity(&f.severity),
                message: f.message,
                category: f.category,
                first_occurrence: None,
                last_occurrence: None,
                occurrence_count: None,
                active: f.active,
                status: None,
                href: f.href,
            })
            .collect();

        Ok(FaultsResult {
            faults: converted,
            status_availability_mask: None,
        })
    }

    async fn get_fault_detail(&self, fault_id: &str) -> BackendResult<Fault> {
        let f = self
            .client
            .get_fault(&self.component_id, fault_id)
            .await
            .map_err(Self::map_err)?;

        Ok(Fault {
            id: f.id,
            code: f.code,
            severity: Self::parse_severity(&f.severity),
            message: f.message,
            category: f.category,
            first_occurrence: None,
            last_occurrence: None,
            occurrence_count: None,
            active: f.active,
            status: None,
            href: f.href,
        })
    }

    async fn clear_faults(&self, _group: Option<u32>) -> BackendResult<ClearFaultsResult> {
        let resp = self
            .client
            .clear_faults(&self.component_id)
            .await
            .map_err(Self::map_err)?;

        Ok(ClearFaultsResult {
            success: resp.success,
            cleared_count: resp.cleared_count.unwrap_or(0),
            message: resp.message.unwrap_or_else(|| "Faults cleared".to_string()),
        })
    }

    // =========================================================================
    // Operations
    // =========================================================================

    async fn list_operations(&self) -> BackendResult<Vec<OperationInfo>> {
        let ops = self
            .client
            .list_operations(&self.component_id)
            .await
            .map_err(Self::map_err)?;

        let prefix = self.sub_entity_prefix.as_deref();
        let converted = ops
            .into_iter()
            .filter_map(|op| {
                let id = if let Some(pfx) = prefix {
                    routing::strip_entity_prefix(&op.id, pfx)?
                } else {
                    op.id
                };
                Some(OperationInfo {
                    id,
                    name: op.name,
                    description: op.description,
                    parameters: Vec::new(),
                    requires_security: op.requires_security,
                    security_level: op.security_level,
                    href: op.href,
                })
            })
            .collect();

        Ok(converted)
    }

    async fn start_operation(
        &self,
        operation_id: &str,
        params: &[u8],
    ) -> BackendResult<OperationExecution> {
        let params_str = if params.is_empty() {
            None
        } else {
            Some(hex::encode(params))
        };

        let prefixed = routing::prefixed_id(operation_id, self.sub_entity_prefix.as_deref());
        let resp = self
            .client
            .execute_operation(
                &self.component_id,
                &prefixed,
                "start",
                params_str.as_deref(),
            )
            .await
            .map_err(Self::map_err)?;

        let status = match resp.status {
            sovd_client::OperationStatus::Pending => OperationStatus::Pending,
            sovd_client::OperationStatus::Running => OperationStatus::Running,
            sovd_client::OperationStatus::Completed => OperationStatus::Completed,
            sovd_client::OperationStatus::Failed => OperationStatus::Failed,
            sovd_client::OperationStatus::Cancelled => OperationStatus::Cancelled,
        };

        let now = chrono::Utc::now();
        let completed_at =
            if status == OperationStatus::Completed || status == OperationStatus::Failed {
                Some(now)
            } else {
                None
            };

        Ok(OperationExecution {
            execution_id: resp.operation_id.clone(),
            operation_id: resp.operation_id,
            status,
            result: resp.result_data.map(serde_json::Value::String),
            error: resp.error,
            started_at: now,
            completed_at,
        })
    }

    // =========================================================================
    // I/O Control (Outputs)
    // =========================================================================

    async fn list_outputs(&self) -> BackendResult<Vec<OutputInfo>> {
        let outputs = self
            .client
            .list_outputs(&self.component_id)
            .await
            .map_err(Self::map_err)?;

        let prefix = self.sub_entity_prefix.as_deref();
        let converted = outputs
            .into_iter()
            .filter_map(|o| {
                let id = if let Some(pfx) = prefix {
                    routing::strip_entity_prefix(&o.id, pfx)?
                } else {
                    o.id.clone()
                };
                Some(OutputInfo {
                    id: id.clone(),
                    name: o.name.unwrap_or_default(),
                    output_id: id,
                    requires_security: o.requires_security.unwrap_or(false),
                    security_level: o.security_level.unwrap_or(0),
                    href: o.href.unwrap_or_default(),
                    data_type: o.data_type,
                    unit: None,
                })
            })
            .collect();

        Ok(converted)
    }

    async fn get_output(&self, output_id: &str) -> BackendResult<OutputDetail> {
        let prefixed = routing::prefixed_id(output_id, self.sub_entity_prefix.as_deref());
        let o = self
            .client
            .get_output(&self.component_id, &prefixed)
            .await
            .map_err(Self::map_err)?;

        Ok(OutputDetail {
            id: o.id.clone(),
            name: o.name.unwrap_or_default(),
            output_id: o.id,
            current_value: o.current_value.unwrap_or_else(|| "00".to_string()),
            default_value: o.default_value.unwrap_or_else(|| "00".to_string()),
            controlled_by_tester: o.controlled_by_tester.unwrap_or(false),
            frozen: o.frozen.unwrap_or(false),
            requires_security: o.requires_security.unwrap_or(false),
            security_level: o.security_level.unwrap_or(0),
            value: o.value,
            default: o.default,
            data_type: o.data_type,
            unit: None,
            min: None,
            max: None,
            allowed: o
                .allowed
                .unwrap_or_default()
                .into_iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect(),
        })
    }

    async fn control_output(
        &self,
        output_id: &str,
        action: IoControlAction,
        value: Option<serde_json::Value>,
    ) -> BackendResult<IoControlResult> {
        let action_str = match action {
            IoControlAction::ShortTermAdjust => "short_term_adjust",
            IoControlAction::ReturnToEcu => "return_to_ecu",
            IoControlAction::ResetToDefault => "reset_to_default",
            IoControlAction::Freeze => "freeze",
        };

        let prefixed = routing::prefixed_id(output_id, self.sub_entity_prefix.as_deref());
        let resp = self
            .client
            .control_output(&self.component_id, &prefixed, action_str, value)
            .await
            .map_err(Self::map_err)?;

        Ok(IoControlResult {
            output_id: resp.output_id,
            action: action_str.to_string(),
            success: resp.success,
            controlled_by_tester: resp.controlled_by_tester,
            frozen: resp.frozen,
            new_value: resp.new_value,
            value: resp.value,
            error: resp.error,
        })
    }

    // =========================================================================
    // Sub-entities
    // =========================================================================

    async fn get_sub_entity(&self, id: &str) -> BackendResult<Arc<dyn DiagnosticBackend>> {
        // Fetch sub-entity detail (includes capabilities per §6.4)
        let app = self
            .client
            .get_app(&self.component_id, id)
            .await
            .map_err(Self::map_err)?;

        let entity_info = EntityInfo {
            id: id.to_string(),
            name: app.name,
            entity_type: app.app_type.unwrap_or_else(|| "ecu".to_string()),
            description: app.description,
            href: app.href.unwrap_or_default(),
            status: app.status,
        };

        let capabilities = to_capabilities(app.capabilities.unwrap_or_default());

        // Create a sub-proxy that routes through the same remote component
        // but with a sub_entity_prefix so session/security calls are targeted
        Ok(Arc::new(SovdProxyBackend {
            client: self.client.clone(),
            component_id: self.component_id.clone(),
            sub_entity_prefix: Some(id.to_string()),
            entity_info,
            capabilities,
        }))
    }

    async fn list_sub_entities(&self) -> BackendResult<Vec<EntityInfo>> {
        let apps = self
            .client
            .list_apps(&self.component_id)
            .await
            .map_err(Self::map_err)?;

        let entities = apps
            .into_iter()
            .map(|app| EntityInfo {
                id: app.id,
                name: app.name,
                entity_type: app.app_type.unwrap_or_else(|| "ecu".to_string()),
                description: app.description,
                href: app.href.unwrap_or_default(),
                status: app.status,
            })
            .collect();

        Ok(entities)
    }

    // =========================================================================
    // Logs
    // =========================================================================

    async fn get_logs(&self, _filter: &LogFilter) -> BackendResult<Vec<LogEntry>> {
        let resp = self
            .client
            .get_logs(&self.component_id)
            .await
            .map_err(Self::map_err)?;

        let entries = resp
            .items
            .into_iter()
            .map(|l| LogEntry {
                id: l.id,
                timestamp: Self::parse_timestamp(&l.timestamp),
                priority: Self::parse_log_priority(&l.priority),
                message: l.message,
                source: l.source,
                pid: l.pid,
                fields: l.metadata,
                log_type: l.log_type,
                size: l.size,
                status: None,
                href: l.href,
                metadata: None,
            })
            .collect();

        Ok(entries)
    }

    async fn get_log(&self, log_id: &str) -> BackendResult<LogEntry> {
        let l = self
            .client
            .get_log(&self.component_id, log_id)
            .await
            .map_err(Self::map_err)?;

        Ok(LogEntry {
            id: l.id,
            timestamp: Self::parse_timestamp(&l.timestamp),
            priority: Self::parse_log_priority(&l.priority),
            message: l.message,
            source: l.source,
            pid: l.pid,
            fields: l.metadata,
            log_type: l.log_type,
            size: l.size,
            status: None,
            href: l.href,
            metadata: None,
        })
    }

    async fn get_log_content(&self, log_id: &str) -> BackendResult<Vec<u8>> {
        self.client
            .get_log_content(&self.component_id, log_id)
            .await
            .map_err(Self::map_err)
    }

    async fn delete_log(&self, log_id: &str) -> BackendResult<()> {
        self.client
            .delete_log(&self.component_id, log_id)
            .await
            .map_err(Self::map_err)
    }

    // =========================================================================
    // Mode Control
    // =========================================================================

    async fn get_session_mode(&self) -> BackendResult<SessionMode> {
        let target = self.sub_entity_prefix.as_deref();
        let resp = self
            .client
            .get_mode_targeted(&self.component_id, "session", target)
            .await
            .map_err(Self::map_err)?;

        let session_name = resp
            .value
            .as_ref()
            .and_then(|v| v.as_str())
            .unwrap_or("default");

        let session_type: sovd_client::SessionType = session_name
            .parse()
            .unwrap_or(sovd_client::SessionType::Default);

        Ok(SessionMode {
            mode: "session".to_string(),
            session: session_name.to_string(),
            session_id: session_type.as_uds_byte(),
        })
    }

    async fn set_session_mode(&self, session: &str) -> BackendResult<SessionMode> {
        let session_type: sovd_client::SessionType = session
            .parse()
            .map_err(|e: String| BackendError::InvalidRequest(e))?;

        let target = self.sub_entity_prefix.as_deref();
        let body = serde_json::json!({ "value": session });
        self.client
            .set_mode_targeted(&self.component_id, "session", body, target)
            .await
            .map_err(Self::map_err)?;

        Ok(SessionMode {
            mode: "session".to_string(),
            session: session.to_string(),
            session_id: session_type.as_uds_byte(),
        })
    }

    async fn get_security_mode(&self) -> BackendResult<SecurityMode> {
        let target = self.sub_entity_prefix.as_deref();
        let resp = self
            .client
            .get_mode_targeted(&self.component_id, "security", target)
            .await
            .map_err(Self::map_err)?;

        let value_str = resp
            .value
            .as_ref()
            .and_then(|v| v.as_str())
            .unwrap_or("locked");

        let (state, level) = if value_str.starts_with("level") {
            // Parse level number from "level1", "level2", etc.
            let level_num = value_str
                .strip_prefix("level")
                .and_then(|s| s.parse::<u8>().ok());
            (SecurityState::Unlocked, level_num)
        } else if value_str.contains("unlocked") {
            (SecurityState::Unlocked, Some(1))
        } else if value_str.contains("seedavailable") {
            let level_num = value_str
                .strip_prefix("level")
                .and_then(|s| s.strip_suffix("_seedavailable"))
                .and_then(|s| s.parse::<u8>().ok());
            (SecurityState::SeedAvailable, level_num)
        } else if resp.seed.is_some() {
            (SecurityState::SeedAvailable, None)
        } else {
            (SecurityState::Locked, None)
        };

        Ok(SecurityMode {
            mode: "security".to_string(),
            state,
            level,
            available_levels: Some(vec![1]),
            seed: resp.seed.map(|s| s.to_string()),
        })
    }

    async fn set_security_mode(
        &self,
        value: &str,
        key: Option<&[u8]>,
    ) -> BackendResult<SecurityMode> {
        let target = self.sub_entity_prefix.as_deref();
        if value.contains("requestseed") {
            let level = sovd_client::SecurityLevel::LEVEL_1;
            let seed = self
                .client
                .security_access_request_seed_targeted(&self.component_id, level, target)
                .await
                .map_err(Self::map_err)?;

            let seed_hex = hex::encode(&seed);
            Ok(SecurityMode {
                mode: "security".to_string(),
                state: SecurityState::SeedAvailable,
                level: Some(1),
                available_levels: Some(vec![1]),
                seed: Some(seed_hex),
            })
        } else if let Some(key_bytes) = key {
            let level = sovd_client::SecurityLevel::LEVEL_1;
            self.client
                .security_access_send_key_targeted(&self.component_id, level, key_bytes, target)
                .await
                .map_err(Self::map_err)?;

            Ok(SecurityMode {
                mode: "security".to_string(),
                state: SecurityState::Unlocked,
                level: Some(1),
                available_levels: Some(vec![1]),
                seed: None,
            })
        } else {
            let body = serde_json::json!({ "value": value });
            let resp = self
                .client
                .set_mode_targeted(&self.component_id, "security", body, target)
                .await
                .map_err(Self::map_err)?;

            let state = if resp.seed.is_some() {
                SecurityState::SeedAvailable
            } else {
                SecurityState::Unlocked
            };

            Ok(SecurityMode {
                mode: "security".to_string(),
                state,
                level: None,
                available_levels: Some(vec![1]),
                seed: resp.seed.map(|s| s.to_string()),
            })
        }
    }

    // =========================================================================
    // Package Management (proxied to upstream)
    // =========================================================================

    async fn receive_package(&self, data: &[u8]) -> BackendResult<String> {
        let url = self.flash_url("/files")?;
        tracing::info!(url = %url, size = data.len(), "Proxy: uploading package");

        let response = self
            .client
            .http_client()
            .post(&url)
            .body(data.to_vec())
            .send()
            .await
            .map_err(|e| BackendError::Transport(e.to_string()))?;

        if !response.status().is_success() {
            return Err(Self::map_response_error(response).await);
        }

        let resp: UploadFileResp = response.json().await.map_err(|e| {
            BackendError::Protocol(format!("Failed to parse upload response: {}", e))
        })?;

        Ok(resp.file_id)
    }

    async fn list_packages(&self) -> BackendResult<Vec<PackageInfo>> {
        let url = self.flash_url("/files")?;
        let response = self
            .client
            .http_client()
            .get(&url)
            .send()
            .await
            .map_err(|e| BackendError::Transport(e.to_string()))?;

        if !response.status().is_success() {
            return Err(Self::map_response_error(response).await);
        }

        let resp: ListFilesResp = response
            .json()
            .await
            .map_err(|e| BackendError::Protocol(format!("Failed to parse files list: {}", e)))?;

        Ok(resp.files)
    }

    async fn get_package(&self, package_id: &str) -> BackendResult<PackageInfo> {
        let url = self.flash_url(&format!("/files/{}", package_id))?;
        let response = self
            .client
            .http_client()
            .get(&url)
            .send()
            .await
            .map_err(|e| BackendError::Transport(e.to_string()))?;

        if !response.status().is_success() {
            return Err(Self::map_response_error(response).await);
        }

        response
            .json()
            .await
            .map_err(|e| BackendError::Protocol(format!("Failed to parse package info: {}", e)))
    }

    async fn verify_package(&self, package_id: &str) -> BackendResult<VerifyResult> {
        let url = self.flash_url(&format!("/files/{}/verify", package_id))?;
        let response = self
            .client
            .http_client()
            .post(&url)
            .send()
            .await
            .map_err(|e| BackendError::Transport(e.to_string()))?;

        if !response.status().is_success() {
            return Err(Self::map_response_error(response).await);
        }

        response
            .json()
            .await
            .map_err(|e| BackendError::Protocol(format!("Failed to parse verify result: {}", e)))
    }

    async fn delete_package(&self, package_id: &str) -> BackendResult<()> {
        let url = self.flash_url(&format!("/files/{}", package_id))?;
        let response = self
            .client
            .http_client()
            .delete(&url)
            .send()
            .await
            .map_err(|e| BackendError::Transport(e.to_string()))?;

        if !response.status().is_success() {
            return Err(Self::map_response_error(response).await);
        }

        Ok(())
    }

    // =========================================================================
    // Flash Transfer (proxied to upstream)
    // =========================================================================

    async fn start_flash(&self, package_id: &str) -> BackendResult<String> {
        let url = self.flash_url("/flash/transfer")?;
        let body = serde_json::json!({ "file_id": package_id });

        tracing::info!(url = %url, package_id = %package_id, "Proxy: starting flash transfer");

        let response = self
            .client
            .http_client()
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| BackendError::Transport(e.to_string()))?;

        if !response.status().is_success() {
            return Err(Self::map_response_error(response).await);
        }

        let resp: StartFlashResp = response.json().await.map_err(|e| {
            BackendError::Protocol(format!("Failed to parse flash response: {}", e))
        })?;

        Ok(resp.transfer_id)
    }

    async fn get_flash_status(&self, transfer_id: &str) -> BackendResult<FlashStatus> {
        let url = self.flash_url(&format!("/flash/transfer/{}", transfer_id))?;
        let response = self
            .client
            .http_client()
            .get(&url)
            .send()
            .await
            .map_err(|e| BackendError::Transport(e.to_string()))?;

        if !response.status().is_success() {
            return Err(Self::map_response_error(response).await);
        }

        response
            .json()
            .await
            .map_err(|e| BackendError::Protocol(format!("Failed to parse flash status: {}", e)))
    }

    async fn list_flash_transfers(&self) -> BackendResult<Vec<FlashStatus>> {
        let url = self.flash_url("/flash/transfer")?;
        let response = self
            .client
            .http_client()
            .get(&url)
            .send()
            .await
            .map_err(|e| BackendError::Transport(e.to_string()))?;

        if !response.status().is_success() {
            return Err(Self::map_response_error(response).await);
        }

        let resp: ListTransfersResp = response.json().await.map_err(|e| {
            BackendError::Protocol(format!("Failed to parse transfers list: {}", e))
        })?;

        Ok(resp.transfers)
    }

    async fn abort_flash(&self, transfer_id: &str) -> BackendResult<()> {
        let url = self.flash_url(&format!("/flash/transfer/{}", transfer_id))?;
        let response = self
            .client
            .http_client()
            .delete(&url)
            .send()
            .await
            .map_err(|e| BackendError::Transport(e.to_string()))?;

        if !response.status().is_success() {
            return Err(Self::map_response_error(response).await);
        }

        Ok(())
    }

    async fn finalize_flash(&self) -> BackendResult<()> {
        let url = self.flash_url("/flash/transferexit")?;
        let response = self
            .client
            .http_client()
            .put(&url)
            .send()
            .await
            .map_err(|e| BackendError::Transport(e.to_string()))?;

        if !response.status().is_success() {
            return Err(Self::map_response_error(response).await);
        }

        Ok(())
    }

    async fn commit_flash(&self) -> BackendResult<()> {
        let url = self.flash_url("/flash/commit")?;
        let response = self
            .client
            .http_client()
            .post(&url)
            .send()
            .await
            .map_err(|e| BackendError::Transport(e.to_string()))?;

        if !response.status().is_success() {
            return Err(Self::map_response_error(response).await);
        }

        Ok(())
    }

    async fn rollback_flash(&self) -> BackendResult<()> {
        let url = self.flash_url("/flash/rollback")?;
        let response = self
            .client
            .http_client()
            .post(&url)
            .send()
            .await
            .map_err(|e| BackendError::Transport(e.to_string()))?;

        if !response.status().is_success() {
            return Err(Self::map_response_error(response).await);
        }

        Ok(())
    }

    async fn get_activation_state(&self) -> BackendResult<ActivationState> {
        let url = self.flash_url("/flash/activation")?;
        let response = self
            .client
            .http_client()
            .get(&url)
            .send()
            .await
            .map_err(|e| BackendError::Transport(e.to_string()))?;

        if !response.status().is_success() {
            return Err(Self::map_response_error(response).await);
        }

        response
            .json()
            .await
            .map_err(|e| BackendError::Protocol(format!("Failed to parse activation state: {}", e)))
    }

    // =========================================================================
    // Software Info -- Not yet supported via client
    // =========================================================================

    async fn get_software_info(&self) -> BackendResult<sovd_core::SoftwareInfo> {
        Err(BackendError::NotSupported(
            "get_software_info (proxy phase 2)".to_string(),
        ))
    }
}
