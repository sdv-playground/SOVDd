//! Cyclic subscription handlers — ISO 17978-3 §7.10.
//!
//! Wire shape:
//!
//!   `POST /vehicle/v1/components/{id}/cyclic-subscriptions`
//!     body: `{resource: "<param-id>", interval, protocol?, duration?}`
//!     → 201 Created + `Location: …/cyclic-subscriptions/{id}` + the
//!       created `CyclicSubscription` body.
//!
//!   `GET  /vehicle/v1/components/{id}/cyclic-subscriptions`
//!     → list of `CyclicSubscription`.
//!
//!   `GET  /vehicle/v1/components/{id}/cyclic-subscriptions/{id}`
//!     → content-negotiated:
//!         * `Accept: text/event-stream` → the SSE event stream (§7.10.3
//!           — the cyclic-subscription resource IS the stream).
//!         * otherwise → the `CyclicSubscription` details (§7.10).
//!
//!   `DELETE /vehicle/v1/components/{id}/cyclic-subscriptions/{id}`
//!     → 204 No Content.
//!
//! There is no separate `streams` resource: per ISO 17978-3 §7.10.3 the
//! temporary subscription resource the create returns (`Location:
//! …/cyclic-subscriptions/{id}`) is itself the SSE stream — clients
//! attach with `Accept: text/event-stream`. C-025: only standardized
//! collection/resource names appear on the entity (Tables 8 & 10);
//! `cyclic-subscriptions` is one such name, the retired `streams` was
//! not. Single resource per subscription per spec; for multi-parameter
//! consumers, open N subscriptions and join the streams client-side.

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::Json;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;
use uuid::Uuid;

use crate::error::ApiError;
use crate::state::AppState;

/// Manager for per-component cyclic subscriptions.
#[derive(Debug, Default)]
pub struct SubscriptionManager {
    subscriptions: RwLock<HashMap<String, CyclicSubscription>>,
}

impl SubscriptionManager {
    pub fn new() -> Self {
        Self {
            subscriptions: RwLock::new(HashMap::new()),
        }
    }

    pub async fn create(
        &self,
        component_id: String,
        request: CyclicSubscriptionRequest,
    ) -> CyclicSubscription {
        let subscription_id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let expires_at = request
            .duration
            .map(|secs| now + Duration::seconds(secs as i64));

        let subscription = CyclicSubscription {
            subscription_id: subscription_id.clone(),
            component_id,
            resource: request.resource,
            interval: request.interval,
            protocol: request.protocol.unwrap_or_else(default_protocol),
            status: "active".to_string(),
            created_at: now,
            expires_at,
        };

        self.subscriptions
            .write()
            .await
            .insert(subscription_id, subscription.clone());

        subscription
    }

    pub async fn get(&self, subscription_id: &str) -> Option<CyclicSubscription> {
        self.subscriptions
            .read()
            .await
            .get(subscription_id)
            .cloned()
    }

    pub async fn list_for_component(&self, component_id: &str) -> Vec<CyclicSubscription> {
        self.subscriptions
            .read()
            .await
            .values()
            .filter(|s| s.component_id == component_id)
            .cloned()
            .collect()
    }

    pub async fn delete(&self, subscription_id: &str) -> bool {
        self.subscriptions
            .write()
            .await
            .remove(subscription_id)
            .is_some()
    }

    /// Update the cadence and/or duration of an existing subscription
    /// in place.  `resource` and `protocol` cannot change — those
    /// require a new subscription resource.  Returns the updated row
    /// when found.
    pub async fn update(
        &self,
        subscription_id: &str,
        interval: Option<SubscriptionInterval>,
        duration: Option<u32>,
    ) -> Option<CyclicSubscription> {
        let mut guard = self.subscriptions.write().await;
        let sub = guard.get_mut(subscription_id)?;
        if let Some(i) = interval {
            sub.interval = i;
        }
        if let Some(d) = duration {
            sub.expires_at = Some(sub.created_at + Duration::seconds(d as i64));
        }
        Some(sub.clone())
    }
}

/// A cyclic-subscription resource (spec §7.10).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CyclicSubscription {
    pub subscription_id: String,
    pub component_id: String,
    pub resource: String,
    pub interval: SubscriptionInterval,
    pub protocol: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

/// Spec line 358 — coarse-grained update cadence enum.
///
/// Server maps to concrete polling rates within the spec's ≤500 ms
/// per-event floor (i.e. ≥2 Hz minimum):
///
///   * `fast`   → 20 Hz (50 ms)
///   * `normal` → 5 Hz  (200 ms)
///   * `slow`   → 2 Hz  (500 ms — the spec floor)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubscriptionInterval {
    Fast,
    Normal,
    Slow,
}

