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
| LiteLLM | `ghcr.io/berriai/litellm@sha256:6c82d338a60e7b273ae46bf1d8db814d2856ae010f96c44eeadde574d3893f76` (tag `main-v1.83.10-stable`) — see "LiteLLM pin verification" below |
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

### LiteLLM pin verification (version / date / origin / justification)

Verified **2026-11-05** before any pilot or result, per protocol
("conferir o pin antes da coleta; decidir a versão antes dos resultados, não
após observar quem vence").

- **Selected pin**: `ghcr.io/berriai/litellm:main-v1.83.10-stable`
  → digest `sha256:6c82d338a60e7b273ae46bf1d8db814d2856ae010f96c44eeadde574d3893f76`
  (resolved and pulled locally; digest confirmed).
- **Origin**: official `ghcr.io/berriai/litellm` registry; `*-stable` release line.
- **Why changed from the previous pin**: the earlier pin
  `main-v1.65.0-stable` (digest `…d781cd3d…`) is **stale** as of the verification
  date — LiteLLM's current stable line is v1.83.x (dev builds at ~v1.109.x), and
  LiteLLM now actively supports only the four most recent stable minor lines, so
  v1.65.0 is outside the active-support window. Presenting v1.65.0 as the current
  state would be inaccurate. The previous digest still resolves and is recorded
  here for history; it is not used.
- **Why v1.83.10-stable specifically**: it is the vendor-recommended stable
  release that fixes CVE-2026-42208 (a SQL-injection in the proxy API-key path
  affecting v1.81.16–v1.83.6). It is a current, actively-supported stable chosen
  for recency and security, **not** for any performance characteristic — the
  version was fixed before running comparisons, and not selected by observing
  which gateway wins.
- **Reproduce**: `scripts/pin_images.sh` re-resolves the digest from the tag; if
  the vendor retags, update the compose files and record the new digest here with
  a new manifest. Do not silently mix results across LiteLLM versions.

> Decision flagged for the user: this block updates the LiteLLM pin to a current
> stable **before** any results. If you prefer a different stable line (e.g. the
> latest v1.x-stable at collection time), say so before the final run; the pin
> must be frozen in the final manifest either way.

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
- **Streaming parity**: the deterministic backend splits the reply on hyphens
  and sends those pieces as SSE frames. That is framing, not a tokenizer token.
  A real provider chunks differently. Compare JSON and SSE separately.

## Pilot (RQ1, deterministic) — reproduce

`harness/run_pilot_rq1.py` is the earlier sequential driver. Its `data/pilot/`
output is schema 1.0.0, historical, and not confirmatory. Comparative cells
use `harness/run_comparative_pilot.py`: each block contains direct, Melis and
LiteLLM in a seeded rotation, with warm-up separated from the measured window.

Estimate the workload before running an extended grid. The historical
sequential driver below sent 5 blocks × {json, sse} × 3 paths × 30 nested
requests = 900 requests, one gateway after another. That order is not the
comparative design. Use the balanced runner for new pilots.

```bash
# backend up; then ONE gateway at a time:
scripts/up.sh deterministic backend
python3 harness/run_pilot_rq1.py --gateway direct  --url http://127.0.0.1:7070 \
    --count-url http://127.0.0.1:7070/__count --blocks 5 --reqs-per-cell 30 \
    --concurrency 1 --modes json sse --out-dir data/pilot --phase pilot
docker compose -f docker-compose.deterministic.yml --profile melis up -d melis
python3 harness/run_pilot_rq1.py --gateway melis   --url http://127.0.0.1:7071 \
    --count-url http://127.0.0.1:7070/__count --blocks 5 --reqs-per-cell 30 \
    --concurrency 1 --modes json sse --out-dir data/pilot --phase pilot
# stop melis, start litellm, repeat with --gateway litellm --url http://127.0.0.1:4000
# then:
python3 instrumentation/validate.py --data-dir data/pilot
python3 instrumentation/aggregate.py --data-dir data/pilot \
    --runs-out data/pilot/runs.csv --comparisons-out data/pilot/comparisons.csv --seed 20261105
python3 instrumentation/validate.py --data-dir data/pilot --report data/pilot/validation_report.json
```

Comparative pilot (new directory each run; does not rewrite `data/pilot/`):

```bash
python3 harness/run_comparative_pilot.py --dry-estimate \
    --blocks 1 --modes json sse --concurrency 1 4 \
    --warmup-s 3 --measure-s 12 --backend-latency-ms 100
python3 harness/run_comparative_pilot.py \
    --out-dir data/pilot-balanced-<id> --phase pilot \
    --blocks 1 --modes json sse --concurrency 1 4 \
    --warmup-s 3 --measure-s 12 --backend-latency-ms 100 --seed 20261105
```

**Known limitations of the historical sequential pilot** (schema 1.0.0):

- That runner recorded `concurrency` while sending requests one at a time.
  `harness/conc_load.py` is a real closed loop; configured worker count and
  the backend's observed peak are different columns.
- The deterministic backend removes model/network variance, so pilot latencies
  are **mediation overhead under this exact setup**, not inference performance,
  and the tiny between-block variance does **not** by itself size the final
  sample — the real-profile and quality pilots drive that.
