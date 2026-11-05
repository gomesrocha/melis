#!/usr/bin/env python3
"""Validation for the raw experiment CSVs and their consistency.

Checks (per PROTOCOL.md "Validação de campos obrigatórios, chaves, duplicatas,
perdas de registros e consistência entre requisições e resumos"):

  1. Header matches the versioned schema exactly (order + names).
  2. ``schema_version`` on every row matches the module version.
  3. Required fields are present and non-empty.
  4. Enum/bool/int/float cells are well-formed; missing stays empty (not zero).
  5. Key uniqueness: no duplicate primary keys within a table.
  6. Referential integrity: attempts/quality/resources reference a known
     requests/runs row where the schema implies it.
  7. Record-loss / consistency between requests and runs summaries:
     ``runs.n_requests`` equals the number of requests.csv rows for that run,
     ``runs.n_ok``/``n_failed`` match the per-status counts, and failures are
     NOT dropped (a run with failures must still account for them).

The validator never mutates data. It returns a structured report and a boolean
``ok``. Run as a script to validate a directory and emit a JSON report.

Usage:
  python validate.py --data-dir ../data/pilot --report validation_report.json

Stdlib only.
"""
from __future__ import annotations

import argparse
import csv
import glob
import json
import os
from collections import Counter, defaultdict
from typing import Optional

from schemas import (
    BOOL,
    ENUM,
    FLOAT,
    INT,
    SCHEMA_VERSION,
    columns,
    header,
    key_columns,
    table_names,
)

_BOOL_OK = {"true", "false", ""}


def _read_csv(path: str):
    with open(path, newline="", encoding="utf-8") as fh:
        reader = csv.reader(fh)
        rows = list(reader)
    if not rows:
        return [], []
    return rows[0], rows[1:]


def _cell_errors(table: str, header_row, data_rows) -> list:
    errs = []
    cols = columns(table)
    name_to_col = {c.name: c for c in cols}
    idx = {name: i for i, name in enumerate(header_row)}
    for rnum, row in enumerate(data_rows, start=2):  # line 1 is header
        if len(row) != len(header_row):
            errs.append(f"{table}:{rnum}: has {len(row)} cells, expected {len(header_row)}")
            continue
        for name, col in name_to_col.items():
            val = row[idx[name]]
            if val == "":
                if col.required:
                    errs.append(f"{table}:{rnum}: required '{name}' is empty")
                continue
            if name == "schema_version" and val != SCHEMA_VERSION:
                errs.append(f"{table}:{rnum}: schema_version {val!r} != {SCHEMA_VERSION!r}")
            if col.kind == BOOL and val not in _BOOL_OK:
                errs.append(f"{table}:{rnum}: bool '{name}'={val!r} not true/false")
            elif col.kind == ENUM and col.enum is not None and val not in col.enum:
                errs.append(f"{table}:{rnum}: enum '{name}'={val!r} not in {col.enum}")
            elif col.kind == INT:
                try:
                    int(val)
                except ValueError:
                    errs.append(f"{table}:{rnum}: int '{name}'={val!r} not an integer")
            elif col.kind == FLOAT:
                try:
                    float(val)
                except ValueError:
                    errs.append(f"{table}:{rnum}: float '{name}'={val!r} not a number")
    return errs


def _duplicate_errors(table: str, header_row, data_rows) -> list:
    keys = key_columns(table)
    idx = {name: i for i, name in enumerate(header_row)}
    if any(k not in idx for k in keys):
        return [f"{table}: key column(s) missing from header; cannot check duplicates"]
    seen = Counter()
    for row in data_rows:
        composite = tuple(row[idx[k]] for k in keys)
        seen[composite] += 1
    return [f"{table}: duplicate key {k} appears {n} times" for k, n in seen.items() if n > 1]


def _load(table_path: str):
    h, rows = _read_csv(table_path)
    return h, rows


def validate_dir(data_dir: str) -> dict:
    report = {
        "data_dir": os.path.abspath(data_dir),
        "schema_version": SCHEMA_VERSION,
        "tables": {},
        "errors": [],
        "warnings": [],
    }
    loaded = {}
    for table in table_names():
        path = os.path.join(data_dir, f"{table}.csv")
        if table == "comparisons" and not os.path.exists(path):
            alts = sorted(glob.glob(os.path.join(data_dir, "comparisons.*.csv")))
            if len(alts) == 1:
                path = alts[0]
            elif len(alts) > 1:
                report["errors"].append(
                    f"comparisons: multiple derived files {alts}; refusing to pick one"
                )
                continue
        if not os.path.exists(path):
            report["tables"][table] = {"present": False, "rows": 0}
            continue
        h, rows = _load(path)
        loaded[table] = (h, rows)
        tinfo = {"present": True, "rows": len(rows)}
        # 1. header
        if h != header(table):
            report["errors"].append(
                f"{table}: header mismatch\n  got={h}\n  want={header(table)}"
            )
        else:
            report["errors"].extend(_cell_errors(table, h, rows))
            report["errors"].extend(_duplicate_errors(table, h, rows))
        report["tables"][table] = tinfo

    _referential_checks(loaded, report)
    _requests_runs_consistency(loaded, report)
    _global_request_ids(loaded, report)
    _phase_directory(data_dir, loaded, report)
    _rq1_population(loaded, report)
    _resource_semantics(loaded, report)
    _comparative_pairing(loaded, report)
    _truncated_windows(loaded, report)

    report["ok"] = len(report["errors"]) == 0
    return report


