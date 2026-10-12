# 🦞 SentryClaw

Self-hosted AI code review for GitHub/GitLab PRs. Runs entirely on your infrastructure — no API keys, no data leaves your network.

SentryClaw patrols your code like an apex predator. It doesn't miss a thing.

## Features

- **Fully offline** — Local LLMs via llama.cpp, Ollama, or vLLM
- **GitHub & GitLab** — Native webhook support for both platforms
- **Pluggable backends** — Swap models without touching review logic
- **Rust-powered** — Fast, safe, minimal resource footprint
- **Smart reviews** — Auto-approve trivial PRs, filter noise, batch commits
- **Review rules** — Custom YAML/TOML rules with severity levels
- **Web dashboard** — Real-time review analytics and history search
- **Metrics** — Prometheus-compatible metrics endpoint
- **Rate limiting** — Protect webhook endpoints from abuse
- **Review caching** — Skip re-reviewing identical diffs
- **Graceful shutdown** — Complete in-flight reviews before exiting

## Quick Start

```bash
# 1. Start your local LLM server
llama-server -m codellama-34b.Q4_K_M.gguf -c 4096 --port 8080

# 2. Configure the bot
cp config.example.toml config.toml
# Edit config.toml with your webhook secrets

# 3. Run
cargo run --release

# 4. Point webhooks to http://your-host:3000/webhook/github
```

## Architecture

```
┌─────────────┐     ┌─────────────┐     ┌─────────────┐
│   GitHub    │────▶│  Webhook    │────▶│  Diff       │
│   GitLab    │     │  Server     │     │  Extractor  │
└─────────────┘     └─────────────┘     └──────┬──────┘
                                                 │
                                        ┌────────▼────────┐
                                        │  Local LLM      │
                                        │  (llama.cpp)    │
                                        └────────┬────────┘
                                                 │
                                        ┌────────▼────────┐
                                        │  Review Poster  │
                                        └─────────────────┘
```

## Configuration

SentryClaw uses a TOML configuration file. Set `CONFIG_PATH` environment variable to point to your config (default: `config.toml`).

### Minimal Configuration

```toml
[server]
host = "0.0.0.0"
port = 3000

[github]
webhook_secret = "your-webhook-secret"
app_id = "123456"
private_key_path = "/path/to/private-key.pem"
use_app_auth = false
installation_id = 12345678

[llm]
provider = "llamacpp"
base_url = "http://localhost:8080"
model = "codellama-34b"
max_tokens = 4096
temperature = 0.1
```

### Full Configuration Reference

```toml
[server]
host = "0.0.0.0"
port = 3000

[github]
webhook_secret = "your-webhook-secret"
app_id = "123456"
private_key_path = "/path/to/private-key.pem"
use_app_auth = false
installation_id = 12345678

[gitlab]
webhook_secret = "your-webhook-secret"
access_token = "glpat-xxxxxxxx"
ci_cd_enabled = false
base_url = "https://gitlab.com"

[llm]
provider = "llamacpp"  # Options: llamacpp, ollama, vllm
base_url = "http://localhost:8080"
model = "codellama-34b"
max_tokens = 4096
temperature = 0.1

[review]
security = true
style = true
performance = true
correctness = true
maintainability = true
inline_comments = true
summary_comment = true

[diff_filter]
enabled = true
lockfile_patterns = ["Cargo.lock", "package-lock.json", "yarn.lock"]
generated_patterns = ["*.min.js", "dist/", "node_modules/"]
include_patterns = []  # Only review files matching these patterns
exclude_patterns = []  # Skip files matching these patterns

[batching]
enabled = false
timeout_seconds = 30
max_size = 10

[database]
path = "sentryclaw.db"

[dashboard]
enabled = true
refresh_seconds = 30

[auto_approve]
enabled = true
docs_patterns = ["*.md", "README", "CHANGELOG"]
skip_lockfiles = true
skip_whitespace = true

[retry]
max_retries = 3
base_delay_ms = 1000
max_delay_ms = 30000

[queue]
worker_count = 4
concurrency_limit = 1

[cache]
enabled = true
ttl_hours = 24

[rules]
rules_dir = "rules"

[logging]
json_format = false
```

## API Documentation

### Endpoints

#### `POST /webhook/github`

Receive GitHub webhook events. Supports `pull_request` events (opened, synchronize).

