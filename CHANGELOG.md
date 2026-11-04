# Changelog

All notable changes to the Melis AI Gateway are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]
### Phase 6A — Virtual models (declarative aliases)
#### Added
- `model_aliases` in `routes.yaml`: `[{name, targets: [{provider, model, weight}]}]`.
  A request `model` can be an alias name that resolves to one or more concrete
  `(provider, model)` targets. `@name` is reserved syntax that forces alias
  interpretation; `@name` and `name` are the same alias (stored canonically without
  `@`). An unknown `@name` returns 400 and is never forwarded as a concrete model; a
  bare unknown name is forwarded as a concrete model (passthrough).
- `route.default_model`: the model used only when the client sends no usable model.
  Unlike `route.model` (mandatory override), it does not shadow a client alias.
  A route may set at most one of `model` / `default_model`.
- Precedence: route `model` (override) > payload alias > concrete payload model >
  `default_model`. Multi-target aliases feed the existing weighted balancer and
  failover; each attempt forwards its target's concrete model.
- `GET /v1/models`: OpenAI-compatible listing of declared aliases and declared
  concrete models. Respects auth and filters by the client's `allowed_models`.
  Never exposes credentials, provider endpoints, weights, or the alias mapping.
- Alias authorization: `allowed_models` is checked for the requested name, the
  effective model, and every resolved alias destination — an alias cannot be used
  to reach a model the client is not allowed to use.
- Cache + effective destination: single-target aliases (and normal routes) key the
  cache by the effective destination (provider + hashed deployment + concrete
  model, namespace version 4), so a remapped alias or a different deployment never
  reuses an incompatible entry. Multi-target aliases bypass cache lookup and store
  this version (non-deterministic destination), counted by
  `melis_cache_skipped_total{reason="multi_target_alias"}`.
- Observability: `melis_model_alias_resolution_total{alias,outcome}` with a bounded
  `alias` label (declared name or `other`). Requested / resolved / effective models
  are distinguished in logs; arbitrary client strings are never used as labels.
- Response contract unchanged: the `model` field returned is the provider's value;
  no public fields were added for alias / resolved / effective model.
- Validation: ambiguous `name`/`@name`, collision with a declared concrete model,
  empty targets, `weight >= 1` and positive sum, unknown target provider, no alias
  chaining, and `model`/`default_model` mutual exclusion. Invalid config is rejected
  at boot and kept-previous on hot reload.
- Hot reload: alias resolution uses the same per-request resolver snapshot as the
  route; a reload mid-request does not mix versions.

### Security — enforce allowed_models (previously inert)
#### Fixed
- `auth.api_keys[].allowed_models` was carried into `ClientIdentity` but never
  enforced: any authenticated client could use any model. It is now enforced on the
  chat path. When a client's `allowed_models` is non-empty, both the model the
  client requested and the model effectively resolved (route override / default)
  must be permitted, otherwise the request is rejected with 403. This prevents a
  route override from silently routing a client to a model it is not allowed to use.
- An empty `allowed_models` means unrestricted, preserving the previous behavior
  exactly. Deployments that already listed models will now have that list enforced
  (the dormant field becomes active) — a security fix, documented here.
- The denial message names only the offending model, never the client's allow-list.
- Tests: unit (`authorize_models_*`) and HTTP E2E (`allowed_models_e2e`): disallowed
  requested model denied, permitted model allowed, unrestricted client unaffected,
  route override to a disallowed model denied. Failing-first verified.

### Security — semantic cache identity isolation (audit fix)
#### Changed (security-relevant default)
- New `semantic_cache.scope: per_identity | shared` per route. **Default is
  `per_identity`**: a cached response is isolated by the authenticated client
  identity, so one caller never receives another caller's cached answer.
  - `shared` (previous behavior) must now be set explicitly and means responses may
    be reused across all authenticated callers of the route — document this when
    enabling it.
  - Routes without the cache are unaffected. Routes with the cache enabled may see
    fewer hits due to isolation; this is a security fix, not a regression.
- Identity is taken from the authenticated `ClientIdentity.client_id` produced by
  the auth middleware — never a client-controlled header. A forged `X-Client-Id`/
  `X-Tenant` header does not change the scope.
- Under `per_identity`, a request with no trusted identity (e.g. auth disabled, or
  an anonymous caller) does **not** consult or populate the cache; unknown callers
  are never grouped into a shared bucket.
- The identity is folded into the cache key's `compatibility_salt` (hashed), so no
  token, API key, or identifier appears in Redis keys, logs, or metrics. The
  identity semantics are the configured `client_id` (a credential/client handle),
  documented as such; user/credential/tenant are not treated as equivalent.
- `CACHE_NAMESPACE_VERSION` bumped to 3 so pre-isolation (v2) entries are never
  reused; no destructive Redis cleanup (old namespaces age out by TTL). Lookup,
  store, streaming replay, and model-change invalidation all respect the scope.