impl SubscriptionInterval {
    /// Concrete polling rate this interval maps to.
    pub fn rate_hz(self) -> u32 {
        match self {
            Self::Fast => 20,
            Self::Normal => 5,
            Self::Slow => 2,
        }
    }
}

/// Request body for creating a cyclic subscription.
#[derive(Debug, Deserialize)]
pub struct CyclicSubscriptionRequest {
    /// URI-reference to the parameter being subscribed to (e.g.
    /// `data/coolant_temperature` or `apps/engine_ecu/data/F405`).
    pub resource: String,
    /// Polling cadence.
    pub interval: SubscriptionInterval,
    /// Stream protocol — defaults to `sse`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    /// Optional auto-expiry in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<u32>,
}

/// Request body for `PUT .../cyclic-subscriptions/{id}` — update cadence
/// and/or duration without recreating the subscription (and without
/// losing the SSE stream URL).
#[derive(Debug, Deserialize, Default)]
pub struct UpdateCyclicSubscriptionRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<SubscriptionInterval>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<u32>,
}

fn default_protocol() -> String {
    "sse".to_string()
}

#[derive(Debug, Serialize)]
pub struct CyclicSubscriptionsResponse {
    pub items: Vec<CyclicSubscription>,
}

/// Verify a single data parameter is GET-able on `backend` the same way
/// the data path resolves it (`read_did_internal` / the sub-entity reader):
/// DidStore resolution under `entity_id`, presence in `list_parameters`, or a
/// non-empty `read_data` probe.  Returns `true` if any of those succeed.
async fn param_is_get_able(
    backend: &Arc<dyn sovd_core::DiagnosticBackend>,
    did_store: &sovd_conv::DidStore,
    param: &str,
) -> bool {
    // A DidStore-known parameter (semantic name or raw hex DID) is
    // addressable — the same first step the data reader takes.
    if did_store.resolve_did(param).is_some() {
        return true;
    }
    // Backend-resolved parameter (proxy/app entities): listed or readable.
    if let Ok(params) = backend.list_parameters().await {
        if params.iter().any(|p| p.id == param) {
            return true;
        }
    }
    matches!(
        backend.read_data(std::slice::from_ref(&param.to_string())).await,
        Ok(values) if !values.is_empty()
    )
}

/// C-073 (§7.10.3): the subscribed `resource` must be a same-entity resource
/// the client can GET.  Resolve it exactly the way the SSE delivery path
/// (`stream_subscription`, above) and the data path do:
///
///   * `child/param` → resolve `child` as a sub-entity (same-entity tree),
///     then validate `param` is GET-able on the child.
///   * `param` → validate `param` is GET-able on the addressed entity.
///
/// A `data/` collection segment (the spec's uri-reference form, e.g.
/// `data/coolant_temperature` or `child/data/F405`) is tolerated.  An
/// unresolvable or cross-entity resource is rejected with `400
/// incomplete-request`.
///
/// Returns the *canonical* resource string to persist — the form
/// `stream_subscription` resolves: a bare `param` for direct, `child/param`
/// for gateway-child (the optional `data/` collection segment is stripped so
/// the stream path's `split_once('/')` routes it correctly).
async fn validate_subscription_resource(
    state: &AppState,
    component_id: &str,
    backend: &Arc<dyn sovd_core::DiagnosticBackend>,
    resource: &str,
) -> Result<String, ApiError> {
    let resource = resource.trim_start_matches('/');
    if resource.is_empty() {
        return Err(ApiError::BadRequest(
            "subscription resource must not be empty".into(),
        ));
    }
    let did_store = state.did_store();

    let bad = |r: &str| {
        ApiError::BadRequest(format!(
            "subscription resource {r:?} is not a GET-able same-entity parameter"
        ))
    };

    // The spec uri-reference form addresses the entity's own `data`
    // collection (`data/<param>`); strip that leading collection segment so
    // it isn't mistaken for a gateway-child id by the split below.
    if let Some(param) = resource.strip_prefix("data/") {
        if !param.is_empty()
            && !param.contains('/')
            && param_is_get_able(backend, did_store, param).await
        {
            return Ok(param.to_string());
        }
        return Err(bad(resource));
    }

    // Gateway-child resource: `child/param` (possibly `child/data/param`).
    if let Some((child, rest)) = resource.split_once('/') {
        let param = rest.strip_prefix("data/").unwrap_or(rest);
        if param.is_empty() || param.contains('/') {
            return Err(ApiError::BadRequest(format!(
                "subscription resource {resource:?} is not a single same-entity parameter"
            )));
        }
        let child_backend = backend.get_sub_entity(child).await.map_err(|e| match e {
            sovd_core::BackendError::EntityNotFound(_) => ApiError::BadRequest(format!(
                "subscription resource {resource:?}: sub-entity {child:?} not found on {component_id:?}"
            )),
            other => ApiError::from(other),
        })?;
        if param_is_get_able(&child_backend, did_store, param).await {
            return Ok(format!("{child}/{param}"));
        }
        return Err(ApiError::BadRequest(format!(
            "subscription resource {resource:?} is not GET-able on sub-entity {child:?}"
        )));
    }

    // Bare direct resource on the addressed entity (param-id or hex DID).
    if param_is_get_able(backend, did_store, resource).await {
        return Ok(resource.to_string());
    }
    Err(bad(resource))
}

