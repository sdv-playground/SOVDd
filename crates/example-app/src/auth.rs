//! Bearer token authentication middleware
//!
//! Validates `Authorization: Bearer <token>` on all requests except `/health`.
//! If no token is configured, all requests pass through.

use axum::{body::Body, extract::Request, http::StatusCode, middleware::Next, response::Response};

/// Auth middleware state — holds the expected token
#[derive(Clone)]
pub struct AuthToken(pub String);

/// Axum middleware function that checks bearer token authentication.
///
/// Skips auth for `/health` endpoint. Returns 401 if token is missing or invalid.
pub async fn auth_middleware(
    token: axum::extract::Extension<AuthToken>,
    request: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    // Skip auth for health endpoint
    if request.uri().path() == "/health" {
        return Ok(next.run(request).await);
    }

    let expected = &token.0 .0;

    let auth_header = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok());

    match auth_header {
        Some(header) if header.starts_with("Bearer ") => {
            let provided = &header[7..];
            if provided == expected {
                Ok(next.run(request).await)
            } else {
                tracing::warn!("Invalid bearer token");
                Err(StatusCode::UNAUTHORIZED)
            }
        }
        _ => {
            tracing::warn!(
                path = %request.uri().path(),
                "Missing or malformed Authorization header"
            );
            Err(StatusCode::UNAUTHORIZED)
        }
    }
}
