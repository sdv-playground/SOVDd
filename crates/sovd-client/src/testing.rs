//! Test utilities for sovd-client
//!
//! Provides helpers for running integration tests against SOVD servers.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;

use crate::{Result, SovdClient};

/// A test server that automatically shuts down when dropped
pub struct TestServer {
    pub addr: SocketAddr,
    pub client: SovdClient,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl TestServer {
    /// Create a new test server from an axum Router
    ///
    /// # Example
    ///
    /// ```ignore
    /// use sovd_client::testing::TestServer;
    /// use sovd_api::{create_router, AppState};
    ///
    /// let state = AppState::new(backends);
    /// let router = create_router(state);
    /// let server = TestServer::start(router).await?;
    ///
    /// // Use server.client to make requests
    /// let components = server.client.list_components().await?;
    /// ```
    pub async fn start<S>(router: axum::Router<S>) -> Result<Self>
    where
        S: Clone + Send + Sync + 'static,
        axum::Router<S>: Into<axum::Router>,
    {
        Self::start_with_timeout(router, Duration::from_secs(5), Duration::from_secs(2)).await
    }

    /// Create a new test server with custom timeouts
    pub async fn start_with_timeout<S>(
        router: axum::Router<S>,
        timeout: Duration,
        connect_timeout: Duration,
    ) -> Result<Self>
    where
        S: Clone + Send + Sync + 'static,
        axum::Router<S>: Into<axum::Router>,
    {
        // Bind to any available port
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

        let router: axum::Router = router.into();

        // Spawn the server
        let handle = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await
                .ok();
        });

        // Give server a moment to start
        tokio::time::sleep(Duration::from_millis(10)).await;

        let base_url = format!("http://{}", addr);
        let client = SovdClient::with_config(&base_url, timeout, connect_timeout)?;

        Ok(Self {
            addr,
            client,
            shutdown_tx: Some(shutdown_tx),
            handle: Some(handle),
        })
    }

    /// Get the base URL of the test server
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Get a reference to the client
    pub fn client(&self) -> &SovdClient {
        &self.client
    }

    /// Shutdown the server gracefully
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.await;
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        // Send shutdown signal if not already done
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        // Abort the task if still running
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

/// Trait for creating mock backends for testing
pub trait MockBackendBuilder {
    /// Build the mock backend
    fn build(self) -> Arc<dyn sovd_core::DiagnosticBackend>;
}

/// Wait for a condition with timeout
pub async fn wait_for<F, Fut>(condition: F, timeout: Duration) -> bool
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + timeout;

    while tokio::time::Instant::now() < deadline {
        if condition().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base_url_format() {
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let url = format!("http://{}", addr);
        assert_eq!(url, "http://127.0.0.1:8080");
    }
}
