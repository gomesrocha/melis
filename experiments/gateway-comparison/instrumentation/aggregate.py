#!/usr/bin/env python3
"""Reproducible aggregation: derive runs.csv and comparisons.csv from raw data.

Inputs are the raw ``requests.csv`` (and optionally ``resilience_events.csv`` /
``safety_cases.csv``). Outputs are DERIVED tables written to separate files;
raw data is never overwritten (PROTOCOL.md "Não sobrescreva dados brutos ao
gerar resumos").

Independence rules enforced here (PROTOCOL.md):
  * A per-run summary aggregates the NESTED requests of that run.
  * A ``comparisons.csv`` contrast's ``n`` is the number of INDEPENDENT units
    (blocks for performance, episodes for resilience, cases for quality) — never
    the raw request count. Intra-run requests and per-case generations are NOT
    counted as independent repetitions.
  * No automatic outlier removal; failures/timeouts are retained and counted.

Statistics here are descriptive/bootstrap only (stdlib ``random`` + explicit
seed). Inferential testing with multiplicity correction is a separate,
pre-registered analysis step; this script does not fabricate p-values.

Usage:
  python aggregate.py --data-dir ../data/pilot \
      --runs-out ../data/pilot/runs.csv \
      --comparisons-out ../data/pilot/comparisons.csv \
      --seed 20261105

Stdlib only.
"""
from __future__ import annotations

import argparse
import csv
import os
import random
import statistics
from collections import defaultdict
from typing import Optional

from csv_writer import CsvTableWriter


def _read(path: str):
    if not os.path.exists(path):
        return [], []
    with open(path, newline="", encoding="utf-8") as fh:
        rows = list(csv.reader(fh))
    return (rows[0], rows[1:]) if rows else ([], [])


def _dict_rows(header, rows):
    return [dict(zip(header, r)) for r in rows]


def _f(v):
    return float(v) if v not in (None, "") else None


def _percentile(sorted_vals, q):
    """Nearest-rank percentile; returns None if too few points to be useful."""
    n = len(sorted_vals)
    if n == 0:
        return None
    rank = max(1, min(n, int(round(q * n))))
    return sorted_vals[rank - 1]


def summarize_runs(requests):
    """Derive one runs.csv row per run_id from nested request rows."""
    by_run = defaultdict(list)
    for r in requests:
        by_run[r["run_id"]].append(r)

    out = []
    for run_id, reqs in by_run.items():
        oks = [r for r in reqs if r["status"] == "ok"]
        ok_lat = sorted(_f(r["latency_ms"]) for r in oks if _f(r["latency_ms"]) is not None)
        sample = reqs[0]
        duration = _f(sample.get("duration_s")) if sample.get("duration_s") else None
        n_ok = len(oks)
        n_req = len(reqs)
        # Useful throughput uses the measured window if present; otherwise empty
        # (we do NOT invent a denominator).
        throughput = (n_ok / duration) if (duration and duration > 0) else None
        row = {
            "experiment_id": sample["experiment_id"],
            "phase": sample["phase"],
            "run_id": run_id,
            "block_id": sample["block_id"],
            "gateway": sample["gateway"],
            "scenario": sample["scenario"],
            "backend_profile": sample["backend_profile"],
            "streaming": sample["streaming"] == "true",
            "concurrency": int(sample["concurrency"]),
            "policy": sample.get("policy") or None,
            "seed": sample.get("seed") or None,
            "order_in_block": None,
            "warmup_s": None,
            "duration_s": duration,
            "n_requests": n_req,
            "n_ok": n_ok,
            "n_failed": n_req - n_ok,
            "useful_throughput_rps": round(throughput, 4) if throughput is not None else None,
            "latency_median_ms": round(statistics.median(ok_lat), 3) if ok_lat else None,
            "latency_p95_ms": round(_percentile(ok_lat, 0.95), 3) if len(ok_lat) >= 20 else None,
            # p99 only when volume plausibly supports it.
            "latency_p99_ms": round(_percentile(ok_lat, 0.99), 3) if len(ok_lat) >= 100 else None,
            "model": sample.get("effective_model") or sample.get("requested_model") or None,
            "artifact_commit": sample.get("artifact_commit") or None,
            "config_hash": None,
            "dataset_hash": None,
        }
        out.append(row)
    return out


