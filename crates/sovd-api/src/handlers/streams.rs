//! Streaming handlers for SOVD API
//!
//! Provides SSE (Server-Sent Events) streaming for real-time data subscriptions.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::Json;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use crate::error::ApiError;
use crate::state::AppState;

/// Request to create a subscription
#[derive(Debug, Deserialize)]
pub struct CreateSubscriptionRequest {
    /// Parameter IDs to subscribe to
    pub parameters: Vec<String>,
    /// Desired update rate in Hz
    #[serde(default = "default_rate")]
    pub rate_hz: u32,
}

fn default_rate() -> u32 {
    10
}

/// Response for subscription creation
#[derive(Debug, Serialize)]
pub struct SubscriptionResponse {
    /// Subscription ID
    pub subscription_id: String,
    /// URL to connect to the stream
    pub stream_url: String,
    /// Parameters included
    pub parameters: Vec<String>,
    /// Actual rate in Hz
    pub rate_hz: u32,
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

/// SSE event format expected by tests
/// Contains seq, ts, and flattened parameter values
#[derive(Debug, Serialize)]
struct StreamEvent {
    /// Unix timestamp in milliseconds
    ts: i64,
    /// Sequence number
    seq: u64,
    /// Flattened parameter values
    #[serde(flatten)]
    values: HashMap<String, serde_json::Value>,
}

/// POST /vehicle/v1/components/:component_id/subscriptions
/// Create a new subscription and return subscription details
pub async fn create_subscription(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
    Json(request): Json<CreateSubscriptionRequest>,
) -> Result<(StatusCode, Json<SubscriptionResponse>), ApiError> {
    let backend = state.get_backend(&component_id)?;

    // Create subscription via backend
    let _receiver = backend
        .subscribe_data(&request.parameters, request.rate_hz)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    // Generate subscription ID
    let subscription_id = uuid::Uuid::new_v4().to_string();

    Ok((
        StatusCode::CREATED,
        Json(SubscriptionResponse {
            subscription_id: subscription_id.clone(),
            stream_url: format!(
                "/vehicle/v1/components/{}/streams/{}",
                component_id, subscription_id
            ),
            parameters: request.parameters,
            rate_hz: request.rate_hz,
        }),
    ))
}

/// GET /vehicle/v1/streams/:subscription_id
/// Stream data for a global subscription (created via /vehicle/v1/subscriptions)
pub async fn stream_subscription(
    State(state): State<AppState>,
    Path(subscription_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    // Look up the subscription
    let subscription = state
        .subscription_manager
        .get(&subscription_id)
        .await
        .ok_or_else(|| {
            ApiError::NotFound(format!("Subscription not found: {}", subscription_id))
        })?;

    // Get the backend for this subscription's component
    let backend = state.get_backend(&subscription.component_id)?;

    // Resolve parameter names to DID hex strings for the backend
    // Also build a reverse mapping from DID to parameter name and numeric DID
    let did_store = state.did_store_arc();
    let mut dids: Vec<String> = Vec::new();
    let mut did_to_info: HashMap<String, (String, u16)> = HashMap::new(); // DID str -> (param name, DID u16)

    for param in &subscription.parameters {
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
        .subscribe_data(&dids, subscription.rate_hz)
        .await
        .map_err(|e| {
            tracing::error!(?e, dids = ?dids, rate_hz = subscription.rate_hz, "subscribe_data failed");
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
                let ts = Utc::now().timestamp_millis();

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

                let mut values = HashMap::new();
                values.insert(param_name, converted_value);

                let event = StreamEvent { ts, seq, values };

                Some(Ok::<_, Infallible>(
                    Event::default().data(serde_json::to_string(&event).unwrap_or_default()),
                ))
            }
            Err(_) => None, // Skip lagged messages
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
                let ts = Utc::now().timestamp_millis();

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

                let mut values = HashMap::new();
                values.insert(param_name, converted_value);

                let event = StreamEvent { ts, seq, values };

                Some(Ok::<_, Infallible>(
                    Event::default().data(serde_json::to_string(&event).unwrap_or_default()),
                ))
            }
            Err(_) => None, // Skip lagged messages
        }
    });

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}
