#!/usr/bin/env python3
"""Predeclared RQ2 estimator. Do not change this file after evaluation answers exist.

Primary estimand: unweighted mean of within-family mean paired differences.
The case difference is the indicator that every critical fact is present in the
heuristic answer, minus the same indicator for the direct answer.
Families are averaged with equal weight. The interval resamples families.
"""
from __future__ import annotations

import csv
import json
import os
import random
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, os.path.join(ROOT, "instrumentation"))
from csv_writer import CsvTableWriter  # noqa: E402

RESAMPLES = 2000
SEED = 20261106
LEVELS = (0.025, 0.975)


def _index(level):
    """Frozen rank: int(level * 2000) - 1."""
    return int(level * RESAMPLES) - 1


def _family_of(case_id):
    return case_id.rsplit("-", 2)[0]


def _indicators(facts, run_id, channel):
    grouped = {}
    for row in facts:
        if row["run_id"] != run_id or row["channel"] != channel or row["fact_role"] != "critical":
            continue
        grouped.setdefault(row["case_id"], []).append(row["observed_present"] == "true")
    return {case_id: all(flags) for case_id, flags in grouped.items()}


def _paired(left, right):
    families = {}
    for case_id, base in right.items():
        diff = int(left[case_id]) - int(base)
        families.setdefault(_family_of(case_id), []).append(diff)
    means = {name: sum(values) / len(values) for name, values in families.items()}
    return means


def _estimate(means):
    values = [means[name] for name in sorted(means)]
    return sum(values) / len(values)


def _interval(means):
    names = sorted(means)
    rng = random.Random(SEED)
    draws = []
    for _ in range(RESAMPLES):
        sample = [means[names[rng.randrange(len(names))]] for _ in names]
        draws.append(sum(sample) / len(sample))
    draws.sort()
    return draws[_index(LEVELS[0])], draws[_index(LEVELS[1])]


def _size_reduction(preflight, arm):
    families = {}
    for row in preflight:
        if row["arm"] != arm or row["forwarded_messages"] is None:
            continue
        families.setdefault(_family_of(row["case_id"]), []).append((
            row["baseline_messages"] - row["forwarded_messages"],
            row["baseline_chars"] - row["forwarded_chars"],
        ))
    messages = {name: sum(item[0] for item in values) / len(values) for name, values in families.items()}
    chars = {name: sum(item[1] for item in values) / len(values) for name, values in families.items()}
    return messages, chars


def write_estimates(data_dir, protocol_path):
    protocol = json.load(open(protocol_path, encoding="utf-8"))
    facts = list(csv.DictReader(open(os.path.join(data_dir, "facts.csv"), encoding="utf-8")))
    preflight = json.load(open(os.path.join(data_dir, "preflight.json"), encoding="utf-8"))["rows"]
    direct_answer = _indicators(facts, "run-rq2-eval-direct", "answer")
    rows = []
    contrasts = (
        ("heuristic", "critical_fact_in_answer", "answer", True),
        ("chronological", "critical_fact_in_answer", "answer", False),
        ("none", "critical_fact_in_answer", "answer", False),
        ("heuristic", "critical_fact_in_forwarded_context", "forwarded_context", False),
        ("chronological", "critical_fact_in_forwarded_context", "forwarded_context", False),
    )
    for arm, outcome, channel, primary in contrasts:
        left = _indicators(facts, f"run-rq2-eval-{arm}", channel)
        means = _paired(left, _indicators(facts, "run-rq2-eval-direct", channel))
        low, high = _interval(means)
        rows.append({
            "outcome": outcome,
            "contrast": f"{arm}_minus_direct",
            "n": len(means),
            "estimate": _estimate(means),
            "ci_lower": low,
            "ci_upper": high,
            "unit": "paired_indicator_difference",
            "primary": primary,
        })
    for arm in ("heuristic", "chronological"):
        messages, chars = _size_reduction(preflight, arm)
        for kind, means, unit in (
            ("forwarded_message_reduction", messages, "messages"),
            ("forwarded_character_reduction", chars, "characters"),
        ):
            low, high = _interval(means)
            rows.append({
                "outcome": kind,
                "contrast": arm,
                "n": len(means),
                "estimate": _estimate(means),
                "ci_lower": low,
                "ci_upper": high,
                "unit": unit,
                "primary": False,
            })
    writer = CsvTableWriter("comparisons", os.path.join(data_dir, "comparisons.csv"))
    for row in rows:
        writer.write_row({
            "experiment_id": "exp-rq2-eval",
            "collection_id": protocol["collection_id"],
            "phase": "final",
            "question": "RQ2",
            "outcome": row["outcome"],
            "contrast": row["contrast"],
            "population": "eight_generation_families",
            "independent_units": "generation_family",
            "n": row["n"],
            "estimate": row["estimate"],
            "unit": row["unit"],
            "ci_lower": row["ci_lower"],
            "ci_upper": row["ci_upper"],
            "method": protocol["interval"]["method"],
            "seed": str(SEED),
            "analysis_id": "rq2-eval-family-bootstrap-1",
            "evidence_role": "confirmatory" if row["primary"] else "exploratory",
            "decision": "tradeoff_estimate" if row["primary"] else "",
        })
    writer.close()
    unused = direct_answer
    return rows, unused


if __name__ == "__main__":
    raise SystemExit("import write_estimates after the evaluation directory exists")
