//! I/O control output handlers (UDS 0x2F InputOutputControlById)

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use sovd_core::{IoControlAction, IoControlResult, OutputDetail, OutputInfo};
use sovd_uds::config::OutputConfig;
use sovd_uds::output_conv;

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Serialize)]
pub struct OutputsListResponse {
    pub items: Vec<OutputInfoResponse>,
}

#[derive(Serialize)]
pub struct OutputInfoResponse {
    pub id: String,
    pub name: String,
    pub output_id: String,
    pub requires_security: bool,
    pub security_level: u8,
    pub href: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

#[derive(Serialize)]
pub struct OutputDetailResponse {
    pub id: String,
    pub name: String,
    pub output_id: String,
    pub current_value: String,
    pub default_value: String,
    pub controlled_by_tester: bool,
    pub frozen: bool,
    pub requires_security: bool,
    pub security_level: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed: Vec<String>,
}

#[derive(Deserialize)]
pub struct IoControlRequest {
    /// Control action: "return_to_ecu", "reset_to_default", "freeze", "short_term_adjust"
    pub action: String,
    /// Value for short_term_adjust (typed JSON value or hex string)
    #[serde(default)]
    pub value: Option<serde_json::Value>,
}

#[derive(Serialize)]
pub struct IoControlResponse {
    pub output_id: String,
    pub action: String,
    pub success: bool,
    /// Whether the output is currently controlled by the tester
    pub controlled_by_tester: bool,
    /// Whether the output value is frozen
    pub frozen: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Get the display name of a DataType
fn data_type_name(dt: &sovd_uds::config::DataType) -> String {
    dt.to_string()
}

/// Enrich an OutputInfoResponse with type metadata from OutputConfig
fn enrich_info(resp: &mut OutputInfoResponse, config: &OutputConfig) {
    if let Some(ref dt) = config.data_type {
        resp.data_type = Some(data_type_name(dt));
    }
    resp.unit = config.unit.clone();
}

/// Enrich an OutputDetailResponse with type metadata and typed values from OutputConfig
fn enrich_detail(resp: &mut OutputDetailResponse, config: &OutputConfig) {
    if let Some(ref dt) = config.data_type {
        resp.data_type = Some(data_type_name(dt));
    }
    resp.unit = config.unit.clone();
    resp.min = config.min;
    resp.max = config.max;
    resp.allowed = config.allowed.clone();

    // Decode current_value to typed JSON
    if config.data_type.is_some() {
        if let Ok(raw) = hex::decode(&resp.current_value) {
            resp.value = Some(output_conv::decode_output_value(config, &raw));
        }
        if let Ok(raw) = hex::decode(&resp.default_value) {
            resp.default = Some(output_conv::decode_output_value(config, &raw));
        }
    }
}

impl From<&OutputInfo> for OutputInfoResponse {
    fn from(o: &OutputInfo) -> Self {
        Self {
            id: o.id.clone(),
            name: o.name.clone(),
            output_id: o.output_id.clone(),
            requires_security: o.requires_security,
            security_level: o.security_level,
            href: o.href.clone(),
            data_type: None,
            unit: None,
        }
    }
}

impl From<&OutputDetail> for OutputDetailResponse {
    fn from(o: &OutputDetail) -> Self {
        Self {
            id: o.id.clone(),
            name: o.name.clone(),
            output_id: o.output_id.clone(),
            current_value: o.current_value.clone(),
            default_value: o.default_value.clone(),
            controlled_by_tester: o.controlled_by_tester,
            frozen: o.frozen,
            requires_security: o.requires_security,
            security_level: o.security_level,
            value: None,
            default: None,
            data_type: None,
            unit: None,
            min: None,
            max: None,
            allowed: Vec::new(),
        }
    }
}

impl From<IoControlResult> for IoControlResponse {
    fn from(r: IoControlResult) -> Self {
        Self {
            output_id: r.output_id,
            action: r.action,
            success: r.success,
            controlled_by_tester: r.controlled_by_tester,
            frozen: r.frozen,
            new_value: r.new_value,
            value: r.value,
            error: r.error,
        }
    }
}

/// Resolve output config for possibly gateway-prefixed output IDs.
/// For a gateway, the output_id may be "child/local_id" or "child/sub/local_id".
/// We try the direct lookup first, then progressively strip prefix segments
/// to find the child ECU's config.
fn resolve_output_config<'a>(
    state: &'a AppState,
    component_id: &str,
    output_id: &str,
) -> Option<&'a OutputConfig> {
    // Direct lookup (works for non-gateway components)
    if let Some(config) = state.get_output_config(component_id, output_id) {
        return Some(config);
    }
    // Try stripping prefix segments: "a/b/c/local" -> try ("a", "b/c/local"), ("b", "c/local"), ("c", "local")
    let mut remaining = output_id;
    while let Some(idx) = remaining.find('/') {
        let child_id = &remaining[..idx];
        let local_id = &remaining[idx + 1..];
        if let Some(config) = state.get_output_config(child_id, local_id) {
            return Some(config);
        }
        remaining = local_id;
    }
    None
}

/// GET /vehicle/v1/components/:component_id/outputs
/// List available I/O outputs
pub async fn list_outputs(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<Json<OutputsListResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;
    let outputs = backend.list_outputs().await?;

    let mut items: Vec<OutputInfoResponse> = outputs.iter().map(OutputInfoResponse::from).collect();

    // Enrich with type metadata from output configs
    for item in &mut items {
        if let Some(config) = resolve_output_config(&state, &component_id, &item.id) {
            enrich_info(item, config);
        }
    }

    Ok(Json(OutputsListResponse { items }))
}

/// GET /vehicle/v1/components/:component_id/outputs/:output_id
/// Get output detail
pub async fn get_output(
    State(state): State<AppState>,
    Path((component_id, output_id)): Path<(String, String)>,
) -> Result<Json<OutputDetailResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;
    let output = backend.get_output(&output_id).await?;

    let mut resp = OutputDetailResponse::from(&output);

    // Enrich with type metadata and typed values
    if let Some(config) = resolve_output_config(&state, &component_id, &output_id) {
        enrich_detail(&mut resp, config);
    }

    Ok(Json(resp))
}

/// POST /vehicle/v1/components/:component_id/outputs/:output_id
/// Control an output
pub async fn control_output(
    State(state): State<AppState>,
    Path((component_id, output_id)): Path<(String, String)>,
    Json(request): Json<IoControlRequest>,
) -> Result<Json<IoControlResponse>, ApiError> {
    let backend = state.get_backend(&component_id)?;

    let action = IoControlAction::from_str(&request.action).ok_or_else(|| {
        ApiError::BadRequest(format!(
            "Invalid action: {}. Use 'return_to_ecu', 'reset_to_default', 'freeze', or 'short_term_adjust'",
            request.action
        ))
    })?;

    // Pass the JSON value directly to the backend — leaf backends (UDS)
    // encode it using their output config; gateways/proxies forward it
    // transparently to the server that owns the config.
    let result = backend
        .control_output(&output_id, action, request.value.clone())
        .await?;

    // Build response
    let mut response = IoControlResponse::from(result);
    response.action = request.action;

    // Decode new_value to typed JSON if local config has type metadata
    let output_config = resolve_output_config(&state, &component_id, &output_id);
    if let Some(config) = output_config {
        if config.data_type.is_some() || !config.allowed.is_empty() {
            if let Some(ref hex_val) = response.new_value {
                if let Ok(raw) = hex::decode(hex_val) {
                    response.value = Some(output_conv::decode_output_value(config, &raw));
                }
            }
        }
    }

    Ok(Json(response))
}
