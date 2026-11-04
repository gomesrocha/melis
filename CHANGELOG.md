# Changelog

All notable changes to the Melis AI Gateway are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Phase 4 — Shared Redis Circuit Breaker

#### Added

- **Shared Redis client** (`src/redis_client.rs`, `SharedRedis`): a single fred pool
  built once at boot from `config.redis`, reused by the semantic cache and the
  circuit breaker. Fail-open — if Redis can't be reached, components degrade and the
  gateway still boots. Closes the Phase 2 debt of each component opening its own
  connection; `redis_available` (previously hardcoded) now reflects a real PING.
- **`RedisCircuitBreaker`** with state shared across replicas, full semantic parity
  with the local breaker (429 opens immediately; failure-rate threshold with minimum
  request count; single-probe half-open with exponential backoff up to `max_ttl`).
  - State encoded in keys: `:state` (Open), `:probe` (half-open probe in flight),
    both absent (Closed); plus `:failures`, `:requests`, `:backoff`.
  - All transitions are atomic via Lua using the Redis server `TIME` (no replica
    clock skew). The half-open probe is claimed with `SET NX EX` so exactly one
    replica probes.
  - `is_available` uses a short-TTL local read-through cache
    (`circuit_breaker.redis_state_cache_ms`, default 150ms) to avoid a Redis
    round-trip per request; a locally observed 429 invalidates the local copy
    immediately (write-through).
  - Fail-open on Redis errors (never blocks all traffic) with a warning and
    `melis_circuit_breaker_degraded`.
- **Backend selection**: `circuit_breaker.backend: local | redis` (default `local`,
  backward compatible). If `redis` is selected but Redis is unavailable, falls back
  to the local breaker with a warning.
- **Injectable embeddings** (`EmbeddingProvider` trait + `OllamaEmbeddingProvider`
  default): decouples the cache's embedding step from a live Ollama, enabling
  deterministic end-to-end tests. The two Phase 3 cache×guardrail scenarios
  (redact feeds the cache key; cache hit passes the outbound guardrail) are now real
  end-to-end HTTP tests. Closes the Phase 2 follow-up: `melis_cache_embed_seconds`
  measures the real embedding path.
- **New metrics**: `melis_circuit_breaker_backend{backend}`,
  `melis_circuit_breaker_degraded`, `melis_cache_embed_seconds`.
- Documentation: `config.yaml.example`, README "Shared Circuit Breaker" section and
  metrics reference (including the transient per-replica divergence note).

#### Notes

- Backward compatible: default `backend: local` preserves current behavior; new
  config fields are optional.
- Redis-dependent tests are `#[ignore]` and run in the pre-merge verification (G1)
  against a live Redis, alongside the Phase 1 Ollama tests.

#### Fixed (Phase 4 follow-up, found by running against a real Redis)

- **Boot hang:** `SharedRedis::connect` awaited the connection-driver handle, which
  only resolves when the connection closes, so boot blocked whenever Redis *was*
  reachable. Now waits only for the first connection, bounded by
  `redis.connect_timeout_secs`.
- **Closed vs half-open ambiguity:** an expired open window looked identical to a
  healthy circuit. `:backoff` is now a persistent cycle marker (no TTL, deleted only
  by a successful probe); expiry of `:state` never means closed.
- **Probe grant cached locally:** a granted probe could be served from the local
  cache as "available", letting a window of traffic through. Grants are no longer
  cached.
- **Late probe completion:** probes now carry an identity token; a completion that
  isn't the current probe is rejected atomically and cannot close a newer round.
  Results from requests admitted before the circuit opened no longer alter state.
- **Traffic leak when Redis was slow/unreachable during half-open:** fail-open
  admitted requests mid-cycle. A replica that saw the circuit open/half-open now
  keeps denying (bounded) while Redis is unreadable; others still fail open.
- **Inconsistent degradation policy:** previously local fallback at boot but
  fail-open at runtime. Now `backend: redis` stays `redis` in both cases, Redis is
  reconnected in the background and picked up again without a restart. Local
  fallback only when no usable Redis URL is configured.
- Redis calls from the breaker and semantic cache are bounded (100ms) and skipped
  for 1s after a failure.

#### Added (follow-up)

- `CircuitBreaker::admit` / `record_outcome` with `Admission { Allow, Probe(token),
  Deny }` (default implementations keep the local breaker unchanged).
