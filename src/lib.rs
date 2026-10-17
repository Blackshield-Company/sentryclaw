pub mod auto_approve;
pub mod cache;
pub mod config;
pub mod dashboard;
pub mod db;
pub mod demo;
pub mod diff_filter;
pub mod github;
pub mod gitlab;
pub mod inline_comments;
pub mod llm;
pub mod metrics;
pub mod queue;
pub mod rate_limit;
pub mod repair;
pub mod retry;
pub mod review;
pub mod rule_engine;
pub mod shutdown;
pub mod templates;

use config::AppConfig;
use db::Database;
use metrics::Metrics;
use rate_limit::RateLimiter;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<AppConfig>,
    pub database: Arc<Database>,
    pub metrics: Arc<Metrics>,
    pub rate_limiter: Arc<RateLimiter>,
}
