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
import hashlib
import os
import random
import statistics
from collections import defaultdict
from typing import Optional

# Bump when the contrast definition changes, even if the file hash also changes.
ANALYSIS_REVISION = "1.2.0"

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


def summarize_runs(requests, facts_by_run=None):
    """Derive one runs.csv row per run_id from nested request rows.

    Execution-level facts (duration, order, backend counter, observed peak)
    come only from execution_facts.csv. They are not reconstructed by summing
    per-request fields.
    """
    facts_by_run = facts_by_run or {}
    by_run = defaultdict(list)
    for r in requests:
        by_run[r["run_id"]].append(r)

    out = []
    for run_id, reqs in by_run.items():
        oks = [r for r in reqs if r["status"] == "ok"]
        ok_lat = sorted(_f(r["latency_ms"]) for r in oks if _f(r["latency_ms"]) is not None)
        sample = reqs[0]
        fact = facts_by_run.get(run_id, {})
        duration = _f(fact.get("duration_s")) if fact else None
        n_ok = len(oks)
        n_req = len(reqs)
        # Useful throughput uses the measured window if present; otherwise empty
        # (we do NOT invent a denominator).
        throughput = (n_ok / duration) if (duration and duration > 0) else None
        window = fact.get("window") or sample.get("window") or None
        row = {
            "experiment_id": sample["experiment_id"],
            "collection_id": sample.get("collection_id") or fact.get("collection_id") or None,
            "phase": sample["phase"],
            "run_id": run_id,
            "block_id": sample["block_id"],
            "gateway": sample["gateway"],
            "scenario": sample["scenario"],
            "backend_profile": sample["backend_profile"],
            "streaming": sample["streaming"] == "true" or sample["streaming"] is True,
            "concurrency": int(sample["concurrency"]),
            "policy": sample.get("policy") or None,
            "seed": fact.get("seed") or sample.get("seed") or None,
            "order_in_block": int(fact["order_in_block"]) if fact.get("order_in_block") not in (None, "") else None,
            "warmup_s": _f(fact.get("warmup_s")) if fact.get("warmup_s") not in (None, "") else None,
            "duration_s": duration,
            "requested_duration_s": _f(fact.get("requested_duration_s")) if fact.get("requested_duration_s") not in (None, "") else None,
            "termination_reason": fact.get("termination_reason") or None,
            "truncated": fact.get("truncated") if fact.get("truncated") not in (None, "") else None,
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
            "feature_profile": sample.get("feature_profile") or None,
            "window": window if window else None,
            "backend_call_count": int(fact["backend_call_count"]) if fact.get("backend_call_count") not in (None, "") else None,
            "backend_peak_inflight": int(fact["backend_peak_inflight"]) if fact.get("backend_peak_inflight") not in (None, "") else None,
            "sampler_enabled": fact.get("sampler_enabled") if fact.get("sampler_enabled") not in (None, "") else None,
        }
        out.append(row)
    return out


# Factors that change the treatment. A contrast never pools across these.
STRATUM_FIELDS = (
    "collection_id",
    "phase",
    "backend_profile",
    "streaming",
    "concurrency",
    "policy",
    "feature_profile",
    "scenario",
)

# Family registered for the final analysis. The pilot leaves p-values empty.
# Holm across these three contrasts is the multiplicity plan, not a result.
RQ1_CONTRASTS = (
    ("melis_minus_direct", "melis", "direct"),
    ("litellm_minus_direct", "litellm", "direct"),
    ("melis_minus_litellm", "melis", "litellm"),
)

REQUIRED_GATEWAYS = ("direct", "melis", "litellm")


def analysis_id() -> str:
    """Identity of this analysis code: revision plus a hash of this file."""
    path = os.path.abspath(__file__)
    with open(path, "rb") as fh:
        digest = hashlib.sha256(fh.read()).hexdigest()[:12]
    return f"agg-{ANALYSIS_REVISION}-{digest}"


def _measured(row) -> bool:
    window = (row.get("window") or "").strip()
    return window in ("", "measured")


def stratum_key(row) -> tuple:
    return tuple((row.get(name) or "").strip() for name in STRATUM_FIELDS)


def stratum_population(key: tuple) -> str:
    parts = [f"{name}={value}" for name, value in zip(STRATUM_FIELDS, key)]
    parts.append("window=measured")
    return "|".join(parts)


def _block_medians(rows):
    """Median latency per (block, gateway). Requests inside a block are nested."""
    acc = defaultdict(lambda: defaultdict(list))
    for r in rows:
        if not _measured(r) or r.get("status") != "ok":
            continue
        lat = _f(r.get("latency_ms"))
        if lat is None:
            continue
        acc[r["block_id"]][r["gateway"]].append(lat)
    out = {}
    for block_id, by_gw in acc.items():
        out[block_id] = {gw: statistics.median(v) for gw, v in by_gw.items() if v}
    return out