def expected_phase(data_dir: str):
    """Phase implied by a directory name. `examples` and other names imply none."""
    base = os.path.basename(os.path.abspath(data_dir.rstrip(os.sep)))
    for phase in ("final", "pilot", "smoke"):
        if base == phase or base.startswith(phase + "-") or base.startswith(phase + "_"):
            return phase
    return None


def _global_request_ids(loaded, report):
    if "requests" not in loaded:
        return
    h, rows = loaded["requests"]
    seen = Counter(_col(h, rows, "request_id"))
    for rid, n in seen.items():
        if rid and n > 1:
            report["errors"].append(f"requests: request_id {rid!r} duplicated {n} times")


def _phase_directory(data_dir, loaded, report):
    expected = expected_phase(data_dir)
    if expected is None:
        return
    for table in ("requests", "runs", "comparisons", "execution_facts"):
        if table not in loaded:
            continue
        h, rows = loaded[table]
        if "phase" not in h:
            continue
        foreign = sorted({p for p in _col(h, rows, "phase") if p and p != expected})
        if foreign:
            report["errors"].append(
                f"{table}: phase {foreign} does not belong in {expected!r} directory "
                f"{os.path.basename(os.path.abspath(data_dir))!r}"
            )


def _rq1_population(loaded, report):
    if "comparisons" not in loaded:
        return
    h, rows = loaded["comparisons"]
    if "question" not in h or "population" not in h:
        return
    iq, ip = h.index("question"), h.index("population")
    ie = h.index("evidence_role") if "evidence_role" in h else None
    iphase = h.index("phase") if "phase" in h else None
    for n, row in enumerate(rows, start=2):
        if row[iq] != "RQ1":
            continue
        pop = row[ip]
        for piece in ("concurrency=", "streaming=", "backend_profile=", "phase="):
            if piece not in pop:
                report["errors"].append(
                    f"comparisons:{n}: RQ1 population missing {piece.rstrip('=')} stratum ({pop!r})"
                )
        if ie is not None and iphase is not None:
            if row[iphase] != "final" and row[ie] == "confirmatory":
                report["errors"].append(
                    f"comparisons:{n}: phase {row[iphase]!r} cannot be evidence_role=confirmatory"
                )


def _resource_semantics(loaded, report):
    if "resources" not in loaded:
        return
    h, rows = loaded["resources"]
    if "memory_rss_bytes" not in h:
        return
    i_rss = h.index("memory_rss_bytes")
    i_method = h.index("memory_rss_method") if "memory_rss_method" in h else None
    banned = ("anon", "memory.current", "memusage", "memory_stats.usage")
    for n, row in enumerate(rows, start=2):
        rss = row[i_rss]
        method = row[i_method].lower() if i_method is not None else ""
        if rss and not method:
            report["errors"].append(
                f"resources:{n}: memory_rss_bytes is set without memory_rss_method"
            )
        if rss and any(tok in method for tok in banned):
            report["errors"].append(
                f"resources:{n}: memory_rss_method {row[i_method]!r} is not an RSS definition"
            )


def _truncated_windows(loaded, report):
    """A duration window cut by a cap is incomplete. The rows stay in the CSV."""
    if "execution_facts" not in loaded:
        return
    h, rows = loaded["execution_facts"]
    if "truncated" not in h:
        return
    i_flag = h.index("truncated")
    i_run = h.index("run_id") if "run_id" in h else None
    for n, row in enumerate(rows, start=2):
        if row[i_flag] == "true":
            run = row[i_run] if i_run is not None else ""
            report["errors"].append(
                f"truncated: execution_facts:{n}: run {run} ended before the requested window"
            )