/// POST /vehicle/v1/components/:component_id/cyclic-subscriptions
pub async fn create_cyclic_subscription(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
    Json(mut request): Json<CyclicSubscriptionRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let backend = state.get_backend(&component_id)?;

    // Spec: duration=0 makes the subscription expire instantly and
    // is nonsensical; reject as incomplete-request.
    if let Some(0) = request.duration {
        return Err(ApiError::BadRequest(
            "duration must be > 0; omit the field for no expiry".to_string(),
        ));
    }

    // C-073: the subscribed resource must be same-entity and GET-able.
    // Persist the canonical (normalized) form the SSE delivery path resolves.
    request.resource =
        validate_subscription_resource(&state, &component_id, backend, &request.resource).await?;

    let subscription = state
        .subscription_manager
        .create(component_id.clone(), request)
        .await;

    let resource_path = format!(
        "/vehicle/v1/components/{}/cyclic-subscriptions/{}",
        component_id, subscription.subscription_id
    );

    // Spec §7.10.3 / C-072: `Location` points at the created temporary
    // subscription resource — which IS the SSE stream. Clients GET it
    // with `Accept: text/event-stream` to receive events, or without to
    // read the details. No separate `streams` URL (C-025).
    let mut headers = HeaderMap::new();
    headers.insert(
        header::LOCATION,
        HeaderValue::from_str(&resource_path)
            .map_err(|e| ApiError::Internal(format!("bad Location header: {e}")))?,
    );

    Ok((StatusCode::CREATED, headers, Json(subscription)))
}

/// GET /vehicle/v1/components/:component_id/cyclic-subscriptions
pub async fn list_cyclic_subscriptions(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
) -> Result<Json<CyclicSubscriptionsResponse>, ApiError> {
    let _backend = state.get_backend(&component_id)?;
    let items = state
        .subscription_manager
        .list_for_component(&component_id)
        .await;
    Ok(Json(CyclicSubscriptionsResponse { items }))
}

/// PUT /vehicle/v1/components/:component_id/cyclic-subscriptions/:subscription_id
///
/// Update cadence and/or duration in place.  Returns 200 with the
/// updated `CyclicSubscription` body.
pub async fn update_cyclic_subscription(
    State(state): State<AppState>,
    Path((_component_id, subscription_id)): Path<(String, String)>,
    Json(request): Json<UpdateCyclicSubscriptionRequest>,
) -> Result<Json<CyclicSubscription>, ApiError> {
    if let Some(0) = request.duration {
        return Err(ApiError::BadRequest(
            "duration must be > 0; omit the field for no expiry".to_string(),
        ));
    }
    state
        .subscription_manager
        .update(&subscription_id, request.interval, request.duration)
        .await
        .map(Json)
        .ok_or_else(|| ApiError::NotFound(format!("Subscription not found: {}", subscription_id)))
}

