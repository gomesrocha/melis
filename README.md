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
- **Real BPE token counting** — opt-in precise tokenization (tiktoken `cl100k_base`/`o200k_base`), with a safe char-count fallback
- **Semantic response cache** — opt-in per-route cache keyed by conversation embedding (Redis + Ollama), fail-open
- **Bidirectional guardrails** — opt-in per-route inbound/outbound content rules (regex/keyword/prompt-injection), block/redact/warn, streaming-aware
- **Load balancing** — weighted round-robin with automatic failover
- **Circuit breaker** — per-provider, with exponential backoff recovery; optional Redis backend shared across replicas
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
  tokenizer_name: "cl100k_base"  # Encoding used when token_counting is "real"
  token_counting: "estimated"    # "estimated" (default) or "real" (BPE) — see Token Counting

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
| `semantic_cache` | Per-route response cache settings (optional, opt-in) |
| `semantic_cache.enabled` | Enable the cache for this route (default `false`) |
| `semantic_cache.similarity_threshold` | Cosine threshold for a hit (default `0.92`) |
| `semantic_cache.ttl_secs` | TTL applied to every entry (default `3600`) |
| `semantic_cache.max_entries` | Max entries per (route, model) namespace (default `1000`) |
| `semantic_cache.embedding_model` | Embedding model (default `nomic-embed-text`) |
| `guardrails` | Per-route content rules (optional, opt-in) |
| `guardrails.fail_closed` | Block on evaluation failure (default inherits global) |
| `guardrails.rules[].name` | Rule name (used as the `rule` metric label) |
| `guardrails.rules[].type` | `regex`, `keyword`, or `prompt_injection_heuristic` |
| `guardrails.rules[].direction` | `inbound`, `outbound`, or `both` |
| `guardrails.rules[].action` | `block`, `redact`, or `warn` |
| `guardrails.rules[].pattern` | Regex pattern (for `type: regex`) |
| `guardrails.rules[].keywords` | Keyword list (for `type: keyword`) |

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

### Shared Circuit Breaker (Redis backend)

By default the breaker is in-process (`circuit_breaker.backend: local`). Set
`backend: redis` to share state across replicas so a failure detected by one
instance protects all of them. Same semantics as the local breaker (429 opens
immediately; failure-rate threshold with a minimum request count; single-probe
half-open with exponential backoff).

State is encoded in Redis keys:

| State | `:state` | `:backoff` | `:probe` |
|-------|----------|------------|----------|
| Closed (healthy) | absent | absent | absent |
| Open | present (TTL) | present | — |
| Half-open, probe available | absent (expired) | present | absent |
| Half-open, probe in flight | absent | present | present (TTL = lease) |

`:backoff` has no TTL and is deleted only by a successful probe, so an expired
open window is never mistaken for a healthy circuit.

Key behaviors:

- **Single probe across replicas:** the probe is claimed atomically (Lua,
  `SET NX EX`). Only one attempt reaches the provider while it is in flight —
  on the same replica or across replicas. A probe grant is never cached locally.
- **Probe identity:** each probe carries a token; its completion changes state
  only if it is still the current probe (checked atomically). A late response
  can't close a newer round. The probe lease is the provider timeout + 1s, so a
  client disconnect mid-probe never leaves the probe stuck.
- **Server clock:** sliding windows use Redis `TIME`, not replica clocks.
- **No per-request round-trip:** reads go through a short-TTL local cache
  (`redis_state_cache_ms`, default 150ms); writes go to Redis.

**Degradation policy** (the same whether Redis fails at boot or later):

| Situation | Active backend | Behavior |
|-----------|----------------|----------|
| Redis down at boot | `redis` | boot continues after `redis.connect_timeout_secs`; reconnects in background |
| Redis goes down while running | `redis` | providers this replica saw open/half-open stay denied (bounded); all others fail open |
| Redis comes back | `redis` | recovers automatically, no restart |
| No usable Redis URL configured | `local` | config error → local breaker + warning |

