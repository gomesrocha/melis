#!/usr/bin/env python3
"""Re-read one collection and write a new audit directory.

The source directory is only read. Medians are recomputed from requests.csv.
Requests inside a block are not treated as independent replicates. The
unadjusted estimator remains the mean, across blocks, of differences of
per-execution completion medians. Stratified means are descriptive.
"""
from __future__ import annotations

import argparse
import csv
import hashlib
import json
import math
import os
import statistics
import sys
from collections import Counter, defaultdict
from datetime import datetime

HARNESS = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HARNESS)
INSTR = os.path.join(ROOT, "instrumentation")
sys.path.insert(0, HARNESS)
sys.path.insert(0, INSTR)

import aggregate  # noqa: E402
import precision  # noqa: E402
import provenance  # noqa: E402
import validate  # noqa: E402

CONTRASTS = (
    ("melis_minus_direct", "melis", "direct"),
    ("litellm_minus_direct", "litellm", "direct"),
    ("melis_minus_litellm", "melis", "litellm"),
)
GATEWAYS = ("direct", "melis", "litellm")


def _f(value):
    if value is None or value == "":
        return None
    return float(value)


def _blank(value):
    return "" if value is None else value


def file_sha256(path: str) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def pearson(xs, ys):
    pairs = [(x, y) for x, y in zip(xs, ys) if x is not None and y is not None]
    if len(pairs) < 3:
        return None
    xs = [p[0] for p in pairs]
    ys = [p[1] for p in pairs]
    mx, my = statistics.mean(xs), statistics.mean(ys)
    num = sum((x - mx) * (y - my) for x, y in zip(xs, ys))
    den = math.sqrt(sum((x - mx) ** 2 for x in xs) * sum((y - my) ** 2 for y in ys))
    if den == 0:
        return None
    return num / den


def scan_requests(path: str) -> dict:
    """One pass. Latencies are kept only for ok measured rows, per run."""
    latencies = defaultdict(list)
    per_run = {}
    by_window = Counter()
    by_status = Counter()
    by_gateway_window = Counter()
    rows = 0
    collection_ids = set()
    experiments = set()
    phases = set()
    profiles = set()
    streams = set()
    concs = set()
    features = set()
    scenarios = set()
    models = set()
    with open(path, newline="") as fh:
        reader = csv.DictReader(fh)
        for row in reader:
            rows += 1
            window = row.get("window") or ""
            status = row.get("status") or ""
            gateway = row.get("gateway") or ""
            run_id = row.get("run_id") or ""
            by_window[window] += 1
            by_status[status] += 1
            by_gateway_window[(gateway, window)] += 1
            collection_ids.add(row.get("collection_id") or "")
            experiments.add(row.get("experiment_id") or "")
            phases.add(row.get("phase") or "")
            profiles.add(row.get("backend_profile") or "")
            streams.add(row.get("streaming") or "")
            concs.add(row.get("concurrency") or "")
            features.add(row.get("feature_profile") or "")
            scenarios.add(row.get("scenario") or "")
            models.add(row.get("effective_model") or "")
            slot = per_run.setdefault(run_id, {
                "run_id": run_id,
                "block_id": row.get("block_id") or "",
                "gateway": gateway,
                "window": window,
                "n": 0,
                "n_ok": 0,
                "n_failed": 0,
                "started_min": "",
                "started_max": "",
            })
            slot["n"] += 1
            if status == "ok":
                slot["n_ok"] += 1
            else:
                slot["n_failed"] += 1
            started = row.get("started_at_utc") or ""
            if started and (not slot["started_min"] or started < slot["started_min"]):
                slot["started_min"] = started
            if started > slot["started_max"]:
                slot["started_max"] = started
            if window == "measured" and status == "ok":
                latencies[run_id].append(float(row["latency_ms"]))
    return {
        "rows": rows,
        "by_window": dict(by_window),
        "by_status": dict(by_status),
        "by_gateway_window": {f"{gw}|{window}": n for (gw, window), n in by_gateway_window.items()},
        "collection_ids": sorted(collection_ids),
        "experiments": sorted(experiments),
        "phases": sorted(phases),
        "backend_profiles": sorted(profiles),
        "streaming": sorted(streams),
        "concurrency": sorted(concs),
        "feature_profiles": sorted(features),
        "scenarios": sorted(scenarios),
        "models": sorted(models),
        "runs": per_run,
        "latencies": latencies,
    }


