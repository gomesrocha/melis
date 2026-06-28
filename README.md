# Melis AI Gateway

<p align="center">
  <strong>Stateless, ultra-high-performance AI Gateway written in Rust</strong><br>
  Unified OpenAI-compatible API • Multi-provider routing • Context compression • Resilience built-in
</p>

<p align="center">
  <img src="https://img.shields.io/badge/language-Rust-orange" alt="Rust">
  <img src="https://img.shields.io/badge/license-MIT-blue" alt="MIT License">
  <img src="https://img.shields.io/badge/version-0.1.0-green" alt="Version">
</p>

---

## Overview

Melis is a **stateless, ultra-high-performance AI Gateway** written in Rust that sits between your clients and multiple LLM providers. It exposes a single **unified OpenAI-compatible API** so your applications can switch between providers without code changes.

**Supported Providers:**

| Provider | Type | Base URL |
|----------|------|----------|
| OpenAI | `openai` | `https://api.openai.com/v1` |
| Anthropic Claude | `anthropic` | `https://api.anthropic.com/v1` |
| Google Gemini | `google_vertex_ai` | `https://generativelanguage.googleapis.com/v1beta/openai` |
| Grok (x.ai) | `openai` (compatible) | `https://api.x.ai/v1` |
| DeepSeek | `openai` (compatible) | `https://api.deepseek.com/v1` |
| Ollama | `ollama` | `http://localhost:11434` |
| OCI GenAI | `oci_genai` | (Oracle Cloud endpoint) |

---

## Features

- **OpenAI-compatible API** — drop-in replacement for any OpenAI SDK client
- **Multi-provider routing** — route-based configuration via `routes.yaml`
- **SSE streaming** — token-by-token streaming with Server-Sent Events
- **Context compression** — adaptive trimming and sliding window strategies
- **Load balancing** — weighted round-robin with automatic failover
- **Circuit breaker** — per-provider, with exponential backoff recovery
- **Failover/retry** — automatic fallback to healthy providers
- **Rate limiting** — Token Bucket algorithm, per-client
- **Prometheus metrics** — full observability via `/metrics` endpoint
- **OpenTelemetry tracing** — OTLP gRPC exporter for distributed tracing
- **Hot-reload of routes** — edit `routes.yaml` without restart (~5s detection)
- **Graceful shutdown** — handles SIGTERM cleanly
- **Docker + Kubernetes ready** — distroless image < 50MB
- **Cross-platform** — Linux (x86_64, ARM64), macOS (Intel, Apple Silicon), Windows

---

## Quick Start

### From Binary