Redis operations are bounded (100ms timeout) and skipped for 1s after a failure,
so an outage costs at most one timeout per second, not one per request. Watch
`melis_circuit_breaker_backend`, `melis_circuit_breaker_degraded`,
`melis_circuit_breaker_degraded_decisions_total{decision}` and `melis_redis_up`.

**Local protection during a Redis outage (limits).** When a replica has observed a
provider in Open/Half-Open and then loses Redis, it keeps denying that provider for
at most

```
on read (admit saw Open/Half-Open):  horizon = max_ttl_secs + probe_lease
on write (this replica opened it):   horizon = max_ttl_secs + open_ttl_secs
probe_lease = provider timeout + 1s
```

counted from the last time it read or wrote the unhealthy state. After the
horizon it fails open again, so a long Redis outage cannot block a provider forever.
When Redis answers again, the next read reconciles the local knowledge with the
shared state automatically (if another replica closed the circuit meanwhile, this
replica allows traffic and drops its local "unhealthy" mark).

This is a **conservative local protection, not a global single-probe guarantee**.
During a Redis partition there is no coordination: replicas that never observed the
cycle (new pods, pods that were idle for that provider) fail open, and each replica
decides on its own. The single-probe guarantee only holds while Redis is reachable.

Distinguishing a Redis outage from a provider failure:

| Signal | Provider-failure circuit | Redis outage |
|--------|-------------------------|--------------|
| `Admission` | `Deny` | `DenyUnverified` (or `Allow` when the replica knows nothing) |
| `melis_failover_total{reason}` / `melis_fallback_mode_total{reason}` | `circuit_open` | `breaker_state_unavailable` |
| `melis_circuit_breaker_degraded_decisions_total{decision}` | — | `allow` \| `deny` \| `write_skipped` |
| Log | — | `redis unavailable: denying (...bounded local protection)` (warn) |
| `melis_redis_up` | 1 | 0 |

**Health endpoints and Redis.**

| Endpoint | Depends on Redis? | Redis down |
|----------|-------------------|------------|
| `/healthz` (liveness) | never | always 200 |
| `/readyz`, `redis.required_for_readiness: false` (default) — Redis is an *optional* dependency | no | 200 `{"status":"ready","degraded":["redis"]}` |
| `/readyz`, `redis.required_for_readiness: true` — Redis is a *required* dependency | yes | 503 `{"status":"not_ready","reason":"Redis connection unavailable"}` |

Optional (default) fits the degradation policy above: cache and breaker keep
working in degraded mode, so a Redis outage must not pull every replica out of the
load balancer at once. Use `true` only if your deployment cannot serve correctly
without Redis.

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

### Token Counting (estimated vs real)

By default, Melis counts tokens with a fast character-count approximation
(`len / 4`). You can opt into precise BPE counting per deployment via the
`compactor.token_counting` setting in `config.yaml`:

```yaml
compactor:
  token_counting: "real"        # "estimated" (default) or "real"
  tokenizer_name: "cl100k_base" # encoding used only in "real" mode
```

| Mode | Behavior | Encoding source |
|------|----------|-----------------|
| `estimated` (default) | `len / 4` approximation — legacy behavior, bit-for-bit | n/a (`tokenizer_name` ignored) |
| `real` | Precise BPE counting via tiktoken | `tokenizer_name`: `cl100k_base` or `o200k_base` |

Notes:

- `tokenizer_name` is **only** the encoding selector; it never enables real
  counting on its own. The switch is always the explicit `token_counting` field.
- In `real` mode, when a provider omits `usage` (or returns zeros), Melis fills
  `usage.prompt_tokens`/`completion_tokens` with real counts.
- The active mode is exported as `melis_token_counting_mode{mode="real|estimated"}`
  (present from boot) so dashboards can tell the base of token numbers.
- If `tokenizer_name` is set to an unsupported encoding while in `real` mode,
  Melis logs a warning and falls back to estimated counting (never fails a request).

> **Heads up on upgrades:** switching from `estimated` to `real` changes the token
> numbers that drive compaction thresholds (`compress_above_tokens`) and the
> `usage`/token metrics. This is intentional — real counts differ from the
> approximation — but it means compaction may trigger at different points. See the
> CHANGELOG for details.

