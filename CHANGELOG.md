# Changelog

All notable changes to the Melis AI Gateway are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

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
