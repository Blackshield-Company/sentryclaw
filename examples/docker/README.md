# Docker Compose Configuration Example

This example uses Docker Compose with a llama.cpp sidecar.

## docker-compose.yml

```yaml
version: "3.8"

services:
  sentryclaw:
    image: ghcr.io/blackshield-company/sentryclaw:latest
    ports:
      - "3000:3000"
    environment:
      - CONFIG_PATH=/app/config.toml
      - RUST_LOG=info
    volumes:
      - ./config.toml:/app/config.toml:ro
      - ./github-private-key.pem:/app/github-private-key.pem:ro
      - sentryclaw-data:/app/data
    depends_on:
      - llama
    restart: unless-stopped
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:3000/health"]
      interval: 30s
      timeout: 3s
      retries: 3
      start_period: 10s

  llama:
    image: ghcr.io/ggerganov/llama.cpp:server
    volumes:
      - ./models:/models:ro
    environment:
      - LLAMA_ARG_MODEL=/models/codellama-34b.Q4_K_M.gguf
      - LLAMA_ARG_CTX_SIZE=4096
      - LLAMA_ARG_PORT=8080
      - LLAMA_ARG_HOST=0.0.0.0
    ports:
      - "8080:8080"
    restart: unless-stopped
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:8080/health"]
      interval: 30s
      timeout: 3s
      retries: 3
      start_period: 60s

volumes:
  sentryclaw-data:
```

## config.toml

```toml
[server]
host = "0.0.0.0"
port = 3000

[github]
webhook_secret = "change-me"
app_id = "123456"
private_key_path = "/app/github-private-key.pem"
use_app_auth = true
installation_id = 12345678

[llm]
provider = "llamacpp"
base_url = "http://llama:8080"
model = "codellama-34b.Q4_K_M"
max_tokens = 4096
temperature = 0.1

[database]
path = "/app/data/sentryclaw.db"

[dashboard]
enabled = true
```

## Usage

```bash
# Start services
docker-compose up -d

# View logs
docker-compose logs -f sentryclaw

# Stop services
docker-compose down
```