**Cost:** BPE counting is backed by cached (`&'static`) tiktoken encodings, so no
vocabulary is loaded per request. Measured on a release build, a ~341-token chat
payload counts in ~80 µs per call — well under 1 ms. You can reproduce it with:

```bash
cargo test --release tokenizer::tests::bench_real_count_typical_chat -- --ignored --nocapture
```

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

### Semantic scoring for compaction (experimental, opt-in)

Routes using `strategy: semantic_guarded_trimming` can rank removable messages by
embedding similarity to the last user message, in addition to the structural
heuristic. This is a **hypothesis under evaluation**: no claim of better selection
is made, and the default weights (0.5 / 0.5) are provisional and **not calibrated**.
Full spec: [`docs/specs/phase-5-semantic-scoring.md`](docs/specs/phase-5-semantic-scoring.md).

```yaml
token_optimization:
  strategy: semantic_guarded_trimming
  semantic_scoring: hybrid          # heuristic (default) | embedding | hybrid, or a block:
  # semantic_scoring:
  #   mode: hybrid
  #   semantic_weight: 0.5          # provisional
  #   structural_weight: 0.5        # provisional
  #   embedding_model: nomic-embed-text
  #   timeout_ms: 1500              # total per request
  #   max_concurrency: 4
  #   batch_size: 32
  #   max_messages: 64              # more candidates → full fallback to the heuristic
  #   max_chars: 2000               # truncation of the text sent for scoring only
  #   tool_group_policy: preserve   # preserve (default) | eligible_for_removal
  #   budget:
  #     mode: legacy                # legacy (default) | strict
  #     max_context_tokens: 8192    # strict only (required)
  #     output_reserve_tokens: 1024 # strict only
  #     formatting_overhead: none   # none | openai_chat_estimate (an estimate, not billing)
```

Behavior:

- **Off by default.** Without `semantic_scoring` (or with `heuristic`) the legacy
  compaction path runs unchanged, with **no embedding calls**. On other strategies
  the field is ignored with a warning.
- Embeddings are computed only when the mode is enabled **and** compaction is
  needed. One batched call per request (`/api/embed`), bounded by `timeout_ms`.
- Score: `w_sem · (cos+1)/2 + w_struct · structural_normalized`. `(cos+1)/2` is a
  scale transform to [0, 1], not a probability. Ties: the oldest message is removed
  first. Kept messages are forwarded as the **original objects**, in original order,
  with no content rewrite.
- Protected: system, last user message, critical markers, and every message the
  bugfix D1 pins. An `assistant.tool_calls` message plus all of its results form an
  indivisible group (OpenAI-compatible payloads only). Incomplete or ambiguous tool
  dependencies are pinned.
- **Tool groups**: by default (`tool_group_policy: preserve`) a complete group is
  never removed. Set `eligible_for_removal` to let a complete group be dropped, but
  only as a whole. Preserving the call/result protocol dependency is not a semantic
  preservation guarantee. Opaque tool-call ids are excluded from the scoring text
  (payload unchanged), so they do not perturb the ranking.
- Any error, timeout, invalid vector (dimension, NaN/∞, zero norm), or too many
  candidates → **full fallback** to the heuristic for that request. The request never
  fails because of scoring.
- `budget.mode: strict`: when mandatory content + overhead + output reserve exceeds
  `max_context_tokens`, the gateway answers **400** with a clear "Insufficient
  context budget" message instead of dropping protected content. `legacy` keeps the
  existing ratio-based budget. Provider `usage` is never overwritten.

**Where the data goes.** The candidates' text and the last user message (truncated
to `max_chars`) are sent to the configured Ollama embedding provider. Inbound
guardrails run before, so redacted text is what gets sent.

**Cost with the semantic cache.** If the semantic cache is also enabled on the route,
a compacted request makes **additional** embedding calls: one for the cache key and
one for scoring. They embed different texts, so nothing is reused between them.

Offline evaluation (three modes, same budget, reproducible report with commit,
dataset hash, provider, model, weights and configuration):

