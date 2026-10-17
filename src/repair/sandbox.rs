//! Sandboxed workspace and test runner.
//!
//! The target repo is copied into a private temp dir (never edited in
//! place). Tests run there with a wall-clock timeout, a scrubbed environment,
//! and, when the host allows it, no network:
//! - `bwrap`: read-only host filesystem, writable sandbox dir only, private
//!   /tmp, no network, own PID namespace.
//! - `unshare -rn`: no network only.
//! - none: plain process (reported as such).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

const SKIP_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    ".venv",
    "venv",
    "__pycache__",
    ".pytest_cache",
    "dist",
    "build",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Isolation {
    Bwrap,
    UnshareNet,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    /// argv for the target's test suite. Empty means auto-detect.
    #[serde(default)]
    pub test_command: Vec<String>,
    #[serde(default = "default_test_timeout")]
    pub timeout_secs: u64,
    /// Deny network to the test process when the host supports it.
    #[serde(default = "default_true")]
    pub deny_network: bool,
    /// Fail instead of running unisolated when no sandbox tool works.
    #[serde(default)]
    pub require_isolation: bool,
    /// Build parallelism passed to cargo/make (keeps RAM low).
    #[serde(default = "default_jobs")]
    pub jobs: u32,
    /// Bytes of combined test output kept (tail) for reports and feedback.
    #[serde(default = "default_output_cap")]
    pub output_cap: usize,
}