- Tests (HTTP E2E, auth enabled): same identity can hit; different identities do not
  share in `per_identity`; a forged identity header does not change the scope; no
  trusted identity bypasses the cache; `shared` allows cross-identity reuse only
  when configured. Failing-first verified.

### Integrity — per-request guardrail configuration snapshot (audit fix)
#### Fixed
- The route/model/cache/scoring config is resolved once per request into a
  consistent `ResolvedRoute`, but compiled guardrails live in a separate `ArcSwap`
  that was read independently up to four times per request (inbound, cache-hit
  outbound, streaming setup, provider-response outbound). A hot reload landing
  mid-request could make the inbound check use one guardrail version and an
  outbound check a newer one. The route's compiled guardrails are now resolved once
  at the start of the request and reused for every inbound/outbound check, so a
  single request always sees one consistent guardrail policy. Public behavior is
  unchanged.

### Security/integrity — streaming guardrail redaction across chunk boundaries (audit fix)
#### Fixed
- Streaming outbound guardrails detected a keyword/pattern split across SSE chunks
  via the sliding window, but on a `redact` action only the current chunk's delta
  was re-scanned — so a keyword straddling two chunks was detected yet neither half
  was redacted, leaking the partial halves. The straddling chunk's content is now
  suppressed (emitted empty) when the in-delta redaction would be a no-op. `block`
  behavior is unchanged.
- Documented the streaming guardrail guarantees and limits honestly (4096-byte
  window, boundary eviction, assistant-content-only scanning, regex case folding).
  Full cross-chunk guarantees for arbitrary rules are explicitly not claimed.
- Failing-first test: `streaming_redact_does_not_leak_keyword_split_across_chunks`.

### Security/integrity — semantic cache isolation (audit fix)
#### Fixed
- The semantic cache keyed responses only by `(route, method, effective model)`,
  and the conversation embedding covers only non-system messages. As a result a
  cached response could be reused across requests that differ in their **system
  instructions**, **generation parameters** (temperature, top_p, max_tokens, stop,
  seed, response_format, penalties, ...), or **tool definitions** — returning an
  answer produced under an incompatible context.
- The cache key now folds a `compatibility_salt` into the namespace: a hash of a
  namespace version plus the system instructions (case-sensitive), the generation
  parameters, and the tools/tool_choice/functions. Any difference now lands in a
  different namespace, so incompatible responses are never reused. A different
  salt simply causes a cache miss (acceptable: a miss is preferred to an
  incompatible hit).
- Namespace versioning (`CACHE_NAMESPACE_VERSION`) is part of the salt, so future
  reuse-rule changes invalidate old entries without destructive Redis cleanup;
  old namespaces age out by TTL. Model-change cleanup now clears every salt for a
  `(route, method, model)` by glob.
- Tenant/identity is still NOT part of the key (auth identity is not currently
  threaded to the handler); cross-tenant isolation is listed as a pending decision
  in the audit, not addressed here.
- Tests: `salt_isolates_system_params_and_tools` (unit) and
  `cache_does_not_reuse_across_incompatible_requests` (HTTP E2E); both fail before
  the fix (verified).

### Test infrastructure — G1 reproducibility
#### Changed
- Live-provider integration tests (`test_json_response_with_valid_payload`,
  `test_sse_response_with_accept_header`, `test_sse_response_with_stream_true_in_payload`,
  `test_metrics_increment_after_chat_completion_request`) now target a real
  OpenAI-compatible endpoint via env, instead of sending a hosted-only model name to
  a bare Ollama:
  - `MELIS_IT_OLLAMA_URL` (default `http://localhost:11434`);
  - `MELIS_IT_CHAT_MODEL` (default `llama3.2`).
  Each test installs a route that resolves to the provider and logs the effective
  endpoint and model. No permanent `gpt-4o` alias is created.
- The cache cosine-scan latency check is now an explicit benchmark that self-skips
  unless `MELIS_RUN_BENCH=1`, so the general `cargo test -- --ignored` (G1
  functional) run no longer fails in debug. Run it with
  `MELIS_RUN_BENCH=1 cargo test --release -- --ignored latency_full_namespace_under_budget`.

### Phase 5 closure — tool-group policy and opaque-id handling
#### Added
- `token_optimization.semantic_scoring.tool_group_policy: preserve | eligible_for_removal`
  (default `preserve`). In `preserve` a complete tool-call group is never removed;
  in `eligible_for_removal` it may be removed, but only as a whole. Incomplete or
  ambiguous groups and protected instructions stay pinned under both. Preserving the
  call/result protocol dependency is not a semantic-preservation guarantee.
#### Fixed
- Opaque tool-call identifiers (e.g. `call_1`) no longer perturb scoring: the text
  used for ranking/counting excludes the ids (function name and arguments kept).
  The forwarded payload and the legacy text-only algorithm are unchanged. Diagnostic
  test: changing only the ids does not change the selection.

