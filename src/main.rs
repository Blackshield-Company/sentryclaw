use axum::{
    extract::ConnectInfo,
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use serde::Serialize;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::{info, instrument};

use sentryclaw::config::AppConfig;
use sentryclaw::db::Database;
use sentryclaw::metrics::Metrics;
use sentryclaw::rate_limit::{extract_client_key, RateLimiter};
use sentryclaw::shutdown::{wait_for_shutdown, ShutdownHandle};
use sentryclaw::AppState;

#[derive(Serialize)]
struct HealthStatus {
    status: String,
    version: String,
    database: String,
    config_loaded: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let demo_env = std::env::var("SENTRYCLAW_DEMO")
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false);
    if args.get(1).map(String::as_str) == Some("repair") {
        let code = sentryclaw::repair::cli::main_with_args(args[1..].to_vec()).await?;
        std::process::exit(code);
    }
    if args.get(1).map(String::as_str) == Some("demo") || demo_env {
        return run_demo(args.get(2).map(String::as_str)).await;
    }

    let config = Arc::new(AppConfig::load()?);
    let logging_config = config.logging_config();

    if logging_config.json_format {
        tracing_subscriber::fmt().json().with_target(true).init();
    } else {
        tracing_subscriber::fmt::init();
    }

    let db_path = config.database_config().path.clone();
    let database = Arc::new(Database::new(&db_path)?);
    let metrics = Arc::new(Metrics::new());
    let rate_limiter = Arc::new(RateLimiter::new(60, 60));
    let rate_limiter_for_shutdown = rate_limiter.clone();
    let shutdown = ShutdownHandle::new();

    let state = AppState {
        config: config.clone(),
        database,
        metrics,
        rate_limiter,
    };

    let dashboard_enabled = state.config.dashboard_config().enabled;

    let mut app = Router::new()
        .route("/webhook/github", post(github_webhook))
        .route("/webhook/gitlab", post(gitlab_webhook))
        .route("/health", get(health_check))
        .route("/metrics", get(metrics_handler));

    if dashboard_enabled {
        app = app
            .route("/dashboard", get(sentryclaw::dashboard::dashboard_handler))
            .route(
                "/dashboard/stats",
                get(sentryclaw::dashboard::stats_api_handler),
            )
            .route(
                "/dashboard/api/search",
                get(sentryclaw::dashboard::search_api_handler),
            )
            .route(
                "/dashboard/knight/:state",
                get(sentryclaw::dashboard::knight_handler),
            );
        info!("\u{1f4ca} Dashboard enabled at /dashboard");
    }

    let app = app.with_state(state.clone());

    let bind_addr = format!("{}:{}", config.server.host, config.server.port);
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    info!(
        "\u{1f988} SentryClaw v{} listening on {}",
        env!("CARGO_PKG_VERSION"),
        listener.local_addr()?
    );

    let shutdown_clone = shutdown.clone();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            wait_for_shutdown().await;
            shutdown_clone.shutdown();
        })
        .await?;

    // Stop background tasks once the server has drained.
    rate_limiter_for_shutdown.shutdown();
    info!("SentryClaw shut down cleanly");
    Ok(())
}

#[instrument(skip(state, headers, body), fields(provider = "github"))]
async fn github_webhook(
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

    sentryclaw::github::webhook_handler(state, headers, body).await
}

#[instrument(skip(state, headers, body), fields(provider = "gitlab"))]
async fn gitlab_webhook(
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

    sentryclaw::gitlab::webhook_handler(state, headers, body).await
}

async fn health_check(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Json<HealthStatus> {
    let db_status = if state.database.get_stats().await.is_ok() {
        "connected"
    } else {
        "error"
    };

    Json(HealthStatus {
        status: "healthy".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        database: db_status.to_string(),
        config_loaded: true,
    })
}

async fn metrics_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Result<(axum::http::header::HeaderMap, String), StatusCode> {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        "text/plain; charset=utf-8"
            .parse()
            .expect("valid content-type header"),
    );

    let body = state.metrics.render_prometheus().await;
    Ok((headers, body))
}

/// `sentryclaw demo [--once]`: rule-based reviews of bundled sample diffs.
/// No model, no GitHub App, no config file. Binds DEMO_HOST:DEMO_PORT (0.0.0.0:8080).
async fn run_demo(arg: Option<&str>) -> anyhow::Result<()> {
    use sentryclaw::demo;
    if arg == Some("--once") {
        for (name, diff) in demo::SAMPLES {
            print!("{}", demo::render_text(&demo::review_diff(name, diff)));
        }
        return Ok(());
    }
    tracing_subscriber::fmt::init();
    let host = std::env::var("DEMO_HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
    let port = std::env::var("DEMO_PORT")
        .or_else(|_| std::env::var("PORT"))
        .unwrap_or_else(|_| "8080".to_string());
    let listener = tokio::net::TcpListener::bind(format!("{}:{}", host, port)).await?;
    info!("SentryClaw demo mode on http://{}", listener.local_addr()?);
    axum::serve(listener, demo::router())
        .with_graceful_shutdown(wait_for_shutdown())
        .await?;
    Ok(())
}