- `melis_redis_up` gauge; `/readyz` now reflects a periodic real PING.
- 18 real-Redis integration tests (concurrency on one and two replicas, late probe,
  healthy-vs-half-open, abandoned probe, degradation at boot / at runtime /
  recovery via a controllable TCP proxy, parity).
#### Changed (follow-up 2 — outage semantics and readiness)
- **`/readyz` no longer fails on a Redis outage by default.** New
  `redis.required_for_readiness` (default `false`): Redis is an optional dependency,
  and `/readyz` answers 200 `{"status":"ready","degraded":["redis"]}` while it is
  unreachable, so an outage doesn't pull every replica out of rotation. Set `true`
  to keep the 503 behavior. `/healthz` (liveness) never depends on Redis.
- **Redis-outage denials are distinguishable from provider-failure circuits:** new
  `Admission::DenyUnverified`; failover/fallback metrics use
  `reason="breaker_state_unavailable"` instead of `circuit_open`; warn log on each
  such denial.
- New metric `melis_circuit_breaker_degraded_decisions_total{decision=allow|deny|write_skipped}`
  (`melis_circuit_breaker_degraded` kept).
- Documented the bounded protection horizon (`max_ttl_secs + probe_lease` on read,
  `max_ttl_secs + open_ttl_secs` when the replica itself opened it), automatic
  reconciliation when Redis returns, and that this is local, conservative protection,
  not a global single-probe guarantee during a partition.
- 2 more real-Redis tests (reconciliation after another replica closed the circuit
  during the outage; protection expiry after the horizon): 20 in total.

### Phase 3 — Bidirectional Guardrails

#### Added

- **Opt-in per-route guardrails.** New `guardrails` block in `routes.yaml` with a
  list of `rules` (`type`: `regex` | `keyword` | `prompt_injection_heuristic`;
  `direction`: `inbound` | `outbound` | `both`; `action`: `block` | `redact` |
  `warn`) and an optional `fail_closed`. Optional, with safe defaults — existing
  routes are unaffected.
- **`src/guardrails.rs`**: compiled matchers (`CompiledRule`/`Matcher`),
  `CompiledGuardrails` with `block > redact > warn` precedence, a `GuardrailEngine`
  (per-route compiled map) recompiled at boot and on every hot reload, and
  `apply_outbound_to_response` for non-streaming/cache-hit delivery.
- **Inbound enforcement** (handler, after auth + rate limit, before cache and
  compaction): `block` → HTTP 400 without touching the provider or cache; `redact`
  → rewrites the payload so the redacted content feeds the cache key and the
  provider; `warn` → log + metric.
- **Outbound enforcement**: applied to every response before delivery, including
  cache hits (reuses the Phase 2 `OutboundGuardrail` hook point). Streaming uses a
  sliding-window buffer in `transform_sse_stream_guarded` — evaluates accumulated
  text before emitting each chunk (≤ 1 chunk latency); a block ends the stream with
  an SSE error event without leaking content; redact rewrites the chunk delta.
- **Safe regex engine**: rules use the `regex` crate (linear-time automaton, no
  catastrophic backtracking). Look-around/backreferences unsupported by design.
  Invalid patterns are dropped at load/reload; hot reload keeps the previous config.
- **`fail_closed`** resolvable per route with a global default in `config.yaml`
  (`guardrails.fail_closed`, default `false`).
- **New metric** `melis_guardrail_actions_total{rule,direction,action}` plus a
  structured log per action.
- Documentation: `routes.yaml.example`, `config.yaml.example`, README "Guardrails"
  section and metrics reference.

#### Notes

- Backward compatible: routes without `guardrails` behave exactly as before.
  `transform_sse_stream` is preserved as a passthrough wrapper over the new
  guardrail-aware variant.

### Phase 2 — Semantic Response Cache

#### Added

- **Opt-in per-route semantic cache.** New `semantic_cache` block in `routes.yaml`
  (`enabled`, `similarity_threshold`, `ttl_secs`, `max_entries`, `embedding_model`),
  all optional with safe defaults — existing routes are unaffected. Responses are
  cached by `(route + model + normalized-conversation embedding)` and returned for
  semantically-equivalent requests without calling the provider.