def read_csv(path):
    with open(path, newline="") as fh:
        return list(csv.DictReader(fh))


def block_rows(scan, facts_by_run) -> list[dict]:
    measured = [run for run in scan["runs"].values() if run["window"] == "measured"]
    by_block = defaultdict(dict)
    for run in measured:
        series = scan["latencies"].get(run["run_id"]) or []
        run["median_ms"] = statistics.median(series) if series else None
        by_block[run["block_id"]][run["gateway"]] = run
    rows = []
    for index, block_id in enumerate(sorted(by_block)):
        gateways = by_block[block_id]
        positioned = []
        for name, run in gateways.items():
            fact = facts_by_run.get(run["run_id"], {})
            position = fact.get("order_in_block")
            positioned.append((int(position) if position not in (None, "") else 99, name, run, fact))
        positioned.sort()
        order = tuple(item[1] for item in positioned)
        record = {
            "block_index": index,
            "block_id": block_id,
            "order": " ".join(order),
            "half": "first" if index < 6 else "second",
        }
        for position, name, run, fact in positioned:
            pred = "" if position == 0 else order[position - 1]
            record[f"{name}_position"] = position
            record[f"{name}_predecessor"] = pred
            record[f"{name}_run_id"] = run["run_id"]
            record[f"{name}_median_ms"] = run["median_ms"]
            record[f"{name}_n"] = run["n"]
            record[f"{name}_n_ok"] = run["n_ok"]
            record[f"{name}_n_failed"] = run["n_failed"]
            record[f"{name}_started_utc"] = run["started_min"]
            record[f"{name}_ended_utc"] = run["started_max"]
            record[f"{name}_duration_s"] = _f(fact.get("duration_s"))
            record[f"{name}_requested_duration_s"] = _f(fact.get("requested_duration_s"))
            record[f"{name}_termination_reason"] = fact.get("termination_reason") or ""
            record[f"{name}_truncated"] = fact.get("truncated") or ""
            record[f"{name}_admitted_n"] = fact.get("admitted_n") or ""
            record[f"{name}_drained_n"] = fact.get("drained_n") or ""
            record[f"{name}_throughput_per_s"] = _f(fact.get("throughput_per_s"))
        for key, left, right in CONTRASTS:
            lv = record.get(f"{left}_median_ms")
            rv = record.get(f"{right}_median_ms")
            record[key + "_ms"] = None if lv is None or rv is None else lv - rv
        record["block_started_utc"] = min(
            (record[f"{name}_started_utc"] for name in GATEWAYS if record.get(f"{name}_started_utc")),
            default="",
        )
        record["block_ended_utc"] = max(
            (record[f"{name}_ended_utc"] for name in GATEWAYS if record.get(f"{name}_ended_utc")),
            default="",
        )
        rows.append(record)
    return rows


def _parse_time(value: str):
    if not value:
        return None
    text = value.replace("Z", "+00:00")
    return datetime.fromisoformat(text)


