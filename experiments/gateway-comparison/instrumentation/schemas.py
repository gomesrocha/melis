#!/usr/bin/env python3
"""Versioned CSV schemas for the gateway-comparison experiment.

Single source of truth for every raw-data CSV: column order, type, unit,
required-ness, and allowed enum values. The protocol (``PROTOCOL.md``) fixes
the column names; this module encodes them so the writer, validator, and
aggregation scripts cannot drift apart.

Stdlib only — importable under Python 3.12 (the pinned backend image) and 3.14.

Schema versioning
-----------------
``SCHEMA_VERSION`` below MUST match the ``SCHEMA_VERSION`` file next to this
module. Every raw row carries a ``schema_version`` column so that data from
different schema revisions is never silently mixed. Bump the version on ANY
change to a column set, type, or unit, and record the change in
``data_dictionary.md``.

Missing vs zero
---------------
A missing/unobservable value is the empty string in the CSV — never ``0``.
Only ``REQUIRED`` columns may not be empty. ``statistic`` or ``count`` columns
that are genuinely zero are written as ``0``; absence is distinct.
"""
from __future__ import annotations

import os
from dataclasses import dataclass
from typing import Optional

SCHEMA_VERSION = "1.2.0"


def _read_version_file() -> Optional[str]:
    path = os.path.join(os.path.dirname(__file__), "SCHEMA_VERSION")
    try:
        with open(path, encoding="utf-8") as fh:
            return fh.read().strip()
    except OSError:
        return None


# Fail fast if the constant and the file ever diverge.
_FILE_VERSION = _read_version_file()
if _FILE_VERSION is not None and _FILE_VERSION != SCHEMA_VERSION:
    raise RuntimeError(
        f"SCHEMA_VERSION mismatch: schemas.py={SCHEMA_VERSION!r} "
        f"SCHEMA_VERSION file={_FILE_VERSION!r}"
    )


# Column data kinds. These describe how the *validator* interprets a cell; the
# writer stores everything as text. "duration_ms"/"duration_s"/"bytes" etc. are
# numeric kinds whose UNIT is encoded in the column name per the protocol.
STR = "str"
INT = "int"
FLOAT = "float"
BOOL = "bool"  # canonical values: "true"/"false"
ENUM = "enum"
TS_UTC = "timestamp_utc"  # ISO-8601 UTC, e.g. 2026-11-05T12:34:56.789Z


@dataclass(frozen=True)
class Column:
    name: str
    kind: str
    required: bool = False
    unit: Optional[str] = None
    enum: Optional[tuple] = None
    doc: str = ""


PHASES = ("smoke", "pilot", "final")
GATEWAYS = ("direct", "melis", "litellm")
SCENARIOS_HINT = (
    # Not enforced as an enum (scenarios grow per RQ); documented for readers.
    "healthy",
    "rq1_fixed_load",
    "rq1_saturation",
    "rq3_429_retry_after",
    "rq3_5xx_transient",
    "rq3_timeout",
    "rq3_primary_down_fallback",
    "rq3_primary_recovers",
    "rq3_two_replicas",
    "rq3_redis_down_recovered",
    "rq3_no_healthy_fallback",
    "rq4_exact_repeat",
    "rq4_paraphrase_eligible",
    "rq4_hard_negative",
    "rq4_ttl",
    "safety_guardrail_case",
)
STATUS_VALUES = ("ok", "http_error", "transport_error", "invalid_response", "timeout")
CACHE_RESULTS = ("hit", "miss", "degraded", "bypass", "disabled")
TOKEN_SOURCES = ("provider", "gateway_estimate", "local_tokenizer")
POLICIES = (
    "none",
    "chronological",
    "heuristic",
    "embedding",
    "hybrid",
)
EVALUATOR_TYPES = ("human", "llm_judge", "automatic")
WINDOWS = ("warmup", "measured")
EVIDENCE_ROLES = ("exploratory", "confirmatory")
COUNT_SCOPES = ("execution",)
TERMINATION_REASONS = ("deadline", "safety_cap", "request_cap", "safety_stop")

