//! End-to-end tests for `sentryclaw repair` using the mock model and the
//! planted-bug fixture in demo/repair-fixture.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use sentryclaw::repair::agents::Verdict;
use sentryclaw::repair::model::{AgentRole, ChatModel, MockScript, ModelRequest};
use sentryclaw::repair::orchestrator::DefectStatus;
use sentryclaw::repair::{run_repair, EndpointConfig, MockModel, OpenAiCompatModel, RepairConfig};

/// (Authorization header, JSON body) captured by the stub server.
type Seen = (Option<String>, serde_json::Value);

/// Serialise the sandboxed cargo runs so the fixture builds one at a time.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture() -> PathBuf {
    root().join("demo/repair-fixture")
}

fn demo_config() -> RepairConfig {
    let mut cfg = RepairConfig::default();
    cfg.sandbox.timeout_secs = 180;
    cfg.sandbox.jobs = 2;
    cfg
}

fn demo_script() -> MockScript {
    let text = std::fs::read_to_string(root().join("demo/repair/mock_script.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

#[tokio::test]
async fn full_loop_fixes_planted_bugs_with_feedback() {
    let _guard = SERIAL.lock().await;
    let before = std::fs::read_to_string(fixture().join("src/lib.rs")).unwrap();
    let model = MockModel::from_script(demo_script());
    let report = run_repair(&fixture(), &demo_config(), &model, false)
        .await
        .unwrap();

    assert!(!report.baseline.passed);
    assert_eq!(report.baseline.failed_tests.len(), 3);
    assert!(report.final_tests.passed, "final: {:?}", report.final_tests);
    assert_eq!(report.fixed_count(), 3);

    // Ranker reordered: D2 (sum_to) first, then D3, then D1.
    let ids: Vec<_> = report
        .results
        .iter()
        .map(|r| r.defect.id.as_str())
        .collect();
    assert_eq!(ids, vec!["D2", "D3", "D1"]);

    // First sum_to patch was wrong, rejected, and fed back to the fixer.
    let sum = &report.results[0];
    assert_eq!(sum.attempts.len(), 2);
    assert!(matches!(sum.attempts[0].verdict, Verdict::Rejected { .. }));
    let fixer_calls: Vec<ModelRequest> = model
        .calls()
        .into_iter()
        .filter(|c| c.role == AgentRole::Fixer)
        .collect();
    assert_eq!(fixer_calls.len(), 4);
    assert!(!fixer_calls[0].user.contains("Previous attempts failed"));
    assert!(fixer_calls[1].user.contains("Previous attempts failed"));
    assert!(fixer_calls[1].user.contains("(0..n).sum()"));

    assert!(report.patch.contains("(1..=n).sum()"));
    assert!(report.patch.contains("year % 400 == 0"));
    assert!(report.patch.contains("values.is_empty()"));
    // Only the source file changes; build by-products (Cargo.lock) stay out.
    assert!(!report.patch.contains("Cargo.lock"), "{}", report.patch);

    // The source repo is never touched.
    let after = std::fs::read_to_string(fixture().join("src/lib.rs")).unwrap();
    assert_eq!(before, after);
    assert!(!fixture().join("target").exists());
}

#[tokio::test]
async fn retry_cap_stops_a_hopeless_defect() {
    let _guard = SERIAL.lock().await;
    let mut script = demo_script();
    script.ranker.clear();
    // Always the same wrong patch.
    let wrong = script.fixer[0].clone();
    script.fixer = vec![wrong.clone(), wrong.clone(), wrong.clone(), wrong];
    let model = MockModel::from_script(script);
    let mut cfg = demo_config();
    cfg.pipeline.max_attempts = 2;
    cfg.pipeline.max_defects = 1;
    let report = run_repair(&fixture(), &cfg, &model, false).await.unwrap();

    assert_eq!(report.results.len(), 1);
    let r = &report.results[0];
    assert_eq!(r.status, DefectStatus::Failed);
    assert_eq!(r.attempts.len(), 2);
    assert!(!report.final_tests.passed);
    assert!(report.patch.trim().is_empty());
    // Ranker failed (empty script): falls back to severity order with a warning.
    assert!(report.warnings.iter().any(|w| w.contains("ranker")));
}

#[tokio::test]
async fn patches_that_touch_tests_are_rejected() {
    let _guard = SERIAL.lock().await;
    let mut script = demo_script();
    let cheat = "```diff\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -36,7 +36,6 @@ mod tests {\n     #[test]\n     fn sum_to_is_inclusive() {\n         assert_eq!(sum_to(0), 0);\n-        assert_eq!(sum_to(1), 1);\n         assert_eq!(sum_to(10), 55);\n     }\n \n```".to_string();
    script.fixer = vec![cheat];
    let model = MockModel::from_script(script);
    let mut cfg = demo_config();
    cfg.pipeline.max_attempts = 1;
    cfg.pipeline.max_defects = 1;
    let report = run_repair(&fixture(), &cfg, &model, false).await.unwrap();
    let a = &report.results[0].attempts[0];
    match &a.verdict {
        Verdict::Rejected { reason } => assert!(reason.contains("test code"), "{}", reason),
        v => panic!("expected rejection, got {:?}", v),
    }
}

#[tokio::test]
async fn finder_failure_still_yields_test_derived_defects() {
    let _guard = SERIAL.lock().await;
    // Finder reply is useless and the other scripts are empty: the model
    // "fails", but the static test-failure signals remain.
    let script = MockScript {
        finder: vec!["I could not find anything.".into()],
        ranker: vec![],
        fixer: vec![],
    };
    let model = MockModel::from_script(script);
    let mut cfg = demo_config();
    cfg.pipeline.max_attempts = 1;
    let report = run_repair(&fixture(), &cfg, &model, false).await.unwrap();
    assert_eq!(report.results.len(), 3);
    assert!(report
        .results
        .iter()
        .all(|r| r.defect.source == "test-failure"));
    assert!(report
        .results
        .iter()
        .all(|r| r.status == DefectStatus::Failed));
}

#[tokio::test]
async fn openai_compatible_client_talks_to_a_local_server() {
    use axum::{routing::post, Json, Router};

    let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
    let seen2 = seen.clone();
    let app = Router::new().route(
        "/v1/chat/completions",
        post(
            move |headers: axum::http::HeaderMap, Json(body): Json<serde_json::Value>| {
                let seen = seen2.clone();
                async move {
                    let auth = headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .map(String::from);
                    seen.lock().unwrap().push((auth, body));
                    Json(serde_json::json!({
                        "choices": [{"message": {"role": "assistant", "content": "hello"}}]
                    }))
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    std::env::set_var("REPAIR_TEST_KEY_XYZ", "sekret");
    let model = OpenAiCompatModel::new(EndpointConfig {
        base_url: format!("http://{}/v1", addr),
        model: "coder".into(),
        api_key_env: Some("REPAIR_TEST_KEY_XYZ".into()),
        ..Default::default()
    });
    let req = ModelRequest {
        role: AgentRole::Fixer,
        system: "sys".into(),
        user: "usr".into(),
    };
    assert_eq!(model.complete(&req).await.unwrap(), "hello");
    let seen = seen.lock().unwrap();
    let (auth, body) = &seen[0];
    assert_eq!(auth.as_deref(), Some("Bearer sekret"));
    assert_eq!(body["model"], "coder");
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][1]["content"], "usr");
}
