//! Demo mode: reviews bundled sample diffs with the rule engine only.
//! Needs no LLM, no GitHub App, no database and no config file.

use axum::{
    extract::Path,
    http::StatusCode,
    response::{Html, Json},
    routing::{get, post},
    Router,
};
use serde::Serialize;

use crate::rule_engine::{ReviewRule, RuleEngine, SeverityLevel};

pub const SAMPLES: &[(&str, &str)] = &[
    (
        "01-hardcoded-secret",
        include_str!("../demo/diffs/01-hardcoded-secret.diff"),
    ),
    (
        "02-unwrap-and-todo",
        include_str!("../demo/diffs/02-unwrap-and-todo.diff"),
    ),
    (
        "03-sql-injection",
        include_str!("../demo/diffs/03-sql-injection.diff"),
    ),
    ("04-clean", include_str!("../demo/diffs/04-clean.diff")),
];

fn rule(name: &str, pattern: &str, severity: SeverityLevel, message: &str) -> ReviewRule {
    ReviewRule {
        name: name.to_string(),
        description: message.to_string(),
        pattern: pattern.to_string(),
        severity,
        message: message.to_string(),
    }
}

/// Built-in heuristic rules used in place of a model.
pub fn demo_engine() -> RuleEngine {
    let mut engine = RuleEngine::new();
    engine.add_rules(vec![
        rule(
            "hardcoded-token",
            r#"(ghp_[A-Za-z0-9]{20,}|AKIA[0-9A-Z]{16}|(?i)(password|secret|token)\s*=\s*"[^"]+")"#,
            SeverityLevel::Critical,
            "Possible hard-coded credential. Load it from the environment or a secret store.",
        ),
        rule(
            "sql-concat",
            r#"(?i)(select|insert|update|delete)\b.*["']\s*\+"#,
            SeverityLevel::Critical,
            "SQL built by string concatenation. Use parameterized queries.",
        ),
        rule(
            "eval",
            r"\beval\(",
            SeverityLevel::Critical,
            "eval() on dynamic input allows code execution.",
        ),
        rule(
            "unsafe-block",
            r"\bunsafe\s*\{",
            SeverityLevel::Warning,
            "New unsafe block. Document the invariants that make it sound.",
        ),
        rule(
            "unwrap",
            r"\.(unwrap|expect)\(",
            SeverityLevel::Warning,
            "unwrap()/expect() can panic. Propagate the error instead.",
        ),
        rule(
            "print-debug",
            r"\b(println!|dbg!|console\.log)\(",
            SeverityLevel::Info,
            "Debug output left in code.",
        ),
        rule(
            "todo",
            r"(?i)\b(TODO|FIXME)\b",
            SeverityLevel::Info,
            "Unresolved TODO/FIXME.",
        ),
    ]);
    engine
}

#[derive(Serialize)]
pub struct Finding {
    pub rule: String,
    pub severity: String,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub message: String,
}

#[derive(Serialize)]
pub struct DemoReview {
    pub sample: String,
    pub verdict: String,
    pub findings: Vec<Finding>,
    pub summary: String,
}

pub fn review_diff(name: &str, diff: &str) -> DemoReview {
    let matches = demo_engine().check_diff(diff);
    let critical = matches
        .iter()
        .filter(|m| m.severity == SeverityLevel::Critical)
        .count();
    let verdict = if critical > 0 {
        "request_changes"
    } else if matches.is_empty() {
        "approve"
    } else {
        "comment"
    };
    let summary = format!(
        "{} finding(s), {} critical. Demo mode: rule-based review, no model used.",
        matches.len(),
        critical
    );
    DemoReview {
        sample: name.to_string(),
        verdict: verdict.to_string(),
        findings: matches
            .into_iter()
            .map(|m| Finding {
                rule: m.rule_name,
                severity: m.severity.to_string(),
                file: m.file_path,
                line: m.line,
                message: m.message,
            })
            .collect(),
        summary,
    }
}

pub fn render_text(r: &DemoReview) -> String {
    let mut out = format!("== {} → {}\n", r.sample, r.verdict);
    for f in &r.findings {
        out.push_str(&format!(
            "  [{}] {}:{} {} ({})\n",
            f.severity,
            f.file.as_deref().unwrap_or("?"),
            f.line.unwrap_or(0),
            f.message,
            f.rule
        ));
    }
    out.push_str(&format!("  {}\n", r.summary));
    out
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

async fn index() -> Html<String> {
    let mut body = String::from(
        "<!doctype html><meta charset=utf-8><title>SentryClaw demo</title>\
         <style>body{font-family:monospace;background:#111;color:#ddd;max-width:960px;margin:2em auto}\
         pre{background:#222;padding:1em;overflow:auto}.critical{color:#f55}.warning{color:#fc5}.info{color:#5cf}</style>\
         <h1>SentryClaw demo</h1><p>Rule-based reviews of bundled sample diffs. No model, no GitHub App.\
         POST a unified diff to <code>/demo/api/review</code> to review your own.</p>",
    );
    for (name, diff) in SAMPLES {
        let r = review_diff(name, diff);
        body.push_str(&format!(
            "<h2>{} — {}</h2><pre>{}</pre><ul>",
            name,
            r.verdict,
            escape(diff)
        ));
        for f in &r.findings {
            body.push_str(&format!(
                "<li class={}>[{}] {}:{} {}</li>",
                f.severity,
                f.severity,
                escape(f.file.as_deref().unwrap_or("?")),
                f.line.unwrap_or(0),
                escape(&f.message)
            ));
        }
        body.push_str(&format!("</ul><p>{}</p>", r.summary));
    }
    Html(body)
}

async fn list() -> Json<Vec<DemoReview>> {
    Json(SAMPLES.iter().map(|(n, d)| review_diff(n, d)).collect())
}

async fn one(Path(name): Path<String>) -> Result<Json<DemoReview>, StatusCode> {
    SAMPLES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(n, d)| Json(review_diff(n, d)))
        .ok_or(StatusCode::NOT_FOUND)
}

async fn custom(body: String) -> Json<DemoReview> {
    Json(review_diff("custom", &body))
}

pub fn router() -> Router {
    Router::new()
        .route("/", get(index))
        .route("/demo", get(index))
        .route("/demo/api/samples", get(list))
        .route("/demo/api/samples/:name", get(one))
        .route("/demo/api/review", post(custom))
        .route("/health", get(|| async { "ok" }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_produce_expected_verdicts() {
        let v: Vec<String> = SAMPLES
            .iter()
            .map(|(n, d)| review_diff(n, d).verdict)
            .collect();
        assert_eq!(
            v,
            vec!["request_changes", "comment", "request_changes", "approve"]
        );
    }
}