def attach_pressure(rows, pressure):
    samples = []
    for sample in pressure:
        stamp = _parse_time(sample["timestamp_utc"])
        samples.append((stamp, sample))
    samples.sort()
    for row in rows:
        start = _parse_time(row["block_started_utc"])
        end = _parse_time(row["block_ended_utc"])
        inside = [sample for stamp, sample in samples if start and end and start <= stamp <= end]
        row["pressure_samples"] = len(inside)
        if len(inside) < 2:
            row["load1_mean"] = None
            row["mem_available_min_bytes"] = None
            row["pswpin_delta"] = None
            row["pswpout_delta"] = None
            continue
        row["load1_mean"] = statistics.mean(float(sample["load1"]) for sample in inside)
        row["mem_available_min_bytes"] = min(int(sample["mem_available_bytes"]) for sample in inside)
        row["pswpin_delta"] = int(inside[-1]["pswpin"]) - int(inside[0]["pswpin"])
        row["pswpout_delta"] = int(inside[-1]["pswpout"]) - int(inside[0]["pswpout"])


def attach_resources(rows, resources):
    by_run = defaultdict(list)
    for sample in resources:
        by_run[sample.get("run_id") or ""].append(sample)
    for row in rows:
        for name in GATEWAYS:
            series = by_run.get(row.get(f"{name}_run_id"), [])
            cpus = [_f(sample.get("cpu_seconds_total")) for sample in series]
            cpus = [value for value in cpus if value is not None]
            working = [_f(sample.get("memory_working_set_bytes")) for sample in series]
            working = [value for value in working if value is not None]
            rss = [sample.get("memory_rss_bytes") or "" for sample in series]
            row[f"{name}_cpu_seconds"] = (max(cpus) - min(cpus)) if len(cpus) >= 2 else None
            row[f"{name}_working_set_mean_bytes"] = statistics.mean(working) if working else None
            row[f"{name}_rss_samples_empty"] = sum(1 for value in rss if value == "")
            row[f"{name}_resource_samples"] = len(series)


def contrast_summary(rows) -> list[dict]:
    out = []
    for key, _left, _right in CONTRASTS:
        values = [row[key + "_ms"] for row in rows if row.get(key + "_ms") is not None]
        out.append({
            "contrast": key,
            "n_blocks": len(values),
            "mean_ms": statistics.mean(values) if values else None,
            "sample_sd_ms": statistics.stdev(values) if len(values) > 1 else None,
            "min_ms": min(values) if values else None,
            "max_ms": max(values) if values else None,
            "estimator": "mean_of_block_median_differences",
            "role": "unadjusted",
        })
    return out


def subgroup_rows(rows) -> list[dict]:
    """Descriptive slices. They do not replace the unadjusted mean."""
    out = []
    for key, left, _right in CONTRASTS:
        slices = [("all", rows), ("half:first", [r for r in rows if r["half"] == "first"]),
                  ("half:second", [r for r in rows if r["half"] == "second"])]
        for position in (0, 1, 2):
            slices.append((
                f"position:{position}",
                [r for r in rows if r.get(f"{left}_position") == position],
            ))
        for label, subset in slices:
            values = [r[key + "_ms"] for r in subset if r.get(key + "_ms") is not None]
            out.append({
                "contrast": key,
                "slice": label,
                "n_blocks": len(values),
                "mean_ms": statistics.mean(values) if values else None,
                "sample_sd_ms": statistics.stdev(values) if len(values) > 1 else None,
                "role": "descriptive_slice",
            })
    return out


def association_rows(rows) -> list[dict]:
    index = [row["block_index"] for row in rows]
    out = []
    covariates = (
        ("block_index", index),
        ("load1_mean", [row.get("load1_mean") for row in rows]),
        ("mem_available_min_bytes", [row.get("mem_available_min_bytes") for row in rows]),
        ("pswpin_delta", [row.get("pswpin_delta") for row in rows]),
        ("pswpout_delta", [row.get("pswpout_delta") for row in rows]),
    )
    for name in GATEWAYS:
        covariates = covariates + (
            (f"{name}_cpu_seconds", [row.get(f"{name}_cpu_seconds") for row in rows]),
            (f"{name}_working_set_mean_bytes", [row.get(f"{name}_working_set_mean_bytes") for row in rows]),
        )
    for key, _left, _right in CONTRASTS:
        ys = [row.get(key + "_ms") for row in rows]
        for name, xs in covariates:
            present = sum(value is not None for value in xs)
            out.append({
                "contrast": key,
                "covariate": name,
                "n_pairs": sum(
                    x is not None and y is not None for x, y in zip(xs, ys)
                ),
                "n_covariate_present": present,
                "pearson_r": pearson(xs, ys),
                "role": "descriptive_association",
            })
    return out