```bash
cargo test --bin melis-gateway semantic_scoring::eval::harness_report -- --ignored --nocapture
MELIS_EVAL_PROVIDER=ollama MELIS_EMBED_MODEL=nomic-embed-text \
  cargo test --bin melis-gateway semantic_scoring::eval::harness_report -- --ignored --nocapture
```

Tune only on `eval/compaction/dev.jsonl`. The `eval` split is reserved for the final
run (`MELIS_EVAL_SPLIT=eval MELIS_EVAL_CONFIRM_FINAL=1`). The bundled fixtures are
small and synthetic, and the default `stub` provider is lexical: they exercise the
harness and don't support conclusions.

---

## Virtual Models (aliases)

A request's `model` field can be a declarative **alias** that resolves to one or
more concrete `(provider, model)` targets, so clients can ask for a logical name
(`fast`, `team-default`) without knowing the backend. This is pure declarative
mapping with configured weights — there is no "lowest cost" / "best quality"
auto-routing.

```yaml
model_aliases:
  - name: "fast"
    targets:
      - provider: "ollama"
        model: "llama3.2"
  - name: "team-default"
    targets:
      - provider: "openai"
        model: "gpt-4o-mini"
        weight: 3
      - provider: "ollama"
        model: "llama3.2"
        weight: 1
```

Resolution and precedence (for a request that matched a route):

1. **Route `model`** (mandatory override) — forces the model; a client alias is
   ignored. Existing routes keep their exact behavior.
2. **Payload alias** — a `model` that names an alias resolves to its targets.
3. **Concrete payload model** — forwarded as-is.
4. **Route `default_model`** — used only when the client sent no usable model; it
   does **not** shadow an alias the client sent. A route sets at most one of
   `model` / `default_model`.

`@name` is reserved syntax that forces alias interpretation: `@fast` must be a
declared alias (else **400**, never forwarded as a concrete model). `@fast` and
`fast` are the same alias. A bare unknown name is treated as a concrete model and
forwarded as-is.

- **Multi-target** aliases feed the weighted load balancer and failover; each
  attempt forwards that target's concrete model. **Single-target** aliases are
  deterministic.
- **Authorization:** `allowed_models` (per API key) is enforced for the requested
  name, the effective model, and every resolved alias destination — an alias cannot
  reach a model the client is not allowed to use.
- **Cache:** single-target aliases (and normal routes) key the cache by the
  effective destination (provider + hashed deployment + concrete model), so a
  remapped alias or a different deployment never reuses an incompatible entry.
  Multi-target aliases **bypass** the cache in this version (the destination is
  chosen after lookup and can fall back), counted by
  `melis_cache_skipped_total{reason="multi_target_alias"}`.
- **Response contract unchanged:** the `model` field in the response is the
  provider's value. The requested / resolved / effective models are recorded only
  in internal observability (`melis_model_alias_resolution_total{alias,outcome}`,
  bounded labels), never as public fields.
- **`GET /v1/models`** lists declared aliases and declared concrete models in
  OpenAI format, respects auth, and filters by `allowed_models`. It never reveals
  credentials, provider endpoints, weights, or the alias mapping.

Validation rejects (at boot; kept-previous on hot reload): an alias name equal to a
declared concrete model, ambiguous `name`/`@name`, empty targets, zero weight,
unknown target provider, alias chaining, and `model`+`default_model` on one route.

See [`docs/specs/phase-6a-virtual-models.md`](docs/specs/phase-6a-virtual-models.md).

---

## Semantic Cache

Melis can cache responses per route and return them for semantically-equivalent
requests, skipping the provider entirely. It is **opt-in per route** and
**fail-open**: if Redis or the embeddings backend is unavailable, the cache
disables itself (logs a warning, increments `melis_cache_degraded`) and the
request proceeds normally.

### How it works

1. **Normalize** — the non-system messages are concatenated, lowercased, and
   whitespace-collapsed.
2. **Embed** — the normalized conversation is embedded via the configured Ollama
   model (default `nomic-embed-text`).
