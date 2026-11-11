use axum::{
    body::Body,
    http::{Request, StatusCode},
    routing::{get, post},
    Router,
};
use std::sync::Arc;
use tower::util::ServiceExt;

use sentryclaw::config::{
    AppConfig, BatchingConfig, DashboardConfig, DatabaseConfig, DiffFilterConfig, GitHubConfig,
    GitLabConfig, LlmConfig, ReviewConfig,
};
use sentryclaw::db::Database;
use sentryclaw::metrics::Metrics;
use sentryclaw::rate_limit::RateLimiter;
use sentryclaw::AppState;

fn create_test_state() -> AppState {
    let config = AppConfig {
        server: sentryclaw::config::ServerConfig {
            host: "0.0.0.0".to_string(),
            port: 3000,
        },
        github: Some(GitHubConfig {
            webhook_secret: "github-secret".to_string(),
            app_id: "test-app-id".to_string(),
            private_key_path: "/tmp/test-key.pem".to_string(),
            use_app_auth: false,
            installation_id: None,
        }),
        gitlab: Some(GitLabConfig {
            webhook_secret: "gitlab-secret".to_string(),
            access_token: "test-token".to_string(),
            ci_cd_enabled: false,
            base_url: "https://gitlab.com".to_string(),
        }),
        llm: LlmConfig {
            provider: "test".to_string(),
            base_url: "http://localhost:8080".to_string(),
            model: "test-model".to_string(),
            max_tokens: 100,
            temperature: 0.1,
        },
        review: Some(ReviewConfig {
            security: true,
            style: true,
            performance: true,
            correctness: true,
            maintainability: true,
            inline_comments: true,
            summary_comment: true,
            template: None,
        }),
        diff_filter: Some(DiffFilterConfig {
            enabled: true,
            lockfile_patterns: vec!["Cargo.lock".to_string()],
            generated_patterns: vec!["*.min.js".to_string()],
            include_patterns: vec![],
            exclude_patterns: vec![],
        }),
        batching: Some(BatchingConfig {
            enabled: false,
            timeout_seconds: 30,
            max_size: 10,
        }),
        database: Some(DatabaseConfig::default()),
        dashboard: Some(DashboardConfig::default()),
        ..Default::default()
    };

    let database = Arc::new(Database::new(":memory:").unwrap());
    let metrics = Arc::new(Metrics::new());
    let rate_limiter = Arc::new(RateLimiter::new(60, 60));

    AppState {
        config: Arc::new(config),
        database,
        metrics,
        rate_limiter,
    }
}

fn create_test_app() -> Router {
    Router::new()
        .route("/webhook/github", post(sentryclaw::github::webhook_handler))
        .route("/webhook/gitlab", post(sentryclaw::gitlab::webhook_handler))
        .route("/health", get(|| async { StatusCode::OK }))
        .route("/metrics", get(metrics_test_handler))
        .with_state(create_test_state())
}

async fn metrics_test_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> String {
    state.metrics.render_prometheus().await
}

