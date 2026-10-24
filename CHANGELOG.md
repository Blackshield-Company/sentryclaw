# Changelog

All notable changes to SentryClaw will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.0.0] — 2026-06-06

### Added
- **Fully offline AI code review** — Local LLMs via llama.cpp, Ollama, or vLLM with no API keys or data leaving your network
- **GitHub & GitLab native webhook support** — HMAC-SHA256 signature verification for GitHub, secret token verification for GitLab
- **Smart review engine** — Auto-approve trivial PRs (docs, lockfiles, whitespace), filter generated/noise from diffs, batch multiple commits
- **Custom review rules** — YAML/TOML rule definitions with severity levels (critical, warning, info)
- **Review templates** — Project-type specific prompts for Rust, Python, JavaScript, and generic codebases
- **Inline comments** — Place review findings on specific lines in the PR/MR
- **Review summary** — Overall verdict comment (Approve, Request Changes, Comment)
- **Async review queue** — Worker pool with job persistence in SQLite, survives restarts
- **Review result caching** — Skip re-reviewing identical diffs with configurable TTL
- **Concurrent review limits** — Per-repository concurrency control
- **Graceful shutdown** — Complete in-flight reviews before exiting
- **Web dashboard** — Real-time review analytics, history search, and filtering
- **Prometheus metrics** — `/metrics` endpoint with review latency, error rates, per-repo stats
- **Webhook rate limiting** — Protect endpoints from abuse with configurable limits
- **GitHub App authentication** — JWT + installation token support
- **GitLab CI/CD integration** — MR discussions API support
- **Structured logging** — JSON and plain text formats with tracing spans per review
- **Security audit** — Dependency scanning, secret detection, input validation
- **Benchmark suite** — Review latency benchmarks under load
- **Docker & Docker Compose** — Multi-stage builds with llama.cpp sidecar
- **Binary releases** — Automated builds for Linux x86_64 and ARM64
- **Kubernetes manifests** — Example deployment with HPA, PVC, and ConfigMap
- **Comprehensive documentation** — README, API docs, deployment guide, configuration examples

### Infrastructure
- CI/CD pipeline with GitHub Actions (test, clippy, build, security audit)
- Docker image publishing to GHCR with multi-arch support (amd64, arm64)
- Binary release automation via GitHub Actions

[1.0.0]: https://github.com/Blackshield-Company/sentryclaw/releases/tag/v1.0.0