**Headers:**
- `X-Hub-Signature-256` — HMAC-SHA256 signature of the payload
- `Content-Type: application/json`

#### `POST /webhook/gitlab`

Receive GitLab webhook events. Supports `merge_request` and `pipeline` events.

**Headers:**
- `X-Gitlab-Token` — Secret token for verification
- `Content-Type: application/json`

#### `GET /health`

Health check endpoint. Returns server status and connectivity info.

**Response:**
```json
{
  "status": "healthy",
  "version": "1.0.0",
  "database": "connected",
  "config_loaded": true
}
```

#### `GET /metrics`

Prometheus-compatible metrics endpoint.

**Metrics:**
- `sentryclaw_reviews_total` — Total reviews performed
- `sentryclaw_reviews_approved` — Approved reviews
- `sentryclaw_reviews_request_changes` — Reviews requesting changes
- `sentryclaw_webhooks_received` — Total webhooks received
- `sentryclaw_webhooks_rejected` — Rejected webhooks (auth failure)
- `sentryclaw_webhooks_rate_limited` — Rate-limited webhooks
- `sentryclaw_cache_hits` / `sentryclaw_cache_misses` — Cache statistics
- `sentryclaw_review_latency_ms` — Average review latency

#### `GET /dashboard`

Web dashboard for review analytics (HTML).

#### `GET /dashboard/stats`

JSON API for dashboard statistics.

#### `GET /dashboard/api/search`

Search review history.

**Query Parameters:**
- `repo` — Filter by repository (e.g., `owner/repo`)
- `verdict` — Filter by verdict (`Approve`, `RequestChanges`, `Comment`)
- `from` / `to` — ISO 8601 datetime range
- `limit` — Maximum results (default: 50)

## Deployment Guide

### Self-Hosted (Bare Metal)

1. Install Rust (1.75+)
2. Install git
3. Clone the repository
4. Copy and edit `config.toml`
5. Run `cargo run --release`
6. Set up reverse proxy (nginx, Caddy) with SSL
7. Configure webhooks in GitHub/GitLab

### Docker

```bash
# Build image
docker build -t sentryclaw .

# Run with config
docker run -d \
  -p 3000:3000 \
  -v $(pwd)/config.toml:/app/config.toml:ro \
  -v $(pwd)/github-private-key.pem:/app/github-private-key.pem:ro \
  sentryclaw
```

### Docker Compose

```bash
docker-compose up -d
```

This starts both SentryClaw and a llama.cpp sidecar. Access the dashboard at `http://localhost:3000/dashboard`.

### Kubernetes

See `examples/kubernetes/` for K8s manifests including:
- Deployment with resource limits
- Service for webhook ingress
- ConfigMap for configuration
- PersistentVolumeClaim for SQLite database
- HorizontalPodAutoscaler for scaling

## Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `CONFIG_PATH` | Path to config.toml | `config.toml` |
| `RUST_LOG` | Log level | `info` |

## Webhook Configuration

### GitHub

1. Go to Repository Settings → Webhooks
2. Add webhook URL: `https://your-host/webhook/github`
3. Content type: `application/json`
4. Secret: Your configured `webhook_secret`
5. Events: Pull requests

### GitLab

1. Go to Project Settings → Webhooks
2. URL: `https://your-host/webhook/gitlab`
3. Secret token: Your configured `webhook_secret`
4. Events: Merge request events

## Development

```bash
# Run tests
cargo test

# Run clippy
cargo clippy -- -D warnings

# Run benchmarks
cargo bench

# Build release binary
cargo build --release
```

## Performance

SentryClaw is designed for minimal resource usage:
- ~10MB binary size
- <50MB RAM under normal load
- Sub-second webhook response times
- Review latency depends on LLM response time

## Security

- HMAC-SHA256 webhook signature verification (GitHub)
- Constant-time token comparison (GitLab)
- Rate limiting on webhook endpoints
- No external API calls (except to your configured LLM)
- No data leaves your network

## License

Apache-2.0 — see [LICENSE](LICENSE). This is the wave. ⚫🦞

---

## ☕ Support the Developer

If this project saved you time, solved a problem, or just made your day a little more neon, you can fuel the next one:

[![Buy Me A Coffee](https://cdn.buymeacoffee.com/buttons/v2/default-yellow.png)](https://buymeacoffee.com/synthalorian)