# ── requests.csv ────────────────────────────────────────────────────────────
# One row per LOGICAL request (including failures). Retries do NOT duplicate the
# logical request here; they go to attempts.csv.
REQUESTS = (
    Column("schema_version", STR, required=True, doc="Schema version of this row."),
    Column("experiment_id", STR, required=True,
           doc="Stable experiment definition (name + protocol). Not unique per collection."),
    Column("collection_id", STR, required=True,
           doc="Unique identity of one collection. Reprocessing that collection keeps it; a new collection does not."),
    Column("phase", ENUM, required=True, enum=PHASES, doc="smoke|pilot|final."),
    Column("run_id", STR, required=True, doc="Measured run (see ids.py)."),
    Column("block_id", STR, required=True, doc="Independent repetition unit."),
    Column("pair_id", STR, required=True, doc="Pairing key across gateways."),
    Column("case_id", STR, required=True, doc="Logical case/corpus item."),
    Column("request_id", STR, required=True, doc="Unique logical request id."),
    Column("gateway", ENUM, required=True, enum=GATEWAYS),
    Column("scenario", STR, required=True, doc="Scenario label (see SCENARIOS_HINT)."),
    Column("backend_profile", ENUM, required=True, enum=("deterministic", "real")),
    Column("streaming", BOOL, required=True),
    Column("concurrency", INT, required=True),
    Column("policy", ENUM, required=False, enum=POLICIES, doc="Context policy; empty when N/A."),
    Column("started_at_utc", TS_UTC, required=True),
    Column("scheduled_delay_ms", FLOAT, unit="ms",
           doc="Open-load scheduling delay (scheduled vs effective send). Empty under closed load."),
    Column("latency_ms", FLOAT, unit="ms", doc="End-to-end wall time to completion."),
    Column("ttfb_ms", FLOAT, unit="ms", doc="Time to first HTTP byte (streaming)."),
    Column("first_content_ms", FLOAT, unit="ms",
           doc="Time to first SSE CONTENT event; distinct from ttfb_ms and latency_ms."),
    Column("status", ENUM, required=True, enum=STATUS_VALUES),
    Column("http_status", INT, doc="HTTP code if an HTTP response arrived; empty for transport errors."),
    Column("error_class", STR, doc="Error taxonomy label; empty on success."),
    Column("timeout", BOOL, required=True, doc="Whether the logical request hit its deadline."),
    Column("completion_valid", BOOL, required=True,
           doc="Response reconstructed and structurally valid (a fast invalid reply is NOT success)."),
    Column("backend_call_count", INT,
           doc="Backend-side calls for this request (deterministic /__count only); empty otherwise."),
    Column("cache_result", ENUM, enum=CACHE_RESULTS, doc="Cache outcome; empty when cache off/not applicable."),
    Column("requested_model", STR, doc="Model the client requested."),
    Column("resolved_model", STR, doc="Model after alias/route resolution."),
    Column("effective_model", STR, doc="Model actually forwarded to the backend."),
    Column("input_tokens_before", INT, doc="Input tokens before compaction."),
    Column("input_tokens_after", INT, doc="Input tokens after compaction."),
    Column("output_tokens", INT, doc="Output tokens."),
    Column("token_count_source", ENUM, enum=TOKEN_SOURCES,
           doc="Origin of token counts; local estimates are NOT billed tokens."),
    Column("scoring_ms", FLOAT, unit="ms", doc="Semantic scoring time (part of latency_ms)."),
    Column("compaction_ms", FLOAT, unit="ms", doc="Compaction time (part of latency_ms)."),
    Column("feature_profile", STR,
           doc="Feature/policy profile of this cell (e.g. minimal). Empty is its own stratum, not 'minimal'."),
    Column("window", ENUM, enum=WINDOWS,
           doc="warmup requests are raw but excluded from measured contrasts; empty means measured."),
)