### Phase 5 — Semantic scoring for compaction (experimental, opt-in)
#### Added
- `token_optimization.semantic_scoring` for `semantic_guarded_trimming`.
  - Accepts `heuristic` (default), `embedding`, `hybrid`, or a block with weights,
    embedding model, limits (`timeout_ms`, `max_concurrency`, `batch_size`,
    `max_messages`, `max_chars`) and `budget`.
  - Invalid values reject `routes.yaml`, like any other validation error.
  - On other strategies the field is ignored with a warning.
- Scoring is computed in the async handler; selection stays sync and deterministic.
  - Score: `(cos+1)/2` (a scale transform, not a probability) combined with the
    normalized structural score.
  - Weights default to 0.5/0.5 (provisional, not calibrated) and are normalized.
  - Ties: oldest message removed first. Original order is restored.
  - Kept messages are the original objects, never rewritten.
- Tool calls (OpenAI-compatible only): `assistant.tool_calls` plus all of its
  results form one indivisible group. Incomplete or ambiguous dependencies are
  pinned.
- Full fallback to the legacy D1 path on error, timeout, invalid vectors, too many
  candidates (no prefix/suffix subsets), or no user message.
- `budget.mode: strict` (opt-in):
  - `max_context_tokens`, `output_reserve_tokens`, and
    `formatting_overhead: openai_chat_estimate` (an estimate).
  - When mandatory content can't fit, returns 400 "Insufficient context budget",
    with no embedding call and no provider call.
  - `legacy` budget semantics are unchanged.
- `EmbeddingProvider::embed_batch`: the default is bounded concurrency. The Ollama
  override chunks `/api/embed` and falls back to `/api/embeddings` on 404.
- Metrics:
  - `melis_compaction_scoring_total{requested,effective}`;
  - `melis_compaction_scoring_seconds`;
  - `melis_compaction_scoring_fallback_total{reason}`;
  - `melis_compaction_scoring_skipped_total{reason}`;
  - `melis_compaction_budget_insufficient_total`.

  Logs contain modes, counts and reasons only.
- Offline harness (`semantic_scoring::eval`): compares three modes on the same
  budget.
  - The report records commit/dirty state, dataset hash, provider, model, weights
    and configuration.
  - `dev`/`eval` splits are kept separate; the eval split requires
    `MELIS_EVAL_CONFIRM_FINAL=1`.
  - The bundled fixtures are synthetic.
- Versioned spec: `docs/specs/phase-5-semantic-scoring.md`.
#### Notes
- No behavior change without opt-in: no new embedding calls, same selection and D1
  preservation.
- With the semantic cache on the same route, scored requests make additional
  embedding calls (different texts, no reuse).
- GAP-OAI-1 unchanged: HTTP still rejects `content: null`/arrays. Those shapes are
  covered internally only.
- Observation: in `hybrid`, structural signals can still outrank similarity at
  0.5/0.5 depending on content. Tool-call ids no longer contribute (see the closure
  entry above). This is an input for the experiment, not a calibrated result.
### Fixed — Context compaction corrupted messages (bugfix D1)
- **Before:** when compaction fired (`adaptive_trimming`, `sliding_window`,
  `semantic_guarded_trimming`), the handler rebuilt every kept message as
  `{"role", "content"}`. As a result:
  - `tool_calls`, `tool_call_id`, `name`, `function_call` and any unknown fields
    were silently dropped;
  - a `role: tool` result could be pruned while its `assistant.tool_calls` was
    kept, or the reverse;
  - messages without a string `role`/`content` disappeared.
  Payloads were only intact when compaction did not fire.
- **Now:**
  - The gateway forwards clones of the original message objects, with all fields
    and in the original order.
  - Text is extracted separately and used only for scoring and token counting.
  - When stop-word removal shortens a kept message, only its `content` string
    changes.
  - Messages the text compactors can't handle safely are pinned: always kept,
    verbatim, still counted toward the budget. Pinned cases: non-string or null
    `content`, `tool_calls`, `tool_call_id`, `function_call`, role
    `tool`/`function`, no string `role`.
  - Pinned messages are never dropped by the `max_history_messages` window and
    don't count toward its limit.
  - Tool-call grouping (pruning a call together with its results) is not part of
    this fix.
- `tool_calls` / `function_call` payloads now count toward the token total of their
  message (they are real prompt tokens and are now forwarded).
- **Unchanged:** text-only conversations (`{role, content: string}` without extra
  fields) produce the same forwarded payload and metrics as before. This is
  verified by a property test against a verbatim copy of the pre-fix algorithms.
- `CompactionResult` gains `kept_indices`; `ContextCompactor` gains
  `compact_with_pins` (`compact` = no pins, unchanged).
- Known gap, not addressed here (GAP-OAI-1): request validation still rejects
  `content: null` (valid in OpenAI for `assistant` with `tool_calls`) and array
  (multimodal) `content` with 400, so those shapes never reach compaction via
  HTTP. The adapter handles them for robustness.

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
