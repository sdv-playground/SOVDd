//! Streaming support for SOVD subscriptions
//!
//! Provides SSE (Server-Sent Events) streaming for real-time parameter data.
//!
//! # Example
//!
//! ```no_run
//! use sovd_client::{SovdClient, StreamEvent};
//! use futures::StreamExt;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let client = SovdClient::new("http://localhost:8080")?;
//!
//! // Create subscription and get stream
//! let mut subscription = client
//!     .subscribe("engine_ecu", vec!["vehicle_speed".into(), "engine_rpm".into()], 10)
//!     .await?;
//!
//! // Consume events
//! while let Some(event) = subscription.next().await {
//!     match event {
//!         Ok(data) => {
//!             println!("seq={}, values={:?}", data.sequence, data.values);
//!         }
//!         Err(e) => {
//!             eprintln!("Stream error: {}", e);
//!             break;
//!         }
//!     }
//! }
//!
//! // Explicit cleanup (also happens on drop)
//! subscription.cancel().await?;
//! # Ok(())
//! # }
//! ```

mod parser;
mod subscription;
mod types;

pub use subscription::Subscription;
pub use types::{StreamError, StreamEvent};
