#!/usr/bin/env python3
"""Write schema tables from the local RQ3 development pilot.

The diagnostic client is not a product gateway. Its rows go to episodes.csv,
whose gateway column is free text. resilience_events.csv keeps the product
gateway enum, so only the direct client is written there. The diagnostic
events stay in the source JSON.
"""
from __future__ import annotations

import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, os.path.join(ROOT, "instrumentation"))
from csv_writer import CsvTableWriter  # noqa: E402

SRC = os.path.join(ROOT, "data", "rq3-pilot-dev-20261106", "episodes.json")
OUT = os.path.dirname(SRC)
COLLECTION = "col-rq3-dev-pilot-20261106"
EXPERIMENT = "exp-rq3-dev-pilot"
ANALYSIS = "rq3-dev-pilot-paired-mean"


def main():
    rows = json.load(open(SRC, encoding="utf-8"))
    episodes = CsvTableWriter("episodes", os.path.join(OUT, "episodes.csv"))
    events = CsvTableWriter("resilience_events", os.path.join(OUT, "resilience_events.csv"))
    comparisons = CsvTableWriter("comparisons", os.path.join(OUT, "comparisons.csv"))
    by_scenario = {}
    for row in rows:
        episode_id = f"{row['scenario']}-{row['episode_index']}"
        episodes.write_row({
            "collection_id": COLLECTION,
            "episode_id": episode_id,
            "gateway": row["client"],
            "scenario": row["scenario"],
            "phase": "pilot",
            "seed": str(row["seed"]),
            "order_in_pair": row["order_in_pair"],
            "deadline_ms": 1000,
            "success_within_deadline": row["success_within_deadline"],
            "success_after_deadline": row["success_after_deadline"],
            "failed": row["failed"],
            "recovery_observed": row["recovery_observed"],
            "recovery_censored": row["recovery_censored"],
            "backend_call_count": row["backend_call_count"],
            "attempt_count_scope": "per_attempt",
        })
        if row["client"] != "direct":
            continue
        for event in row["events"]:
            events.write_row({
                "run_id": f"run-rq3-dev-{episode_id}-direct",
                "episode_id": episode_id,
                "gateway": "direct",
                "scenario": row["scenario"],
                "relative_ms": event["relative_ms"],
                "event_kind": event["event_kind"],
                "event_type": event["event_type"],
                "state": event.get("state"),
                "destination_id": event["event_type"] if event["event_kind"] == "observed" else None,
            })
        by_scenario.setdefault(row["scenario"], {})[row["episode_index"]] = row
    other = {
        (r["scenario"], r["episode_index"]): r
        for r in rows if r["client"] == "aligned_failover"
    }
    for scenario, direct_rows in by_scenario.items():
        diffs = []
        for index, direct in direct_rows.items():
            failover = other[(scenario, index)]
            diffs.append(int(failover["success_within_deadline"]) - int(direct["success_within_deadline"]))
        comparisons.write_row({
            "experiment_id": EXPERIMENT,
            "collection_id": COLLECTION,
            "phase": "pilot",
            "question": "RQ3",
            "outcome": "success_within_deadline",
            "contrast": "aligned_failover_minus_direct",
            "population": scenario,
            "independent_units": "episode",
            "n": len(diffs),
            "estimate": sum(diffs) / len(diffs),
            "unit": "paired_indicator_difference",
            "method": "mean of paired episode indicators; not a confidence interval and not a test",
            "seed": "20261106",
            "analysis_id": ANALYSIS,
            "evidence_role": "exploratory",
        })
    episodes.close()
    events.close()
    comparisons.close()
    print("episodes", len(rows), "scenarios", len(by_scenario))


if __name__ == "__main__":
    main()
