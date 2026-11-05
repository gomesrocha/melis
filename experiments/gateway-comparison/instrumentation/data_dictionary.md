# Data dictionary — gateway-comparison raw CSVs

Schema version: **1.1.0** (see `SCHEMA_VERSION`; every row carries
`schema_version`). Authoritative column definitions live in `schemas.py`; this
document is the human-readable companion and the protocol→schema traceability.

## Conventions (apply to every table)

- **Encoding / format**: UTF-8, comma separator, `.` decimal, one stable header
  row. Text cells are quoted/escaped by the CSV writer.
- **Missing ≠ zero**: an unobservable or not-applicable value is an **empty
  cell**, never `0`. A genuine measured zero is written as `0`. Required columns
  may never be empty.
- **Units are explicit in the column name**: `_ms` = milliseconds, `_s` =
  seconds, `_bytes` = bytes, `_pct` = percent, `_rps` = requests per second.
- **Time**: `*_at_utc` / `*_utc` are ISO-8601 UTC (e.g.
  `2026-11-05T12:34:56.789Z`). Durations are measured with a monotonic clock.
- **Booleans**: literal `true` / `false`.
- **Phase**: `smoke | pilot | final`. Phases live in **separate directories**
  (`../data/smoke`, `../data/pilot`, `../data/final`) and are never mixed.
- **Independence**: a *block* (performance) / *episode* (resilience) / *case*
  (quality) is an independent unit. Requests within a run, and generations
  within a case, are **nested** observations, not independent repetitions.

## Stable identifiers (`ids.py`)

| Id | Scope | Pairing property |
|----|-------|------------------|
| `experiment_id` | experiment definition (name + protocol + schema) | — |
| `block_id` | one independent repetition unit | — |
| `pair_id` | one (block, case, scenario, streaming, concurrency) cell | **gateway-independent**: identical across direct/melis/litellm |
| `run_id` | one gateway running one cell | — |
| `request_id` | one logical request | nested under run + pair |
| `case_id` | one corpus item (grouped by family) | — |
| `episode_id` | one resilience episode | — |
| `answer_id` | one generation of one case under one policy | links `quality.csv` to the answer JSONL |

Ids are deterministic functions of their inputs (SHA-256 short hashes), so
reruns and third-party reproductions align.

## Tables

### `requests.csv` — one row per logical request (incl. failures)

Retries do **not** add rows here (they go to `attempts.csv`). Failures,
timeouts and invalid responses each produce a row.

| Column | Kind/Unit | Required | Notes |
|--------|-----------|----------|-------|
| schema_version | str | yes | matches `SCHEMA_VERSION` |
| experiment_id, phase, run_id, block_id, pair_id, case_id, request_id | str/enum | yes | keys & grouping |
| gateway | enum (direct/melis/litellm) | yes | |
| scenario | str | yes | scenario label |
| backend_profile | enum (deterministic/real) | yes | |
| streaming | bool | yes | JSON vs SSE |
| concurrency | int | yes | |
| policy | enum | no | context policy; empty when N/A |
| started_at_utc | ts | yes | |
| scheduled_delay_ms | ms | no | open-load scheduling delay; empty under closed load |
| latency_ms | ms | no | end-to-end to completion |
| ttfb_ms | ms | no | **time to first HTTP byte** |
| first_content_ms | ms | no | **time to first SSE content event** — distinct from `ttfb_ms` and `latency_ms` |
| status | enum (ok/http_error/transport_error/invalid_response/timeout) | yes | |
| http_status | int | no | empty for transport errors |
| error_class | str | no | empty on success |
| timeout | bool | yes | hit the deadline |
| completion_valid | bool | yes | a fast but invalid reply is **not** success |
| backend_call_count | int | no | deterministic `/__count` only; empty on real backend |
| cache_result | enum (hit/miss/degraded/bypass/disabled) | no | empty when cache off |
| requested_model / resolved_model / effective_model | str | no | model at each stage |
| input_tokens_before / input_tokens_after / output_tokens | int | no | token counts |
| token_count_source | enum (provider/gateway_estimate/local_tokenizer) | no | local estimates are **not** billed tokens |
| scoring_ms | ms | no | part of `latency_ms` |
| compaction_ms | ms | no | part of `latency_ms` |

> **TTFB vs first content vs completion** are three distinct measures. We do not
> call an SSE chunk a "token": `output_tokens` is only populated when tokens are
> actually observed (e.g. from provider usage), with `token_count_source`
> recording the origin.

### `attempts.csv` — one row per observable attempt

Keyed to `requests.csv` by `(run_id, request_id)`. `destination_id` is
pseudonymized (never a secret/URL with credentials). If the external
instrumentation cannot distinguish attempts, record only an aggregate count and
state the limitation in the manifest.

### `runs.csv` — one row per measured run (**derived**)

Produced by `aggregate.py` from `requests.csv`; raw data is never overwritten.
Holds factors, seeds, order-in-block, warm-up, duration, counts, useful
throughput, median/p95/p99, model, and artifact/config/dataset hashes. `p99` is
emitted only when volume plausibly supports it (≥100 ok requests); otherwise
empty.

### `resources.csv` — time-sampled resource usage

One row per `(run_id, component, sample_index)`. Components: `gateway`,
`embeddings`, `redis`, `backend`, `host`, `other`. Unavailable metrics stay
empty. Peak RAM / mean CPU are derived downstream with documented denominators.

Schema 1.1.0 separates three memory definitions. They are not interchangeable
and must not be compared across gateways inside one column:

| Column | Definition | When empty |
|---|---|---|
| `memory_cgroup_current_bytes` | Docker Engine `memory_stats.usage`, which is cgroup `memory.current` (includes cache) | stats read failed |
| `memory_working_set_bytes` | Kubernetes working set: `usage - inactive_file` | `inactive_file` absent |
| `memory_rss_bytes` | `memory_stats.stats.rss` only | the engine has no `rss` field. cgroup v2 does not; `anon` is not written here |

`cpu_seconds_total` is the Docker counter `cpu_stats.cpu_usage.total_usage`
converted from nanoseconds to seconds (`cpu_method`). A CPU percentage is not
converted into this counter. The same Engine API is used for every container,
including images without a shell. Host CPU, when sampled, uses `/proc/stat`
non-idle jiffies (`source=procfs`) and does not fill the memory columns.

Each row records `source`, the method string for each measure that was
actually present, `collection_started_at_utc`, `collection_duration_ms`,
`configured_interval_s`, `effective_interval_s` and `sample_lag_s`.
`timestamp_utc` is the engine `read` time of that container, not one clock
reading stamped onto every container in the batch. The first sample of a
`(run, component)` leaves the effective interval and lag empty.

Container samples come from a Docker Engine stats **stream** (one Unix-socket
connection per container, not `docker exec`). The daemon emits about one
object per second. A configured interval shorter than that is not claimed;
`effective_interval_s` and `sample_lag_s` record the cadence that actually
happened. `collection_duration_ms` is empty for streamed samples because the
one-second wait is the engine period, already recorded as the effective
interval, not a separate per-read cost.

### `execution_facts.csv` — one row per execution window

Raw cell bookkeeping that must not be copied onto every request: order in the
block, warm-up duration, measured duration, configured concurrency,
`backend_peak_inflight` (observed after a per-cell reset) and
`backend_call_count`. The call count is an execution-level counter delta
(`count_scope=execution`). A delta that goes backwards is left empty. It is
not divided into per-request counts. `requests.backend_call_count` stays empty
unless a request-scoped observation exists.

### Analysis identity

`comparisons.analysis_id` is `agg-<revision>-<sha256 prefix of aggregate.py>`.
`evidence_role` is `exploratory` for smoke and pilot. A confirmatory label
requires phase `final` and an explicit flag. Pilot intervals, including a
three-block pilot, are not confirmatory evidence. Derived files are written to
new paths; an existing `comparisons.csv` is not replaced.

### `quality.csv` — one row per evaluation

`generation_rep` and multiple `evaluator_id` are nested observations; they do
not inflate the independent-case count. `score` empty when not scored.
`blinded` records whether the evaluator was blind to the policy.

### `resilience_events.csv` — fault/recovery events

One row per scheduled/observed event within an `episode_id`. `relative_ms` is
measured from episode start. `event_kind` = scheduled|observed.

### `safety_cases.csv` — security/cache labelled cases

Expected vs observed label, pseudonymized identity, cache `hit_category`,
`leak_detected`, and `detection_action`. `pass_fail` is a **contract** result,
not a p-value. **No sensitive content is stored here** — prompts/answers live in
separate JSONL keyed by id.

### `comparisons.csv` — derived results (**never** raw)

One row per contrast. `independent_units` names the unit of `n`
(block/episode/case) and `n` counts **independent** units, not nested requests.
Inferential rows carry `estimate`, CI bounds, `method`, `margin`, `decision`,
and (when a pre-registered test applies) `p_raw`/`p_adjusted`. **Contract rows**
carry `expected`/`observed`/`pass_fail` with empty p-values — we do not
fabricate inferential statistics for contract checks.

## Protocol → schema traceability

The protocol (`PROTOCOL.md`) fixes the canonical column lists. All names are
implemented verbatim. Adaptations, where any, are listed below (semantics and
pairing preserved):

| Protocol wording | Schema column(s) | Adaptation |
|------------------|------------------|------------|
| "throughput útil" | `useful_throughput_rps` | unit suffix made explicit (`_rps`) |
| "mediana, p95, p99" (runs) | `latency_median_ms`, `latency_p95_ms`, `latency_p99_ms` | unit suffix `_ms` added |
| "tempo até primeiro evento de conteúdo" | `first_content_ms` | named measure, distinct from `ttfb_ms` |
| "origem das medidas" | `source`, `*_method` columns, manifest `measurement_origin` | per-metric method on the row; manifest repeats the human-readable definition |
| memory RSS vs cgroup usage | `memory_rss_bytes` and `memory_cgroup_current_bytes` | protocol listed RSS; 1.1.0 adds the cgroup column instead of overloading RSS |
| warm-up vs measured window | `window` | appended so warm-up rows stay raw without entering the measured contrast |
| feature profile | `feature_profile` | stratifies minimal mediation from cache/compaction/guardrails |
| analysis code | `analysis_id`, `evidence_role` | appended on derived `comparisons.csv` |
| execution-level backend count | `execution_facts.backend_call_count` | not fanned out to `requests.backend_call_count` |
| "destinos pseudonimizados" | `destination_id`, `identity_pseudonym` | pseudonyms only; no secrets |
| contract tests "esperado/observado/pass-fail" | `comparisons.expected/observed/pass_fail`, `safety_cases.pass_fail` | p-values intentionally empty |
| "chamadas ao backend" | `requests.backend_call_count` when request-scoped; otherwise `execution_facts.backend_call_count` | a process-wide counter is not split across concurrent requests |

## Schema history

| Version | Change |
|---|---|
| 1.0.0 | Initial protocol columns. Historical smoke/pilot files stay at this version and are not rewritten. |
| 1.1.0 | Separate cgroup memory, working set and RSS; per-sample timing and method; stratum fields; execution facts; analysis id. |

Any future change to a column, unit, type, or enum **bumps `SCHEMA_VERSION`**
and is recorded here and in the git history. Data written under an older schema
version is kept as-is and never silently mixed with a newer version.
