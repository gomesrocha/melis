#!/usr/bin/env python3
"""Predefined RQ1 tests. Run only after a collection exists.

Two-sided Student t tests of the block differences against zero, Holm across
the three contrasts, family alpha 0.05, and marginal 95% t intervals. The
interval is not simultaneous. This script does not drop blocks and does not
change N.
"""
from __future__ import annotations

import json
import math
import os
import statistics
import sys

HARNESS = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HARNESS)
sys.path.insert(0, HARNESS)
sys.path.insert(0, os.path.join(ROOT, "instrumentation"))

import aggregate  # noqa: E402
import precision  # noqa: E402


def _read_requests(data_dir):
    path = os.path.join(data_dir, "requests.csv")
    header, rows = aggregate._read(path)
    return aggregate._dict_rows(header, rows)


def analyze(data_dir: str, seed: int = 20261105) -> dict:
    requests = _read_requests(data_dir)
    requests = [row for row in requests if row.get("phase") == "final"]
    if not requests:
        raise SystemExit("no phase=final rows")
    decisions = aggregate.load_block_decisions(data_dir)
    sink = []
    aggregate.rq1_contrasts(
        requests, requests[0]["experiment_id"], seed,
        evidence_role="confirmatory", facts=[], decisions=decisions, diff_sink=sink,
    )
    family = []
    for item in sink:
        diffs = [float(value) for value in item["diffs_ms"]]
        n = len(diffs)
        mean = statistics.fmean(diffs)
        if n < 2:
            family.append({
                "contrast": item["contrast"],
                "n_blocks": n,
                "mean_ms": mean,
                "p_raw": None,
                "note": "fewer than two blocks",
            })
            continue
        sd = statistics.stdev(diffs)
        df = n - 1
        se = sd / math.sqrt(n)
        t_stat = 0.0 if se == 0 else mean / se
        p_raw = precision.two_sided_t_p(t_stat, df)
        critical = precision.t_ppf(0.975, df)
        half = critical * se
        family.append({
            "contrast": item["contrast"],
            "population": item["population"],
            "n_blocks": n,
            "block_ids": item["block_ids"],
            "diffs_ms": diffs,
            "mean_ms": mean,
            "sd_ms": sd,
            "df": df,
            "t": t_stat,
            "p_raw": p_raw,
            "marginal_ci95_ms": [mean - half, mean + half],
            "marginal_interval": "student_t_95_not_simultaneous",
        })
    raw = [item.get("p_raw") for item in family]
    if all(value is not None for value in raw):
        adjusted = precision.holm(raw)
        for item, value in zip(family, adjusted):
            item["p_holm"] = value
            item["reject_at_family_alpha_0.05"] = value < 0.05
    return {
        "procedure": "two_sided_student_t_of_block_differences_against_zero",
        "multiplicity": "holm",
        "family_alpha": 0.05,
        "assumptions": [
            "block_differences_are_independent",
            "block_differences_are_drawn_from_a_normal_distribution",
            "the_unit_is_the_planned_block_not_the_request",
        ],
        "n_was_not_increased": True,
        "high_load_did_not_exclude_blocks": True,
        "high_latency_did_not_exclude_blocks": True,
        "contrasts": family,
    }


def main(argv=None) -> int:
    argv = list(sys.argv[1:] if argv is None else argv)
    if len(argv) != 1:
        raise SystemExit("usage: rq1_inference.py DATA_DIR")
    report = analyze(argv[0])
    path = os.path.join(argv[0], "rq1_inference.json")
    with open(path, "w", encoding="utf-8") as fh:
        json.dump(report, fh, indent=2)
        fh.write("\n")
    print(path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