3. **Lookup** — all entries in the `(route, model)` namespace are fetched in a
   single Redis round-trip and compared by cosine similarity. If the best match
   is `>= similarity_threshold`, it is a **hit** (no provider call).
4. **Store on miss** — the provider response is stored with a TTL and indexed for
   the namespace; the oldest entries are evicted when `max_entries` is exceeded.

Cache hits are delivered through the same outbound path as normal responses
(reserved for output guardrails in a later phase). Streaming requests replay a
cached response as an OpenAI SSE stream.

**Reuse scope (identity isolation).** `semantic_cache.scope` controls who may reuse
a cached response:

- `per_identity` (default): the cache is isolated by the authenticated client
  identity (`client_id` from the auth middleware). A client only ever receives its
  own cached responses. A request with no trusted identity (auth disabled, or an
  anonymous caller) does not consult or populate the cache. The identity is hashed
  into the key; it never appears in Redis keys, logs, or metrics, and a
  client-controlled header (e.g. `X-Client-Id`) cannot change the scope.
- `shared`: responses may be reused across all authenticated callers of the route.
  This must be set explicitly, because it means one user can receive a response
  generated for another.

The key also folds the system prompt, generation parameters, and tool definitions
(see the isolation note below), so reuse requires a compatible context as well as a
matching scope. Changing `scope` or any of those lands requests in a different
namespace; old entries are never reused (they age out by TTL).

### Configuration (per route in `routes.yaml`)

```yaml
routes:
  - path: "/v1/chat/cached"
    method: "POST"
    provider: "ollama"
    model: "llama3.2"
    semantic_cache:
      enabled: true                 # default false
      scope: per_identity           # per_identity (default) | shared
      similarity_threshold: 0.92    # cosine threshold applied at lookup
      ttl_secs: 3600                # TTL on every entry (required)
      max_entries: 1000             # per (route, model) namespace
      embedding_model: "nomic-embed-text"
```

### Key design points

- **Key = route + model + conversation embedding.** The same conversation on a
  different route or model is a distinct entry (no cross-context leakage).
- **Threshold is a query parameter**, not part of the key. Changing it only
  changes the hit rate — no invalidation needed.
- **Model invalidation is implicit by namespace**, backed by a mandatory TTL and a
  best-effort background cleanup (`SCAN`+`DEL`) when a route's model changes on
  hot reload. The request path never blocks on cleanup.
- **Batch fetch:** a namespace scan reads all candidate embeddings in one Redis
  round-trip (not one per candidate), keeping lookup latency low (p95 < 5 ms at
  the default 1000-entry capacity).

### Limitations (v1)

- **Streaming does not populate the cache.** Only non-streaming responses are
  stored. A `stream: true` request can *replay* an existing cached entry, but its
  own aggregated response is not written back. Aggregating and storing stream
  responses is tracked as future work.
- Multi-turn incremental/prefix caching and real LSH bucketing are future work;
  v1 keys on the full normalized conversation with a bounded brute-force scan.
- The lookup latency budget (p95 < 5 ms) covers the Redis scan; the embedding
  call (Ollama) is measured separately via `melis_cache_embed_seconds` and is
  typically the dominant cost of a lookup.

---

## Guardrails

Melis can inspect and filter content in both directions, opt-in per route. Rules
are evaluated with a safe regex engine and can block, redact, or warn.

### Pipeline order

Inbound guardrails run **after** auth and rate limiting (so content inspection is
never spent on unauthenticated/limited traffic) and **before** compaction and the
semantic cache:

- A **blocked** request returns HTTP 400 and never reaches the provider or the
  cache (no lookup, no store).
- A **redacted** request has its content rewritten (`[REDACTED]`) *before* the
  cache key is computed and before the provider call — so a cache hit always
  corresponds to what was actually sent upstream.

Outbound guardrails are applied to every response before delivery, **including
cache hits**. For streaming, a sliding-window buffer evaluates the accumulated
text before emitting each chunk (emission latency ≤ 1 chunk); a blocking match
ends the stream with an SSE error event without leaking the blocked content.

**Streaming guardrails: supported guarantees and limits.** Scanning runs over a
sliding window of the accumulated assistant text (last 4096 bytes), so patterns
that span chunk boundaries are detected, not just per-chunk matches.