# ── attempts.csv ──────────────────────────────────────────────────────────────
# One row per OBSERVABLE attempt. Keyed to a requests.csv request_id.
ATTEMPTS = (
    Column("schema_version", STR, required=True),
    Column("collection_id", STR, required=True),
    Column("run_id", STR, required=True),
    Column("request_id", STR, required=True),
    Column("attempt_index", INT, required=True, doc="0-based attempt ordinal within the request."),
    Column("destination_id", STR, required=True, doc="Pseudonymized destination; never a secret."),
    Column("started_at_utc", TS_UTC, required=True),
    Column("latency_ms", FLOAT, unit="ms"),
    Column("status", ENUM, required=True, enum=STATUS_VALUES),
    Column("http_status", INT),
    Column("is_probe", BOOL, required=True, doc="Circuit-breaker half-open probe."),
    Column("is_fallback", BOOL, required=True, doc="Attempt was a fallback destination."),
)

# ── runs.csv ────────────────────────────────────────────────────────────────
# One row per measured run. Summaries are DERIVED from requests.csv by
# aggregate.py; raw data is never overwritten.
RUNS = (
    Column("schema_version", STR, required=True),
    Column("experiment_id", STR, required=True),
    Column("collection_id", STR, required=True),
    Column("phase", ENUM, required=True, enum=PHASES),
    Column("run_id", STR, required=True),
    Column("block_id", STR, required=True),
    Column("gateway", ENUM, required=True, enum=GATEWAYS),
    Column("scenario", STR, required=True),
    Column("backend_profile", ENUM, required=True, enum=("deterministic", "real")),
    Column("streaming", BOOL, required=True),
    Column("concurrency", INT, required=True),
    Column("policy", ENUM, enum=POLICIES),
    Column("seed", STR, doc="Seed(s) fixing scheduling/corpus order for this run."),
    Column("order_in_block", INT, doc="Randomized order of this gateway within the block."),
    Column("warmup_s", FLOAT, unit="s"),
    Column("duration_s", FLOAT, unit="s",
           doc="Effective wall time, including drain of in-flight requests. Not the configured duration."),
    Column("requested_duration_s", FLOAT, unit="s",
           doc="Configured window. Empty when the cell was count-capped on purpose."),
    Column("termination_reason", ENUM, enum=TERMINATION_REASONS,
           doc="Why admission stopped: deadline, safety_cap, request_cap, or safety_stop."),
    Column("truncated", BOOL, doc="True when a safety or request cap cut a duration window short."),
    Column("n_requests", INT, required=True),
    Column("n_ok", INT, required=True),
    Column("n_failed", INT, required=True),
    Column("useful_throughput_rps", FLOAT, unit="req_per_s",
           doc="Successful requests per second over the measured window."),
    Column("latency_median_ms", FLOAT, unit="ms"),
    Column("latency_p95_ms", FLOAT, unit="ms"),
    Column("latency_p99_ms", FLOAT, unit="ms",
           doc="Only when volume supports a useful estimate; otherwise empty."),
    Column("model", STR),
    Column("artifact_commit", STR, doc="Melis candidate commit under test."),
    Column("config_hash", STR, doc="Hash of the effective gateway config."),
    Column("dataset_hash", STR, doc="Hash of the corpus/dataset, when applicable."),
    Column("feature_profile", STR, doc="Same stratum field as requests.feature_profile."),
    Column("window", ENUM, enum=WINDOWS),
    Column("backend_call_count", INT,
           doc="Execution-level backend counter delta. NOT a sum of per-request counts."),
    Column("backend_peak_inflight", INT,
           doc="Peak simultaneous in-flight requests observed at the backend for this execution. Distinct from configured concurrency."),
    Column("sampler_enabled", BOOL, doc="Whether the resource sampler ran during the measured window."),
)