def _comparative_pairing(loaded, report):
    """When all three gateways appear, every measured block must include all three.

    A single-gateway smoke file is not a comparative set and is not failed here.
    """
    if "requests" not in loaded:
        return
    h, rows = loaded["requests"]
    if "gateway" not in h:
        return
    gateways = set(_col(h, rows, "gateway"))
    required = {"direct", "melis", "litellm"}
    if not required.issubset(gateways):
        return
    records = _dict_rows_local(h, rows)
    # Local import avoids a cycle if aggregate ever imports validate.
    from aggregate import pairing_report
    report_p = pairing_report(records)
    for entry in report_p["incomplete"]:
        report["errors"].append(
            "pairing: incomplete block "
            f"{entry['block_id']} in {entry['population']} "
            f"has {entry['gateways']}, missing {entry['missing']}"
        )


def _dict_rows_local(header_row, data_rows):
    return [dict(zip(header_row, r)) for r in data_rows]


def _col(h, rows, name):
    if name not in h:
        return []
    i = h.index(name)
    return [r[i] for r in rows]


def _referential_checks(loaded, report):
    if "requests" in loaded:
        rh, rrows = loaded["requests"]
        req_ids = set(zip(_col(rh, rrows, "run_id"), _col(rh, rrows, "request_id")))
        if "attempts" in loaded:
            ah, arows = loaded["attempts"]
            for run_id, request_id in zip(_col(ah, arows, "run_id"), _col(ah, arows, "request_id")):
                if (run_id, request_id) not in req_ids:
                    report["errors"].append(
                        f"attempts: references unknown requests row "
                        f"(run_id={run_id}, request_id={request_id})"
                    )
    if "runs" in loaded:
        run_ids = set(_col(*loaded["runs"], "run_id"))
        for child in ("requests", "resources", "quality", "resilience_events", "safety_cases"):
            if child in loaded:
                ch, crows = loaded[child]
                for rid in _col(ch, crows, "run_id"):
                    if rid and rid not in run_ids:
                        report["warnings"].append(
                            f"{child}: run_id {rid!r} not found in runs.csv "
                            f"(ok if runs.csv not yet generated)"
                        )


def _requests_runs_consistency(loaded, report):
    """Cross-check per-run counts in runs.csv against requests.csv.

    Detects record loss and dropped failures: a run's declared totals must
    equal the actual request rows, with failures accounted for (never discarded
    to improve numbers).
    """
    if "requests" not in loaded or "runs" not in loaded:
        return
    rh, rrows = loaded["requests"]
    run_i = rh.index("run_id")
    status_i = rh.index("status")
    per_run = defaultdict(Counter)
    per_run_total = Counter()
    for r in rrows:
        per_run[r[run_i]][r[status_i]] += 1
        per_run_total[r[run_i]] += 1

    nh, nrows = loaded["runs"]
    def col(name):
        return nh.index(name) if name in nh else None
    i_run, i_n, i_ok, i_fail = col("run_id"), col("n_requests"), col("n_ok"), col("n_failed")
    for row in nrows:
        run_id = row[i_run]
        actual_total = per_run_total.get(run_id, 0)
        actual_ok = per_run[run_id].get("ok", 0)
        actual_fail = actual_total - actual_ok
        if i_n is not None and row[i_n] != "":
            if int(row[i_n]) != actual_total:
                report["errors"].append(
                    f"runs: run_id={run_id} n_requests={row[i_n]} != "
                    f"{actual_total} requests.csv rows (record loss?)"
                )
        if i_ok is not None and row[i_ok] != "":
            if int(row[i_ok]) != actual_ok:
                report["errors"].append(
                    f"runs: run_id={run_id} n_ok={row[i_ok]} != {actual_ok} ok rows"
                )
        if i_fail is not None and row[i_fail] != "":
            if int(row[i_fail]) != actual_fail:
                report["errors"].append(
                    f"runs: run_id={run_id} n_failed={row[i_fail]} != {actual_fail} "
                    f"(failures must not be dropped)"
                )


def main(argv: Optional[list] = None) -> int:
    ap = argparse.ArgumentParser(description="Validate experiment CSVs against the versioned schema.")
    ap.add_argument("--data-dir", required=True)
    ap.add_argument("--report", default=None, help="Optional JSON report output path.")
    args = ap.parse_args(argv)

    report = validate_dir(args.data_dir)
    if args.report:
        with open(args.report, "w", encoding="utf-8") as fh:
            json.dump(report, fh, indent=2, ensure_ascii=False)
            fh.write("\n")
    print(json.dumps(
        {"ok": report["ok"], "n_errors": len(report["errors"]),
         "n_warnings": len(report["warnings"]),
         "tables": {k: v.get("rows", 0) for k, v in report["tables"].items()}},
        indent=2, ensure_ascii=False))
    for e in report["errors"]:
        print("ERROR:", e)
    for w in report["warnings"]:
        print("WARN :", w)
    return 0 if report["ok"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