def _block_medians_by_gateway(requests):
    """For RQ1-style contrasts: median latency per (block, gateway) — the block
    is the independent unit; requests within it are nested observations."""
    acc = defaultdict(lambda: defaultdict(list))
    for r in requests:
        if r["status"] != "ok":
            continue
        lat = _f(r["latency_ms"])
        if lat is None:
            continue
        acc[r["block_id"]][r["gateway"]].append(lat)
    block_med = {}
    for block_id, by_gw in acc.items():
        block_med[block_id] = {gw: statistics.median(v) for gw, v in by_gw.items() if v}
    return block_med


def _bootstrap_ci(diffs, seed, iters=2000, alpha=0.05):
    """Percentile bootstrap CI over the INDEPENDENT paired differences."""
    if len(diffs) < 2:
        return (None, None)
    rng = random.Random(seed)
    means = []
    n = len(diffs)
    for _ in range(iters):
        sample = [diffs[rng.randrange(n)] for _ in range(n)]
        means.append(statistics.fmean(sample))
    means.sort()
    lo = means[int((alpha / 2) * iters)]
    hi = means[int((1 - alpha / 2) * iters) - 1]
    return (round(lo, 4), round(hi, 4))


def rq1_contrasts(requests, experiment_id, phase, seed):
    """Paired latency contrasts between gateways, blocks as independent units.

    Produces descriptive estimates with bootstrap CIs. No p-values: the
    pre-registered inferential test (Friedman/Holm, etc.) runs separately.
    """
    block_med = _block_medians_by_gateway(requests)
    rows = []
    gateways = ["melis", "litellm"]  # contrasts vs direct baseline
    for gw in gateways:
        diffs = []
        for block_id, meds in block_med.items():
            if gw in meds and "direct" in meds:
                diffs.append(meds[gw] - meds["direct"])  # ms, paired per block
        if not diffs:
            continue
        est = round(statistics.fmean(diffs), 4)
        lo, hi = _bootstrap_ci(diffs, seed)
        rows.append({
            "experiment_id": experiment_id,
            "phase": phase,
            "question": "RQ1",
            "outcome": "median_latency_overhead",
            "contrast": f"{gw}_minus_direct",
            "population": "ok_requests_by_block_median",
            "independent_units": "block",
            "n": len(diffs),
            "estimate": est,
            "unit": "ms",
            "effect_size": est,
            "ci_lower": lo,
            "ci_upper": hi,
            "method": "paired_block_median_bootstrap_percentile",
            "seed": str(seed),
            "p_raw": None,         # not fabricated — inferential step is separate
            "p_adjusted": None,
            "margin": None,
            "decision": None,
            "expected": None,
            "observed": None,
            "pass_fail": None,
        })
    return rows


def build(data_dir, runs_out, comparisons_out, experiment_id, phase, seed):
    rh, rr = _read(os.path.join(data_dir, "requests.csv"))
    requests = _dict_rows(rh, rr)

    runs_rows = summarize_runs(requests)
    if runs_out:
        w = CsvTableWriter("runs", runs_out)
        w.write_rows(runs_rows)

    comparisons_rows = []
    # Infer experiment_id/phase from data if not supplied.
    if requests:
        experiment_id = experiment_id or requests[0]["experiment_id"]
        phase = phase or requests[0]["phase"]
    if experiment_id and phase:
        comparisons_rows.extend(rq1_contrasts(requests, experiment_id, phase, seed))
    if comparisons_out and comparisons_rows:
        w = CsvTableWriter("comparisons", comparisons_out)
        w.write_rows(comparisons_rows)

    return {"runs": len(runs_rows), "comparisons": len(comparisons_rows)}


def main(argv: Optional[list] = None) -> int:
    ap = argparse.ArgumentParser(description="Derive runs.csv and comparisons.csv from raw requests.csv.")
    ap.add_argument("--data-dir", required=True)
    ap.add_argument("--runs-out", default=None)
    ap.add_argument("--comparisons-out", default=None)
    ap.add_argument("--experiment-id", default=None)
    ap.add_argument("--phase", default=None)
    ap.add_argument("--seed", type=int, default=20261105)
    args = ap.parse_args(argv)

    for out in (args.runs_out, args.comparisons_out):
        if out and os.path.abspath(out) == os.path.abspath(os.path.join(args.data_dir, "requests.csv")):
            raise SystemExit("refusing to overwrite raw requests.csv")

    counts = build(args.data_dir, args.runs_out, args.comparisons_out,
                   args.experiment_id, args.phase, args.seed)
    print(counts)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