- **`src/semantic_cache.rs`**: `SemanticCache` trait, `RedisSemanticCache` (fred),
  `DisabledCache`, conversation normalization, cosine similarity, and an
  `OutboundGuardrail` hook (`NoopOutboundGuardrail`) reserved for Phase 3 output
  filtering — cache hits already pass through this hook.
- **Batch fetch:** namespace candidates are read in a single Redis round-trip (Lua),
  never one round-trip per candidate. Lookup p95 stays < 5 ms at the default
  1000-entry capacity.
- **Streaming replay:** cache hits for streaming requests are replayed as an
  OpenAI-compatible SSE stream.
- **New metrics:** `melis_cache_hits_total`, `melis_cache_misses_total`,
  `melis_cache_lookup_seconds`, `melis_cache_entry_age_seconds`,
  `melis_cache_degraded{reason}` and `melis_cache_entries{namespace}` (gauge).
- **`embeddings::embed_single`**: reusable single-text embedding helper (Ollama).
- Model-change hook on hot reload (`RouteConfigManager`) triggers best-effort
  background cleanup (`SCAN`+`DEL`) of the old namespace.
- Documentation: `routes.yaml.example`, README "Semantic Cache" section and metrics
  reference.

#### Behavior / Design Notes

- **Fail-open everywhere:** if Redis or the embeddings backend is unavailable, the
  cache degrades to a miss/no-op (warning + `melis_cache_degraded`); the request
  proceeds normally. The cache never returns an error to the client.
- **Threshold is a query parameter** (applied at lookup), not part of the key;
  changing it only affects the hit rate, requiring no invalidation.
- **TTL is mandatory on every entry;** model invalidation is implicit by namespace
  (the model is part of the key) plus background cleanup. Isolation is guaranteed:
  the same conversation on a different route/model is a distinct entry.
- **Backward compatible:** routes without `semantic_cache` behave exactly as before
  (a single boolean check on the critical path). No new mandatory config.

#### Out of Scope (tracked as future work)

- Multi-turn incremental/prefix caching and real LSH bucketing.
- Centralizing a real Redis connection in `main.rs` and removing the hardcoded
  `redis_available` flag.

### Phase 1 — Real BPE Token Counting

#### Added

- **Opt-in real BPE token counting.** New `compactor.token_counting` setting in
  `config.yaml` with values `estimated` (default) and `real`. In `real` mode the
  compactor counts tokens with a real tiktoken BPE encoding selected by
  `compactor.tokenizer_name` (supported: `cl100k_base`, `o200k_base`).
- **`tiktoken-rs`** dependency for real token counting. Encodings are cached as
  process-wide singletons, so no vocabulary is loaded per request.
- **`melis_token_counting_mode`** Prometheus counter with label `mode`
  (`real` | `estimated`). Both series are present from boot so dashboards can tell
  which base the token numbers were computed on.
- **Synthetic `usage` fill.** In `real` mode, when a provider omits `usage` (or
  returns zeros), the gateway fills `usage.prompt_tokens` / `completion_tokens` /
  `total_tokens` with real counts (prompt from the request messages, completion
  from the response content). Provider-supplied non-zero values are preserved.
- **Startup discoverability log.** If `compactor.tokenizer_name` is explicitly set
  while `token_counting` is still `estimated`, Melis logs a one-line `info`
  suggesting `token_counting: real`.
- Documentation: `config.yaml.example`, README "Token Counting" section and metrics
  reference.

#### Changed

- **`src/compactor.rs`** now routes all counting through a `TokenCounter`
  (`SimpleCompactor::count_tokens_with` / `total_tokens_with`). The legacy
  `count_tokens` / `total_tokens` remain as wrappers delegating to the estimated
  strategy, preserving behavior bit-for-bit.

#### Removed

- Unused `tokenizers` (HuggingFace) dependency, which was declared but never used.
  Replaced by `tiktoken-rs`, which matches the project's OpenAI-compatible
  encodings and avoids runtime file/network access.

#### Notes / Migration

- **Backward compatible by default.** Configs without `token_counting` continue to
  use the `estimated` approximation (`len / 4`), identical to previous behavior.
  `tokenizer_name` alone does **not** enable real counting.
- **Behavior change when opting in:** switching to `token_counting: real` changes
  the token numbers that drive compaction thresholds (`compress_above_tokens`) and
  the `usage` / token metrics. Real counts differ from the approximation, so
  compaction may trigger at different points. This is intentional and documented.
