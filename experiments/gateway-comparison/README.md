# Gateway comparison infrastructure (preparation only)

Reproducible infrastructure to compare three request paths to the **same** backend:

1. **direct** — client → provider
2. **melis** — client → Melis → same provider
3. **litellm** — client → LiteLLM → same provider

This is **preparation**, not the final evaluation. No protocol, sample size, or
statistical analysis is fixed here; those come later. Nothing here is tuned to make
any gateway win.

## Artifact under test

- Melis candidate commit: run `git -C ../.. rev-parse HEAD` (recorded in every
  harness run via `--meta`). The experiment worktree is a clean detached checkout of
  that commit; the original repo keeps its `bedrock.rs` WIP untouched.

## Pinned external images (digests, not `latest`)

| Component | Reference |
|-----------|-----------|
| LiteLLM | `ghcr.io/berriai/litellm@sha256:d781cd3d736b1bd34f9964ebc491a9f91fb8fa140f35d4e6b4176c19dd2b306b` (tag `main-v1.65.0-stable`) |
| Deterministic backend | `python@sha256:b18b261e1b4974b4f321451ad5251ded85abb22c2b8b262da527dda1d031ab30` (tag `python:3.12-slim`, runs stdlib-only `mock_openai.py`) |
| Redis (cache paths only) | `redis@sha256:858f009f9709ce576febc734aa78b8f6d624b82571f9ddb6bda4377c833b3499` (tag `redis:7-alpine`) |

Re-resolve with `scripts/pin_images.sh` before final runs and update the compose
files if a digest changed. Melis is built from the worktree `Dockerfile`
(`rust:1.86-bookworm` builder, `distroless/static-debian12:nonroot` runtime).

## Ports (chosen to AVOID existing conflicts on this host)

`8080`, `8081`, and `11434` are already in use on this machine (other services +
Ollama), so the stack uses:

| Port (localhost) | Service |
|------------------|---------|
| `7070` | deterministic mock backend |
| `7071` | Melis |
| `4000` | LiteLLM |
| `11434` | host Ollama (real profile only; already running) |
| `6379` | Redis (only for cache-path experiments, started separately) |

All published ports bind to `127.0.0.1` only. **Before starting**, confirm these are
free: `ss -ltn | grep -E ':(7070|7071|4000)'` should print nothing. If any is busy,
edit the compose port mappings; do not reuse `8080/8081/11434`.

## Two execution profiles

- **Deterministic backend** (`docker-compose.deterministic.yml`): `mock_openai.py`
  gives reproducible JSON and SSE responses and exposes `/__count` + `/__reset` for
  **backend-side call counting**. Use this to verify mediation, concurrency, and
  controlled failures without model noise. Fixed latency (`MOCK_LATENCY_MS`, default
  0) keeps overhead about the gateway, not the backend.
- **Real backend** (`docker-compose.real.yml`): the host Ollama (`llama3.2`,
  `nomic-embed-text`) behind all three paths with the same model and parameters, to
  verify the inference path and later evaluate quality / context governance. No
  backend-side call counter exists for Ollama; rely on client-side measurement and,
  where needed, request logs.

Run the gateways **separately** (compose `profiles`) so they don't contend for CPU/
memory during measurement. Each service has `cpus: 1.0` and a memory limit
(`256M` Melis/backend, `512M` LiteLLM) — recorded here so the protocol can hold them
constant; adjust centrally before final runs. The backend location is local
(same host) for all paths.

## Equivalent configuration (minimal-mediation profile first)

Both gateways are configured for **minimal mediation**, matched field-by-field:

| Aspect | Melis | LiteLLM |
|--------|-------|---------|
| Cache | off (`strategy: none`, no `semantic_cache`) | `cache: false` |
| Compaction | off (`strategy: none`) | n/a (no compaction feature) |
| Guardrails | none configured | n/a |
| Retries | single provider (no failover) | `num_retries: 0` |
| Fallback | none | none (no `fallbacks`) |
| Auth | disabled | no master key |
| Observability | `observability.enabled: false` | `telemetry: false` |
| Model / params | same model, `temperature: 0`, `max_tokens: 16` | same |
| Streaming | separate JSON vs SSE scenarios | same |

Resilience and Melis-specific policies (cache, circuit breaker, scoring, guardrails)
have **separate** config files to be added for their own experiments — their effects
must not be mixed into the basic-overhead comparison.

### LiteLLM capability parity (to confirm on the pinned version)

- Caching, retries, fallbacks: supported in LiteLLM; **disabled** here for parity.
- Context compaction / semantic scoring / bidirectional guardrails: not comparable
  built-ins on the pinned LiteLLM; these are Melis-specific and must be evaluated on
  Melis-only configs, not claimed as "absent" in LiteLLM merely because off by
  default. Document any LiteLLM plugin/enterprise requirement and edition/license
  restrictions when scoping those comparisons.

## Instrumentation (independent of gateway internals)

`harness/bench_client.py` is the **same client for all three paths**. It measures
client-side wall time (and TTFB for SSE), and reads backend `/__count` on the
deterministic profile for verifiable call counting. It does **not** trust each
gateway's internal metrics (definitions differ). It classifies every request as
`ok` / `http_error` / `transport_error` / `invalid_response`, and records the
artifact id, image digests, and config in the output. Output is labelled
`smoke`/`pilot` — never a final result.

## Commands

Startup (one profile + one gateway at a time; non-destructive):

```bash
# deterministic profile
scripts/up.sh deterministic backend
scripts/up.sh deterministic melis       # or: litellm
# real profile (host Ollama must already be running)
scripts/up.sh real melis                 # or: litellm
```

Smoke (plumbing check, records metadata):

```bash
scripts/smoke.sh direct   deterministic json
scripts/smoke.sh melis    deterministic json
scripts/smoke.sh litellm  deterministic json
scripts/smoke.sh melis    deterministic sse
scripts/smoke.sh melis    real          json
```

Teardown (containers only; **never** removes volumes or data):

```bash
scripts/down.sh deterministic
scripts/down.sh real
```

Redis for cache-path experiments (separate, opt-in):

```bash
docker run --rm --name melis-exp-redis -p 6379:6379 \
  redis@sha256:858f009f9709ce576febc734aa78b8f6d624b82571f9ddb6bda4377c833b3499
```

No real credentials are stored in any file here. The real profile uses the host
Ollama with the dummy `ollama` key.

## Differences / limitations to consider in the protocol

- **No backend call counter on the real profile.** Provider-call avoidance (e.g.
  cache experiments) is verifiable by count only on the deterministic profile; on
  the real profile use the cache smoke test in the Melis suite (counts via an
  in-process counting client) or Ollama request logs.
- **LiteLLM talks to Ollama via its OpenAI-compatible `/v1`** for contract parity;
  confirm this matches how "direct" is measured (direct also uses `/v1`).
- **Resource limits** are set but the host is shared; pin CPU governor / avoid other
  load during final runs. Record CPU, memory, and that the backend is co-located.
- **Melis in-process cache smoke test** (Redis + embeddings + chat) lives in the
  Melis test suite (`cache_real_services_smoke`, opt-in `MELIS_CACHE_SMOKE=1`); it is
  the authoritative check that a cache hit makes no provider call, by count.
- **Warm-up, repetitions, concurrency levels, sample size, and statistics are NOT
  fixed here** — to be defined in the protocol, not chosen to favor a gateway.
- **Streaming parity**: the deterministic backend emits one SSE chunk per token
  segment; a real provider chunks differently. Compare JSON and SSE separately.