def pairing_report(requests, required=REQUIRED_GATEWAYS):
    """Blocks that do not contain every required gateway are incomplete.

    Warm-up rows and non-ok bookkeeping are ignored. Concurrency, streaming,
    backend profile, policy and feature profile are part of the stratum, so
    c=1 is never paired with c=8.
    """
    by_stratum = defaultdict(lambda: defaultdict(set))
    for r in requests:
        if not _measured(r):
            continue
        if r.get("scenario") == "rq1_warmup":
            continue
        by_stratum[stratum_key(r)][r["block_id"]].add(r["gateway"])
    incomplete = []
    complete = []
    for key, blocks in by_stratum.items():
        for block_id, gateways in blocks.items():
            missing = [gw for gw in required if gw not in gateways]
            entry = {
                "population": stratum_population(key),
                "block_id": block_id,
                "gateways": sorted(gateways),
                "missing": missing,
            }
            if missing:
                incomplete.append(entry)
            else:
                complete.append(entry)
    return {"complete": complete, "incomplete": incomplete}


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


def truncated_block_ids(facts):
    """Measured executions whose duration window was cut by a cap."""
    found = set()
    for fact in facts or []:
        window = (fact.get("window") or "").strip()
        if window not in ("", "measured"):
            continue
        flag = str(fact.get("truncated") or "").strip().lower()
        if flag == "true":
            found.add(fact.get("block_id"))
    found.discard(None)
    found.discard("")
    return found


def load_block_decisions(data_dir):
    """Ledger of original and repeat attempts. Absent means historical behavior.

    A planned block contributes at most one attempt to the primary mean.
    """
    path = os.path.join(data_dir, "block_decisions.csv")
    if not os.path.exists(path):
        return None
    with open(path, newline="", encoding="utf-8") as fh:
        rows = list(csv.DictReader(fh))
    primaries = {}
    for row in rows:
        row["include_in_primary"] = row.get("include_in_primary") == "true"
        if not row["include_in_primary"]:
            continue
        key = row.get("planned_block_index")
        if key in primaries:
            raise ValueError(
                f"planned block {key} has two attempts in the primary estimate"
            )
        primaries[key] = row.get("block_id")
    return rows


def rq1_contrasts(requests, experiment_id, seed, evidence_role="exploratory",
                  facts=None, decisions=None):
    """Mean, across blocks, of differences of per-execution completion medians.

    A block contributes to the estimate only when direct, Melis and LiteLLM
    each have a successful median in that stratum, and the block was not
    truncated. Blocks that fail or are truncated stay in ``excluded``; they
    are not dropped without a trace and they are not removed because the
    latency looks large. When a decision ledger is present, a repeat is not
    a second planned block. p-values stay empty. The proposed t-tests and
    Holm are not computed here.
    """
    grouped = defaultdict(list)
    for r in requests:
        if not _measured(r):
            continue
        if (r.get("scenario") or "") == "rq1_warmup":
            continue
        grouped[stratum_key(r)].append(r)

    truncated = truncated_block_ids(facts)
    rows = []
    excluded = []
    aid = analysis_id()
    for key, group in grouped.items():
        phase = key[1] if key else ""
        collection = key[0] if key else ""
        report = pairing_report(group)
        complete_ids = {entry["block_id"] for entry in report["complete"]}
        if not complete_ids:
            continue
        kept = [r for r in group if r["block_id"] in complete_ids]
        block_med = _block_medians(kept)
        population = stratum_population(key)
        role = evidence_role if phase == "final" else "exploratory"
        by_block = {}
        if decisions:
            for decision in decisions:
                if decision.get("block_id"):
                    by_block[decision["block_id"]] = decision
        usable = []
        for block_id in sorted(complete_ids):
            meds = block_med.get(block_id, {})
            decision = by_block.get(block_id)
            if decisions is not None and decision is None:
                excluded.append({
                    "block_id": block_id, "population": population,
                    "reason": "attempt_not_in_decision_ledger",
                })
                continue
            if decision is not None and not decision["include_in_primary"]:
                excluded.append({
                    "block_id": block_id, "population": population,
                    "reason": decision.get("reason"),
                    "attempt": decision.get("attempt"),
                    "planned_block_index": decision.get("planned_block_index"),
                    "attribution": decision.get("attribution"),
                })
                continue
            if block_id in truncated:
                excluded.append({
                    "block_id": block_id, "population": population, "reason": "truncated",
                })
                continue
            if not all(name in meds for name in REQUIRED_GATEWAYS):
                reason = "no_successful_median"
                if decision is not None and decision.get("attribution") == "system":
                    reason = "system_under_test"
                excluded.append({
                    "block_id": block_id, "population": population,
                    "reason": reason,
                    "attempt": None if decision is None else decision.get("attempt"),
                    "planned_block_index": None if decision is None else decision.get("planned_block_index"),
                    "attribution": None if decision is None else decision.get("attribution"),
                    "gateways_with_success": sorted(meds),
                })
                continue
            usable.append((block_id, meds))
        for contrast, left, right in RQ1_CONTRASTS:
            diffs = [meds[left] - meds[right] for _, meds in usable]
            if not diffs:
                continue
            est = round(statistics.fmean(diffs), 4)
            lo, hi = _bootstrap_ci(diffs, seed)
            rows.append({
                "experiment_id": experiment_id,
                "collection_id": collection,
                "phase": phase,
                "question": "RQ1",
                "outcome": "median_latency_overhead",
                "contrast": contrast,
                "population": population,
                "independent_units": "block",
                "n": len(diffs),
                "estimate": est,
                "unit": "ms",
                "effect_size": est,
                "ci_lower": lo,
                "ci_upper": hi,
                "method": "mean_of_paired_block_median_differences_bootstrap_percentile",
                "seed": str(seed),
                "p_raw": None,
                "p_adjusted": None,
                "margin": None,
                "decision": None,
                "expected": None,
                "observed": None,
                "pass_fail": None,
                "analysis_id": aid,
                "evidence_role": role,
            })
    return rows, excluded


