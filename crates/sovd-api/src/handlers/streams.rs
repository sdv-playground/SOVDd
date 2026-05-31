//! Streaming handlers for SOVD API
//!
//! Provides SSE (Server-Sent Events) streaming for real-time data subscriptions.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use crate::error::ApiError;
use crate::state::AppState;

fn default_rate() -> u32 {
    10
}

/// Query parameters for inline streaming
#[derive(Debug, Deserialize)]
pub struct StreamQuery {
    /// Comma-separated parameter IDs
    pub parameters: String,
    /// Update rate in Hz
    #[serde(default = "default_rate")]
    pub rate_hz: u32,
}

/// SSE EventEnvelope — ISO 17978-3 §5.6 Table 5.
///
/// Each event SHALL carry one envelope via the SSE `data:` line.
/// `payload` is the success payload (`AnyValue`); `error` is mutually
/// exclusive and carries a `GenericError` if the publisher hit a
/// transient producer error.
#[derive(Debug, Serialize)]
struct StreamEvent {
    /// RFC 3339 UTC time the server emitted this event (C-050).
    timestamp: String,
    /// Conditional success payload.
    #[serde(skip_serializing_if = "Option::is_none")]
    payload: Option<serde_json::Value>,
    /// Conditional error payload (mutually exclusive with `payload`).
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<sovd_core::GenericError>,
}

