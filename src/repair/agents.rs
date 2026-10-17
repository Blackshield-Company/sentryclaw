//! The four agents: Finder, Ranker, Fixer, Tester.
//!
//! Each agent is a small struct with one async method. They only share
//! plain data types ([`Defect`], [`TestOutcome`]) so any one of them can be
//! swapped (e.g. a static-analysis-only finder) without touching the others.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::model::{AgentRole, ChatModel, ModelRequest};
use super::patch;
use super::sandbox::{is_skipped_dir, Sandbox, TestOutcome};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Critical,
    High,
    #[default]
    Medium,
    Low,
}

impl Severity {
    fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "critical" | "blocker" => Severity::Critical,
            "high" | "major" | "error" => Severity::High,
            "low" | "minor" | "info" => Severity::Low,
            _ => Severity::Medium,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Defect {
    pub id: String,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub title: String,
    pub description: String,
    pub severity: Severity,
    /// Test names this defect is believed to cause (may be empty).
    #[serde(default)]
    pub tests: Vec<String>,
    /// "model" or "test-failure".
    pub source: String,
}

/// Source files handed to the models, capped by size.
#[derive(Debug, Clone)]
pub struct RepoSnapshot {
    pub files: Vec<(String, String)>,
}

const SOURCE_EXTS: &[&str] = &[
    "rs", "py", "js", "ts", "go", "c", "h", "cc", "cpp", "hpp", "java", "kt", "rb", "toml",
];

impl RepoSnapshot {
    pub fn collect(root: &Path, max_file: usize, max_total: usize) -> Self {
        let mut files = Vec::new();
        let mut total = 0usize;
        let mut stack = vec![root.to_path_buf()];
        let mut paths = Vec::new();
        while let Some(dir) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                let name = e.file_name().to_string_lossy().to_string();
                match e.file_type() {
                    Ok(ft) if ft.is_dir() => {
                        if !is_skipped_dir(&name) && !name.starts_with('.') {
                            stack.push(p);
                        }
                    }
                    Ok(ft) if ft.is_file() => {
                        let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("");
                        if SOURCE_EXTS.contains(&ext) {
                            paths.push(p);
                        }
                    }
                    _ => {}
                }
            }
        }
        paths.sort();
        for p in paths {
            let Ok(text) = std::fs::read_to_string(&p) else {
                continue;
            };
            if text.len() > max_file || total + text.len() > max_total {
                continue;
            }
            total += text.len();
            let rel = p
                .strip_prefix(root)
                .unwrap_or(&p)
                .to_string_lossy()
                .to_string();
            files.push((rel, text));
        }
        Self { files }
    }

    pub fn get(&self, rel: &str) -> Option<&str> {
        self.files
            .iter()
            .find(|(p, _)| p == rel)
            .map(|(_, t)| t.as_str())
    }

    pub fn render(&self, only: Option<&str>) -> String {
        let mut s = String::new();
        for (p, t) in &self.files {
            if only.is_some_and(|o| o != p) {
                continue;
            }
            s.push_str(&format!("=== FILE: {} ===\n{}\n", p, t));
        }
        s
    }
}