def write_dicts(path, rows, fieldnames):
    with open(path, "w", newline="", encoding="utf-8") as fh:
        writer = csv.DictWriter(fh, fieldnames=fieldnames, extrasaction="ignore", restval="")
        writer.writeheader()
        for row in rows:
            writer.writerow({key: _blank(row.get(key)) for key in fieldnames})


def column_dictionary() -> list[dict]:
    return [
        {"file": "block_contrasts.csv", "column": "block_index", "unit": "index",
         "meaning": "0-based order of the block in this collection", "missing": "not used"},
        {"file": "block_contrasts.csv", "column": "*_median_ms", "unit": "ms",
         "meaning": "median completion latency of ok measured requests in that execution",
         "missing": "empty when the execution has no ok latency"},
        {"file": "block_contrasts.csv", "column": "*_minus_*_ms", "unit": "ms",
         "meaning": "difference of the two execution medians in the same block",
         "missing": "empty when either median is missing"},
        {"file": "block_contrasts.csv", "column": "*_duration_s", "unit": "s",
         "meaning": "effective wall time including drain of in-flight requests",
         "missing": "empty when the execution fact is absent"},
        {"file": "block_contrasts.csv", "column": "*_requested_duration_s", "unit": "s",
         "meaning": "configured window; not the effective duration", "missing": "empty"},
        {"file": "block_contrasts.csv", "column": "*_throughput_per_s", "unit": "1/s",
         "meaning": "finished requests, ok and failed, divided by effective duration",
         "missing": "empty"},
        {"file": "block_contrasts.csv", "column": "*_drained_n", "unit": "count",
         "meaning": "in-flight requests drained after admission stopped", "missing": "empty"},
        {"file": "block_contrasts.csv", "column": "pswpin_delta", "unit": "pages",
         "meaning": "swap pages in during pressure samples inside the block span",
         "missing": "empty when fewer than two samples fall inside the span"},
        {"file": "block_contrasts.csv", "column": "load1_mean", "unit": "runnable_processes",
         "meaning": "mean load1 of pressure samples inside the block span", "missing": "empty"},
        {"file": "descriptive_slices.csv", "column": "mean_ms", "unit": "ms",
         "meaning": "unweighted mean of block differences inside the slice",
         "missing": "empty when the slice is empty"},
        {"file": "descriptive_association.csv", "column": "pearson_r", "unit": "correlation",
         "meaning": "descriptive Pearson correlation across blocks; not a confirmatory test",
         "missing": "empty when fewer than 3 complete pairs or either side has no variance"},
        {"file": "planning_n.csv", "column": "half_width_plugin_ms", "unit": "ms",
         "meaning": "marginal 95% t half-width using the pilot sample sd as if plugged in",
         "missing": "not used"},
        {"file": "planning_n.csv", "column": "half_width_sigma_upper_ms", "unit": "ms",
         "meaning": "same half-width using the chi-square 95% upper bound of sigma",
         "missing": "not used"},
        {"file": "planning_n.csv", "column": "configured_load_s", "unit": "s",
         "meaning": "N blocks times 3 paths times 80 s; excludes startup and switching",
         "missing": "not used"},
        {"file": "planning_n.csv", "column": "switch_overhead_s", "unit": "s",
         "meaning": "N times the per-block overhead observed in this pilot",
         "missing": "empty when the observed wall clock was not supplied"},
        {"file": "planning_n.csv", "column": "approved", "unit": "boolean",
         "meaning": "always false in this export", "missing": "not used"},
    ]