- `block`: a match anywhere in the window ends the stream with an error event; the
  offending chunk is not emitted. Chunks emitted *before* the match was completable
  cannot be retracted (streaming is incremental by nature).
- `redact`: the matched span in the current chunk is replaced with `[REDACTED]`.
  When a keyword/pattern straddles a chunk boundary, the window still detects it and
  the straddling chunk's content is **suppressed** (emitted empty) rather than
  leaked; this can drop benign text sharing that chunk.
- Limits: a match split across the 4096-byte window trim boundary (i.e. whose start
  was already evicted) may escape detection. Only assistant `delta.content` is
  scanned, not tool-call deltas or other fields. Regex rules are matched without
  case folding, so use `(?i)` for case-insensitive regex (keyword and
  prompt-injection rules are case-insensitive).

These are the honest guarantees of a bounded-buffer, low-latency design. Full
cross-chunk guarantees for arbitrary rules would require buffering the whole
response (defeating streaming) or per-rule maximum-length bounds; that tradeoff is
left to a future opt-in and is not claimed today.

### Configuration (per route in `routes.yaml`)

```yaml
routes:
  - path: "/v1/chat/guarded"
    method: "POST"
    provider: "ollama"
    model: "llama3.2"
    guardrails:
      fail_closed: false            # optional; inherits config.yaml global if omitted
      rules:
        - name: "no-ssn"
          type: "regex"             # regex | keyword | prompt_injection_heuristic
          direction: "inbound"      # inbound | outbound | both
          action: "block"          # block | redact | warn
          pattern: "\\d{3}-\\d{2}-\\d{4}"
        - name: "mask-emails"
          type: "regex"
          direction: "outbound"
          action: "redact"
          pattern: "[a-z0-9._%+-]+@[a-z0-9.-]+\\.[a-z]{2,}"
        - name: "prompt-injection"
          type: "prompt_injection_heuristic"
          direction: "inbound"
          action: "warn"
```

Global default (optional, in `config.yaml`):

```yaml
guardrails:
  fail_closed: false
```

### Rule types

| Type | Matches |
|------|---------|
| `regex` | A `regex`-crate pattern (linear-time automaton) |
| `keyword` | Any of `keywords`, case-insensitive substring |
| `prompt_injection_heuristic` | Built-in instruction-override phrases |

### Actions and precedence

`block` > `redact` > `warn`. If any rule blocks, the request/response is rejected.
Otherwise all redactions are applied; warnings only log and increment
`melis_guardrail_actions_total{rule,direction,action}`.

### fail_closed

When guardrail evaluation fails internally, `fail_closed: true` blocks the
request/response; the default (`false`) fails open and proceeds. Per-route config
takes precedence over the global default.

### Safety: regex engine and ReDoS

