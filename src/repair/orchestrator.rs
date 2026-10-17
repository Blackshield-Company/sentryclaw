//! State machine that drives Finder -> Ranker -> (Fixer <-> Tester)*.

use std::path::Path;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use super::agents::{Defect, Feedback, Finder, Fixer, Ranker, RepoSnapshot, Tester, Verdict};
use super::model::ChatModel;
use super::sandbox::{Isolation, Sandbox, TestOutcome};
use super::RepairConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptRecord {
    pub attempt: u32,
    pub patch: Option<String>,
    pub verdict: Verdict,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum DefectStatus {
    Fixed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefectResult {
    pub defect: Defect,
    pub status: DefectStatus,
    pub note: Option<String>,
    pub attempts: Vec<AttemptRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestSummary {
    pub passed: bool,
    pub timed_out: bool,
    pub failed_tests: Vec<String>,
}

impl From<&TestOutcome> for TestSummary {
    fn from(t: &TestOutcome) -> Self {
        Self {
            passed: t.passed,
            timed_out: t.timed_out,
            failed_tests: t.failed_tests.iter().cloned().collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairReport {
    pub repo: String,
    pub model: String,
    pub isolation: Isolation,
    pub test_command: Vec<String>,
    pub baseline: TestSummary,
    pub final_tests: TestSummary,
    pub ranking_rationale: Option<String>,
    pub results: Vec<DefectResult>,
    pub model_calls: u32,
    pub warnings: Vec<String>,
    pub duration_ms: u128,
    /// Combined unified diff of every accepted fix.
    pub patch: String,
}

impl RepairReport {
    pub fn fixed_count(&self) -> usize {
        self.results
            .iter()
            .filter(|r| r.status == DefectStatus::Fixed)
            .count()
    }

    pub fn to_markdown(&self) -> String {
        let mut s = String::new();
        s.push_str("# sentryclaw repair report\n\n");
        s.push_str(&format!(
            "- Repo: `{}`\n- Model: `{}`\n",
            self.repo, self.model
        ));
        s.push_str(&format!(
            "- Sandbox: `{:?}`, test command: `{}`\n",
            self.isolation,
            self.test_command.join(" ")
        ));
        s.push_str(&format!(
            "- Baseline: {} ({} failing)\n- Final: {} ({} failing)\n",
            if self.baseline.passed { "pass" } else { "FAIL" },
            self.baseline.failed_tests.len(),
            if self.final_tests.passed {
                "pass"
            } else {
                "FAIL"
            },
            self.final_tests.failed_tests.len()
        ));
        s.push_str(&format!(
            "- Fixed {}/{} defects, {} model calls, {:.1}s\n\n",
            self.fixed_count(),
            self.results.len(),
            self.model_calls,
            self.duration_ms as f64 / 1000.0
        ));
        if let Some(r) = &self.ranking_rationale {
            s.push_str(&format!("Ranking rationale: {}\n\n", r));
        }
        s.push_str("| # | Defect | Location | Status | Attempts |\n|---|---|---|---|---|\n");
        for r in &self.results {
            let loc = match (&r.defect.file, r.defect.line) {
                (Some(f), Some(l)) => format!("{}:{}", f, l),
                (Some(f), None) => f.clone(),
                _ => "-".into(),
            };
            s.push_str(&format!(
                "| {} | {} | {} | {:?} | {} |\n",
                r.defect.id,
                r.defect.title.replace('|', "\\|"),
                loc,
                r.status,
                r.attempts.len()
            ));
        }
        for r in &self.results {
            for a in &r.attempts {
                if let Verdict::Rejected { reason } = &a.verdict {
                    let first = reason.lines().next().unwrap_or("");
                    s.push_str(&format!(
                        "\n- {} attempt {} rejected: {}",
                        r.defect.id, a.attempt, first
                    ));
                }
            }
        }
        if !self.warnings.is_empty() {
            s.push_str("\n\n## Warnings\n");
            for w in &self.warnings {
                s.push_str(&format!("- {}\n", w));
            }
        }
        s.push_str("\n\n## Patch\n\n```diff\n");
        s.push_str(&self.patch);
        s.push_str("```\n");
        s
    }
}

fn say(verbose: bool, msg: &str) {
    if verbose {
        eprintln!("[repair] {}", msg);
    }
}

fn short(reason: &str, max: usize) -> String {
    if reason.len() <= max {
        return reason.to_string();
    }
    let mut end = reason.len() - max;
    while !reason.is_char_boundary(end) {
        end += 1;
    }
    format!("[...]\n{}", &reason[end..])
}

/// Run the whole loop on `repo` (which is copied, never modified).
pub async fn run_repair(
    repo: &Path,
    cfg: &RepairConfig,
    model: &dyn ChatModel,
    verbose: bool,
) -> anyhow::Result<RepairReport> {
    let started = Instant::now();
    let p = &cfg.pipeline;
    let mut warnings = Vec::new();
    let mut calls = 0u32;

    let mut sandbox = Sandbox::create(repo, cfg.sandbox.clone())?;
    say(
        verbose,
        &format!(
            "sandbox {:?}, tests: {}",
            sandbox.isolation(),
            sandbox.test_command().join(" ")
        ),
    );
    if sandbox.isolation() == Isolation::None {
        warnings.push("tests ran without a sandbox (no bwrap/unshare available)".into());
    }

    // 0. Baseline.
    let baseline = sandbox.run_tests().await?;
    sandbox.mark_baseline()?;
    say(
        verbose,
        &format!(
            "baseline: {} ({} failing)",
            if baseline.passed { "pass" } else { "FAIL" },
            baseline.failed_tests.len()
        ),
    );

    // 1. Finder.
    let mut snapshot =
        RepoSnapshot::collect(sandbox.work_dir(), p.max_file_chars, p.max_context_chars);
    calls += 1;
    let (defects, w) = Finder.run(model, &snapshot, &baseline).await;
    warnings.extend(w);
    say(verbose, &format!("finder: {} defects", defects.len()));

    // 2. Ranker.
    if defects.len() > 1 {
        calls += 1;
    }
    let (mut ranked, rationale, w) = Ranker.run(model, defects).await;
    warnings.extend(w);
    let dropped = ranked.len().saturating_sub(p.max_defects);
    ranked.truncate(p.max_defects);
    if dropped > 0 {
        warnings.push(format!(
            "{} lower-ranked defects not attempted (max_defects)",
            dropped
        ));
    }
    say(
        verbose,
        &format!(
            "ranker: {}",
            ranked
                .iter()
                .map(|d| d.id.as_str())
                .collect::<Vec<_>>()
                .join(" > ")
        ),
    );

    // 3. Fix/test loop per defect.
    let fixer = Fixer {
        protect_tests: p.protect_tests,
    };
    let mut current = baseline.clone();
    let mut results = Vec::new();
    for defect in ranked {
        if p.stop_when_green && current.passed && !baseline.passed {
            results.push(DefectResult {
                defect,
                status: DefectStatus::Skipped,
                note: Some("suite already green".into()),
                attempts: vec![],
            });
            continue;
        }
        let mut history: Vec<Feedback> = Vec::new();
        let mut attempts = Vec::new();
        let mut status = DefectStatus::Failed;
        let mut note = None;
        for n in 1..=p.max_attempts.max(1) {
            if calls >= p.max_model_calls {
                note = Some("model call budget exhausted".into());
                break;
            }
            calls += 1;
            let diff = match fixer.propose(model, &snapshot, &defect, &history).await {
                Ok(d) => d,
                Err(problem) => {
                    say(
                        verbose,
                        &format!("{} attempt {}: no usable patch ({})", defect.id, n, problem),
                    );
                    attempts.push(AttemptRecord {
                        attempt: n,
                        patch: None,
                        verdict: Verdict::Rejected {
                            reason: problem.clone(),
                        },
                    });
                    history.push(Feedback {
                        patch: None,
                        problem,
                    });
                    continue;
                }
            };
            let (verdict, after) = Tester.check(&sandbox, &diff, &current).await?;
            match &verdict {
                Verdict::AllPass | Verdict::Progress { .. } => {
                    sandbox.commit_paths(
                        &format!("fix {}: {}", defect.id, defect.title),
                        &super::patch::touched_paths(&diff),
                    )?;
                    if let Some(a) = after {
                        current = a;
                    }
                    snapshot = RepoSnapshot::collect(
                        sandbox.work_dir(),
                        p.max_file_chars,
                        p.max_context_chars,
                    );
                    status = DefectStatus::Fixed;
                    if baseline.passed {
                        note = Some(
                            "suite was already green; fix is not proven by a failing test".into(),
                        );
                    }
                    say(
                        verbose,
                        &format!("{} attempt {}: accepted ({:?})", defect.id, n, verdict),
                    );
                }
                Verdict::Rejected { reason } => {
                    sandbox.reset()?;
                    say(
                        verbose,
                        &format!(
                            "{} attempt {}: rejected, {}",
                            defect.id,
                            n,
                            reason.lines().next().unwrap_or("")
                        ),
                    );
                    history.push(Feedback {
                        patch: Some(diff.clone()),
                        problem: short(reason, 4000),
                    });
                }
            }
            attempts.push(AttemptRecord {
                attempt: n,
                patch: Some(diff),
                verdict: verdict.clone(),
            });
            if status == DefectStatus::Fixed {
                break;
            }
        }
        results.push(DefectResult {
            defect,
            status,
            note,
            attempts,
        });
    }

    let patch = sandbox.diff_from_baseline()?;
    Ok(RepairReport {
        repo: repo.display().to_string(),
        model: model.name(),
        isolation: sandbox.isolation(),
        test_command: sandbox.test_command().to_vec(),
        baseline: (&baseline).into(),
        final_tests: (&current).into(),
        ranking_rationale: rationale,
        results,
        model_calls: calls,
        warnings,
        duration_ms: started.elapsed().as_millis(),
        patch,
    })
}