def run_audit(data_dir: str, out_dir: str, process_wall_s: float | None) -> dict:
    data_dir = os.path.abspath(data_dir)
    out_dir = os.path.abspath(out_dir)
    if os.path.commonpath([data_dir, out_dir]) == data_dir:
        raise SystemExit("refusing to write the audit inside the source collection")
    os.makedirs(out_dir, exist_ok=True)
    requests_path = os.path.join(data_dir, "requests.csv")
    manifest_path = os.path.join(data_dir, "manifest.json")
    scan = scan_requests(requests_path)
    facts = {row["run_id"]: row for row in read_csv(os.path.join(data_dir, "execution_facts.csv"))
             if row.get("window") == "measured"}
    # Warmup facts stay available for the reconciliation of both windows.
    all_facts = read_csv(os.path.join(data_dir, "execution_facts.csv"))
    rows = block_rows(scan, facts)
    attach_pressure(rows, read_csv(os.path.join(data_dir, "host_pressure.csv")))
    attach_resources(rows, read_csv(os.path.join(data_dir, "resources.csv")))
    summaries = contrast_summary(rows)
    slices = subgroup_rows(rows)
    associations = association_rows(rows)

    analysis = aggregate.analysis_id()
    aggregate.build(
        data_dir,
        os.path.join(out_dir, f"runs.{analysis}.csv"),
        os.path.join(out_dir, f"comparisons.{analysis}.csv"),
        scan["experiments"][0] if scan["experiments"] else None,
        "pilot",
        20261105,
    )
    validation = validate.validate_dir(data_dir)
    with open(os.path.join(out_dir, "validation_reread.json"), "w", encoding="utf-8") as fh:
        json.dump(validation, fh, indent=2)
        fh.write("\n")

    manifest_sha = file_sha256(manifest_path)
    with open(manifest_path, encoding="utf-8") as fh:
        manifest = json.load(fh)
    switch_per_block = None
    if process_wall_s is not None:
        configured = 12 * 3 * 80
        switch_per_block = (process_wall_s - configured) / 12
    planning = []
    for summary in summaries:
        planning.extend(precision.planning_rows(
            contrast=summary["contrast"],
            sample_sd_ms=summary["sample_sd_ms"],
            pilot_blocks=summary["n_blocks"],
            observed_switch_s_per_block=switch_per_block,
        ))

    contrast_fields = ["block_index", "block_id", "half", "order", "block_started_utc", "block_ended_utc",
                       "pressure_samples", "load1_mean", "mem_available_min_bytes",
                       "pswpin_delta", "pswpout_delta"]
    for name in GATEWAYS:
        contrast_fields.extend([
            f"{name}_position", f"{name}_predecessor", f"{name}_run_id", f"{name}_median_ms",
            f"{name}_n", f"{name}_n_ok", f"{name}_n_failed", f"{name}_started_utc",
            f"{name}_ended_utc", f"{name}_duration_s", f"{name}_requested_duration_s",
            f"{name}_termination_reason", f"{name}_truncated", f"{name}_admitted_n",
            f"{name}_drained_n", f"{name}_throughput_per_s", f"{name}_cpu_seconds",
            f"{name}_working_set_mean_bytes", f"{name}_rss_samples_empty",
            f"{name}_resource_samples",
        ])
    for key, _left, _right in CONTRASTS:
        contrast_fields.append(key + "_ms")
    write_dicts(os.path.join(out_dir, "block_contrasts.csv"), rows, contrast_fields)
    write_dicts(
        os.path.join(out_dir, "contrast_summary.csv"), summaries,
        ["contrast", "n_blocks", "mean_ms", "sample_sd_ms", "min_ms", "max_ms", "estimator", "role"],
    )
    write_dicts(
        os.path.join(out_dir, "descriptive_slices.csv"), slices,
        ["contrast", "slice", "n_blocks", "mean_ms", "sample_sd_ms", "role"],
    )
    write_dicts(
        os.path.join(out_dir, "descriptive_association.csv"), associations,
        ["contrast", "covariate", "n_pairs", "n_covariate_present", "pearson_r", "role"],
    )
    planning_fields = list(planning[0].keys()) if planning else []
    write_dicts(os.path.join(out_dir, "planning_n.csv"), planning, planning_fields)
    write_dicts(
        os.path.join(out_dir, "columns.csv"), column_dictionary(),
        ["file", "column", "unit", "meaning", "missing"],
    )

    measured_n = sum(run["n"] for run in scan["runs"].values() if run["window"] == "measured")
    warmup_n = sum(run["n"] for run in scan["runs"].values() if run["window"] == "warmup")
    fact_measured = [fact for fact in all_facts if fact.get("window") == "measured"]
    orders = Counter(row["order"] for row in rows)
    report = {
        "collection_ids": scan["collection_ids"],
        "experiments": scan["experiments"],
        "request_rows": scan["rows"],
        "request_rows_plus_header": scan["rows"] + 1,
        "measured_requests": measured_n,
        "warmup_requests": warmup_n,
        "rows_accounted": measured_n + warmup_n,
        "by_window": scan["by_window"],
        "by_status": scan["by_status"],
        "by_gateway_window": scan["by_gateway_window"],
        "cell": {
            "backend_profiles": scan["backend_profiles"],
            "streaming": scan["streaming"],
            "concurrency": scan["concurrency"],
            "feature_profiles": scan["feature_profiles"],
            "scenarios": scan["scenarios"],
            "models": scan["models"],
            "phases": scan["phases"],
        },
        "blocks": len(rows),
        "permutation_counts": dict(orders),
        "measured_facts": len(fact_measured),
        "termination_reasons": dict(Counter(fact.get("termination_reason") for fact in all_facts)),
        "truncated": dict(Counter(fact.get("truncated") for fact in all_facts)),
        "failed_measured": sum(run["n_failed"] for run in scan["runs"].values() if run["window"] == "measured"),
        "contrasts": summaries,
        "validation_ok": validation["ok"],
        "validation_errors": validation["errors"],
        "analysis_id": analysis,
        "aggregate_py_sha256": file_sha256(aggregate.__file__),
        "audit_py_sha256": file_sha256(os.path.abspath(__file__)),
        "precision_py_sha256": file_sha256(precision.__file__),
        "original_manifest_sha256": manifest_sha,
        "original_requests_sha256": file_sha256(requests_path),
        "manifest_commit": (manifest.get("artifact") or {}).get("commit"),
        "manifest_collection_id": (manifest.get("extra") or {}).get("collection_id"),
        "process_wall_s": process_wall_s,
        "switch_overhead_s_per_block": switch_per_block,
        "n_approved": False,
        "targets_approved": False,
    }
    with open(os.path.join(out_dir, "audit_summary.json"), "w", encoding="utf-8") as fh:
        json.dump(report, fh, indent=2)
        fh.write("\n")
    return report


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description="Audit one existing collection into a new directory")
    parser.add_argument("--data-dir", required=True)
    parser.add_argument("--out-dir", required=True)
    parser.add_argument("--process-wall-s", type=float, default=None)
    args = parser.parse_args(argv)
    report = run_audit(args.data_dir, args.out_dir, args.process_wall_s)
    print(json.dumps({
        "blocks": report["blocks"],
        "measured_requests": report["measured_requests"],
        "warmup_requests": report["warmup_requests"],
        "request_rows": report["request_rows"],
        "failed_measured": report["failed_measured"],
        "validation_ok": report["validation_ok"],
        "analysis_id": report["analysis_id"],
        "contrasts": report["contrasts"],
    }, indent=2))
    return 0 if report["validation_ok"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