Regex rules use the [`regex`](https://crates.io/crates/regex) crate, a finite
automaton with linear-time matching and **no catastrophic backtracking**.
Look-around and backreferences are not supported by design — a guardrail must
never become a ReDoS vector. Invalid patterns are dropped at load/reload with an
error log; on hot reload the previous valid config is kept.

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
| `melis_token_counting_mode` | Counter | `mode` | Token-counting operations by mode (`real` or `estimated`) |
| `melis_cache_hits_total` | Counter | — | Semantic cache hits |
| `melis_cache_misses_total` | Counter | — | Semantic cache misses |
| `melis_cache_lookup_seconds` | Histogram | — | Semantic cache lookup duration |
| `melis_cache_entry_age_seconds` | Histogram | — | Age of a cache entry at hit time |
| `melis_cache_degraded` | Counter | `reason` | Cache fail-open events (`redis` or `embeddings`) |
| `melis_cache_entries` | Gauge | `namespace` | Cache entry count per (route, model) namespace |
| `melis_guardrail_actions_total` | Counter | `rule`, `direction`, `action` | Guardrail actions taken (block/redact/warn) |
| `melis_circuit_breaker_backend` | Gauge | `backend` | Active circuit breaker backend (`local` or `redis`); value 1 for the active one |
| `melis_circuit_breaker_degraded` | Counter | — | Redis circuit breaker operations that could not reach Redis |
| `melis_circuit_breaker_degraded_decisions_total` | Counter | `decision` | Breaker decisions while Redis was unavailable: `allow`, `deny` (bounded local protection), `write_skipped` |
| `melis_redis_up` | Gauge | — | Shared Redis reachability (1 = PING ok), refreshed every 5s; drives `/readyz` only when `redis.required_for_readiness: true` |
| `melis_cache_embed_seconds` | Histogram | — | Embedding computation time in the cache lookup path |
| `melis_compaction_scoring_total` | Counter | `requested`, `effective` | Compactions on scoring-enabled routes: requested vs effectively used mode |
| `melis_compaction_scoring_seconds` | Histogram | — | Semantic scoring duration (embedding included) |
| `melis_compaction_scoring_fallback_total` | Counter | `reason` | Fallbacks to the heuristic: `limit_exceeded`, `timeout`, `provider_error`, `count_mismatch`, `invalid_dimension`, `non_finite`, `zero_norm`, `no_query` |
| `melis_compaction_scoring_skipped_total` | Counter | `reason` | Scoring not needed: `no_candidates`, `mandatory_exceeds_target` |
| `melis_compaction_budget_insufficient_total` | Counter | — | Requests rejected by `budget.mode: strict` |
| `melis_model_alias_resolution_total` | Counter | `alias`, `outcome` | Alias resolutions; `alias` is a declared name or `other`; `outcome` ∈ resolved_single/resolved_multi/unknown_alias/no_alias |
| `melis_cache_skipped_total` | Counter | `reason` | Cache lookups/stores skipped (e.g. `multi_target_alias`) |

> **Note on `melis_circuit_breaker_state` with the Redis backend:** replicas read
> state through a short-TTL local cache (`circuit_breaker.redis_state_cache_ms`,
> default 150ms), so this gauge may **disagree between replicas for up to that TTL**
> after a transition. This is expected convergence lag, not an alarm — Redis is the
> source of truth.

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

### Embeddings (OpenAI-compatible)

Melis supports `POST /v1/embeddings` using Ollama as backend. The endpoint is fully compatible with the OpenAI embeddings API format.

**Single input:**

```bash
curl -X POST http://localhost:9090/v1/embeddings \
  -H "Content-Type: application/json" \
  -d '{
    "model": "nomic-embed-text:latest",
    "input": "Cadastro de cliente com CPF e CNPJ"
  }'
```

**Multiple inputs (batch):**

```bash
curl -X POST http://localhost:9090/v1/embeddings \
  -H "Content-Type: application/json" \
  -d '{
    "model": "nomic-embed-text:latest",
    "input": [
      "Cadastro de pessoa física com CPF",
      "Cadastro de pessoa jurídica com CNPJ"
    ]
  }'
```

**Response format:**

```json
{
  "object": "list",
  "data": [
    {
      "object": "embedding",
      "index": 0,
      "embedding": [0.01, 0.02, 0.03, ...]
    }
  ],
  "model": "nomic-embed-text:latest",
  "usage": {
    "prompt_tokens": 0,
    "total_tokens": 0
  }
}
```

**Docker with Ollama (host.docker.internal):**

```bash
docker run --rm \
  --add-host=host.docker.internal:host-gateway \
  -p 9090:8080 \
  -v "$PWD/config.yaml:/app/config.yaml:ro" \
  -v "$PWD/routes.yaml:/app/routes.yaml:ro" \
  melis-gateway:latest

# Then from another terminal:
curl http://localhost:9090/v1/embeddings \
  -H "Content-Type: application/json" \
  -d '{"model": "nomic-embed-text:latest", "input": "Test embedding via Docker"}'
```

> **Note:** The `nomic-embed-text:latest` model produces 768-dimensional embeddings by default.
> Ollama must be running with the model pulled (`ollama pull nomic-embed-text`).
> The gateway tries `/api/embed` first (modern Ollama), then falls back to `/api/embeddings` (legacy).

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