# ── resources.csv ─────────────────────────────────────────────────────────────
# Common time-sampled resource usage. One row per (run, component, sample).
# Schema 1.1.0 keeps the protocol columns and appends source/method/timing.
# memory_rss_bytes is RSS only. cgroup/container usage is a different column.
# Definitions that the source did not provide stay empty (missing != zero).
RESOURCES = (
    Column("schema_version", STR, required=True),
    Column("collection_id", STR, required=True),
    Column("run_id", STR, required=True),
    Column("timestamp_utc", TS_UTC, required=True,
           doc="Time of THIS sample. Engine read time when the source provides one."),
    Column("elapsed_s", FLOAT, required=True, unit="s"),
    Column("component", STR, required=True, doc="gateway|embeddings|redis|backend|host|other."),
    Column("cpu_seconds_total", FLOAT, unit="s",
           doc="Cumulative CPU seconds (counter). Never a conversion of a CPU percentage."),
    Column("memory_rss_bytes", INT, unit="bytes",
           doc="Resident set size, only when the source has an RSS field. Never cgroup usage."),
    Column("memory_working_set_bytes", INT, unit="bytes",
           doc="Kubernetes working set: cgroup usage minus inactive_file, when both exist."),
    Column("network_rx_bytes", INT, unit="bytes"),
    Column("network_tx_bytes", INT, unit="bytes"),
    Column("gpu_utilization_pct", FLOAT, unit="pct"),
    Column("gpu_memory_bytes", INT, unit="bytes"),
    Column("sample_index", INT, required=True, doc="0-based ordinal within (run_id, component)."),
    Column("collection_started_at_utc", TS_UTC, doc="Wall time when this sample's read started."),
    Column("collection_duration_ms", FLOAT, unit="ms", doc="How long this sample's read took."),
    Column("configured_interval_s", FLOAT, unit="s"),
    Column("effective_interval_s", FLOAT, unit="s",
           doc="Seconds since the previous sample of this run and component. Empty on the first."),
    Column("sample_lag_s", FLOAT, unit="s",
           doc="effective_interval_s - configured_interval_s. Positive means the loop was late."),
    Column("source", STR, doc="Common source id for this row, e.g. docker_engine_stats or procfs."),
    Column("cpu_method", STR, doc="Definition of cpu_seconds_total. Empty when the counter is absent."),
    Column("memory_cgroup_current_bytes", INT, unit="bytes",
           doc="cgroup memory.current / Docker memory_stats.usage. Not RSS and not working set."),
    Column("memory_cgroup_method", STR),
    Column("memory_working_set_method", STR),
    Column("memory_rss_method", STR, doc="Empty when RSS was not provided. anon is not a method for this column."),
    Column("network_method", STR),
    Column("collection_error", STR, doc="Present when the read failed; metrics then stay empty."),
)

# ── quality.csv ───────────────────────────────────────────────────────────────
# One row per EVALUATION. Multiple evaluators/generations are NESTED
# observations; they do not multiply the count of independent cases.
QUALITY = (
    Column("schema_version", STR, required=True),
    Column("run_id", STR, required=True),
    Column("case_id", STR, required=True),
    Column("pair_id", STR, required=True),
    Column("policy", ENUM, required=True, enum=POLICIES),
    Column("generation_rep", INT, required=True, doc="Repeated generation index for the case."),
    Column("answer_id", STR, required=True, doc="Links to the answer JSONL blob."),
    Column("evaluator_id", STR, required=True),
    Column("evaluator_type", ENUM, required=True, enum=EVALUATOR_TYPES),
    Column("score", FLOAT, doc="Rubric score (e.g. 0-3); empty if not scored."),
    Column("required_facts", INT),
    Column("retained_facts", INT),
    Column("position_stratum", STR, doc="start|middle|end|distributed."),
    Column("domain", STR),
    Column("context_length_stratum", STR),
    Column("blinded", BOOL, doc="Evaluator blind to the policy."),
)

