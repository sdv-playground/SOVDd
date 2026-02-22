//! Subscription implementation

use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures::stream::{Stream, StreamExt};
use reqwest::Client;
use tracing::{debug, warn};
use url::Url;

use super::parser::SseParser;
use super::types::{StreamError, StreamEvent, StreamResult};

/// An active subscription that streams events from the server
///
/// Implements `Stream<Item = Result<StreamEvent, StreamError>>` for easy consumption.
///
/// # Lifecycle
///
/// - Created via `SovdClient::subscribe()` or `SovdClient::subscribe_inline()`
/// - Events are consumed via `next()` or the `Stream` trait
/// - Call `cancel()` for explicit cleanup, or let it drop
///
/// # Example
///
/// ```ignore
/// let mut sub = client.subscribe("ecu", params, 10).await?;
///
/// while let Some(event) = sub.next().await {
///     println!("{:?}", event?);
/// }
/// ```
pub struct Subscription {
    /// Subscription ID (for cleanup)
    subscription_id: String,

    /// Component ID
    component_id: Option<String>,

    /// Base URL for API calls
    base_url: Url,

    /// HTTP client for cleanup request
    http_client: Client,

    /// Inner stream state (wrapped for pinning)
    inner: Pin<Box<SubscriptionInner>>,

    /// Whether the subscription has been cancelled
    cancelled: bool,
}

struct SubscriptionInner {
    /// The underlying byte stream from reqwest
    byte_stream: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,

    /// SSE parser
    parser: SseParser,

    /// Buffered events from the parser
    event_buffer: Vec<StreamResult<StreamEvent>>,
}

impl Subscription {
    /// Create a new subscription from a stream URL
    pub(crate) async fn connect(
        base_url: Url,
        http_client: Client,
        subscription_id: String,
        component_id: Option<String>,
        stream_url: &str,
    ) -> StreamResult<Self> {
        // Build full stream URL
        let full_url = base_url
            .join(stream_url)
            .map_err(|e| StreamError::Parse(format!("Invalid stream URL: {}", e)))?;

        debug!("Connecting to SSE stream: {}", full_url);

        // Connect to the stream
        let response = http_client
            .get(full_url)
            .header("Accept", "text/event-stream")
            .send()
            .await?;

        // Check response status
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let message = response.text().await.unwrap_or_default();
            return Err(StreamError::Server { status, message });
        }

        // Get the byte stream
        let byte_stream = response.bytes_stream();

        Ok(Self {
            subscription_id,
            component_id,
            base_url,
            http_client,
            inner: Box::pin(SubscriptionInner {
                byte_stream: Box::pin(byte_stream),
                parser: SseParser::new(),
                event_buffer: Vec::new(),
            }),
            cancelled: false,
        })
    }

    /// Get the subscription ID
    pub fn id(&self) -> &str {
        &self.subscription_id
    }

    /// Get the next event from the stream
    ///
    /// Returns `None` when the stream ends or is cancelled.
    pub async fn next(&mut self) -> Option<StreamResult<StreamEvent>> {
        <Self as StreamExt>::next(self).await
    }

    /// Cancel the subscription and clean up resources
    ///
    /// This sends a DELETE request to remove the subscription from the server.
    /// After calling this, the stream will return `None`.
    pub async fn cancel(mut self) -> StreamResult<()> {
        self.cancelled = true;
        self.cleanup().await
    }

    /// Internal cleanup - delete the subscription from the server
    async fn cleanup(&self) -> StreamResult<()> {
        // Determine the correct cleanup URL
        let delete_url = if self.component_id.is_some() {
            // Component-level subscription
            format!(
                "/vehicle/v1/components/{}/subscriptions/{}",
                self.component_id.as_ref().unwrap(),
                self.subscription_id
            )
        } else {
            // Global subscription
            format!("/vehicle/v1/subscriptions/{}", self.subscription_id)
        };

        let url = self
            .base_url
            .join(&delete_url)
            .map_err(|e| StreamError::Parse(format!("Invalid cleanup URL: {}", e)))?;

        debug!("Cleaning up subscription: {}", url);

        let response = self.http_client.delete(url).send().await?;

        if !response.status().is_success() && response.status().as_u16() != 404 {
            let status = response.status().as_u16();
            let message = response.text().await.unwrap_or_default();
            warn!("Failed to cleanup subscription: {} {}", status, message);
            // Don't fail - subscription might already be gone
        }

        Ok(())
    }
}

impl Stream for Subscription {
    type Item = StreamResult<StreamEvent>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.cancelled {
            return Poll::Ready(None);
        }

        // First, check if we have buffered events
        if !self.inner.event_buffer.is_empty() {
            return Poll::Ready(Some(self.inner.event_buffer.remove(0)));
        }

        // Poll the underlying byte stream
        let inner = self.inner.as_mut();

        // SAFETY: We're not moving the inner struct, just accessing its fields
        let inner_ref = unsafe { Pin::get_unchecked_mut(inner) };

        match Pin::new(&mut inner_ref.byte_stream).poll_next(cx) {
            Poll::Ready(Some(Ok(bytes))) => {
                // Parse the bytes
                let events = inner_ref.parser.feed(bytes);

                if events.is_empty() {
                    // No complete events yet, need more data
                    cx.waker().wake_by_ref();
                    Poll::Pending
                } else {
                    // Store extra events in buffer
                    inner_ref.event_buffer = events;
                    Poll::Ready(Some(inner_ref.event_buffer.remove(0)))
                }
            }
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(StreamError::Connection(e)))),
            Poll::Ready(None) => {
                // Stream ended
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if !self.cancelled {
            // Spawn cleanup task
            let http_client = self.http_client.clone();
            let base_url = self.base_url.clone();
            let subscription_id = self.subscription_id.clone();
            let component_id = self.component_id.clone();

            tokio::spawn(async move {
                let delete_url = if component_id.is_some() {
                    format!(
                        "/vehicle/v1/components/{}/subscriptions/{}",
                        component_id.as_ref().unwrap(),
                        subscription_id
                    )
                } else {
                    format!("/vehicle/v1/subscriptions/{}", subscription_id)
                };

                if let Ok(url) = base_url.join(&delete_url) {
                    let _ = http_client.delete(url).send().await;
                }
            });
        }
    }
}