Download the latest release for your platform from [GitHub Releases](https://github.com/your-org/melis/releases):

```bash
# Download and extract
tar xzf melis-gateway-x86_64-unknown-linux-musl.tar.gz

# Copy example configs
cp config.yaml.example config.yaml
cp routes.yaml.example routes.yaml

# Edit config.yaml with your API keys
vim config.yaml

# Run
./melis-gateway
```

The gateway will start on `http://0.0.0.0:9090` by default.

### From Docker

```bash
# Build the image
docker build -t melis-gateway .

# Run with your config
docker run -p 9090:8080 \
  -v $(pwd)/config.yaml:/app/config.yaml:ro \
  -v $(pwd)/routes.yaml:/app/routes.yaml:ro \
  melis-gateway
```

Or use Docker Compose (includes Redis):

```bash
docker compose up --build
```

### From Source

```bash
# Clone
git clone https://github.com/your-org/melis.git
cd melis

# Build (release mode)
cargo build --release

# Run
./target/release/melis-gateway
```

---

## Configuration

Melis uses two configuration files:
- **`config.yaml`** — server settings, providers, rate limits, circuit breaker, observability
- **`routes.yaml`** — routing rules, token optimization (hot-reloadable)

### config.yaml

```yaml
server:
  host: "0.0.0.0"
  port: 9090
  max_payload_size: 10485760        # 10MB max request body
  graceful_shutdown_timeout_secs: 30
  max_concurrent_connections: 5000

redis:
  cluster_urls:
    - "redis://localhost:6379"
  pool_size: 10
  connect_timeout_secs: 5
  command_timeout_secs: 2

providers:
  - id: "openai"
    provider_type: "openai"
    base_url: "https://api.openai.com/v1"
    api_key: "sk-proj-YOUR_KEY"
    weight: 1
    timeout_secs: 60
    models: ["gpt-4o", "gpt-4o-mini"]

  - id: "anthropic"
    provider_type: "anthropic"
    base_url: "https://api.anthropic.com/v1"
    api_key: "sk-ant-YOUR_KEY"
    weight: 1
    timeout_secs: 60
    models: ["claude-sonnet-4-6", "claude-haiku-4-5-20251001"]

rate_limit:
  burst_capacity: 100       # Max tokens in the bucket
  refill_rate: 10.0         # Tokens added per second

compactor:
  token_threshold: 4096     # Compress context above this token count
  stop_words: []            # Words to remove during compression
  tokenizer_name: "cl100k_base"

circuit_breaker:
  failure_threshold_percent: 50.0   # Open circuit at 50% failure rate
  window_duration_secs: 60          # Evaluation window
  min_requests_in_window: 5         # Min requests before evaluating
  open_ttl_secs: 30                 # Initial open state duration
  max_ttl_secs: 300                 # Maximum open state duration
  backoff_factor: 2.0               # Exponential backoff multiplier

observability:
  otlp_endpoint: "http://localhost:4317"
  service_name: "melis-gateway"
  enabled: false

auth:
  enabled: false
  api_keys: []              # ["key-1", "key-2"] when enabled

routes_config_path: "./routes.yaml"
```

#### Provider Types Reference

| Provider | `provider_type` | `base_url` |
|----------|----------------|------------|
| OpenAI | `openai` | `https://api.openai.com/v1` |
| Anthropic | `anthropic` | `https://api.anthropic.com/v1` |
| Google Gemini | `google_vertex_ai` | `https://generativelanguage.googleapis.com/v1beta/openai` |
| Grok (x.ai) | `openai` | `https://api.x.ai/v1` |
| DeepSeek | `openai` | `https://api.deepseek.com/v1` |
| Ollama | `ollama` | `http://localhost:11434` |
| OCI GenAI | `oci_genai` | `https://<region>.oci.oraclecloud.com` |

### routes.yaml

Routes define how incoming requests are mapped to providers. This file supports **hot-reload** — changes are detected in ~5 seconds without restart.

```yaml
# Register non-built-in providers as custom_providers
custom_providers:
  - name: "grok"
    base_url: "https://api.x.ai/v1"
    api_format: "openai_compatible"
  - name: "deepseek"
    base_url: "https://api.deepseek.com/v1"
    api_format: "openai_compatible"

routes:
  # Single provider route
  - path: "/v1/chat/completions"
    method: "POST"
    provider: "ollama"
    model: "llama3.2"
    token_optimization:
      strategy: "adaptive_trimming"
      max_history_messages: 20
      compress_above_tokens: 4096
      local_tokenizer: "cl100k_base"

  # Multi-provider route (load balancing + failover)
  - path: "/v1/chat/resilient"
    method: "POST"
    providers:
      - name: "openai"
        weight: 80
        model: "gpt-4o"
      - name: "anthropic"
        weight: 20
        model: "claude-sonnet-4-6"
```

#### Route Configuration Fields

| Field | Description |
|-------|-------------|
| `path` | URL path the client calls (e.g., `/v1/chat/completions`) |
| `method` | HTTP method (`POST`) |
| `provider` | Single provider name (must exist in `config.yaml` or `custom_providers`) |
| `providers[]` | Multi-provider list for load balancing (use instead of `provider`) |
| `providers[].name` | Provider name |
| `providers[].weight` | Weight for round-robin selection (higher = more traffic) |
| `providers[].model` | Model to use with this provider |
| `model` | Default model (for single-provider routes) |
| `token_optimization` | Context compression settings (optional) |
| `token_optimization.strategy` | `adaptive_trimming` or `sliding_window` |
| `token_optimization.max_history_messages` | Max messages to keep in conversation history |
| `token_optimization.compress_above_tokens` | Token threshold to trigger compression |
| `token_optimization.local_tokenizer` | Tokenizer for counting (e.g., `cl100k_base`) |

---

## How to Add a New Provider

**Step 1:** Add the provider in `config.yaml`:

```yaml
providers:
  - id: "my-new-provider"
    provider_type: "openai"          # Use "openai" for any OpenAI-compatible API
    base_url: "https://api.example.com/v1"
    api_key: "your-api-key"
    weight: 1
    timeout_secs: 60
    models: ["model-a", "model-b"]
```

**Step 2:** If it's not a built-in type, register it in `routes.yaml` under `custom_providers`:

```yaml
custom_providers:
  - name: "my-new-provider"
    base_url: "https://api.example.com/v1"
    api_format: "openai_compatible"
```

**Step 3:** Add a route:

```yaml
routes:
  - path: "/v1/chat/my-provider"
    method: "POST"
    provider: "my-new-provider"
    model: "model-a"
    token_optimization:
      strategy: "adaptive_trimming"
      max_history_messages: 20
      compress_above_tokens: 4096
      local_tokenizer: "cl100k_base"
```

**Step 4:** Save the file. The gateway detects changes in ~5 seconds (hot-reload).

---

## Resilience & Failover

Melis provides automatic resilience for multi-provider routes through three mechanisms:

### How Multi-Provider Routes Work

When a route uses `providers[]` (instead of a single `provider`), Melis performs **weighted round-robin** selection with automatic failover:

1. **Selection** — A provider is selected based on weight (e.g., 80/20 split)
2. **Health check** — The circuit breaker state is verified before sending
3. **Request** — The request is sent to the selected provider
4. **Failure handling** — On 5xx, timeout, or 429, the next provider is tried
5. **Recovery** — Failed providers are retried after the circuit breaker cooldown

### Circuit Breaker Behavior

Each provider has its own circuit breaker with three states:

```
CLOSED (healthy) → OPEN (unhealthy) → HALF-OPEN (testing)
                          ↓                    ↓
              failure_threshold_percent    single probe request
              exceeded in window           succeeds → CLOSED
                                           fails → OPEN (longer TTL)
```

- **Closed**: All requests pass through normally
- **Open**: All requests are immediately rejected (failover to next provider)
- **Half-Open**: One probe request is allowed through to test recovery

The `open_ttl_secs` starts at 30s and increases exponentially (×`backoff_factor`) up to `max_ttl_secs` on repeated failures.

### Retry/Failover Loop

```
Client Request
    ↓
[Select Provider via Weighted Round-Robin]
    ↓
[Check Circuit Breaker]
    ├── OPEN → skip, try next provider
    └── CLOSED/HALF-OPEN → send request
            ↓
        [Response]
            ├── 2xx → return to client ✓
            ├── 5xx/timeout/429 → record failure
            │       ↓
            │   [Try Next Provider]
            │       ├── available → retry with next
            │       └── all exhausted → return 503
            └── 4xx → return to client (client error)
```

### Example: Resilient Route

```yaml
# routes.yaml
routes:
  - path: "/v1/chat/resilient"
    method: "POST"
    providers:
      - name: "openai"
        weight: 70
        model: "gpt-4o"
      - name: "anthropic"
        weight: 20
        model: "claude-sonnet-4-6"
      - name: "gemini"
        weight: 10
        model: "gemini-2.0-flash"
```

If OpenAI returns 503, the gateway automatically retries with Anthropic. If Anthropic also fails, it tries Gemini. All failures are tracked by the circuit breaker.

---

## Context Compression

Melis can automatically compress conversation context before sending to the LLM provider, reducing token usage and costs.

### Strategies

#### `adaptive_trimming`

Intelligently trims older messages while preserving the system prompt and most recent messages. It removes the oldest user/assistant messages first, keeping the conversation coherent.

#### `sliding_window`

Keeps only the N most recent messages (`max_history_messages`), discarding everything older. Simple and predictable.

### Configuration

```yaml
token_optimization:
  strategy: "adaptive_trimming"    # or "sliding_window"
  max_history_messages: 20         # Maximum messages to retain
  compress_above_tokens: 4096      # Only compress if context exceeds this
  local_tokenizer: "cl100k_base"   # Tokenizer for counting tokens locally
```

### How It Works

1. **Count tokens** — The gateway counts tokens in the full message array using the local tokenizer
2. **Check threshold** — If total tokens < `compress_above_tokens`, no compression occurs
3. **Apply strategy** — If above threshold:
   - `adaptive_trimming`: Removes oldest messages progressively until under threshold, always preserving the system message and last user message
   - `sliding_window`: Keeps only the last `max_history_messages` messages
4. **Forward** — The compressed context is sent to the provider

### Example: Before and After

**Before compression** (12 messages, ~6000 tokens):
```json
{
  "messages": [
    {"role": "system", "content": "You are a helpful assistant..."},
    {"role": "user", "content": "Old question 1..."},
    {"role": "assistant", "content": "Old answer 1..."},
    {"role": "user", "content": "Old question 2..."},
    {"role": "assistant", "content": "Old answer 2..."},
    {"role": "user", "content": "Old question 3..."},
    {"role": "assistant", "content": "Old answer 3..."},
    {"role": "user", "content": "Old question 4..."},
    {"role": "assistant", "content": "Old answer 4..."},
    {"role": "user", "content": "Old question 5..."},
    {"role": "assistant", "content": "Old answer 5..."},
    {"role": "user", "content": "Current question"}
  ]
}
```

**After adaptive_trimming** (compress_above_tokens: 4096, ~3800 tokens):
```json
{
  "messages": [
    {"role": "system", "content": "You are a helpful assistant..."},
    {"role": "user", "content": "Old question 4..."},
    {"role": "assistant", "content": "Old answer 4..."},
    {"role": "user", "content": "Old question 5..."},
    {"role": "assistant", "content": "Old answer 5..."},
    {"role": "user", "content": "Current question"}
  ]
}
```

The system prompt and recent context are preserved. Older messages are trimmed.

---

## Monitoring & Observability

### Prometheus Metrics

Melis exposes all metrics at `GET /metrics` in Prometheus text exposition format.

### Metrics Reference

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `melis_gateway_requests_total` | Counter | `route`, `client`, `status` | Total requests handled by the gateway |
| `melis_llm_tokens_total` | Counter | `direction`, `model`, `client_id` | Total LLM tokens processed (input/output) |
| `melis_context_compression_ratio` | Histogram | — | Context compression ratio (final/original) |
| `melis_backend_latency_seconds` | Histogram | `provider` | Backend (LLM provider) request latency |
| `melis_gateway_overhead_seconds` | Histogram | — | Gateway internal processing overhead |
| `melis_request_duration_seconds` | Histogram | `route`, `provider` | Total end-to-end request duration |
| `melis_gateway_internal_overhead_seconds` | Histogram | — | Gateway overhead (total - backend) |
| `melis_payload_translation_seconds` | Histogram | — | Payload format translation duration |
| `melis_compaction_duration_seconds` | Histogram | — | Context compaction processing time |
| `melis_compaction_applied_total` | Counter | — | Total compaction operations applied |
| `melis_compaction_skipped_total` | Counter | `reason` | Compaction operations skipped |
| `melis_context_original_tokens` | Counter | — | Total original tokens before compaction |
| `melis_context_final_tokens` | Counter | — | Total final tokens after compaction |
| `melis_context_saved_tokens_total` | Counter | — | Total tokens saved by compaction |
| `melis_failover_total` | Counter | `provider`, `reason` | Total failover events |
| `melis_circuit_breaker_state` | Gauge | `provider` | Circuit breaker state (0=closed, 1=open, 2=half-open) |
| `melis_provider_errors_total` | Counter | `provider`, `status_code` | Total provider errors |
| `melis_model_substitution_total` | Counter | `requested_model`, `resolved_model`, `reason` | Model substitution events |
| `melis_fallback_mode_total` | Counter | `original_provider`, `fallback_provider`, `reason` | Fallback mode activations |

### Grafana Setup

Use the included monitoring stack:

```bash
cd monitoring
docker compose -f docker-compose.monitoring.yml up -d
```

- **Grafana**: http://localhost:3000 (admin/admin)
- **Prometheus**: http://localhost:9091

A pre-built Grafana dashboard is included at `monitoring/grafana/dashboards/melis-gateway.json`.

### Example PromQL Queries

```promql
# Request rate (requests/second)
rate(melis_gateway_requests_total[5m])

# P99 backend latency by provider
histogram_quantile(0.99, rate(melis_backend_latency_seconds_bucket[5m]))

# Tokens saved by compression (per minute)
rate(melis_context_saved_tokens_total[1m]) * 60

# Compression ratio average
rate(melis_context_final_tokens[5m]) / rate(melis_context_original_tokens[5m])

# Failover rate by provider
rate(melis_failover_total[5m])

# Circuit breaker state (0=closed, 1=open, 2=half-open)
melis_circuit_breaker_state

# Error rate by provider
rate(melis_provider_errors_total[5m])

# Gateway overhead P95
histogram_quantile(0.95, rate(melis_gateway_internal_overhead_seconds_bucket[5m]))

# Total token cost tracking (input tokens per model)
rate(melis_llm_tokens_total{direction="input"}[5m])
```

---

## API Usage Examples

### JSON Response (non-streaming)

```bash
curl -X POST http://localhost:9090/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "llama3.2",
    "messages": [
      {"role": "system", "content": "You are a helpful assistant."},
      {"role": "user", "content": "Hello, how are you?"}
    ],
    "stream": false
  }'
```

### SSE Streaming (token-by-token)

```bash
curl -X POST http://localhost:9090/v1/chat/completions \
  -H "Content-Type: application/json" \
  -N \
  -d '{
    "model": "llama3.2",
    "messages": [
      {"role": "user", "content": "Write a haiku about Rust programming"}
    ],
    "stream": true
  }'
```

### Provider-Specific Routes

```bash
# OpenAI
curl -X POST http://localhost:9090/v1/chat/openai \
  -H "Content-Type: application/json" \
  -d '{
    "messages": [{"role": "user", "content": "Hello from OpenAI route"}],
    "stream": false
  }'

# Anthropic Claude
curl -X POST http://localhost:9090/v1/chat/claude \
  -H "Content-Type: application/json" \
  -d '{
    "messages": [{"role": "user", "content": "Hello from Claude route"}],
    "stream": true
  }'

# Google Gemini
curl -X POST http://localhost:9090/v1/chat/gemini \
  -H "Content-Type: application/json" \
  -d '{
    "messages": [{"role": "user", "content": "Hello from Gemini route"}],
    "stream": false
  }'

# Grok (x.ai)
curl -X POST http://localhost:9090/v1/chat/grok \
  -H "Content-Type: application/json" \
  -d '{
    "messages": [{"role": "user", "content": "Hello from Grok route"}],
    "stream": false
  }'

# DeepSeek
curl -X POST http://localhost:9090/v1/chat/deepseek \
  -H "Content-Type: application/json" \
  -d '{
    "messages": [{"role": "user", "content": "Hello from DeepSeek route"}],
    "stream": false
  }'

# Ollama (local)
curl -X POST http://localhost:9090/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "messages": [{"role": "user", "content": "Hello from Ollama"}],
    "stream": false
  }'
```

### Python with OpenAI SDK

Since Melis is OpenAI-compatible, you can use the official OpenAI Python SDK:

```python
from openai import OpenAI

# Point the client at your Melis gateway
client = OpenAI(
    base_url="http://localhost:9090/v1/chat",
    api_key="any-key-if-auth-disabled"
)

# Non-streaming
response = client.chat.completions.create(
    model="llama3.2",
    messages=[
        {"role": "system", "content": "You are a helpful assistant."},
        {"role": "user", "content": "Explain quantum computing in simple terms."}
    ]
)
print(response.choices[0].message.content)

# Streaming
stream = client.chat.completions.create(
    model="gpt-4o",
    messages=[
        {"role": "user", "content": "Write a poem about AI."}
    ],
    stream=True
)
for chunk in stream:
    if chunk.choices[0].delta.content:
        print(chunk.choices[0].delta.content, end="")
```

### With Authentication Enabled

```bash
curl -X POST http://localhost:9090/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer your-api-key-here" \
  -d '{
    "messages": [{"role": "user", "content": "Hello"}],
    "stream": false
  }'
```

---

## Deployment

### Docker (Local Build)

```bash
# Build the image (~50MB final size, distroless)
docker build -t melis-gateway:latest .

# Run
docker run -d \
  --name melis-gateway \
  -p 9090:8080 \
  -v $(pwd)/config.yaml:/app/config.yaml:ro \
  -v $(pwd)/routes.yaml:/app/routes.yaml:ro \
  -e RUST_LOG=info \
  melis-gateway:latest
```

### Docker Compose (with Redis + Monitoring)

```bash
# Start gateway + Redis
docker compose up -d

# Start monitoring stack (Prometheus + Grafana)
cd monitoring
docker compose -f docker-compose.monitoring.yml up -d
```

### Kubernetes

#### Deployment

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: melis-gateway
  labels:
    app: melis-gateway
spec:
  replicas: 3
  selector:
    matchLabels:
      app: melis-gateway
  template:
    metadata:
      labels:
        app: melis-gateway
      annotations:
        prometheus.io/scrape: "true"
        prometheus.io/port: "8080"
        prometheus.io/path: "/metrics"
    spec:
      containers:
        - name: melis-gateway
          image: ghcr.io/your-org/melis-gateway:latest
          ports:
            - containerPort: 8080
          env:
            - name: MELIS_SERVER_PORT
              value: "8080"
            - name: MELIS_REDIS_URL
              value: "redis://redis-service:6379"
            - name: RUST_LOG
              value: "info"
          volumeMounts:
            - name: config
              mountPath: /app/config.yaml
              subPath: config.yaml
            - name: routes
              mountPath: /app/routes.yaml
              subPath: routes.yaml
          resources:
            requests:
              cpu: 100m
              memory: 64Mi
            limits:
              cpu: 1000m
              memory: 256Mi
          livenessProbe:
            httpGet:
              path: /metrics
              port: 8080
            initialDelaySeconds: 5
            periodSeconds: 10
          readinessProbe:
            httpGet:
              path: /metrics
              port: 8080
            initialDelaySeconds: 3
            periodSeconds: 5
      volumes:
        - name: config
          configMap:
            name: melis-config
        - name: routes
          configMap:
            name: melis-routes
```

#### Service

```yaml
apiVersion: v1
kind: Service
metadata:
  name: melis-gateway
spec:
  selector:
    app: melis-gateway
  ports:
    - port: 80
      targetPort: 8080
  type: ClusterIP
```

#### ConfigMap

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: melis-config
data:
  config.yaml: |
    server:
      host: "0.0.0.0"
      port: 8080
      max_payload_size: 10485760
      graceful_shutdown_timeout_secs: 30
      max_concurrent_connections: 5000
    # ... rest of your config
```

#### Horizontal Pod Autoscaler

```yaml
apiVersion: autoscaling/v2
kind: HorizontalPodAutoscaler
metadata:
  name: melis-gateway-hpa
spec:
  scaleTargetRef:
    apiVersion: apps/v1
    kind: Deployment
    name: melis-gateway
  minReplicas: 2
  maxReplicas: 20
  metrics:
    - type: Resource
      resource:
        name: cpu
        target:
          type: Utilization
          averageUtilization: 70
    - type: Pods
      pods:
        metric:
          name: melis_gateway_requests_total
        target:
          type: AverageValue
          averageValue: "1000"
```

---

## Cross-Platform Builds

### Makefile Targets

```bash
# Build for current platform
make build

# Linux x86_64 (static musl binary)
make build-linux

# Linux ARM64 (static musl binary)
make build-linux-arm

# macOS Intel
make build-mac

# macOS Apple Silicon (ARM64)
make build-mac-arm

# Windows x86_64
make build-windows

# All platforms at once
make build-all

# Package releases into dist/
make release

# Build Docker image
make docker
```

### Prerequisites

```bash
# Install cross-compilation tool
cargo install cross --version 0.2.5
# or
make install-cross
```

### GitHub Actions CI/CD

The project includes an automated release pipeline (`.github/workflows/release.yml`) that triggers on tag push:

```bash
# Tag and push to trigger automatic release
git tag v0.1.0
git push origin v0.1.0
```

This produces:

| Platform | Architecture | Artifact |
|----------|-------------|----------|
| Linux | x86_64 | `melis-gateway-x86_64-unknown-linux-musl.tar.gz` |
| Linux | ARM64 | `melis-gateway-aarch64-unknown-linux-musl.tar.gz` |
| macOS | Intel | `melis-gateway-x86_64-apple-darwin.tar.gz` |
| macOS | Apple Silicon | `melis-gateway-aarch64-apple-darwin.tar.gz` |
| Windows | x86_64 | `melis-gateway-x86_64-pc-windows-msvc.zip` |

---

## Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `MELIS_SERVER_PORT` | Gateway listen port | `9090` |
| `MELIS_SERVER_HOST` | Gateway listen address | `0.0.0.0` |
| `MELIS_ROUTES_CONFIG` | Path to routes.yaml | `./routes.yaml` |
| `MELIS_REDIS_URL` | Redis connection URL | `redis://localhost:6379` |
| `MELIS_OTLP_ENDPOINT` | OpenTelemetry OTLP endpoint | `http://localhost:4317` |
| `RUST_LOG` | Log level filter (tracing) | `info` |

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                          CLIENT REQUEST                               │
│                    (OpenAI-compatible format)                         │
└────────────────────────────────┬────────────────────────────────────┘
                                 │
                                 ▼
┌─────────────────────────────────────────────────────────────────────┐
│                         MELIS GATEWAY                                 │
│                                                                       │
│  ┌──────────┐  ┌──────────────┐  ┌────────────┐  ┌──────────────┐ │
│  │   Auth   │→ │ Rate Limiter │→ │  Compactor │→ │   Router     │ │
│  │Middleware│  │(Token Bucket)│  │(Compression)│  │(Route Match) │ │
│  └──────────┘  └──────────────┘  └────────────┘  └──────┬───────┘ │
│                                                           │         │
│  ┌────────────────────────────────────────────────────────▼───────┐ │
│  │                    LOAD BALANCER                                 │ │
│  │              (Weighted Round-Robin)                              │ │
│  └─────┬──────────────┬──────────────┬──────────────┬─────────────┘ │
│        │              │              │              │               │
│  ┌─────▼─────┐  ┌─────▼─────┐  ┌─────▼─────┐  ┌─────▼─────┐     │
│  │  Circuit  │  │  Circuit  │  │  Circuit  │  │  Circuit  │     │
│  │  Breaker  │  │  Breaker  │  │  Breaker  │  │  Breaker  │     │
│  └─────┬─────┘  └─────┬─────┘  └─────┬─────┘  └─────┬─────┘     │
│        │              │              │              │               │
│  ┌─────▼─────┐  ┌─────▼─────┐  ┌─────▼─────┐  ┌─────▼─────┐     │
│  │Transpiler │  │Transpiler │  │Transpiler │  │Transpiler │     │
│  │(Payload)  │  │(Payload)  │  │(Payload)  │  │(Payload)  │     │
│  └─────┬─────┘  └─────┬─────┘  └─────┬─────┘  └─────┬─────┘     │
└────────┼──────────────┼──────────────┼──────────────┼─────────────┘
         │              │              │              │
         ▼              ▼              ▼              ▼
   ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐
   │  OpenAI  │  │  Claude  │  │  Gemini  │  │  Ollama  │
   └──────────┘  └──────────┘  └──────────┘  └──────────┘
```

**Pipeline stages:**
1. **Auth** — Validates API key (if enabled)
2. **Rate Limiter** — Token bucket per-client rate limiting
3. **Compactor** — Context compression (adaptive trimming / sliding window)
4. **Router** — Matches request path to route configuration
5. **Load Balancer** — Selects provider (weighted round-robin)
6. **Circuit Breaker** — Checks provider health, enables failover
7. **Transpiler** — Translates payload between OpenAI format and provider-native format
8. **Provider** — Sends request to LLM backend, streams response back

---

## License

MIT License. See [LICENSE](LICENSE) for details.

---


---

<br><br>

# 🇧🇷 Melis AI Gateway (Português)

<p align="center">
  <strong>Gateway de IA stateless e de altíssima performance escrito em Rust</strong><br>
  API unificada compatível com OpenAI • Roteamento multi-provedor • Compressão de contexto • Resiliência nativa
</p>

---

## Visão Geral

Melis é um **gateway de IA stateless e de altíssima performance** escrito em Rust que fica entre seus clientes e múltiplos provedores de LLM. Ele expõe uma única **API unificada compatível com OpenAI** para que suas aplicações possam trocar de provedor sem alterar código.

**Provedores Suportados:**

| Provedor | Tipo | URL Base |
|----------|------|----------|
| OpenAI | `openai` | `https://api.openai.com/v1` |
| Anthropic Claude | `anthropic` | `https://api.anthropic.com/v1` |
| Google Gemini | `google_vertex_ai` | `https://generativelanguage.googleapis.com/v1beta/openai` |
| Grok (x.ai) | `openai` (compatível) | `https://api.x.ai/v1` |
| DeepSeek | `openai` (compatível) | `https://api.deepseek.com/v1` |
| Ollama | `ollama` | `http://localhost:11434` |
| OCI GenAI | `oci_genai` | (endpoint Oracle Cloud) |

---

## Funcionalidades

- **API compatível com OpenAI** — substituto direto para qualquer cliente SDK da OpenAI
- **Roteamento multi-provedor** — configuração baseada em rotas via `routes.yaml`
- **SSE streaming** — streaming token a token com Server-Sent Events
- **Compressão de contexto** — estratégias adaptive trimming e sliding window
- **Balanceamento de carga** — round-robin ponderado com failover automático
- **Circuit breaker** — por provedor, com recuperação exponencial
- **Failover/retry** — fallback automático para provedores saudáveis
- **Rate limiting** — algoritmo Token Bucket, por cliente
- **Métricas Prometheus** — observabilidade completa via endpoint `/metrics`
- **Tracing OpenTelemetry** — exportador OTLP gRPC para tracing distribuído
- **Hot-reload de rotas** — edite `routes.yaml` sem reiniciar (~5s de detecção)
- **Graceful shutdown** — encerramento limpo via SIGTERM
- **Docker + Kubernetes ready** — imagem distroless < 50MB
- **Cross-platform** — Linux (x86_64, ARM64), macOS (Intel, Apple Silicon), Windows

---

## Início Rápido

### Via Binário

Baixe a última release para sua plataforma no [GitHub Releases](https://github.com/your-org/melis/releases):

```bash
# Baixar e extrair
tar xzf melis-gateway-x86_64-unknown-linux-musl.tar.gz

# Copiar configs de exemplo
cp config.yaml.example config.yaml
cp routes.yaml.example routes.yaml

# Editar config.yaml com suas chaves de API
vim config.yaml

# Executar
./melis-gateway
```

O gateway inicia em `http://0.0.0.0:9090` por padrão.

### Via Docker

```bash
# Build da imagem
docker build -t melis-gateway .

# Executar com sua configuração
docker run -p 9090:8080 \
  -v $(pwd)/config.yaml:/app/config.yaml:ro \
  -v $(pwd)/routes.yaml:/app/routes.yaml:ro \
  melis-gateway
```

Ou use Docker Compose (inclui Redis):

```bash
docker compose up --build
```

### A partir do Código-Fonte

```bash
# Clonar
git clone https://github.com/your-org/melis.git
cd melis

# Compilar (modo release)
cargo build --release

# Executar
./target/release/melis-gateway
```

---

## Configuração

O Melis utiliza dois arquivos de configuração:
- **`config.yaml`** — configurações do servidor, provedores, rate limit, circuit breaker, observabilidade
- **`routes.yaml`** — regras de roteamento, otimização de tokens (hot-reloadable)

### config.yaml

```yaml
server:
  host: "0.0.0.0"
  port: 9090
  max_payload_size: 10485760        # 10MB tamanho máximo do corpo
  graceful_shutdown_timeout_secs: 30
  max_concurrent_connections: 5000

redis:
  cluster_urls:
    - "redis://localhost:6379"
  pool_size: 10
  connect_timeout_secs: 5
  command_timeout_secs: 2

providers:
  - id: "openai"
    provider_type: "openai"
    base_url: "https://api.openai.com/v1"
    api_key: "sk-proj-SUA_CHAVE"
    weight: 1
    timeout_secs: 60
    models: ["gpt-4o", "gpt-4o-mini"]

  - id: "anthropic"
    provider_type: "anthropic"
    base_url: "https://api.anthropic.com/v1"
    api_key: "sk-ant-SUA_CHAVE"
    weight: 1
    timeout_secs: 60
    models: ["claude-sonnet-4-6", "claude-haiku-4-5-20251001"]

rate_limit:
  burst_capacity: 100       # Máximo de tokens no bucket
  refill_rate: 10.0         # Tokens adicionados por segundo

compactor:
  token_threshold: 4096     # Comprimir contexto acima desse número de tokens
  stop_words: []            # Palavras a remover durante compressão
  tokenizer_name: "cl100k_base"

circuit_breaker:
  failure_threshold_percent: 50.0   # Abrir circuito com 50% de falhas
  window_duration_secs: 60          # Janela de avaliação
  min_requests_in_window: 5         # Mínimo de requests antes de avaliar
  open_ttl_secs: 30                 # Duração inicial do estado aberto
  max_ttl_secs: 300                 # Duração máxima do estado aberto
  backoff_factor: 2.0               # Multiplicador de backoff exponencial

observability:
  otlp_endpoint: "http://localhost:4317"
  service_name: "melis-gateway"
  enabled: false

auth:
  enabled: false
  api_keys: []              # ["chave-1", "chave-2"] quando habilitado

routes_config_path: "./routes.yaml"
```

#### Referência de Tipos de Provedores

| Provedor | `provider_type` | `base_url` |
|----------|----------------|------------|
| OpenAI | `openai` | `https://api.openai.com/v1` |
| Anthropic | `anthropic` | `https://api.anthropic.com/v1` |
| Google Gemini | `google_vertex_ai` | `https://generativelanguage.googleapis.com/v1beta/openai` |
| Grok (x.ai) | `openai` | `https://api.x.ai/v1` |
| DeepSeek | `openai` | `https://api.deepseek.com/v1` |
| Ollama | `ollama` | `http://localhost:11434` |
| OCI GenAI | `oci_genai` | `https://<region>.oci.oraclecloud.com` |

### routes.yaml

As rotas definem como requisições de entrada são mapeadas para provedores. Este arquivo suporta **hot-reload** — alterações são detectadas em ~5 segundos sem reiniciar.

```yaml
# Registre provedores não built-in como custom_providers
custom_providers:
  - name: "grok"
    base_url: "https://api.x.ai/v1"
    api_format: "openai_compatible"
  - name: "deepseek"
    base_url: "https://api.deepseek.com/v1"
    api_format: "openai_compatible"

routes:
  # Rota com provedor único
  - path: "/v1/chat/completions"
    method: "POST"
    provider: "ollama"
    model: "llama3.2"
    token_optimization:
      strategy: "adaptive_trimming"
      max_history_messages: 20
      compress_above_tokens: 4096
      local_tokenizer: "cl100k_base"

  # Rota multi-provedor (balanceamento + failover)
  - path: "/v1/chat/resilient"
    method: "POST"
    providers:
      - name: "openai"
        weight: 80
        model: "gpt-4o"
      - name: "anthropic"
        weight: 20
        model: "claude-sonnet-4-6"
```

#### Campos da Configuração de Rotas

| Campo | Descrição |
|-------|-----------|
| `path` | Caminho URL que o cliente chama (ex: `/v1/chat/completions`) |
| `method` | Método HTTP (`POST`) |
| `provider` | Nome do provedor único (deve existir no `config.yaml` ou `custom_providers`) |
| `providers[]` | Lista multi-provedor para balanceamento (usar no lugar de `provider`) |
| `providers[].name` | Nome do provedor |
| `providers[].weight` | Peso para seleção round-robin (maior = mais tráfego) |
| `providers[].model` | Modelo a usar com este provedor |
| `model` | Modelo padrão (para rotas de provedor único) |
| `token_optimization` | Configurações de compressão de contexto (opcional) |
| `token_optimization.strategy` | `adaptive_trimming` ou `sliding_window` |
| `token_optimization.max_history_messages` | Máximo de mensagens a manter no histórico |
| `token_optimization.compress_above_tokens` | Limite de tokens para acionar compressão |
| `token_optimization.local_tokenizer` | Tokenizer para contagem (ex: `cl100k_base`) |

---

## Como Adicionar um Novo Provedor

**Passo 1:** Adicione o provedor no `config.yaml`:

```yaml
providers:
  - id: "meu-provedor"
    provider_type: "openai"          # Use "openai" para qualquer API OpenAI-compatible
    base_url: "https://api.exemplo.com/v1"
    api_key: "sua-chave-api"
    weight: 1
    timeout_secs: 60
    models: ["modelo-a", "modelo-b"]
```

**Passo 2:** Se não for um tipo built-in, registre no `routes.yaml` em `custom_providers`:

```yaml
custom_providers:
  - name: "meu-provedor"
    base_url: "https://api.exemplo.com/v1"
    api_format: "openai_compatible"
```

**Passo 3:** Adicione uma rota:

```yaml
routes:
  - path: "/v1/chat/meu-provedor"
    method: "POST"
    provider: "meu-provedor"
    model: "modelo-a"
    token_optimization:
      strategy: "adaptive_trimming"
      max_history_messages: 20
      compress_above_tokens: 4096
      local_tokenizer: "cl100k_base"
```

**Passo 4:** Salve o arquivo. O gateway detecta mudanças em ~5 segundos (hot-reload).

---

## Resiliência & Failover

O Melis fornece resiliência automática para rotas multi-provedor através de três mecanismos:

### Como Funcionam as Rotas Multi-Provedor

Quando uma rota usa `providers[]` (ao invés de um único `provider`), o Melis executa **round-robin ponderado** com failover automático:

1. **Seleção** — Um provedor é selecionado com base no peso (ex: divisão 80/20)
2. **Verificação de saúde** — O estado do circuit breaker é verificado antes do envio
3. **Requisição** — A requisição é enviada ao provedor selecionado
4. **Tratamento de falha** — Em 5xx, timeout ou 429, o próximo provedor é tentado
5. **Recuperação** — Provedores com falha são testados novamente após o cooldown do circuit breaker

### Comportamento do Circuit Breaker

Cada provedor tem seu próprio circuit breaker com três estados:

```
CLOSED (saudável) → OPEN (indisponível) → HALF-OPEN (testando)
                          ↓                       ↓
              failure_threshold_percent      uma requisição de teste
              excedido na janela             sucesso → CLOSED
                                            falha → OPEN (TTL maior)
```

- **Closed**: Todas as requisições passam normalmente
- **Open**: Todas as requisições são rejeitadas imediatamente (failover para próximo provedor)
- **Half-Open**: Uma requisição de teste é permitida para verificar recuperação

O `open_ttl_secs` começa em 30s e cresce exponencialmente (×`backoff_factor`) até `max_ttl_secs` em falhas repetidas.

### Loop de Retry/Failover

```
Requisição do Cliente
    ↓
[Selecionar Provedor via Round-Robin Ponderado]
    ↓
[Verificar Circuit Breaker]
    ├── OPEN → pular, tentar próximo provedor
    └── CLOSED/HALF-OPEN → enviar requisição
            ↓
        [Resposta]
            ├── 2xx → retornar ao cliente ✓
            ├── 5xx/timeout/429 → registrar falha
            │       ↓
            │   [Tentar Próximo Provedor]
            │       ├── disponível → retry com próximo
            │       └── todos esgotados → retornar 503
            └── 4xx → retornar ao cliente (erro do cliente)
```

### Exemplo: Rota Resiliente

```yaml
# routes.yaml
routes:
  - path: "/v1/chat/resilient"
    method: "POST"
    providers:
      - name: "openai"
        weight: 70
        model: "gpt-4o"
      - name: "anthropic"
        weight: 20
        model: "claude-sonnet-4-6"
      - name: "gemini"
        weight: 10
        model: "gemini-2.0-flash"
```

Se a OpenAI retorna 503, o gateway automaticamente tenta a Anthropic. Se a Anthropic também falha, tenta o Gemini. Todas as falhas são rastreadas pelo circuit breaker.

---

## Compressão de Contexto

O Melis pode comprimir automaticamente o contexto da conversa antes de enviar ao provedor LLM, reduzindo o uso de tokens e custos.

### Estratégias

#### `adaptive_trimming`

Remove inteligentemente mensagens mais antigas preservando o prompt de sistema e as mensagens mais recentes. Remove as mensagens user/assistant mais antigas primeiro, mantendo a conversa coerente.

#### `sliding_window`

Mantém apenas as N mensagens mais recentes (`max_history_messages`), descartando tudo mais antigo. Simples e previsível.

### Configuração

```yaml
token_optimization:
  strategy: "adaptive_trimming"    # ou "sliding_window"
  max_history_messages: 20         # Máximo de mensagens a reter
  compress_above_tokens: 4096      # Só comprime se contexto exceder este limite
  local_tokenizer: "cl100k_base"   # Tokenizer para contagem local de tokens
```

### Como Funciona

1. **Contar tokens** — O gateway conta tokens no array completo de mensagens usando o tokenizer local
2. **Verificar limite** — Se o total de tokens < `compress_above_tokens`, nenhuma compressão ocorre
3. **Aplicar estratégia** — Se acima do limite:
   - `adaptive_trimming`: Remove mensagens mais antigas progressivamente até ficar abaixo do limite, sempre preservando a mensagem de sistema e a última mensagem do usuário
   - `sliding_window`: Mantém apenas as últimas `max_history_messages` mensagens
4. **Encaminhar** — O contexto comprimido é enviado ao provedor

### Exemplo: Antes e Depois

**Antes da compressão** (12 mensagens, ~6000 tokens):
```json
{
  "messages": [
    {"role": "system", "content": "Você é um assistente útil..."},
    {"role": "user", "content": "Pergunta antiga 1..."},
    {"role": "assistant", "content": "Resposta antiga 1..."},
    {"role": "user", "content": "Pergunta antiga 2..."},
    {"role": "assistant", "content": "Resposta antiga 2..."},
    {"role": "user", "content": "Pergunta antiga 3..."},
    {"role": "assistant", "content": "Resposta antiga 3..."},
    {"role": "user", "content": "Pergunta antiga 4..."},
    {"role": "assistant", "content": "Resposta antiga 4..."},
    {"role": "user", "content": "Pergunta antiga 5..."},
    {"role": "assistant", "content": "Resposta antiga 5..."},
    {"role": "user", "content": "Pergunta atual"}
  ]
}
```

**Após adaptive_trimming** (compress_above_tokens: 4096, ~3800 tokens):
```json
{
  "messages": [
    {"role": "system", "content": "Você é um assistente útil..."},
    {"role": "user", "content": "Pergunta antiga 4..."},
    {"role": "assistant", "content": "Resposta antiga 4..."},
    {"role": "user", "content": "Pergunta antiga 5..."},
    {"role": "assistant", "content": "Resposta antiga 5..."},
    {"role": "user", "content": "Pergunta atual"}
  ]
}
```

O prompt de sistema e o contexto recente são preservados. Mensagens mais antigas são removidas.

---

## Monitoramento & Observabilidade

### Métricas Prometheus

O Melis expõe todas as métricas em `GET /metrics` no formato de exposição Prometheus.

### Referência de Métricas

| Métrica | Tipo | Labels | Descrição |
|---------|------|--------|-----------|
| `melis_gateway_requests_total` | Counter | `route`, `client`, `status` | Total de requisições processadas pelo gateway |
| `melis_llm_tokens_total` | Counter | `direction`, `model`, `client_id` | Total de tokens LLM processados (input/output) |
| `melis_context_compression_ratio` | Histogram | — | Razão de compressão de contexto (final/original) |
| `melis_backend_latency_seconds` | Histogram | `provider` | Latência da requisição ao backend (provedor LLM) |
| `melis_gateway_overhead_seconds` | Histogram | — | Overhead de processamento interno do gateway |
| `melis_request_duration_seconds` | Histogram | `route`, `provider` | Duração total end-to-end da requisição |
| `melis_gateway_internal_overhead_seconds` | Histogram | — | Overhead do gateway (total - backend) |
| `melis_payload_translation_seconds` | Histogram | — | Duração da tradução de formato do payload |
| `melis_compaction_duration_seconds` | Histogram | — | Tempo de processamento da compactação |
| `melis_compaction_applied_total` | Counter | — | Total de operações de compactação aplicadas |
| `melis_compaction_skipped_total` | Counter | `reason` | Operações de compactação ignoradas |
| `melis_context_original_tokens` | Counter | — | Total de tokens originais antes da compactação |
| `melis_context_final_tokens` | Counter | — | Total de tokens finais após compactação |
| `melis_context_saved_tokens_total` | Counter | — | Total de tokens economizados pela compactação |
| `melis_failover_total` | Counter | `provider`, `reason` | Total de eventos de failover |
| `melis_circuit_breaker_state` | Gauge | `provider` | Estado do circuit breaker (0=closed, 1=open, 2=half-open) |
| `melis_provider_errors_total` | Counter | `provider`, `status_code` | Total de erros por provedor |
| `melis_model_substitution_total` | Counter | `requested_model`, `resolved_model`, `reason` | Eventos de substituição de modelo |
| `melis_fallback_mode_total` | Counter | `original_provider`, `fallback_provider`, `reason` | Ativações do modo fallback |

### Configuração do Grafana

Use a stack de monitoramento incluída:

```bash
cd monitoring
docker compose -f docker-compose.monitoring.yml up -d
```

- **Grafana**: http://localhost:3000 (admin/admin)
- **Prometheus**: http://localhost:9091

Um dashboard Grafana pré-construído está em `monitoring/grafana/dashboards/melis-gateway.json`.

### Exemplos de Queries PromQL

```promql
# Taxa de requisições (requests/segundo)
rate(melis_gateway_requests_total[5m])

# Latência P99 do backend por provedor
histogram_quantile(0.99, rate(melis_backend_latency_seconds_bucket[5m]))

# Tokens economizados por compressão (por minuto)
rate(melis_context_saved_tokens_total[1m]) * 60

# Média da razão de compressão
rate(melis_context_final_tokens[5m]) / rate(melis_context_original_tokens[5m])

# Taxa de failover por provedor
rate(melis_failover_total[5m])

# Estado do circuit breaker (0=closed, 1=open, 2=half-open)
melis_circuit_breaker_state

# Taxa de erros por provedor
rate(melis_provider_errors_total[5m])

# Overhead do gateway P95
histogram_quantile(0.95, rate(melis_gateway_internal_overhead_seconds_bucket[5m]))

# Rastreamento de custo de tokens (tokens de entrada por modelo)
rate(melis_llm_tokens_total{direction="input"}[5m])
```

---

## Exemplos de Uso da API

### Resposta JSON (sem streaming)

```bash
curl -X POST http://localhost:9090/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "llama3.2",
    "messages": [
      {"role": "system", "content": "Você é um assistente útil."},
      {"role": "user", "content": "Olá, como vai?"}
    ],
    "stream": false
  }'
```

### SSE Streaming (token a token)

```bash
curl -X POST http://localhost:9090/v1/chat/completions \
  -H "Content-Type: application/json" \
  -N \
  -d '{
    "model": "llama3.2",
    "messages": [
      {"role": "user", "content": "Escreva um haiku sobre programação em Rust"}
    ],
    "stream": true
  }'
```

### Rotas por Provedor

```bash
# OpenAI
curl -X POST http://localhost:9090/v1/chat/openai \
  -H "Content-Type: application/json" \
  -d '{
    "messages": [{"role": "user", "content": "Olá pela rota OpenAI"}],
    "stream": false
  }'

# Anthropic Claude
curl -X POST http://localhost:9090/v1/chat/claude \
  -H "Content-Type: application/json" \
  -d '{
    "messages": [{"role": "user", "content": "Olá pela rota Claude"}],
    "stream": true
  }'

# Google Gemini
curl -X POST http://localhost:9090/v1/chat/gemini \
  -H "Content-Type: application/json" \
  -d '{
    "messages": [{"role": "user", "content": "Olá pela rota Gemini"}],
    "stream": false
  }'

# Grok (x.ai)
curl -X POST http://localhost:9090/v1/chat/grok \
  -H "Content-Type: application/json" \
  -d '{
    "messages": [{"role": "user", "content": "Olá pela rota Grok"}],
    "stream": false
  }'

# DeepSeek
curl -X POST http://localhost:9090/v1/chat/deepseek \
  -H "Content-Type: application/json" \
  -d '{
    "messages": [{"role": "user", "content": "Olá pela rota DeepSeek"}],
    "stream": false
  }'

# Ollama (local)
curl -X POST http://localhost:9090/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "messages": [{"role": "user", "content": "Olá pelo Ollama"}],
    "stream": false
  }'
```

### Python com SDK da OpenAI

Como o Melis é compatível com OpenAI, você pode usar o SDK oficial Python da OpenAI:

```python
from openai import OpenAI

# Apontar o cliente para o gateway Melis
client = OpenAI(
    base_url="http://localhost:9090/v1/chat",
    api_key="qualquer-chave-se-auth-desabilitado"
)

# Sem streaming
response = client.chat.completions.create(
    model="llama3.2",
    messages=[
        {"role": "system", "content": "Você é um assistente útil."},
        {"role": "user", "content": "Explique computação quântica em termos simples."}
    ]
)
print(response.choices[0].message.content)

# Com streaming
stream = client.chat.completions.create(
    model="gpt-4o",
    messages=[
        {"role": "user", "content": "Escreva um poema sobre IA."}
    ],
    stream=True
)
for chunk in stream:
    if chunk.choices[0].delta.content:
        print(chunk.choices[0].delta.content, end="")
```

### Com Autenticação Habilitada

```bash
curl -X POST http://localhost:9090/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sua-chave-api-aqui" \
  -d '{
    "messages": [{"role": "user", "content": "Olá"}],
    "stream": false
  }'
```

---

## Deploy

### Docker (Build Local)

```bash
# Build da imagem (~50MB final, distroless)
docker build -t melis-gateway:latest .

# Executar
docker run -d \
  --name melis-gateway \
  -p 9090:8080 \
  -v $(pwd)/config.yaml:/app/config.yaml:ro \
  -v $(pwd)/routes.yaml:/app/routes.yaml:ro \
  -e RUST_LOG=info \
  melis-gateway:latest
```

### Docker Compose (com Redis + Monitoramento)

```bash
# Iniciar gateway + Redis
docker compose up -d

# Iniciar stack de monitoramento (Prometheus + Grafana)
cd monitoring
docker compose -f docker-compose.monitoring.yml up -d
```

### Kubernetes

#### Deployment

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: melis-gateway
  labels:
    app: melis-gateway
spec:
  replicas: 3
  selector:
    matchLabels:
      app: melis-gateway
  template:
    metadata:
      labels:
        app: melis-gateway
      annotations:
        prometheus.io/scrape: "true"
        prometheus.io/port: "8080"
        prometheus.io/path: "/metrics"
    spec:
      containers:
        - name: melis-gateway
          image: ghcr.io/your-org/melis-gateway:latest
          ports:
            - containerPort: 8080
          env:
            - name: MELIS_SERVER_PORT
              value: "8080"
            - name: MELIS_REDIS_URL
              value: "redis://redis-service:6379"
            - name: RUST_LOG
              value: "info"
          volumeMounts:
            - name: config
              mountPath: /app/config.yaml
              subPath: config.yaml
            - name: routes
              mountPath: /app/routes.yaml
              subPath: routes.yaml
          resources:
            requests:
              cpu: 100m
              memory: 64Mi
            limits:
              cpu: 1000m
              memory: 256Mi
          livenessProbe:
            httpGet:
              path: /metrics
              port: 8080
            initialDelaySeconds: 5
            periodSeconds: 10
          readinessProbe:
            httpGet:
              path: /metrics
              port: 8080
            initialDelaySeconds: 3
            periodSeconds: 5
      volumes:
        - name: config
          configMap:
            name: melis-config
        - name: routes
          configMap:
            name: melis-routes
```

#### Service

```yaml
apiVersion: v1
kind: Service
metadata:
  name: melis-gateway
spec:
  selector:
    app: melis-gateway
  ports:
    - port: 80
      targetPort: 8080
  type: ClusterIP
```

#### ConfigMap

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: melis-config
data:
  config.yaml: |
    server:
      host: "0.0.0.0"
      port: 8080
      max_payload_size: 10485760
      graceful_shutdown_timeout_secs: 30
      max_concurrent_connections: 5000
    # ... restante da sua configuração
```

#### Horizontal Pod Autoscaler

```yaml
apiVersion: autoscaling/v2
kind: HorizontalPodAutoscaler
metadata:
  name: melis-gateway-hpa
spec:
  scaleTargetRef:
    apiVersion: apps/v1
    kind: Deployment
    name: melis-gateway
  minReplicas: 2
  maxReplicas: 20
  metrics:
    - type: Resource
      resource:
        name: cpu
        target:
          type: Utilization
          averageUtilization: 70
    - type: Pods
      pods:
        metric:
          name: melis_gateway_requests_total
        target:
          type: AverageValue
          averageValue: "1000"
```

---

## Builds Cross-Platform

### Targets do Makefile

```bash
# Build para plataforma atual
make build

# Linux x86_64 (binário estático musl)
make build-linux

# Linux ARM64 (binário estático musl)
make build-linux-arm

# macOS Intel
make build-mac

# macOS Apple Silicon (ARM64)
make build-mac-arm

# Windows x86_64
make build-windows

# Todas as plataformas de uma vez
make build-all

# Empacotar releases em dist/
make release

# Build da imagem Docker
make docker
```

### Pré-requisitos

```bash
# Instalar ferramenta de cross-compilação
cargo install cross --version 0.2.5
# ou
make install-cross
```

### GitHub Actions CI/CD

O projeto inclui um pipeline de release automático (`.github/workflows/release.yml`) que dispara ao criar uma tag:

```bash
# Criar tag e push para disparar release automático
git tag v0.1.0
git push origin v0.1.0
```

Isso produz:

| Plataforma | Arquitetura | Artefato |
|------------|-------------|----------|
| Linux | x86_64 | `melis-gateway-x86_64-unknown-linux-musl.tar.gz` |
| Linux | ARM64 | `melis-gateway-aarch64-unknown-linux-musl.tar.gz` |
| macOS | Intel | `melis-gateway-x86_64-apple-darwin.tar.gz` |
| macOS | Apple Silicon | `melis-gateway-aarch64-apple-darwin.tar.gz` |
| Windows | x86_64 | `melis-gateway-x86_64-pc-windows-msvc.zip` |

---

## Variáveis de Ambiente

| Variável | Descrição | Padrão |
|----------|-----------|--------|
| `MELIS_SERVER_PORT` | Porta de escuta do gateway | `9090` |
| `MELIS_SERVER_HOST` | Endereço de escuta do gateway | `0.0.0.0` |
| `MELIS_ROUTES_CONFIG` | Caminho para routes.yaml | `./routes.yaml` |
| `MELIS_REDIS_URL` | URL de conexão Redis | `redis://localhost:6379` |
| `MELIS_OTLP_ENDPOINT` | Endpoint OpenTelemetry OTLP | `http://localhost:4317` |
| `RUST_LOG` | Filtro de nível de log (tracing) | `info` |

---

## Arquitetura

```
┌─────────────────────────────────────────────────────────────────────┐
│                       REQUISIÇÃO DO CLIENTE                           │
│                  (formato compatível com OpenAI)                      │
└────────────────────────────────┬────────────────────────────────────┘
                                 │
                                 ▼
┌─────────────────────────────────────────────────────────────────────┐
│                         MELIS GATEWAY                                 │
│                                                                       │
│  ┌──────────┐  ┌──────────────┐  ┌────────────┐  ┌──────────────┐ │
│  │   Auth   │→ │ Rate Limiter │→ │ Compactor  │→ │   Router     │ │
│  │Middleware│  │(Token Bucket)│  │(Compressão)│  │(Match Rota)  │ │
│  └──────────┘  └──────────────┘  └────────────┘  └──────┬───────┘ │
│                                                           │         │
│  ┌────────────────────────────────────────────────────────▼───────┐ │
│  │                  BALANCEADOR DE CARGA                            │ │
│  │              (Round-Robin Ponderado)                             │ │
│  └─────┬──────────────┬──────────────┬──────────────┬─────────────┘ │
│        │              │              │              │               │
│  ┌─────▼─────┐  ┌─────▼─────┐  ┌─────▼─────┐  ┌─────▼─────┐     │
│  │  Circuit  │  │  Circuit  │  │  Circuit  │  │  Circuit  │     │
│  │  Breaker  │  │  Breaker  │  │  Breaker  │  │  Breaker  │     │
│  └─────┬─────┘  └─────┬─────┘  └─────┬─────┘  └─────┬─────┘     │
│        │              │              │              │               │
│  ┌─────▼─────┐  ┌─────▼─────┐  ┌─────▼─────┐  ┌─────▼─────┐     │
│  │Transpiler │  │Transpiler │  │Transpiler │  │Transpiler │     │
│  │(Payload)  │  │(Payload)  │  │(Payload)  │  │(Payload)  │     │
│  └─────┬─────┘  └─────┬─────┘  └─────┬─────┘  └─────┬─────┘     │
└────────┼──────────────┼──────────────┼──────────────┼─────────────┘
         │              │              │              │
         ▼              ▼              ▼              ▼
   ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐
   │  OpenAI  │  │  Claude  │  │  Gemini  │  │  Ollama  │
   └──────────┘  └──────────┘  └──────────┘  └──────────┘
```

**Estágios do pipeline:**
1. **Auth** — Valida chave de API (se habilitado)
2. **Rate Limiter** — Rate limiting Token Bucket por cliente
3. **Compactor** — Compressão de contexto (adaptive trimming / sliding window)
4. **Router** — Faz match do path da requisição com a configuração de rotas
5. **Balanceador de Carga** — Seleciona provedor (round-robin ponderado)
6. **Circuit Breaker** — Verifica saúde do provedor, habilita failover
7. **Transpiler** — Traduz payload entre formato OpenAI e formato nativo do provedor
8. **Provedor** — Envia requisição ao backend LLM, faz streaming da resposta

---

## Licença

Licença MIT. Veja [LICENSE](LICENSE) para detalhes.