fn default_test_timeout() -> u64 {
    300
}
fn default_true() -> bool {
    true
}
fn default_jobs() -> u32 {
    2
}
fn default_output_cap() -> usize {
    12_000
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            test_command: Vec::new(),
            timeout_secs: default_test_timeout(),
            deny_network: true,
            require_isolation: false,
            jobs: default_jobs(),
            output_cap: default_output_cap(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TestOutcome {
    pub passed: bool,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    /// The runner printed a recognisable summary (so the code compiled).
    pub summary_seen: bool,
    pub failed_tests: BTreeSet<String>,
    pub duration_ms: u128,
    /// Tail of stdout+stderr, capped.
    pub output: String,
}

pub struct Sandbox {
    root: PathBuf,
    work: PathBuf,
    target_dir: PathBuf,
    cfg: SandboxConfig,
    isolation: Isolation,
    test_command: Vec<String>,
    baseline_rev: Option<String>,
    _tmp: tempfile::TempDir,
}

impl Sandbox {
    /// Copy `repo` into a fresh sandbox and snapshot it in a throwaway git repo.
    pub fn create(repo: &Path, cfg: SandboxConfig) -> anyhow::Result<Self> {
        let repo = repo
            .canonicalize()
            .map_err(|e| anyhow::anyhow!("target repo {}: {}", repo.display(), e))?;
        let tmp = tempfile::Builder::new()
            .prefix("sentryclaw-repair-")
            .tempdir()?;
        let root = tmp.path().canonicalize()?;
        let work = root.join("work");
        let target_dir = root.join("target");
        std::fs::create_dir_all(&target_dir)?;
        copy_tree(&repo, &work)?;

        let isolation = detect_isolation(cfg.deny_network);
        if cfg.require_isolation && isolation == Isolation::None {
            anyhow::bail!("require_isolation is set but neither bwrap nor unshare works here");
        }
        let test_command = if cfg.test_command.is_empty() {
            detect_test_command(&work).ok_or_else(|| {
                anyhow::anyhow!("could not detect a test command; set sandbox.test_command")
            })?
        } else {
            cfg.test_command.clone()
        };

        let sb = Self {
            root,
            work,
            target_dir,
            cfg,
            isolation,
            test_command,
            baseline_rev: None,
            _tmp: tmp,
        };
        sb.git(&["init", "-q"])?;
        sb.commit("baseline")?;
        Ok(sb)
    }

    pub fn work_dir(&self) -> &Path {
        &self.work
    }
    pub fn isolation(&self) -> Isolation {
        self.isolation
    }
    pub fn test_command(&self) -> &[String] {
        &self.test_command
    }

    /// Run git (trusted, outside the sandbox) in the work dir.
    pub fn git(&self, args: &[&str]) -> anyhow::Result<String> {
        let out = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=sentryclaw-repair",
                "-c",
                "user.email=repair@localhost",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(&self.work)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()?;
        if !out.status.success() {
            anyhow::bail!(
                "git {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    pub fn commit(&self, msg: &str) -> anyhow::Result<()> {
        self.git(&["add", "-A"])?;
        self.git(&["commit", "-q", "--allow-empty", "-m", msg])?;
        Ok(())
    }

    /// Commit only `paths` (files a patch touched), so build or test
    /// by-products never leak into the final patch.
    pub fn commit_paths(&self, msg: &str, paths: &[String]) -> anyhow::Result<()> {
        let mut args = vec!["add", "-A", "--"];
        args.extend(paths.iter().map(String::as_str));
        self.git(&args)?;
        self.git(&["commit", "-q", "--allow-empty", "-m", msg])?;
        Ok(())
    }

    /// Snapshot the tree after the baseline test run (picks up generated
    /// files such as Cargo.lock) and diff later fixes against it.
    pub fn mark_baseline(&mut self) -> anyhow::Result<()> {
        self.commit("baseline after first test run")?;
        self.baseline_rev = Some(self.git(&["rev-parse", "HEAD"])?.trim().to_string());
        Ok(())
    }

    /// Throw away uncommitted changes (a rejected attempt).
    pub fn reset(&self) -> anyhow::Result<()> {
        self.git(&["reset", "-q", "--hard"])?;
        self.git(&["clean", "-q", "-fd"])?;
        Ok(())
    }

    /// Unified diff of everything accepted since the baseline.
    pub fn diff_from_baseline(&self) -> anyhow::Result<String> {
        let base = match &self.baseline_rev {
            Some(r) => r.clone(),
            None => self
                .git(&["rev-list", "--max-parents=0", "HEAD"])?
                .trim()
                .to_string(),
        };
        self.git(&["diff", &base, "HEAD"])
    }

    /// Apply a unified diff to the work tree. Returns git's error text on failure.
    pub fn apply_patch(&self, patch: &str) -> Result<(), String> {
        let path = self.root.join("attempt.patch");
        std::fs::write(&path, patch).map_err(|e| e.to_string())?;
        let p = path.to_string_lossy().to_string();
        for check in [true, false] {
            let mut args = vec!["apply", "--recount", "--whitespace=nowarn"];
            if check {
                args.push("--check");
            }
            args.push(&p);
            self.git(&args).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub async fn run_tests(&self) -> anyhow::Result<TestOutcome> {
        let argv = self.wrapped_command();
        let mut cmd = std::process::Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .current_dir(&self.work)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in self.sandbox_env() {
            cmd.env(k, v);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let mut cmd = tokio::process::Command::from(cmd);
        cmd.kill_on_drop(true);

        let start = Instant::now();
        let child = cmd.spawn()?;
        let pid = child.id();
        let res = tokio::time::timeout(
            Duration::from_secs(self.cfg.timeout_secs),
            child.wait_with_output(),
        )
        .await;
        let duration_ms = start.elapsed().as_millis();

        match res {
            Ok(out) => {
                let out = out?;
                let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
                text.push_str(&String::from_utf8_lossy(&out.stderr));
                let parsed = parse_test_output(&text);
                Ok(TestOutcome {
                    passed: out.status.success(),
                    exit_code: out.status.code(),
                    timed_out: false,
                    summary_seen: parsed.summary_seen,
                    failed_tests: parsed.failed,
                    duration_ms,
                    output: tail(&text, self.cfg.output_cap),
                })
            }
            Err(_) => {
                if let Some(pid) = pid {
                    // Kill the whole process group (cargo, rustc, test bins).
                    let _ = std::process::Command::new("kill")
                        .args(["-KILL", &format!("-{}", pid)])
                        .status();
                }
                Ok(TestOutcome {
                    passed: false,
                    exit_code: None,
                    timed_out: true,
                    summary_seen: false,
                    failed_tests: BTreeSet::new(),
                    duration_ms,
                    output: format!("timed out after {}s", self.cfg.timeout_secs),
                })
            }
        }
    }

    fn wrapped_command(&self) -> Vec<String> {
        let root = self.root.to_string_lossy().to_string();
        let mut argv: Vec<String> = match self.isolation {
            Isolation::Bwrap => {
                let mut v: Vec<String> = [
                    "bwrap",
                    "--ro-bind",
                    "/",
                    "/",
                    "--dev",
                    "/dev",
                    "--proc",
                    "/proc",
                    "--tmpfs",
                    "/tmp",
                    "--bind",
                    &root,
                    &root,
                    "--unshare-pid",
                    "--die-with-parent",
                ]
                .iter()
                .map(|s| s.to_string())
                .collect();
                if self.cfg.deny_network {
                    v.push("--unshare-net".into());
                }
                v.push("--chdir".into());
                v.push(self.work.to_string_lossy().to_string());
                v.push("--".into());
                v
            }
            Isolation::UnshareNet => vec!["unshare".into(), "-rn".into(), "--".into()],
            Isolation::None => Vec::new(),
        };
        argv.extend(self.test_command.iter().cloned());
        argv
    }

    fn sandbox_env(&self) -> Vec<(String, String)> {
        let mut env = Vec::new();
        for key in [
            "PATH",
            "HOME",
            "CARGO_HOME",
            "RUSTUP_HOME",
            "RUSTUP_TOOLCHAIN",
            "LANG",
        ] {
            if let Ok(v) = std::env::var(key) {
                env.push((key.to_string(), v));
            }
        }
        let jobs = self.cfg.jobs.max(1).to_string();
        env.push((
            "CARGO_TARGET_DIR".into(),
            self.target_dir.to_string_lossy().to_string(),
        ));
        env.push(("CARGO_BUILD_JOBS".into(), jobs.clone()));
        env.push(("CARGO_NET_OFFLINE".into(), "true".into()));
        env.push(("CARGO_TERM_COLOR".into(), "never".into()));
        env.push(("RUST_TEST_THREADS".into(), jobs.clone()));
        env.push(("MAKEFLAGS".into(), format!("-j{}", jobs)));
        env.push(("TERM".into(), "dumb".into()));
        env.push(("NO_COLOR".into(), "1".into()));
        env.push(("TMPDIR".into(), self.root.to_string_lossy().to_string()));
        env
    }
}

/// Probe which isolation tool actually works on this host.
pub fn detect_isolation(deny_network: bool) -> Isolation {
    let ok = |prog: &str, args: &[&str]| {
        std::process::Command::new(prog)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    if ok(
        "bwrap",
        &[
            "--ro-bind",
            "/",
            "/",
            "--dev",
            "/dev",
            "--proc",
            "/proc",
            "--unshare-net",
            "--unshare-pid",
            "--die-with-parent",
            "true",
        ],
    ) {
        return Isolation::Bwrap;
    }
    if deny_network && ok("unshare", &["-rn", "true"]) {
        return Isolation::UnshareNet;
    }
    Isolation::None
}

/// Pick the target's own test command from its manifest files.
pub fn detect_test_command(dir: &Path) -> Option<Vec<String>> {
    let has = |f: &str| dir.join(f).exists();
    let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    if has("Cargo.toml") {
        Some(v(&["cargo", "test", "--offline", "--quiet"]))
    } else if has("go.mod") {
        Some(v(&["go", "test", "./..."]))
    } else if has("pyproject.toml") || has("pytest.ini") || has("setup.py") || has("tests") {
        Some(v(&[
            "python3",
            "-m",
            "pytest",
            "-q",
            "-p",
            "no:cacheprovider",
        ]))
    } else if has("package.json") {
        Some(v(&["npm", "test", "--silent"]))
    } else {
        None
    }
}

pub struct ParsedTests {
    pub summary_seen: bool,
    pub failed: BTreeSet<String>,
}

/// Recognise failing test names from cargo, pytest and go output.
pub fn parse_test_output(text: &str) -> ParsedTests {
    let mut failed = BTreeSet::new();
    let mut summary_seen = false;
    let mut in_failures_list = false;
    for line in text.lines() {
        let l = line.trim_end();
        // cargo
        if l.starts_with("test result:") {
            summary_seen = true;
            in_failures_list = false;
        }
        if let Some(rest) = l.strip_prefix("test ") {
            if let Some(name) = rest.strip_suffix(" ... FAILED") {
                failed.insert(name.trim().to_string());
            }
        }
        if l == "failures:" {
            in_failures_list = true;
            continue;
        }
        if in_failures_list {
            let t = l.trim();
            if t.is_empty() || t.starts_with("----") {
                if t.starts_with("----") {
                    in_failures_list = false;
                }
                continue;
            }
            if l.starts_with("    ") && !t.contains(' ') {
                failed.insert(t.to_string());
            }
        }
        // pytest
        if let Some(rest) = l.strip_prefix("FAILED ") {
            let name = rest.split(" - ").next().unwrap_or(rest).trim();
            failed.insert(name.to_string());
        }
        if l.starts_with('=') && (l.contains(" passed") || l.contains(" failed")) {
            summary_seen = true;
        }
        if (l.contains(" passed") || l.contains(" failed"))
            && l.contains(" in ")
            && !l.starts_with("test")
        {
            summary_seen = true;
        }
        // go
        if let Some(rest) = l.trim_start().strip_prefix("--- FAIL: ") {
            if let Some(name) = rest.split_whitespace().next() {
                failed.insert(name.to_string());
            }
        }
        if l.starts_with("ok  \t") || l.starts_with("FAIL\t") {
            summary_seen = true;
        }
    }
    ParsedTests {
        summary_seen,
        failed,
    }
}

fn tail(s: &str, cap: usize) -> String {
    if s.len() <= cap {
        return s.to_string();
    }
    let mut start = s.len() - cap;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    format!("[...truncated...]\n{}", &s[start..])
}

fn copy_tree(src: &Path, dst: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_s = name.to_string_lossy();
        let ft = entry.file_type()?;
        let to = dst.join(&name);
        if ft.is_dir() {
            if SKIP_DIRS.contains(&name_s.as_ref()) {
                continue;
            }
            copy_tree(&entry.path(), &to)?;
        } else if ft.is_file() {
            std::fs::copy(entry.path(), &to)?;
        }
        // Symlinks are skipped on purpose: they could point outside the repo.
    }
    Ok(())
}

pub fn is_skipped_dir(name: &str) -> bool {
    SKIP_DIRS.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cargo_failures() {
        let out = "running 3 tests\ntest a::x ... ok\ntest a::y ... FAILED\n\nfailures:\n\n---- a::y stdout ----\npanicked\n\nfailures:\n    a::y\n    a::z\n\ntest result: FAILED. 1 passed; 2 failed;";
        let p = parse_test_output(out);
        assert!(p.summary_seen);
        assert_eq!(
            p.failed.into_iter().collect::<Vec<_>>(),
            vec!["a::y".to_string(), "a::z".to_string()]
        );
    }

    #[test]
    fn parses_pytest_and_go() {
        let py = "FAILED tests/test_m.py::test_add - assert 1 == 2\n==== 1 failed, 3 passed in 0.1s ====";
        let p = parse_test_output(py);
        assert!(p.summary_seen);
        assert!(p.failed.contains("tests/test_m.py::test_add"));
        let go = "--- FAIL: TestSum (0.00s)\nFAIL\texample.com/m\t0.01s";
        let p = parse_test_output(go);
        assert!(p.summary_seen);
        assert!(p.failed.contains("TestSum"));
    }

    #[test]
    fn compile_error_has_no_summary() {
        let p = parse_test_output("error[E0425]: cannot find value `x`\nerror: could not compile");
        assert!(!p.summary_seen);
        assert!(p.failed.is_empty());
    }

    #[test]
    fn detects_commands() {
        let d = tempfile::tempdir().unwrap();
        assert!(detect_test_command(d.path()).is_none());
        std::fs::write(d.path().join("Cargo.toml"), "").unwrap();
        assert_eq!(detect_test_command(d.path()).unwrap()[0], "cargo");
    }

    #[test]
    fn tail_caps_output() {
        let s = "x".repeat(100);
        assert!(tail(&s, 10).ends_with("xxxxxxxxxx"));
        assert!(tail(&s, 10).len() < 40);
    }

    #[test]
    fn copy_skips_target_and_git() {
        let src = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(src.path().join("target/debug")).unwrap();
        std::fs::create_dir_all(src.path().join(".git")).unwrap();
        std::fs::create_dir_all(src.path().join("src")).unwrap();
        std::fs::write(src.path().join("src/lib.rs"), "x").unwrap();
        let dst = tempfile::tempdir().unwrap();
        copy_tree(src.path(), &dst.path().join("w")).unwrap();
        assert!(dst.path().join("w/src/lib.rs").exists());
        assert!(!dst.path().join("w/target").exists());
        assert!(!dst.path().join("w/.git").exists());
    }

    #[tokio::test]
    async fn timeout_kills_tests() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("f"), "x").unwrap();
        let cfg = SandboxConfig {
            test_command: vec!["sleep".into(), "30".into()],
            timeout_secs: 1,
            ..Default::default()
        };
        let sb = Sandbox::create(src.path(), cfg).unwrap();
        let out = sb.run_tests().await.unwrap();
        assert!(out.timed_out);
        assert!(!out.passed);
    }

    #[tokio::test]
    async fn network_is_denied_when_isolated() {
        if detect_isolation(true) == Isolation::None {
            eprintln!("no isolation tool on this host; skipping");
            return;
        }
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("f"), "x").unwrap();
        // Only loopback should exist in a fresh network namespace.
        let cfg = SandboxConfig {
            test_command: vec![
                "sh".into(),
                "-c".into(),
                "test $(tail -n +3 /proc/net/dev | grep -vc '^ *lo:') -eq 0".into(),
            ],
            timeout_secs: 10,
            ..Default::default()
        };
        let sb = Sandbox::create(src.path(), cfg).unwrap();
        let out = sb.run_tests().await.unwrap();
        assert!(out.passed, "non-loopback interface visible: {}", out.output);
    }
}
