//! Global subscription handlers for SOVD API
//!
//! Manages subscriptions across all components.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::error::ApiError;
use crate::state::AppState;

/// Global subscription manager
#[derive(Debug, Default)]
pub struct SubscriptionManager {
    subscriptions: RwLock<HashMap<String, Subscription>>,
}

impl SubscriptionManager {
    pub fn new() -> Self {
        Self {
            subscriptions: RwLock::new(HashMap::new()),
        }
    }

    pub async fn create(&self, request: CreateSubscriptionRequest) -> Subscription {
        let subscription_id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let expires_at = request
            .duration_secs
            .map(|secs| now + Duration::seconds(secs as i64));

        let subscription = Subscription {
            subscription_id: subscription_id.clone(),
            component_id: request.component_id,
            parameters: request.parameters,
            rate_hz: request.rate_hz.unwrap_or(10),
            mode: request.mode.unwrap_or_else(|| "periodic".to_string()),
            status: "active".to_string(),
            created_at: now,
            expires_at,
            stream_url: format!("/vehicle/v1/streams/{}", subscription_id),
        };

        self.subscriptions
            .write()
            .await
            .insert(subscription_id, subscription.clone());

        subscription
    }

    pub async fn get(&self, subscription_id: &str) -> Option<Subscription> {
        self.subscriptions
            .read()
            .await
            .get(subscription_id)
            .cloned()
    }

    pub async fn list(&self) -> Vec<Subscription> {
        self.subscriptions.read().await.values().cloned().collect()
    }

    pub async fn delete(&self, subscription_id: &str) -> bool {
        self.subscriptions
            .write()
            .await
            .remove(subscription_id)
            .is_some()
    }
}

/// A subscription entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscription {
    pub subscription_id: String,
    pub component_id: String,
    pub parameters: Vec<String>,
    pub rate_hz: u32,
    pub mode: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    pub stream_url: String,
}

/// Request to create a subscription
#[derive(Debug, Deserialize)]
pub struct CreateSubscriptionRequest {
    pub component_id: String,
    pub parameters: Vec<String>,
    #[serde(default)]
    pub rate_hz: Option<u32>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub duration_secs: Option<u64>,
}

/// Response for subscription list
#[derive(Debug, Serialize)]
pub struct SubscriptionListResponse {
    pub items: Vec<SubscriptionInfo>,
}

/// Subscription info for list response
#[derive(Debug, Serialize)]
pub struct SubscriptionInfo {
    pub subscription_id: String,
    pub component_id: String,
    pub status: String,
    pub parameters: Vec<String>,
    pub stream_url: String,
}

impl From<&Subscription> for SubscriptionInfo {
    fn from(s: &Subscription) -> Self {
        Self {
            subscription_id: s.subscription_id.clone(),
            component_id: s.component_id.clone(),
            status: s.status.clone(),
            parameters: s.parameters.clone(),
            stream_url: s.stream_url.clone(),
        }
    }
}

/// POST /vehicle/v1/subscriptions
/// Create a new subscription
pub async fn create_subscription(
    State(state): State<AppState>,
    Json(request): Json<CreateSubscriptionRequest>,
) -> Result<(StatusCode, Json<Subscription>), ApiError> {
    // Validate component exists
    let _backend = state.get_backend(&request.component_id)?;

    // Validate all parameters exist in DidStore
    let did_store = state.did_store();
    for param in &request.parameters {
        if did_store.resolve_did(param).is_none() {
            return Err(ApiError::NotFound(format!(
                "Parameter not found: {}",
                param
            )));
        }
    }

    let subscription = state.subscription_manager.create(request).await;

    Ok((StatusCode::CREATED, Json(subscription)))
}

/// GET /vehicle/v1/subscriptions
/// List all subscriptions
pub async fn list_subscriptions(
    State(state): State<AppState>,
) -> Result<Json<SubscriptionListResponse>, ApiError> {
    let subscriptions = state.subscription_manager.list().await;
    let items: Vec<SubscriptionInfo> = subscriptions.iter().map(SubscriptionInfo::from).collect();

    Ok(Json(SubscriptionListResponse { items }))
}

/// GET /vehicle/v1/subscriptions/:subscription_id
/// Get subscription details
pub async fn get_subscription(
    State(state): State<AppState>,
    Path(subscription_id): Path<String>,
) -> Result<Json<Subscription>, ApiError> {
    state
        .subscription_manager
        .get(&subscription_id)
        .await
        .map(Json)
        .ok_or_else(|| ApiError::NotFound(format!("Subscription not found: {}", subscription_id)))
}

/// DELETE /vehicle/v1/subscriptions/:subscription_id
/// Delete a subscription
pub async fn delete_subscription(
    State(state): State<AppState>,
    Path(subscription_id): Path<String>,
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
