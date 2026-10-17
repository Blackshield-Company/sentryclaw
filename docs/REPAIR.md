# sentryclaw repair: multi-agent defect repair

`sentryclaw repair` analyzes a repository, finds defects, ranks them, writes
patches and checks them against the repo's own test suite. Failed patches go
back to the fixer with the test output until the defect is fixed or the
retry cap is hit. The target repo is copied into a sandbox and never edited
in place; the result is a patch file plus JSON and Markdown reports.

## Pipeline

```text
target repo ──copy──▶ sandbox (throwaway git repo, own CARGO_TARGET_DIR)
                          │
                          ▼ baseline test run
      ┌──────────── Finder ────────────┐   model findings + one defect per
      │                                │   unexplained failing test
      ▼                                │
   Ranker  (model order, else severity)│
      │                                │
      ▼   for each defect (max_defects)│
   Fixer ──unified diff──▶ vet ──▶ Tester (apply, run tests, judge)
      ▲                                     │
      └──── patch + test output ◀── reject ─┘   (max_attempts per defect)
                                            │ accept: commit, refresh context
                                            ▼
                         fix.patch, report.json, report.md
```

| Agent | Input | Output | Fallback if the model fails |
|---|---|---|---|
| Finder | source files (capped), baseline test output | defects as JSON | one defect per failing test |
| Ranker | defect list | ordered ids + rationale | severity order |
| Fixer | defect, file contents, earlier failed attempts | unified diff | attempt counted as failed |
| Tester | diff, previous test outcome | verdict | (no model) |

The Tester accepts a patch when the whole suite passes, or when the code
still builds, no new test fails, and at least one failing test now passes.
It rejects patches that do not apply, break the build, time out, add new
failures or fix nothing. Before a patch is applied it is vetted: paths
must stay inside the repo, `.git` is off limits, and with
`protect_tests = true` (default) it may not edit test files, remove
asserts or `#[test]` lines, or add `#[ignore]`/skip markers. Only files a
patch touches are committed, so build by-products such as `Cargo.lock`
never leak into the final patch.

Code: `src/repair/` (`model.rs`, `agents.rs`, `sandbox.rs`, `patch.rs`,
`orchestrator.rs`, `cli.rs`). Agents share plain data types only, so any of
them can be replaced without touching the others.

## Models

Any OpenAI-compatible `/v1/chat/completions` server works: vLLM (including
on an AMD GPU with ROCm), llama.cpp server, Ollama or a hosted API. Nothing
is tied to one model. The mock model replays a JSON script per agent role
and is used by the tests and the offline demo.

```bash
# On a GPU box (e.g. a ROCm notebook):
vllm serve Qwen/Qwen2.5-Coder-7B-Instruct --port 8000
# Anywhere that can reach it:
sentryclaw repair path/to/repo --base-url http://HOST:8000/v1 --model Qwen/Qwen2.5-Coder-7B-Instruct
```

Set `api_key_env` (or `REPAIR_API_KEY_ENV`) to the *name* of an env var
holding a bearer token if the server needs one. Keys are never stored in
the config file.

## Sandbox

Tests run with a wall-clock timeout (`timeout_secs`, whole process group
killed), a scrubbed environment, `CARGO_BUILD_JOBS`/`MAKEFLAGS` set from
`jobs` (default 2, to keep RAM low), and the best isolation the host offers:

1. `bwrap`: read-only host filesystem, only the sandbox dir writable,
   private `/tmp`, own PID namespace, no network.
2. `unshare -rn`: no network only.
3. none: runs unisolated and the report says so. Set
   `require_isolation = true` to refuse instead.

Only the target repo's own test command runs (auto-detected from
`Cargo.toml`, `go.mod`, `pyproject.toml`/`pytest.ini`, `package.json`, or set
with `--test-cmd` / `sandbox.test_command`). Cargo runs with `--offline`, so
dependencies must already be in the local cargo cache.

## Running

```bash
cargo build -j2

# Offline demo: mock model, planted-bug fixture, full loop incl. one rejected patch
./target/debug/sentryclaw repair demo/repair-fixture \
  --config demo/repair/repair.toml --out repair-out

# Real model
cp repair.example.toml repair.toml   # edit base_url / model
./target/debug/sentryclaw repair /path/to/repo --config repair.toml
```

Options: `--mock-script`, `--base-url`, `--model`, `--max-attempts`,
`--max-defects`, `--timeout`, `--test-cmd "cargo test --offline"`,
`--allow-network`, `--json`, `--quiet`, `--out DIR`.
Env overrides: `REPAIR_BASE_URL`, `REPAIR_MODEL`, `REPAIR_API_KEY_ENV`.
Exit code: 0 = suite green at the end, 2 = still failing, 1 = error.

## Demo fixture

`demo/repair-fixture` is a dependency-free Rust crate with three planted
bugs (off-by-one, missing leap-year rule, `Some(NaN)` on empty input) and a
correct control function. `demo/repair/mock_script.json` scripts the agents;
the first fixer reply is deliberately wrong so the feedback loop shows up in
the report.

## Limits (for now)

- One defect is fixed at a time, in rank order; there is no parallel search.
- The context is whole files up to `max_context_chars`; no retrieval or
  repo map yet, so large repos will need one.
- The Tester judges by test results only. A defect with no failing test can
  only be checked for "no regression" and is marked as such.
- Test-name parsing covers cargo, pytest and go; other runners fall back to
  the exit code.
