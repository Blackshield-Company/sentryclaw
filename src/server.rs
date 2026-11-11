//! HTTP server helpers. The webhook routes extract `ConnectInfo` for rate
//! limiting, so the listener must be served with connection info.

use std::future::Future;
use std::net::SocketAddr;

use axum::{extract::ConnectInfo, http::StatusCode, routing::post, Router};
use tracing::instrument;

use crate::rate_limit::extract_client_key;
use crate::AppState;

/// Serves `app` on `listener` until `shutdown` resolves. Supplies
/// `ConnectInfo<SocketAddr>`, which the webhook routes use for rate limiting.
pub async fn serve<F>(
    listener: tokio::net::TcpListener,
    app: Router,
    shutdown: F,
) -> Result<(), std::io::Error>
where
    F: Future<Output = ()> + Send + 'static,
{
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
}

/// GitHub webhook entry point used by the binary: records metrics, applies
/// the per-client rate limit, then hands off to the signature-checking handler.
#[instrument(skip(state, headers, body), fields(provider = "github"))]
pub async fn github_webhook(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    state: axum::extract::State<AppState>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> StatusCode {
    let client_key = extract_client_key(Some(addr));
    state.metrics.record_webhook_received();

    if !state.rate_limiter.is_allowed(&client_key).await {
        state.metrics.record_webhook_rate_limited();
        tracing::warn!("Rate limit exceeded for {}", client_key);
        return StatusCode::TOO_MANY_REQUESTS;
    }

    crate::github::webhook_handler(state, headers, body).await
}

/// GitLab webhook entry point used by the binary: records metrics, applies
/// the per-client rate limit, then hands off to the token-checking handler.
#[instrument(skip(state, headers, body), fields(provider = "gitlab"))]
pub async fn gitlab_webhook(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    state: axum::extract::State<AppState>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> StatusCode {
    let client_key = extract_client_key(Some(addr));
    state.metrics.record_webhook_received();

    if !state.rate_limiter.is_allowed(&client_key).await {
        state.metrics.record_webhook_rate_limited();
        tracing::warn!("Rate limit exceeded for {}", client_key);
        return StatusCode::TOO_MANY_REQUESTS;
    }

    crate::gitlab::webhook_handler(state, headers, body).await
}

/// The production webhook routes (ConnectInfo extractors included).
pub fn webhook_routes() -> Router<AppState> {
    Router::new()
        .route("/webhook/github", post(github_webhook))
        .route("/webhook/gitlab", post(gitlab_webhook))
}
