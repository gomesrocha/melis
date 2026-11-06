#!/usr/bin/env python3
"""Export the RQ3 final log. Descriptive counts only; no test and no interval."""
from __future__ import annotations

import json
import os
import sys
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, os.path.join(ROOT, "instrumentation"))
from csv_writer import CsvTableWriter  # noqa: E402

COLLECTION = "col-rq3-final-20261106"
EXPERIMENT = "exp-rq3-final"
METHOD = (
    "Mean of the paired episode differences. No test and no interval. "
    "Agreement of every pair is a count, not evidence of equivalence."
)
DIRECT_METHOD = (
    "Mean over the direct-control episodes. These episodes are not in the "
    "Melis/LiteLLM paired N. The direct client has no fallback and uses the "
    "2500 ms logical deadline as its HTTP timeout."
)


def _parse(value):
    return datetime.strptime(value, "%Y-%m-%dT%H:%M:%S.%fZ").replace(tzinfo=timezone.utc)


def _status(row):
    kind = row.get("error_kind")
    if kind == "timeout":
        return "timeout"
    if kind == "transport":
        return "transport_error"
    if row["http_status"] == 200:
        return "ok"
    return "http_error"


def export(data_dir):
    rows = [json.loads(line) for line in open(os.path.join(data_dir, "episodes.jsonl"), encoding="utf-8") if line.strip()]
    measured = [row for row in rows if row.get("role") != "preflight"]
    requests = CsvTableWriter("requests", os.path.join(data_dir, "requests.csv"))
    attempts = CsvTableWriter("attempts", os.path.join(data_dir, "attempts.csv"))
    episodes = CsvTableWriter("episodes", os.path.join(data_dir, "episodes.csv"))
    events = CsvTableWriter("resilience_events", os.path.join(data_dir, "resilience_events.csv"))
    for row in measured:
        gateway = row["client"]
        block = f"{row['scenario']}-{row['episode_index']:02d}"
        request_id = f"{block}-{gateway}"
        run_id = f"run-rq3-final-{row['scenario']}-{gateway}"
        pair = block if row["role"] == "gateway_pair" else block + "-direct-control"
        http = row["http_status"] if row["http_status"] else None
        requests.write_row({
            "experiment_id": EXPERIMENT,
            "collection_id": COLLECTION,
            "phase": "final",
            "run_id": run_id,
            "block_id": block,
            "pair_id": pair,
            "case_id": block,
            "request_id": request_id,
            "gateway": gateway,
            "scenario": row["scenario"],
            "backend_profile": "deterministic",
            "streaming": False,
            "concurrency": 1,
            "started_at_utc": row["started_at_utc"],
            "latency_ms": row["latency_ms"],
            "status": _status(row),
            "http_status": http,
            "timeout": row["error_kind"] == "timeout" or row["success_after_deadline"],
            "completion_valid": row["http_status"] == 200,
            "backend_call_count": row["backend_call_count"],
            "window": "measured",
        })
        calls = [("primary", call) for call in row["primary_calls"]]
        calls += [("fallback", call) for call in row["fallback_calls"]]
        calls.sort(key=lambda item: item[1].get("observed_at_utc") or "")
        scope = "per_attempt" if calls and all(call.get("observed_at_utc") for _, call in calls) else "aggregate_only"
        start = _parse(row["started_at_utc"])
        for index, (destination, call) in enumerate(calls):
            stamp = call.get("observed_at_utc")
            if not stamp:
                continue
            code = int(call["status"])
            attempts.write_row({
                "collection_id": COLLECTION,
                "run_id": run_id,
                "request_id": request_id,
                "attempt_index": index,
                "destination_id": destination,
                "started_at_utc": stamp,
                "latency_ms": None,
                "status": "ok" if code == 200 else "http_error",
                "http_status": code,
                "is_probe": False,
                "is_fallback": destination == "fallback",
            })
            relative = (_parse(stamp) - start).total_seconds() * 1000.0
            events.write_row({
                "run_id": run_id,
                "episode_id": block,
                "gateway": gateway,
                "scenario": row["scenario"],
                "relative_ms": round(relative, 3),
                "event_kind": "observed",
                "event_type": f"backend_call_{destination}_{call['n']}",
                "destination_id": destination,
            })
        episodes.write_row({
            "collection_id": COLLECTION,
            "episode_id": block,
            "gateway": gateway,
            "scenario": row["scenario"],
            "phase": "final",
            "seed": str(row["seed"]),
            "order_in_pair": row["order_in_pair"],
            "deadline_ms": row["deadline_ms"],
            "success_within_deadline": row["success_within_deadline"],
            "success_after_deadline": row["success_after_deadline"],
            "failed": row["failed"],
            "recovery_observed": row["recovery_observed"],
            "recovery_censored": row["recovery_censored"],
            "recovery_ms": None,
            "backend_call_count": row["backend_call_count"],
            "attempt_count_scope": scope if calls else "not_observable",
        })
    for writer in (requests, attempts, episodes, events):
        writer.close()
    _comparisons(data_dir, measured)
    _summary(data_dir, measured)
    return len(measured)