/// GET /vehicle/v1/components/:component_id/cyclic-subscriptions/:subscription_id
///
/// Content-negotiated per ISO 17978-3 §7.10.3 — the cyclic-subscription
/// resource itself is the SSE stream:
///
///   * `Accept: text/event-stream` → the SSE event stream (the events
///     this subscription's cadence produces).
///   * any other `Accept` → the `CyclicSubscription` details (JSON).
///
/// This is why there is no separate non-standard `streams` resource
/// (C-025: only standardized names on the entity).
pub async fn get_cyclic_subscription(
    State(state): State<AppState>,
    Path((component_id, subscription_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<axum::response::Response, ApiError> {
    if wants_event_stream(&headers) {
        return stream_subscription(&state, &component_id, &subscription_id)
            .await
            .map(IntoResponse::into_response);
    }

    state
        .subscription_manager
        .get(&subscription_id)
        .await
        .map(|sub| Json(sub).into_response())
        .ok_or_else(|| ApiError::NotFound(format!("Subscription not found: {}", subscription_id)))
}

/// True when the request's `Accept` header asks for `text/event-stream`
/// (the SSE media type). Matches a bare `text/event-stream` token in the
/// comma-separated header, ignoring `;q=` weights.
fn wants_event_stream(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|accept| {
            accept.split(',').any(|part| {
                part.split(';')
                    .next()
                    .map(str::trim)
                    .map(str::to_ascii_lowercase)
                    .as_deref()
                    == Some("text/event-stream")
            })
        })
        .unwrap_or(false)
}

/// SSE EventEnvelope — ISO 17978-3 §5.2.2 / Table 5.
///
/// Each event SHALL carry one envelope via the SSE `data:` line.
/// `payload` is the success payload; `error` is mutually exclusive and
/// carries a `GenericError` if the publisher hit a transient producer
/// error.
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

/// SSE delivery for a cyclic subscription (§7.10.3). Invoked by
/// [`get_cyclic_subscription`] when the client sends
/// `Accept: text/event-stream`; the subscription resource IS the stream.
async fn stream_subscription(
    state: &AppState,
    component_id: &str,
    subscription_id: &str,
) -> Result<impl IntoResponse, ApiError> {
    // Look up the cyclic subscription.
    let subscription = state
        .subscription_manager
        .get(subscription_id)
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
    // Resolve it against DidStore the same way as the data flow — DID hex
    // strings pass through unchanged.
    let did_store = state.did_store_arc();
    let mut did_to_info: HashMap<String, (String, u16)> = HashMap::new();
    let resource_param = subscription.resource.clone();
    // Resolve the resource to the id handed to the backend. A gateway
    // child resource is `child/param`: resolve the `param` to its DID but
    // KEEP the `child/` prefix so GatewayBackend::subscribe_data can route
    // it to the child. The forwarded data points come back keyed by the
    // child-local id (the gateway does not re-prefix them), so did_to_info
    // is keyed on that local id, not the prefixed resource.
    let did_str = if let Some((child, param)) = resource_param.split_once('/') {
        if let Some(did) = did_store.resolve_did(param) {
            let did_hex = format!("{:04X}", did);
            did_to_info.insert(did_hex.clone(), (param.to_string(), did));
            format!("{child}/{did_hex}")
        } else {
            did_to_info.insert(param.to_string(), (param.to_string(), 0));
            format!("{child}/{param}")
        }
    } else if let Some(did) = did_store.resolve_did(&resource_param) {
        let did_hex = format!("{:04X}", did);
        did_to_info.insert(did_hex.clone(), (resource_param.clone(), did));
        did_hex
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

    // Sequence counter for events.
    let seq_counter = Arc::new(AtomicU64::new(1));

    // Convert the broadcast receiver to an SSE stream of EventEnvelopes.
    let stream = BroadcastStream::new(receiver).filter_map(move |result| {
        let did_to_info = did_to_info.clone();
        let seq_counter = seq_counter.clone();
        let did_store = did_store.clone();

        match result {
            Ok(data_point) => {
                let seq = seq_counter.fetch_add(1, Ordering::SeqCst);
                let timestamp = Utc::now().to_rfc3339();

                // Look up parameter name and DID from the data point ID.
                let (param_name, did) = did_to_info
                    .get(&data_point.id)
                    .cloned()
                    .unwrap_or_else(|| (data_point.id.clone(), 0));

                // Convert hex value to typed value using DidStore.
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

                // EventEnvelope.payload: {seq, values{<param>: <val>}}.
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
                // Broadcast lag — consumer can't keep up. Spec
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

    // C-070 (§5.2.2): axum's `Sse` responder emits `Content-Type:
    // text/event-stream`; `KeepAlive` adds the comment-line heartbeat the
    // spec's `Connection: keep-alive` requirement maps to. Both asserted in
    // `spec_update_flow::sse_subscription_stream_carries_event_stream_ct`.
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

/// DELETE /vehicle/v1/components/:component_id/cyclic-subscriptions/:subscription_id
pub async fn delete_cyclic_subscription(
    State(state): State<AppState>,
    Path((_component_id, subscription_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    if state.subscription_manager.delete(&subscription_id).await {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!(
            "Subscription not found: {}",
            subscription_id
        )))
    }
}