/// Find a JSON value (array or object) in a model reply.
pub fn extract_json(reply: &str) -> Option<serde_json::Value> {
    if let Some(i) = reply.find("```json") {
        let body = &reply[i + 7..];
        let end = body.find("```").unwrap_or(body.len());
        if let Ok(v) = serde_json::from_str(body[..end].trim()) {
            return Some(v);
        }
    }
    if let Ok(v) = serde_json::from_str(reply.trim()) {
        return Some(v);
    }
    for (open, close) in [('[', ']'), ('{', '}')] {
        if let (Some(a), Some(b)) = (reply.find(open), reply.rfind(close)) {
            if a < b {
                if let Ok(v) = serde_json::from_str(&reply[a..=b]) {
                    return Some(v);
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------- Finder

pub struct Finder;

const FINDER_SYSTEM: &str = "You are the Finder agent in an automated code repair system. \
Identify real defects (bugs, crashes, wrong results, security issues) in the repository. \
Use the failing test output as strong evidence. Do not report style nits. \
Reply with ONLY a JSON array; each item: \
{\"file\": \"relative/path\", \"line\": 12, \"title\": \"short\", \"description\": \"what is wrong and why\", \
\"severity\": \"critical|high|medium|low\", \"tests\": [\"failing test names this explains\"]}";

impl Finder {
    pub async fn run(
        &self,
        model: &dyn ChatModel,
        snapshot: &RepoSnapshot,
        baseline: &TestOutcome,
    ) -> (Vec<Defect>, Option<String>) {
        let user = format!(
            "Test command result: {}\nFailing tests: {:?}\n\nTest output (tail):\n{}\n\nRepository files:\n{}",
            if baseline.passed { "PASS" } else { "FAIL" },
            baseline.failed_tests,
            baseline.output,
            snapshot.render(None)
        );
        let req = ModelRequest {
            role: AgentRole::Finder,
            system: FINDER_SYSTEM.into(),
            user,
        };
        let mut warning = None;
        let mut defects = match model.complete(&req).await {
            Ok(reply) => parse_defects(&reply),
            Err(e) => {
                warning = Some(format!("finder model call failed: {}", e));
                Vec::new()
            }
        };
        // Static signal: every failing test not explained by a model finding
        // becomes its own defect, so the loop still works with a weak model.
        let explained: BTreeSet<String> = defects.iter().flat_map(|d| d.tests.clone()).collect();
        for t in &baseline.failed_tests {
            if !explained
                .iter()
                .any(|e| e == t || t.ends_with(e.as_str()) || e.ends_with(t.as_str()))
            {
                defects.push(Defect {
                    id: String::new(),
                    file: None,
                    line: None,
                    title: format!("failing test {}", t),
                    description: format!("Test `{}` fails on the unmodified code.", t),
                    severity: Severity::High,
                    tests: vec![t.clone()],
                    source: "test-failure".into(),
                });
            }
        }
        if defects.is_empty() && !baseline.passed && baseline.failed_tests.is_empty() {
            defects.push(Defect {
                id: String::new(),
                file: None,
                line: None,
                title: "build or test command fails".into(),
                description: "The test command fails before reporting any test results (likely a build error).".into(),
                severity: Severity::Critical,
                tests: Vec::new(),
                source: "test-failure".into(),
            });
        }
        for (i, d) in defects.iter_mut().enumerate() {
            d.id = format!("D{}", i + 1);
            if let Some(f) = &d.file {
                if snapshot.get(f).is_none() {
                    // Model named a file we don't have; don't trust it.
                    d.file = None;
                }
            }
        }
        (defects, warning)
    }
}

pub fn parse_defects(reply: &str) -> Vec<Defect> {
    let Some(v) = extract_json(reply) else {
        return Vec::new();
    };
    let items = match v {
        serde_json::Value::Array(a) => a,
        serde_json::Value::Object(mut o) => match o.remove("defects") {
            Some(serde_json::Value::Array(a)) => a,
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    items
        .into_iter()
        .filter_map(|it| {
            let s = |k: &str| it.get(k).and_then(|x| x.as_str()).map(|x| x.to_string());
            let title = s("title").or_else(|| s("description"))?;
            Some(Defect {
                id: String::new(),
                file: s("file").map(|f| f.trim_start_matches("./").to_string()),
                line: it.get("line").and_then(|x| x.as_u64()).map(|x| x as u32),
                description: s("description").unwrap_or_else(|| title.clone()),
                title,
                severity: Severity::parse(&s("severity").unwrap_or_default()),
                tests: it
                    .get("tests")
                    .and_then(|x| x.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|t| t.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default(),
                source: "model".into(),
            })
        })
        .collect()
}

// ---------------------------------------------------------------- Ranker

pub struct Ranker;

const RANKER_SYSTEM: &str = "You are the Ranker agent in an automated code repair system. \
Order the defects so the most important and most fixable come first: real failures and crashes \
before speculative issues, and root causes before symptoms. Drop duplicates. \
Reply with ONLY a JSON object: {\"order\": [\"D2\", \"D1\"], \"rationale\": \"one sentence\"}";

impl Ranker {
    /// Returns defects in the order to fix, and the model's rationale if any.
    pub async fn run(
        &self,
        model: &dyn ChatModel,
        defects: Vec<Defect>,
    ) -> (Vec<Defect>, Option<String>, Option<String>) {
        if defects.len() <= 1 {
            return (defects, None, None);
        }
        let req = ModelRequest {
            role: AgentRole::Ranker,
            system: RANKER_SYSTEM.into(),
            user: format!(
                "Defects:\n{}",
                serde_json::to_string_pretty(&defects).unwrap_or_default()
            ),
        };
        match model.complete(&req).await {
            Ok(reply) => {
                let (order, rationale) = parse_ranking(&reply);
                (apply_order(defects, &order), rationale, None)
            }
            Err(e) => (
                apply_order(defects, &[]),
                None,
                Some(format!(
                    "ranker model call failed, used severity order: {}",
                    e
                )),
            ),
        }
    }
}

pub fn parse_ranking(reply: &str) -> (Vec<String>, Option<String>) {
    let Some(v) = extract_json(reply) else {
        return (Vec::new(), None);
    };
    let arr = match &v {
        serde_json::Value::Array(a) => Some(a.clone()),
        serde_json::Value::Object(o) => o.get("order").and_then(|x| x.as_array()).cloned(),
        _ => None,
    };
    let order = arr
        .unwrap_or_default()
        .iter()
        .filter_map(|x| x.as_str().map(String::from))
        .collect();
    let rationale = v
        .get("rationale")
        .and_then(|x| x.as_str())
        .map(String::from);
    (order, rationale)
}

/// Model order first (unknown ids ignored), then the rest by severity.
pub fn apply_order(mut defects: Vec<Defect>, order: &[String]) -> Vec<Defect> {
    let mut out = Vec::new();
    for id in order {
        if let Some(i) = defects.iter().position(|d| &d.id == id) {
            out.push(defects.remove(i));
        }
    }
    defects.sort_by_key(|d| d.severity);
    out.extend(defects);
    out
}

// ---------------------------------------------------------------- Fixer

pub struct Fixer {
    pub protect_tests: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Feedback {
    pub patch: Option<String>,
    pub problem: String,
}

const FIXER_SYSTEM: &str = "You are the Fixer agent in an automated code repair system. \
Write the smallest correct change that fixes the given defect. Never edit, delete or disable tests. \
Reply with ONLY a unified diff in a ```diff fenced block, with paths relative to the repo root \
using a/ and b/ prefixes (--- a/src/x.rs, +++ b/src/x.rs) and correct @@ hunk headers with 3 lines of context.";

impl Fixer {
    pub async fn propose(
        &self,
        model: &dyn ChatModel,
        snapshot: &RepoSnapshot,
        defect: &Defect,
        history: &[Feedback],
    ) -> Result<String, String> {
        let files = match &defect.file {
            Some(f) => snapshot.render(Some(f)),
            None => snapshot.render(None),
        };
        let mut user = format!(
            "Defect {}: {}\nSeverity: {:?}\nLocation: {}{}\nDetails: {}\nRelated tests: {:?}\n\nFiles:\n{}",
            defect.id,
            defect.title,
            defect.severity,
            defect.file.as_deref().unwrap_or("unknown"),
            defect.line.map(|l| format!(":{}", l)).unwrap_or_default(),
            defect.description,
            defect.tests,
            files
        );
        if !history.is_empty() {
            user.push_str("\nPrevious attempts failed. Do not repeat them.\n");
            for (i, h) in history.iter().enumerate() {
                user.push_str(&format!(
                    "\n--- Attempt {} ---\nPatch:\n{}\nResult:\n{}\n",
                    i + 1,
                    h.patch.as_deref().unwrap_or("(no usable diff)"),
                    h.problem
                ));
            }
        }
        let req = ModelRequest {
            role: AgentRole::Fixer,
            system: FIXER_SYSTEM.into(),
            user,
        };
        let reply = model
            .complete(&req)
            .await
            .map_err(|e| format!("fixer model call failed: {}", e))?;
        let diff = patch::extract_diff(&reply)
            .ok_or_else(|| "reply contained no unified diff".to_string())?;
        patch::vet_diff(&diff, self.protect_tests)?;
        Ok(diff)
    }
}

// ---------------------------------------------------------------- Tester

pub struct Tester;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum Verdict {
    /// Whole suite passes.
    AllPass,
    /// Fewer failing tests, no new ones.
    Progress {
        remaining: Vec<String>,
    },
    Rejected {
        reason: String,
    },
}

impl Tester {
    /// Apply `diff` in the sandbox, run the target's tests, judge against `before`.
    pub async fn check(
        &self,
        sandbox: &Sandbox,
        diff: &str,
        before: &TestOutcome,
    ) -> anyhow::Result<(Verdict, Option<TestOutcome>)> {
        if let Err(e) = sandbox.apply_patch(diff) {
            return Ok((
                Verdict::Rejected {
                    reason: format!("patch did not apply: {}", e),
                },
                None,
            ));
        }
        let after = sandbox.run_tests().await?;
        Ok((judge(before, &after), Some(after)))
    }
}

pub fn judge(before: &TestOutcome, after: &TestOutcome) -> Verdict {
    if after.timed_out {
        return Verdict::Rejected {
            reason: "tests timed out".into(),
        };
    }
    if after.passed {
        return Verdict::AllPass;
    }
    if !after.summary_seen {
        return Verdict::Rejected {
            reason: format!("build or test run failed:\n{}", after.output),
        };
    }
    let new: Vec<&String> = after
        .failed_tests
        .difference(&before.failed_tests)
        .collect();
    if !new.is_empty() {
        return Verdict::Rejected {
            reason: format!("introduced new failures {:?}:\n{}", new, after.output),
        };
    }
    if after.failed_tests.len() < before.failed_tests.len() {
        return Verdict::Progress {
            remaining: after.failed_tests.iter().cloned().collect(),
        };
    }
    Verdict::Rejected {
        reason: format!(
            "no failing test was fixed (still failing: {:?}):\n{}",
            after.failed_tests, after.output
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(passed: bool, summary: bool, failed: &[&str]) -> TestOutcome {
        TestOutcome {
            passed,
            exit_code: Some(if passed { 0 } else { 101 }),
            timed_out: false,
            summary_seen: summary,
            failed_tests: failed.iter().map(|s| s.to_string()).collect(),
            duration_ms: 0,
            output: String::new(),
        }
    }

    #[test]
    fn judge_rules() {
        let before = outcome(false, true, &["a", "b"]);
        assert_eq!(judge(&before, &outcome(true, true, &[])), Verdict::AllPass);
        assert!(matches!(
            judge(&before, &outcome(false, true, &["b"])),
            Verdict::Progress { .. }
        ));
        assert!(matches!(
            judge(&before, &outcome(false, true, &["a", "b"])),
            Verdict::Rejected { .. }
        ));
        assert!(matches!(
            judge(&before, &outcome(false, true, &["c"])),
            Verdict::Rejected { .. }
        ));
        assert!(matches!(
            judge(&before, &outcome(false, false, &[])),
            Verdict::Rejected { .. }
        ));
    }

    #[test]
    fn parses_defects_and_ranking() {
        let reply = "Found:\n```json\n[{\"file\":\"src/lib.rs\",\"line\":3,\"title\":\"off by one\",\"severity\":\"HIGH\",\"tests\":[\"t1\"]}]\n```";
        let d = parse_defects(reply);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::High);
        assert_eq!(d[0].tests, vec!["t1".to_string()]);
        let (order, why) = parse_ranking("{\"order\":[\"D2\",\"D1\"],\"rationale\":\"r\"}");
        assert_eq!(order, vec!["D2", "D1"]);
        assert_eq!(why.as_deref(), Some("r"));
        assert!(parse_defects("nothing useful").is_empty());
    }

    #[test]
    fn apply_order_falls_back_to_severity() {
        let mk = |id: &str, sev| Defect {
            id: id.into(),
            file: None,
            line: None,
            title: id.into(),
            description: String::new(),
            severity: sev,
            tests: vec![],
            source: "model".into(),
        };
        let ds = vec![
            mk("D1", Severity::Low),
            mk("D2", Severity::Critical),
            mk("D3", Severity::Medium),
        ];
        let ids = |v: Vec<Defect>| v.into_iter().map(|d| d.id).collect::<Vec<_>>();
        assert_eq!(ids(apply_order(ds.clone(), &[])), vec!["D2", "D3", "D1"]);
        assert_eq!(
            ids(apply_order(ds, &["D1".into(), "bogus".into()])),
            vec!["D1", "D2", "D3"]
        );
    }
}