def _comparisons(data_dir, measured):
    writer = CsvTableWriter("comparisons", os.path.join(data_dir, "comparisons.csv"))
    scenarios = []
    for row in measured:
        if row["scenario"] not in scenarios:
            scenarios.append(row["scenario"])
    for scenario in scenarios:
        paired = {}
        direct = []
        for row in measured:
            if row["scenario"] != scenario:
                continue
            if row["role"] == "backend_control":
                direct.append(row)
            else:
                paired.setdefault(row["episode_index"], {})[row["client"]] = row
        success = []
        calls = []
        for episode in sorted(paired):
            left = paired[episode]["melis"]
            right = paired[episode]["litellm"]
            success.append(int(left["success_within_deadline"]) - int(right["success_within_deadline"]))
            calls.append(left["backend_call_count"] - right["backend_call_count"])
        for outcome, values, unit in (
            ("success_within_deadline", success, "paired_episode_indicator"),
            ("backend_call_count", calls, "paired_backend_calls"),
        ):
            writer.write_row({
                "experiment_id": EXPERIMENT,
                "collection_id": COLLECTION,
                "phase": "final",
                "question": "RQ3",
                "outcome": outcome,
                "contrast": "melis_minus_litellm",
                "population": scenario,
                "independent_units": "episode",
                "n": len(values),
                "estimate": sum(values) / len(values),
                "unit": unit,
                "method": METHOD,
                "decision": "descriptive",
                "analysis_id": "rq3-final-descriptive-1",
                "evidence_role": "exploratory",
            })
        writer.write_row({
            "experiment_id": EXPERIMENT,
            "collection_id": COLLECTION,
            "phase": "final",
            "question": "RQ3",
            "outcome": "success_within_deadline",
            "contrast": "direct_control_level",
            "population": scenario + "-direct-control",
            "independent_units": "episode",
            "n": len(direct),
            "estimate": sum(int(row["success_within_deadline"]) for row in direct) / len(direct),
            "unit": "episode_indicator",
            "method": DIRECT_METHOD,
            "decision": "descriptive",
            "analysis_id": "rq3-final-descriptive-1",
            "evidence_role": "exploratory",
        })
        for gateway in ("melis", "litellm", "direct"):
            chosen = [row for row in measured if row["scenario"] == scenario and row["client"] == gateway]
            population = scenario if gateway != "direct" else scenario + "-direct-control"
            writer.write_row({
                "experiment_id": EXPERIMENT,
                "collection_id": COLLECTION,
                "phase": "final",
                "question": "RQ3",
                "outcome": "latency_ms",
                "contrast": gateway + "_level",
                "population": population,
                "independent_units": "episode",
                "n": len(chosen),
                "estimate": sum(row["latency_ms"] for row in chosen) / len(chosen),
                "unit": "ms",
                "method": "Arithmetic mean of episode latency. No test.",
                "decision": "descriptive",
                "analysis_id": "rq3-final-descriptive-1",
                "evidence_role": "exploratory",
            })
    writer.close()


def _summary(data_dir, measured):
    path = os.path.join(data_dir, "scenario_summary.csv")
    with open(path, "w", encoding="utf-8") as fh:
        fh.write("scenario,gateway,role,n,success_within,failed,mean_latency_ms,mean_backend_calls,recovery_observed,recovery_censored,zero_call\n")
        groups = {}
        for row in measured:
            groups.setdefault((row["scenario"], row["client"], row["role"]), []).append(row)
        for key in sorted(groups):
            rows = groups[key]
            observed = [row["recovery_observed"] for row in rows if row["recovery_observed"] is not None]
            censored = sum(bool(row["recovery_censored"]) for row in rows)
            fh.write(",".join(str(item) for item in [
                key[0], key[1], key[2], len(rows),
                sum(bool(row["success_within_deadline"]) for row in rows),
                sum(bool(row["failed"]) for row in rows),
                round(sum(row["latency_ms"] for row in rows) / len(rows), 3),
                round(sum(row["backend_call_count"] for row in rows) / len(rows), 3),
                sum(bool(item) for item in observed),
                censored,
                sum(row["backend_call_count"] == 0 for row in rows),
            ]) + "\n")


if __name__ == "__main__":
    print(export(sys.argv[1]))
