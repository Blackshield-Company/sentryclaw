//! `sentryclaw repair <REPO> [options]`

use std::path::PathBuf;

use clap::Parser;

use super::{run_repair, RepairConfig};

#[derive(Debug, Parser)]
#[command(
    name = "sentryclaw repair",
    about = "Find, rank, patch and test defects in a repo with a multi-agent loop. \
             The repo is copied to a sandbox; it is never modified."
)]
pub struct RepairArgs {
    /// Path to the target repository.
    pub repo: PathBuf,
    /// TOML config (see repair.example.toml).
    #[arg(long, short)]
    pub config: Option<PathBuf>,
    /// Use the scripted mock model with this JSON script (no server needed).
    #[arg(long)]
    pub mock_script: Option<PathBuf>,
    /// OpenAI-compatible base URL, e.g. http://localhost:8000/v1
    #[arg(long)]
    pub base_url: Option<String>,
    /// Model name served by the endpoint.
    #[arg(long)]
    pub model: Option<String>,
    /// Where to write fix.patch, report.json and report.md.
    #[arg(long, short, default_value = "repair-out")]
    pub out: PathBuf,
    #[arg(long)]
    pub max_attempts: Option<u32>,
    #[arg(long)]
    pub max_defects: Option<usize>,
    /// Test timeout in seconds.
    #[arg(long)]
    pub timeout: Option<u64>,
    /// Override the detected test command (split on whitespace).
    #[arg(long)]
    pub test_cmd: Option<String>,
    /// Let tests use the network (default: denied when possible).
    #[arg(long)]
    pub allow_network: bool,
    /// Print the JSON report to stdout instead of the summary.
    #[arg(long)]
    pub json: bool,
    #[arg(long, short)]
    pub quiet: bool,
}

impl RepairArgs {
    pub fn into_config(&self) -> anyhow::Result<RepairConfig> {
        let mut cfg = match &self.config {
            Some(p) => RepairConfig::load(p)?,
            None => RepairConfig::default(),
        };
        cfg.apply_env();
        if let Some(s) = &self.mock_script {
            cfg.model.kind = "mock".into();
            cfg.model.mock_script = Some(s.clone());
        }
        if let Some(u) = &self.base_url {
            cfg.model.endpoint.base_url = u.clone();
        }
        if let Some(m) = &self.model {
            cfg.model.endpoint.model = m.clone();
        }
        if let Some(n) = self.max_attempts {
            cfg.pipeline.max_attempts = n;
        }
        if let Some(n) = self.max_defects {
            cfg.pipeline.max_defects = n;
        }
        if let Some(t) = self.timeout {
            cfg.sandbox.timeout_secs = t;
        }
        if let Some(c) = &self.test_cmd {
            cfg.sandbox.test_command = c.split_whitespace().map(String::from).collect();
        }
        if self.allow_network {
            cfg.sandbox.deny_network = false;
        }
        Ok(cfg)
    }
}

/// Entry point used by `main`. `args[0]` is treated as the program name.
/// Exit code: 0 = suite green at the end, 2 = still failing, 1 = error.
pub async fn main_with_args(args: Vec<String>) -> anyhow::Result<i32> {
    let args = RepairArgs::parse_from(args);
    let cfg = args.into_config()?;
    let model = cfg.build_model()?;
    let report = run_repair(&args.repo, &cfg, model.as_ref(), !args.quiet).await?;

    std::fs::create_dir_all(&args.out)?;
    std::fs::write(args.out.join("fix.patch"), &report.patch)?;
    std::fs::write(
        args.out.join("report.json"),
        serde_json::to_string_pretty(&report)?,
    )?;
    std::fs::write(args.out.join("report.md"), report.to_markdown())?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "fixed {}/{} defects | baseline failing: {} | final failing: {} | suite {} | model calls: {}",
            report.fixed_count(),
            report.results.len(),
            report.baseline.failed_tests.len(),
            report.final_tests.failed_tests.len(),
            if report.final_tests.passed { "PASS" } else { "FAIL" },
            report.model_calls
        );
        println!(
            "wrote {}/{{fix.patch,report.json,report.md}}",
            args.out.display()
        );
    }
    Ok(if report.final_tests.passed { 0 } else { 2 })
}