# ── resilience_events.csv ─────────────────────────────────────────────────────
# One row per scheduled/observed fault or recovery event within an episode.
RESILIENCE_EVENTS = (
    Column("schema_version", STR, required=True),
    Column("run_id", STR, required=True),
    Column("episode_id", STR, required=True, doc="Independent resilience unit (reset state)."),
    Column("gateway", ENUM, required=True, enum=GATEWAYS),
    Column("scenario", STR, required=True),
    Column("relative_ms", FLOAT, required=True, unit="ms", doc="Time from episode start."),
    Column("event_kind", STR, required=True, doc="scheduled|observed."),
    Column("event_type", STR, required=True, doc="e.g. fault_injected, recovered, probe, failover."),
    Column("state", STR, doc="Observed component/breaker state."),
    Column("destination_id", STR, doc="Pseudonymized destination when observable."),
)

# ── safety_cases.csv ──────────────────────────────────────────────────────────
# One row per security/cache labelled case. NEVER store sensitive content here.
SAFETY_CASES = (
    Column("schema_version", STR, required=True),
    Column("run_id", STR, required=True),
    Column("case_id", STR, required=True),
    Column("family", STR, required=True, doc="Case family/template to avoid pseudo-replication."),
    Column("expected_label", STR, required=True, doc="allowed|blocked|redacted|hit|miss|..."),
    Column("observed_label", STR, required=True),
    Column("identity_pseudonym", STR, doc="Pseudonymized caller identity."),
    Column("hit_category", STR, doc="exact|paraphrase|hard_negative|cold|warm; cache cases."),
    Column("leak_detected", BOOL, doc="Whether a cross-identity/content leak was observed."),
    Column("detection_action", STR, doc="block|redact|warn|none."),
    Column("pass_fail", ENUM, required=True, enum=("pass", "fail"),
           doc="Contract pass/fail — NOT an inferential p-value."),
)

# ── comparisons.csv ───────────────────────────────────────────────────────────
# DERIVED results. Preserves independent units and pairing. Contract tests write
# expected/observed/pass-fail with empty p-values; do not fabricate p-values.
COMPARISONS = (
    Column("schema_version", STR, required=True),
    Column("experiment_id", STR, required=True),
    Column("collection_id", STR, required=True),
    Column("phase", ENUM, required=True, enum=PHASES),
    Column("question", STR, required=True, doc="RQ1|RQ2|RQ3|RQ4|guardrails."),
    Column("outcome", STR, required=True, doc="Primary/secondary outcome name."),
    Column("contrast", STR, required=True, doc="e.g. melis_minus_direct."),
    Column("population", STR, required=True, doc="Which rows this aggregates."),
    Column("independent_units", STR, required=True, doc="block|episode|case — the unit of n."),
    Column("n", INT, required=True, doc="Number of INDEPENDENT units (not nested requests)."),
    Column("estimate", FLOAT, doc="Point estimate; empty for pure contract rows."),
    Column("unit", STR, doc="Unit of the estimate (ms, rps, score, proportion)."),
    Column("effect_size", FLOAT),
    Column("ci_lower", FLOAT),
    Column("ci_upper", FLOAT),
    Column("method", STR, required=True, doc="Estimation/test method or 'contract'."),
    Column("seed", STR, doc="Resampling seed, when applicable."),
    Column("p_raw", FLOAT, doc="Raw p-value when an inferential test applies; else empty."),
    Column("p_adjusted", FLOAT, doc="Multiplicity-corrected p-value; else empty."),
    Column("margin", FLOAT, doc="Non-inferiority/equivalence margin delta; else empty."),
    Column("decision", STR, doc="Pre-specified decision label; empty if exploratory."),
    Column("expected", STR, doc="Contract expected value (contract rows)."),
    Column("observed", STR, doc="Contract observed value (contract rows)."),
    Column("pass_fail", ENUM, enum=("pass", "fail"), doc="Contract pass/fail; empty for inferential rows."),
    Column("analysis_id", STR, required=True,
           doc="Identity of the analysis code that wrote this derived row (version + source hash)."),
    Column("evidence_role", ENUM, required=True, enum=EVIDENCE_ROLES,
           doc="exploratory for smoke/pilot. confirmatory only when a final, pre-registered analysis is explicitly requested."),
)