def _facts_by_run(data_dir):
    fh, fr = _read(os.path.join(data_dir, "execution_facts.csv"))
    facts = _dict_rows(fh, fr)
    # Measured-window fact wins when both windows exist for a run id.
    out = {}
    for fact in facts:
        run_id = fact.get("run_id")
        if not run_id:
            continue
        if fact.get("window") == "warmup" and run_id in out:
            continue
        out[run_id] = fact
    return out


def build(data_dir, runs_out, comparisons_out, experiment_id, phase, seed,
          evidence_role="exploratory"):
    rh, rr = _read(os.path.join(data_dir, "requests.csv"))
    requests = _dict_rows(rh, rr)
    if phase:
        requests = [r for r in requests if r.get("phase") == phase]

    facts = _facts_by_run(data_dir)
    runs_rows = summarize_runs(requests, facts)
    if runs_out:
        w = CsvTableWriter("runs", runs_out)
        w.write_rows(runs_rows)
        w.close()

    decisions = load_block_decisions(data_dir)
    comparisons_rows = []
    excluded = []
    if requests:
        experiment_id = experiment_id or requests[0]["experiment_id"]
        comparisons_rows, excluded = rq1_contrasts(
            requests, experiment_id, seed, evidence_role=evidence_role,
            facts=list(facts.values()), decisions=decisions,
        )
    if comparisons_out and comparisons_rows:
        w = CsvTableWriter("comparisons", comparisons_out)
        w.write_rows(comparisons_rows)
        w.close()

    report = pairing_report(requests)
    report["excluded"] = excluded
    return {
        "runs": len(runs_rows),
        "comparisons": len(comparisons_rows),
        "analysis_id": analysis_id(),
        "complete_blocks": len(report["complete"]),
        "incomplete_blocks": len(report["incomplete"]),
        "excluded_blocks": len(excluded),
        "pairing": report,
    }


def main(argv: Optional[list] = None) -> int:
    ap = argparse.ArgumentParser(description="Derive runs.csv and comparisons.csv from raw requests.csv.")
    ap.add_argument("--data-dir", required=True)
    ap.add_argument("--runs-out", default=None)
    ap.add_argument("--comparisons-out", default=None)
    ap.add_argument("--experiment-id", default=None)
    ap.add_argument("--phase", default=None)
    ap.add_argument("--seed", type=int, default=20261105)
    ap.add_argument("--evidence-role", default="exploratory",
                    choices=["exploratory", "confirmatory"],
                    help="confirmatory is refused unless phase=final is also set")
    ap.add_argument("--pairing-report", default=None,
                    help="Write the pairing report JSON here (new file).")
    args = ap.parse_args(argv)

    protected = {
        os.path.abspath(os.path.join(args.data_dir, name))
        for name in ("requests.csv", "resources.csv", "attempts.csv",
                     "quality.csv", "resilience_events.csv", "safety_cases.csv",
                     "execution_facts.csv")
    }
    for out in (args.runs_out, args.comparisons_out, args.pairing_report):
        if not out:
            continue
        if os.path.abspath(out) in protected:
            raise SystemExit(f"refusing to overwrite raw data: {out}")
        if os.path.exists(out):
            raise SystemExit(f"refusing to overwrite existing derived file: {out}")

    evidence = args.evidence_role
    if evidence == "confirmatory" and args.phase != "final":
        raise SystemExit("confirmatory evidence_role requires --phase final")

    counts = build(args.data_dir, args.runs_out, args.comparisons_out,
                   args.experiment_id, args.phase, args.seed, evidence_role=evidence)
    if args.pairing_report:
        import json
        with open(args.pairing_report, "w", encoding="utf-8") as fh:
            json.dump({"analysis_id": counts["analysis_id"], "pairing": counts["pairing"]},
                      fh, indent=2, ensure_ascii=False)
            fh.write("\n")
    printable = {k: v for k, v in counts.items() if k != "pairing"}
    print(printable)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