/// GET /vehicle/v1/components/:component_id/streams/:subscription_id
///
/// SSE delivery for a cyclic subscription created via
/// `POST .../cyclic-subscriptions` (ISO 17978-3 §7.10).
pub async fn stream_subscription(
    State(state): State<AppState>,
    Path((component_id, subscription_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    // Look up the cyclic subscription.
    let subscription = state
        .subscription_manager
        .get(&subscription_id)
        .await
        .ok_or_else(|| {
            ApiError::NotFound(format!("Subscription not found: {}", subscription_id))
        })?;

    if subscription.component_id != component_id {
        return Err(ApiError::NotFound(format!(
            "Subscription {} not registered on component {}",
            subscription_id, component_id
        )));
    }

    let backend = state.get_backend(&subscription.component_id)?;

    // Spec subscriptions carry a single `resource` (path or param-id).
    // Resolve it against DidStore the same way as the old multi-param
    // flow — DID hex strings pass through unchanged.
    let did_store = state.did_store_arc();
    let mut did_to_info: HashMap<String, (String, u16)> = HashMap::new();
    let resource_param = subscription.resource.clone();
    let did_str = if let Some(did) = did_store.resolve_did(&resource_param) {
        let did_str = format!("{:04X}", did);
        did_to_info.insert(did_str.clone(), (resource_param.clone(), did));
        did_str
    } else {
        did_to_info.insert(resource_param.clone(), (resource_param.clone(), 0));
        resource_param.clone()
    };

    let rate_hz = subscription.interval.rate_hz();
    let receiver = backend
        .subscribe_data(std::slice::from_ref(&did_str), rate_hz)
        .await
        .map_err(|e| {
            tracing::error!(?e, did = %did_str, rate_hz, "subscribe_data failed");
            ApiError::from(e)
        })?;

    // Sequence counter for events
    let seq_counter = Arc::new(AtomicU64::new(1));

    // Convert to SSE stream with expected format
    let stream = BroadcastStream::new(receiver).filter_map(move |result| {
        let did_to_info = did_to_info.clone();
        let seq_counter = seq_counter.clone();
        let did_store = did_store.clone();

        match result {
            Ok(data_point) => {
                let seq = seq_counter.fetch_add(1, Ordering::SeqCst);
                let timestamp = Utc::now().to_rfc3339();

                // Look up parameter name and DID from the data point ID
                let (param_name, did) = did_to_info
                    .get(&data_point.id)
                    .cloned()
                    .unwrap_or_else(|| (data_point.id.clone(), 0));

                // Convert hex value to typed value using DidStore
                let converted_value = if let Some(hex_str) = data_point.value.as_str() {
                    if let Ok(bytes) = hex::decode(hex_str) {
                        if did != 0 {
                            did_store.decode_or_raw(did, &bytes)
                        } else {
                            data_point.value
                        }
                    } else {
                        data_point.value
                    }
                } else {
                    data_point.value
                };

                // EventEnvelope.payload: {seq, values{<param>: <val>}}
                let payload = serde_json::json!({
                    "seq": seq,
                    "values": { param_name: converted_value },
                });
                let event = StreamEvent {
                    timestamp,
                    payload: Some(payload),
                    error: None,
                };

                Some(Ok::<_, Infallible>(
                    Event::default().data(serde_json::to_string(&event).unwrap_or_default()),
                ))
            }
            Err(lag) => {
                // Broadcast lag — consumer can't keep up.  Spec
                // EventEnvelope (Table 5) carries error events; surface
                // the lag rather than dropping it silently.
                let timestamp = Utc::now().to_rfc3339();
                let err = sovd_core::GenericError::vendor(
                    "broadcast-lag",
                    format!("subscriber lagged behind producer ({})", lag),
                );
                let event = StreamEvent {
                    timestamp,
                    payload: None,
                    error: Some(err),
                };
                Some(Ok::<_, Infallible>(
                    Event::default().data(serde_json::to_string(&event).unwrap_or_default()),
                ))
            }
        }
    });

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

/// GET /vehicle/v1/components/:component_id/streams
/// Stream data using query parameters (inline subscription)
///
/// Example: GET /vehicle/v1/components/engine_ecu/streams?parameters=engine_rpm,coolant_temp&rate_hz=10
/// Gateway example: GET /vehicle/v1/components/vehicle_gateway/streams?parameters=vtx_ecm/coolant_temp&rate_hz=10
pub async fn stream_data(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
    Query(query): Query<StreamQuery>,
) -> Result<impl IntoResponse, ApiError> {
    // Parse parameter IDs
    let param_ids: Vec<String> = query
        .parameters
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if param_ids.is_empty() {
        return Err(ApiError::BadRequest("No parameters specified".to_string()));
    }

    let did_store = state.did_store_arc();

    // Check if parameters have gateway prefix (e.g., "vtx_ecm/coolant_temp")
    // If so, route to the child backend via the gateway's sub-entities
    let first_param = &param_ids[0];
    if let Some((child_backend_id, _child_param)) = first_param.split_once('/') {
        // Gateway routing: get the child backend from gateway's sub-entities
        let gateway_backend = state.get_backend(&component_id)?;
        let child_backend = gateway_backend
            .get_sub_entity(child_backend_id)
            .await
            .map_err(|_| {
                ApiError::NotFound(format!("Sub-entity not found: {}", child_backend_id))
            })?;

        // Resolve all parameters for the child backend (strip prefix)
        let mut dids: Vec<String> = Vec::new();
        let mut did_to_info: HashMap<String, (String, u16)> = HashMap::new();

        for param in &param_ids {
            let (_, child_param_name) = param.split_once('/').unwrap_or(("", param));
            if let Some(did) = did_store.resolve_did(child_param_name) {
                let did_str = format!("{:04X}", did);
                dids.push(did_str.clone());
                did_to_info.insert(did_str, (child_param_name.to_string(), did));
            } else {
                dids.push(child_param_name.to_string());
                did_to_info.insert(
                    child_param_name.to_string(),
                    (child_param_name.to_string(), 0),
                );
            }
        }

        // Subscribe to data via child backend
        let receiver = child_backend
            .subscribe_data(&dids, query.rate_hz)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;

        return create_sse_stream(receiver, did_to_info, did_store);
    }

    // Regular component: direct access
    let backend = state.get_backend(&component_id)?;

    // Resolve parameter names to DID hex strings for the backend
    // Also build a reverse mapping from DID to parameter name and numeric DID
    let mut dids: Vec<String> = Vec::new();
    let mut did_to_info: HashMap<String, (String, u16)> = HashMap::new();

    for param in &param_ids {
        if let Some(did) = did_store.resolve_did(param) {
            let did_str = format!("{:04X}", did);
            dids.push(did_str.clone());
            did_to_info.insert(did_str, (param.clone(), did));
        } else {
            // Try treating as hex DID directly
            dids.push(param.clone());
            did_to_info.insert(param.clone(), (param.clone(), 0));
        }
    }

    // Subscribe to data using resolved DIDs
    let receiver = backend
        .subscribe_data(&dids, query.rate_hz)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    create_sse_stream(receiver, did_to_info, did_store)
}

/// Helper to create SSE stream from a broadcast receiver
fn create_sse_stream(
    receiver: tokio::sync::broadcast::Receiver<sovd_core::DataPoint>,
    did_to_info: HashMap<String, (String, u16)>,
    did_store: Arc<sovd_conv::DidStore>,
) -> Result<impl IntoResponse, ApiError> {
    // Sequence counter for events
    let seq_counter = Arc::new(AtomicU64::new(1));

    // Convert to SSE stream with expected format
    let stream = BroadcastStream::new(receiver).filter_map(move |result| {
        let did_to_info = did_to_info.clone();
        let seq_counter = seq_counter.clone();
        let did_store = did_store.clone();

        match result {
            Ok(data_point) => {
                let seq = seq_counter.fetch_add(1, Ordering::SeqCst);
                let timestamp = Utc::now().to_rfc3339();

                // Look up parameter name and DID from the data point ID
                let (param_name, did) = did_to_info
                    .get(&data_point.id)
                    .cloned()
                    .unwrap_or_else(|| (data_point.id.clone(), 0));

                // Convert hex value to typed value using DidStore
                let converted_value = if let Some(hex_str) = data_point.value.as_str() {
                    if let Ok(bytes) = hex::decode(hex_str) {
                        if did != 0 {
                            did_store.decode_or_raw(did, &bytes)
                        } else {
                            data_point.value
                        }
                    } else {
                        data_point.value
                    }
                } else {
                    data_point.value
                };

                let payload = serde_json::json!({
                    "seq": seq,
                    "values": { param_name: converted_value },
                });
                let event = StreamEvent {
                    timestamp,
                    payload: Some(payload),
                    error: None,
                };

                Some(Ok::<_, Infallible>(
                    Event::default().data(serde_json::to_string(&event).unwrap_or_default()),
                ))
            }
            Err(lag) => {
                // Broadcast lag — consumer can't keep up.  Spec
                // EventEnvelope (Table 5) carries error events; surface
                // the lag rather than dropping it silently.
                let timestamp = Utc::now().to_rfc3339();
                let err = sovd_core::GenericError::vendor(
                    "broadcast-lag",
                    format!("subscriber lagged behind producer ({})", lag),
                );
                let event = StreamEvent {
                    timestamp,
                    payload: None,
                    error: Some(err),
                };
                Some(Ok::<_, Infallible>(
                    Event::default().data(serde_json::to_string(&event).unwrap_or_default()),
                ))
            }
        }
    });

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}
