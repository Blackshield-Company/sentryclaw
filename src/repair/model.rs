//! Model abstraction for the repair agents.
//!
//! Every agent talks to a [`ChatModel`]. Two implementations ship:
//! - [`OpenAiCompatModel`]: any OpenAI-compatible `/v1/chat/completions`
//!   endpoint (vLLM on ROCm, llama.cpp server, Ollama, hosted APIs).
//! - [`MockModel`]: scripted, deterministic responses for tests and demos.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::llm::{ChatRequest as WireRequest, ChatResponse, Message};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Which agent is asking. Lets a mock (or a router) answer per role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentRole {
    Finder,
    Ranker,
    Fixer,
}

impl AgentRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentRole::Finder => "finder",
            AgentRole::Ranker => "ranker",
            AgentRole::Fixer => "fixer",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelRequest {
    pub role: AgentRole,
    pub system: String,
    pub user: String,
}

pub trait ChatModel: Send + Sync {
    /// Short label for reports, e.g. `openai:qwen2.5-coder` or `mock`.
    fn name(&self) -> String;
    fn complete<'a>(&'a self, req: &'a ModelRequest) -> BoxFuture<'a, anyhow::Result<String>>;
}

/// Settings for an OpenAI-compatible endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EndpointConfig {
    /// e.g. `http://localhost:8000/v1` (vLLM). `/v1` is appended if missing.
    pub base_url: String,
    pub model: String,
    /// Name of an env var holding a bearer token. Optional; vLLM needs none
    /// unless started with `--api-key`.
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default)]
    pub temperature: f32,
    #[serde(default = "default_request_timeout")]
    pub timeout_secs: u64,
}

fn default_max_tokens() -> u32 {
    2048
}
fn default_request_timeout() -> u64 {
    180
}

impl Default for EndpointConfig {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:8000/v1".to_string(),
            model: "default".to_string(),
            api_key_env: None,
            max_tokens: default_max_tokens(),
            temperature: 0.0,
            timeout_secs: default_request_timeout(),
        }
    }
}

pub struct OpenAiCompatModel {
    client: reqwest::Client,
    cfg: EndpointConfig,
    url: String,
}

impl OpenAiCompatModel {
    pub fn new(cfg: EndpointConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(cfg.timeout_secs))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        let url = chat_url(&cfg.base_url);
        Self { client, cfg, url }
    }

    pub fn url(&self) -> &str {
        &self.url
    }
}

/// Normalise a base URL to the chat completions endpoint.
pub fn chat_url(base: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.ends_with("/chat/completions") {
        base.to_string()
    } else if base.ends_with("/v1") {
        format!("{}/chat/completions", base)
    } else {
        format!("{}/v1/chat/completions", base)
    }
}

impl ChatModel for OpenAiCompatModel {
    fn name(&self) -> String {
        format!("openai-compat:{}", self.cfg.model)
    }

    fn complete<'a>(&'a self, req: &'a ModelRequest) -> BoxFuture<'a, anyhow::Result<String>> {
        Box::pin(async move {
            let body = WireRequest {
                model: self.cfg.model.clone(),
                messages: vec![
                    Message {
                        role: "system".into(),
                        content: req.system.clone(),
                    },
                    Message {
                        role: "user".into(),
                        content: req.user.clone(),
                    },
                ],
                max_tokens: self.cfg.max_tokens,
                temperature: self.cfg.temperature,
            };
            let mut rb = self.client.post(&self.url).json(&body);
            if let Some(var) = &self.cfg.api_key_env {
                if let Ok(key) = std::env::var(var) {
                    if !key.is_empty() {
                        rb = rb.bearer_auth(key);
                    }
                }
            }
            let resp = rb.send().await?.error_for_status()?;
            let parsed: ChatResponse = resp.json().await?;
            parsed
                .choices
                .into_iter()
                .next()
                .map(|c| c.message.content)
                .ok_or_else(|| anyhow::anyhow!("model returned no choices"))
        })
    }
}

/// Scripted model: each role pops responses from its own queue, in order.
///
/// Script file format (JSON):
/// `{"finder": ["..."], "ranker": ["..."], "fixer": ["...", "..."]}`
#[derive(Default)]
pub struct MockModel {
    queues: Mutex<HashMap<AgentRole, VecDeque<String>>>,
    calls: Mutex<Vec<ModelRequest>>,
}

#[derive(Debug, Default, Deserialize)]
pub struct MockScript {
    #[serde(default)]
    pub finder: Vec<String>,
    #[serde(default)]
    pub ranker: Vec<String>,
    #[serde(default)]
    pub fixer: Vec<String>,
}

impl MockModel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_script(script: MockScript) -> Self {
        let m = Self::new();
        for r in script.finder {
            m.push(AgentRole::Finder, r);
        }
        for r in script.ranker {
            m.push(AgentRole::Ranker, r);
        }
        for r in script.fixer {
            m.push(AgentRole::Fixer, r);
        }
        m
    }

    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("reading mock script {}: {}", path.display(), e))?;
        let script: MockScript = serde_json::from_str(&text)?;
        Ok(Self::from_script(script))
    }

    pub fn push(&self, role: AgentRole, response: impl Into<String>) {
        self.queues
            .lock()
            .expect("mock lock")
            .entry(role)
            .or_default()
            .push_back(response.into());
    }

    /// Every request received so far (for assertions in tests).
    pub fn calls(&self) -> Vec<ModelRequest> {
        self.calls.lock().expect("mock lock").clone()
    }
}

impl ChatModel for MockModel {
    fn name(&self) -> String {
        "mock".to_string()
    }

    fn complete<'a>(&'a self, req: &'a ModelRequest) -> BoxFuture<'a, anyhow::Result<String>> {
        Box::pin(async move {
            self.calls.lock().expect("mock lock").push(req.clone());
            self.queues
                .lock()
                .expect("mock lock")
                .get_mut(&req.role)
                .and_then(|q| q.pop_front())
                .ok_or_else(|| {
                    anyhow::anyhow!("mock script exhausted for role {}", req.role.as_str())
                })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_url_normalises() {
        assert_eq!(
            chat_url("http://h:8000/v1"),
            "http://h:8000/v1/chat/completions"
        );
        assert_eq!(
            chat_url("http://h:8000/"),
            "http://h:8000/v1/chat/completions"
        );
        assert_eq!(
            chat_url("http://h/v1/chat/completions"),
            "http://h/v1/chat/completions"
        );
    }

    #[tokio::test]
    async fn mock_pops_per_role_and_records() {
        let m = MockModel::new();
        m.push(AgentRole::Fixer, "a");
        m.push(AgentRole::Fixer, "b");
        let req = ModelRequest {
            role: AgentRole::Fixer,
            system: "s".into(),
            user: "u".into(),
        };
        assert_eq!(m.complete(&req).await.unwrap(), "a");
        assert_eq!(m.complete(&req).await.unwrap(), "b");
        assert!(m.complete(&req).await.is_err());
        let finder = ModelRequest {
            role: AgentRole::Finder,
            ..req.clone()
        };
        assert!(m.complete(&finder).await.is_err());
        assert_eq!(m.calls().len(), 4);
    }
}
