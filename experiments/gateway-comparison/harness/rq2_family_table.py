#!/usr/bin/env python3
"""Read-only tables for the closed RQ2 evaluation.

Uses the frozen estimator's family means. Does not change the primary formula,
the seed, or the index rule.
"""
from __future__ import annotations

import csv
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)

import rq2_family_estimate as est  # noqa: E402

ARMS = ("direct", "heuristic", "chronological", "none")


def _levels(facts, channel):
    levels = {}
    for arm in ARMS:
        flags = est._indicators(facts, f"run-rq2-eval-{arm}", channel)
        for case_id, present in flags.items():
            levels.setdefault(est._family_of(case_id), {}).setdefault(arm, []).append(int(present))
    return levels


def write(data_dir, out_dir):
    os.makedirs(out_dir, exist_ok=True)
    facts = list(csv.DictReader(open(os.path.join(data_dir, "facts.csv"), encoding="utf-8")))
    preflight = json.load(open(os.path.join(data_dir, "preflight.json"), encoding="utf-8"))
    cases = {row["case_id"]: {} for row in facts}
    for row in facts:
        if row["fact_role"] != "critical":
            continue
        key = (row["case_id"], row["run_id"].removeprefix("run-rq2-eval-"), row["channel"])
        cases[row["case_id"]].setdefault(key[1], {})
        bucket = cases[row["case_id"]][key[1]].setdefault(key[2], [])
        bucket.append(row["observed_present"] == "true")
    with open(os.path.join(out_dir, "rq2_case_results.csv"), "w", newline="", encoding="utf-8") as fh:
        writer = csv.writer(fh)
        writer.writerow(["case_id", "family", "arm", "critical_in_answer", "critical_in_forwarded_context"])
        for case_id in sorted(cases):
            for arm in ARMS:
                answer = all(cases[case_id][arm]["answer"])
                context = all(cases[case_id][arm]["forwarded_context"])
                writer.writerow([case_id, est._family_of(case_id), arm, int(answer), int(context)])
    answer_levels = _levels(facts, "answer")
    with open(os.path.join(out_dir, "rq2_family_differences.csv"), "w", newline="", encoding="utf-8") as fh:
        writer = csv.writer(fh)
        writer.writerow(["family", "n_cases", "direct", "heuristic", "chronological", "none",
                         "heuristic_minus_direct", "chronological_minus_direct", "none_minus_direct"])
        grand = []
        for family in sorted(answer_levels):
            means = {arm: sum(answer_levels[family][arm]) / len(answer_levels[family][arm]) for arm in ARMS}
            diffs = (
                means["heuristic"] - means["direct"],
                means["chronological"] - means["direct"],
                means["none"] - means["direct"],
            )
            grand.append(diffs)
            writer.writerow([family, len(answer_levels[family]["direct"]),
                             *[means[arm] for arm in ARMS], *diffs])
        writer.writerow(["unweighted_mean_of_family_means", len(answer_levels), "", "", "", "",
                         *[sum(item[i] for item in grand) / len(grand) for i in range(3)]])
    with open(os.path.join(out_dir, "rq2_size_by_family.csv"), "w", newline="", encoding="utf-8") as fh:
        writer = csv.writer(fh)
        writer.writerow(["family", "arm", "n_cases", "baseline_messages", "forwarded_messages",
                         "baseline_chars", "forwarded_chars"])
        grouped = {}
        for row in preflight["rows"]:
            grouped.setdefault((est._family_of(row["case_id"]), row["arm"]), []).append(row)
        for key in sorted(grouped):
            rows = grouped[key]
            writer.writerow([
                key[0], key[1], len(rows),
                sum(r["baseline_messages"] for r in rows) / len(rows),
                sum(r["forwarded_messages"] for r in rows) / len(rows),
                sum(r["baseline_chars"] for r in rows) / len(rows),
                sum(r["forwarded_chars"] for r in rows) / len(rows),
            ])


if __name__ == "__main__":
    write(sys.argv[1], sys.argv[2])