/// Serving over a real TCP listener must supply `ConnectInfo`, which the
/// webhook routes extract for rate limiting.
#[tokio::test]
async fn real_listener_serves_webhooks_with_connect_info() {
    let app = sentryclaw::server::webhook_routes()
        .route("/health", get(|| async { StatusCode::OK }))
        .with_state(create_test_state());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(sentryclaw::server::serve(listener, app, async {
        let _ = rx.await;
    }));

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{}/webhook/github", addr))
        .header("x-hub-signature-256", "sha256=00")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);

    let resp = client
        .get(format!("http://{}/health", addr))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);

    tx.send(()).unwrap();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn test_health_check() {
    let app = create_test_app();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_metrics_endpoint() {
    let app = create_test_app();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_github_webhook_valid_signature() {
    let app = create_test_app();

    let body = r#"{
        "action": "opened",
        "pull_request": {
            "number": 1,
            "title": "Test PR",
            "body": "Test body",
            "head": {
                "ref": "feature-branch",
                "sha": "abc123",
                "repo": {
                    "clone_url": "https://github.com/test/repo.git"
                }
            },
            "base": {
                "ref": "main",
                "sha": "def456"
            }
        },
        "repository": {
            "full_name": "test/repo",
            "clone_url": "https://github.com/test/repo.git"
        }
    }"#;

    let signature = compute_github_signature("github-secret", body);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhook/github")
                .header("x-hub-signature-256", signature)
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_github_webhook_invalid_signature() {
    let app = create_test_app();

    let body = r#"{"action": "opened", "repository": {"full_name": "test/repo", "clone_url": "https://github.com/test/repo.git"}}"#;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhook/github")
                .header("x-hub-signature-256", "sha256=invalid")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_github_webhook_missing_signature() {
    let app = create_test_app();

    let body = r#"{"action": "opened", "repository": {"full_name": "test/repo", "clone_url": "https://github.com/test/repo.git"}}"#;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhook/github")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_gitlab_webhook_valid_token() {
    let app = create_test_app();

    let body = r#"{
        "object_kind": "merge_request",
        "project": {
            "id": 123,
            "path_with_namespace": "test/repo",
            "git_http_url": "https://gitlab.com/test/repo.git"
        },
        "object_attributes": {
            "iid": 1,
            "title": "Test MR",
            "description": "Test description",
            "source_branch": "feature-branch",
            "target_branch": "main",
            "last_commit": {
                "id": "abc123"
            },
            "state": "opened",
            "action": "open"
        }
    }"#;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhook/gitlab")
                .header("x-gitlab-token", "gitlab-secret")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_gitlab_webhook_invalid_token() {
    let app = create_test_app();

    let body = r#"{"object_kind": "merge_request", "project": {"id": 123, "path_with_namespace": "test/repo", "git_http_url": "https://gitlab.com/test/repo.git"}, "object_attributes": {"iid": 1, "title": "Test MR", "source_branch": "feature", "target_branch": "main", "last_commit": {"id": "abc"}, "state": "opened"}}"#;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhook/gitlab")
                .header("x-gitlab-token", "wrong-token")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_gitlab_webhook_missing_token() {
    let app = create_test_app();

    let body = r#"{"object_kind": "merge_request", "project": {"id": 123, "path_with_namespace": "test/repo", "git_http_url": "https://gitlab.com/test/repo.git"}, "object_attributes": {"iid": 1, "title": "Test MR", "source_branch": "feature", "target_branch": "main", "last_commit": {"id": "abc"}, "state": "opened"}}"#;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhook/gitlab")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_gitlab_webhook_non_merge_request() {
    let app = create_test_app();

    let body = r#"{
        "object_kind": "push",
        "project": {
            "id": 123,
            "path_with_namespace": "test/repo",
            "git_http_url": "https://gitlab.com/test/repo.git"
        },
        "object_attributes": {
            "iid": 1,
            "title": "Test",
            "description": null,
            "source_branch": "feature",
            "target_branch": "main",
            "last_commit": {"id": "abc"},
            "state": "opened",
            "action": null
        }
    }"#;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhook/gitlab")
                .header("x-gitlab-token", "gitlab-secret")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[test]
fn test_config_validation_valid() {
    let config = AppConfig {
        server: sentryclaw::config::ServerConfig {
            host: "0.0.0.0".to_string(),
            port: 3000,
        },
        github: Some(GitHubConfig {
            webhook_secret: "secret".to_string(),
            app_id: "app-id".to_string(),
            private_key_path: "/tmp/test.pem".to_string(),
            use_app_auth: false,
            installation_id: None,
        }),
        gitlab: None,
        llm: LlmConfig {
            provider: "test".to_string(),
            base_url: "http://localhost:8080".to_string(),
            model: "test".to_string(),
            max_tokens: 100,
            temperature: 0.1,
        },
        review: Some(ReviewConfig::default()),
        diff_filter: Some(DiffFilterConfig::default()),
        batching: Some(BatchingConfig::default()),
        database: Some(DatabaseConfig::default()),
        dashboard: Some(DashboardConfig::default()),
        ..Default::default()
    };

    assert!(config.validate().is_ok());
}

#[test]
fn test_config_validation_empty_base_url() {
    let config = AppConfig {
        server: sentryclaw::config::ServerConfig {
            host: "0.0.0.0".to_string(),
            port: 3000,
        },
        github: Some(GitHubConfig {
            webhook_secret: "secret".to_string(),
            app_id: "app-id".to_string(),
            private_key_path: "/tmp/test.pem".to_string(),
            use_app_auth: false,
            installation_id: None,
        }),
        gitlab: None,
        llm: LlmConfig {
            provider: "test".to_string(),
            base_url: "".to_string(),
            model: "test".to_string(),
            max_tokens: 100,
            temperature: 0.1,
        },
        review: Some(ReviewConfig::default()),
        diff_filter: Some(DiffFilterConfig::default()),
        batching: Some(BatchingConfig::default()),
        database: Some(DatabaseConfig::default()),
        dashboard: Some(DashboardConfig::default()),
        ..Default::default()
    };

    let result = config.validate();
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("llm.base_url"));
}

#[test]
fn test_config_validation_no_provider() {
    let config = AppConfig {
        server: sentryclaw::config::ServerConfig {
            host: "0.0.0.0".to_string(),
            port: 3000,
        },
        github: None,
        gitlab: None,
        llm: LlmConfig {
            provider: "test".to_string(),
            base_url: "http://localhost:8080".to_string(),
            model: "test".to_string(),
            max_tokens: 100,
            temperature: 0.1,
        },
        review: Some(ReviewConfig::default()),
        diff_filter: Some(DiffFilterConfig::default()),
        batching: Some(BatchingConfig::default()),
        database: Some(DatabaseConfig::default()),
        dashboard: Some(DashboardConfig::default()),
        ..Default::default()
    };

    let result = config.validate();
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("github or gitlab"));
}

#[test]
fn test_config_validation_invalid_temperature() {
    let config = AppConfig {
        server: sentryclaw::config::ServerConfig {
            host: "0.0.0.0".to_string(),
            port: 3000,
        },
        github: Some(GitHubConfig {
            webhook_secret: "secret".to_string(),
            app_id: "app-id".to_string(),
            private_key_path: "/tmp/test.pem".to_string(),
            use_app_auth: false,
            installation_id: None,
        }),
        gitlab: None,
        llm: LlmConfig {
            provider: "test".to_string(),
            base_url: "http://localhost:8080".to_string(),
            model: "test".to_string(),
            max_tokens: 100,
            temperature: 3.0,
        },
        review: Some(ReviewConfig::default()),
        diff_filter: Some(DiffFilterConfig::default()),
        batching: Some(BatchingConfig::default()),
        database: Some(DatabaseConfig::default()),
        dashboard: Some(DashboardConfig::default()),
        ..Default::default()
    };

    let result = config.validate();
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("temperature"));
}

fn compute_github_signature(secret: &str, body: &str) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    type HmacSha256 = Hmac<Sha256>;
    let mut mac = match HmacSha256::new_from_slice(secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => panic!("HMAC initialization failed"),
    };
    mac.update(body.as_bytes());
    let result = mac.finalize();
    let code_bytes = result.into_bytes();
    format!("sha256={}", hex::encode(code_bytes))
}
