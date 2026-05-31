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
//!     → `CyclicSubscription`.
//!
//!   `DELETE /vehicle/v1/components/{id}/cyclic-subscriptions/{id}`
//!     → 204 No Content.
//!
//! SSE stream delivery happens on a separate `streams` resource
//! (`GET /vehicle/v1/components/{id}/streams/{subscription_id}`).
//! Single resource per subscription per spec; for multi-parameter
//! consumers, open N subscriptions and join the streams client-side.

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::sync::RwLock;
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

/// POST /vehicle/v1/components/:component_id/cyclic-subscriptions
pub async fn create_cyclic_subscription(
    State(state): State<AppState>,
    Path(component_id): Path<String>,
    Json(request): Json<CyclicSubscriptionRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let _backend = state.get_backend(&component_id)?;

    // Spec: duration=0 makes the subscription expire instantly and
    // is nonsensical; reject as incomplete-request.
    if let Some(0) = request.duration {
        return Err(ApiError::BadRequest(
            "duration must be > 0; omit the field for no expiry".to_string(),
        ));
    }

    let subscription = state
        .subscription_manager
        .create(component_id.clone(), request)
        .await;

    let stream_path = format!(
        "/vehicle/v1/components/{}/streams/{}",
        component_id, subscription.subscription_id
    );
    let resource_path = format!(
        "/vehicle/v1/components/{}/cyclic-subscriptions/{}",
        component_id, subscription.subscription_id
    );

    // Spec: Location header points at the created subscription
    // resource.  Add an additional `Link` header advertising the SSE
    // stream (custom but harmless) so clients can discover where to
    // attach without an extra round-trip.
    let mut headers = HeaderMap::new();
    headers.insert(
        header::LOCATION,
        HeaderValue::from_str(&resource_path)
            .map_err(|e| ApiError::Internal(format!("bad Location header: {e}")))?,
    );
    if let Ok(link) = HeaderValue::from_str(&format!("<{}>; rel=\"stream\"", stream_path)) {
        headers.insert(header::LINK, link);
    }

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
pub async fn get_cyclic_subscription(
    State(state): State<AppState>,
    Path((_component_id, subscription_id)): Path<(String, String)>,
) -> Result<Json<CyclicSubscription>, ApiError> {
    state
        .subscription_manager
        .get(&subscription_id)
        .await
        .map(Json)
        .ok_or_else(|| ApiError::NotFound(format!("Subscription not found: {}", subscription_id)))
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