# ── execution_facts.csv ───────────────────────────────────────────────────────
# One row per measured execution. Raw bookkeeping the request log must not
# repeat: order in the block, warm-up, backend counter delta, observed peak.
# backend_call_count here is the execution total. It is NOT fanned out to
# requests.csv when the backend only exposes a process-wide counter.
EXECUTION_FACTS = (
    Column("schema_version", STR, required=True),
    Column("experiment_id", STR, required=True),
    Column("collection_id", STR, required=True),
    Column("phase", ENUM, required=True, enum=PHASES),
    Column("run_id", STR, required=True),
    Column("block_id", STR, required=True),
    Column("gateway", ENUM, required=True, enum=GATEWAYS),
    Column("scenario", STR, required=True),
    Column("backend_profile", ENUM, required=True, enum=("deterministic", "real")),
    Column("streaming", BOOL, required=True),
    Column("concurrency", INT, required=True, doc="Configured closed-loop workers. Not the observed peak."),
    Column("policy", ENUM, enum=POLICIES),
    Column("feature_profile", STR),
    Column("window", ENUM, required=True, enum=WINDOWS),
    Column("order_in_block", INT, doc="Position of this gateway inside the block (0-based)."),
    Column("seed", STR),
    Column("warmup_s", FLOAT, unit="s"),
    Column("duration_s", FLOAT, unit="s",
           doc="Effective wall time, including in-flight drain. Not the configured duration."),
    Column("requested_duration_s", FLOAT, unit="s",
           doc="Configured window. Empty when the cell was a planned request cap."),
    Column("termination_reason", ENUM, required=True, enum=TERMINATION_REASONS),
    Column("truncated", BOOL, required=True,
           doc="True when safety_cap or an explicit request cap ended a duration window early."),
    Column("throughput_per_s", FLOAT, unit="req_per_s",
           doc="Finished requests divided by effective wall time, including drain. Not divided by the configured window."),
    Column("admitted_n", INT, doc="Requests that started. In-flight work at the deadline is included."),
    Column("drained_n", INT, doc="Admitted requests that finished after the admission deadline."),
    Column("backend_call_count", INT,
           doc="Counter delta for this execution only. Empty when the delta is ambiguous."),
    Column("backend_peak_inflight", INT,
           doc="Peak observed after a per-cell reset. Empty when the reset did not succeed."),
    Column("count_scope", ENUM, enum=COUNT_SCOPES,
           doc="execution: the count belongs to the run, not to individual requests."),
    Column("sampler_enabled", BOOL, required=True),
    Column("sampler_interval_s", FLOAT, unit="s"),
)

# Registry: logical table name -> (columns, key columns used for duplicate checks).
SCHEMAS = {
    "requests": (REQUESTS, ("run_id", "request_id")),
    "attempts": (ATTEMPTS, ("run_id", "request_id", "attempt_index")),
    "runs": (RUNS, ("run_id",)),
    "resources": (RESOURCES, ("run_id", "component", "sample_index")),
    "quality": (QUALITY, ("run_id", "case_id", "policy", "generation_rep", "answer_id", "evaluator_id")),
    "resilience_events": (RESILIENCE_EVENTS, ("run_id", "episode_id", "relative_ms", "event_type")),
    "safety_cases": (SAFETY_CASES, ("run_id", "case_id")),
    "comparisons": (COMPARISONS, ("collection_id", "question", "outcome", "contrast", "population")),
    "execution_facts": (EXECUTION_FACTS, ("run_id", "window")),
}


def header(table: str) -> list:
    """Ordered column names for a logical table."""
    cols, _ = SCHEMAS[table]
    return [c.name for c in cols]


def columns(table: str) -> tuple:
    return SCHEMAS[table][0]


def key_columns(table: str) -> tuple:
    return SCHEMAS[table][1]


def table_names() -> tuple:
    return tuple(SCHEMAS.keys())
