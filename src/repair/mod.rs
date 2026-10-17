//! `sentryclaw repair`: a four-agent defect repair loop.
//!
//! ```text
//! repo ─copy─▶ sandbox ─baseline tests─▶ Finder ─▶ Ranker ─▶ ┌▶ Fixer ─diff─▶ Tester ─┐
//!                                                             └──── failure feedback ◀─┘
//!                                                   (per defect, up to max_attempts)
//! ```
//! Output: a combined patch plus JSON and Markdown reports. The original
//! repo is never modified.

pub mod agents;
pub mod cli;
pub mod model;
pub mod orchestrator;
pub mod patch;
pub mod sandbox;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use model::{ChatModel, EndpointConfig, MockModel, OpenAiCompatModel};
pub use orchestrator::{run_repair, RepairReport};
pub use sandbox::SandboxConfig;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RepairConfig {
    pub model: ModelConfig,
    pub pipeline: PipelineConfig,
    pub sandbox: SandboxConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelConfig {
    /// "openai" (any OpenAI-compatible server) or "mock".
    pub kind: String,
    /// JSON script for the mock model.
    pub mock_script: Option<PathBuf>,
    #[serde(flatten)]
    pub endpoint: EndpointConfig,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            kind: "openai".into(),
            mock_script: None,
            endpoint: EndpointConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PipelineConfig {
    /// Fix at most this many ranked defects.
    pub max_defects: usize,
    /// Fixer/Tester rounds per defect before giving up on it.
    pub max_attempts: u32,
    /// Hard cap on model calls for the whole run.
    pub max_model_calls: u32,
    /// Reject patches that edit, delete or disable tests.
    pub protect_tests: bool,
    /// Stop once the suite is green (if it started red).
    pub stop_when_green: bool,
    pub max_file_chars: usize,
    pub max_context_chars: usize,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            max_defects: 5,
            max_attempts: 3,
            max_model_calls: 40,
            protect_tests: true,
            stop_when_green: true,
            max_file_chars: 20_000,
            max_context_chars: 60_000,
        }
    }
}

impl RepairConfig {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("reading {}: {}", path.display(), e))?;
        let mut cfg: Self = toml::from_str(&text)?;
        // Resolve a relative mock script against the config file's folder.
        if let (Some(ms), Some(dir)) = (&cfg.model.mock_script, path.parent()) {
            if ms.is_relative() && !ms.exists() {
                cfg.model.mock_script = Some(dir.join(ms));
            }
        }
        Ok(cfg)
    }

    /// `REPAIR_BASE_URL`, `REPAIR_MODEL`, `REPAIR_API_KEY_ENV` override the file.
    pub fn apply_env(&mut self) {
        if let Ok(v) = std::env::var("REPAIR_BASE_URL") {
            self.model.endpoint.base_url = v;
        }
        if let Ok(v) = std::env::var("REPAIR_MODEL") {
            self.model.endpoint.model = v;
        }
        if let Ok(v) = std::env::var("REPAIR_API_KEY_ENV") {
            self.model.endpoint.api_key_env = Some(v);
        }
    }

    pub fn build_model(&self) -> anyhow::Result<Box<dyn ChatModel>> {
        match self.model.kind.as_str() {
            "mock" => {
                let path = self.model.mock_script.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("model.kind = \"mock\" needs model.mock_script")
                })?;
                Ok(Box::new(MockModel::from_file(path)?))
            }
            "openai" | "openai-compatible" | "vllm" | "llamacpp" | "ollama" => Ok(Box::new(
                OpenAiCompatModel::new(self.model.endpoint.clone()),
            )),
            other => anyhow::bail!(
                "unknown model.kind {:?} (use \"openai\" or \"mock\")",
                other
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_example_config() {
        let text = include_str!("../../repair.example.toml");
        let cfg: RepairConfig = toml::from_str(text).unwrap();
        assert_eq!(cfg.model.kind, "openai");
        assert!(cfg.model.endpoint.base_url.ends_with("/v1"));
        assert_eq!(cfg.sandbox.jobs, 2);
        assert!(cfg.sandbox.deny_network);
        assert_eq!(cfg.pipeline.max_attempts, 3);
    }

    #[test]
    fn empty_config_uses_defaults() {
        let cfg: RepairConfig = toml::from_str("").unwrap();
        assert_eq!(cfg.pipeline.max_defects, 5);
        assert_eq!(cfg.sandbox.timeout_secs, 300);
        assert!(cfg.build_model().is_ok());
    }

    #[test]
    fn mock_without_script_is_an_error() {
        let cfg: RepairConfig = toml::from_str("[model]\nkind = \"mock\"\n").unwrap();
        assert!(cfg.build_model().is_err());
    }
}
