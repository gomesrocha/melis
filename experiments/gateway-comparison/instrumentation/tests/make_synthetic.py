#!/usr/bin/env python3
"""Generate a small SYNTHETIC dataset with a known result.

This data is for testing the instrumentation scripts ONLY. It is NOT
experimental evidence and must never be presented as a result (PROTOCOL.md:
"Testes pequenos dos scripts com dados artificiais de resultado conhecido. Não
use estes dados como evidência experimental.").

Known construction (so tests can assert exact expectations):
  * 3 blocks, 3 gateways (direct/melis/litellm), 1 scenario, JSON, concurrency 1.
  * Per (block, gateway): 4 successful requests + 1 failure (timeout).
  * Latencies are fixed so the per-block median is deterministic:
      direct  block k -> [10, 10, 10, 10]  => median 10
      melis   block k -> [13, 13, 13, 13]  => median 13   (overhead +3 ms)
      litellm block k -> [16, 16, 16, 16]  => median 16   (overhead +6 ms)
  * Therefore RQ1 paired contrast melis_minus_direct = +3 ms on every block
    (n=3 independent blocks), litellm_minus_direct = +6 ms.
  * Every run has n_requests=5, n_ok=4, n_failed=1.
"""
from __future__ import annotations

import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
INSTR = os.path.dirname(HERE)
sys.path.insert(0, INSTR)

from csv_writer import CsvTableWriter  # noqa: E402
import ids  # noqa: E402
from schemas import SCHEMA_VERSION  # noqa: E402

EXPERIMENT = ids.experiment_id("synthetic-known", "1.0", SCHEMA_VERSION)
PHASE = "smoke"
LAT = {"direct": 10.0, "melis": 13.0, "litellm": 16.0}


def generate(out_dir: str):
    os.makedirs(out_dir, exist_ok=True)
    req_writer = CsvTableWriter("requests", os.path.join(out_dir, "requests.csv"))
    att_writer = CsvTableWriter("attempts", os.path.join(out_dir, "attempts.csv"))

    for b in range(3):
        block = ids.block_id(EXPERIMENT, PHASE, b)
        case = ids.case_id(EXPERIMENT, "probe", 0)
        for gw in ("direct", "melis", "litellm"):
            pair = ids.pair_id(block, case, "rq1_fixed_load", False, 1)
            run = ids.run_id(EXPERIMENT, PHASE, gw, block, "rq1_fixed_load", False, 1)
            idx = 0
            # 4 successful requests at a fixed latency.
            for _ in range(4):
                rid = ids.request_id(run, pair, idx)
                req_writer.write_row({
                    "experiment_id": EXPERIMENT, "phase": PHASE, "run_id": run,
                    "block_id": block, "pair_id": pair, "case_id": case, "request_id": rid,
                    "gateway": gw, "scenario": "rq1_fixed_load",
                    "backend_profile": "deterministic", "streaming": False, "concurrency": 1,
                    "policy": "none", "feature_profile": "minimal", "window": "measured",
                    "started_at_utc": "2026-11-05T00:00:00.000Z",
                    "latency_ms": LAT[gw], "status": "ok",
                    "http_status": 200, "timeout": False, "completion_valid": True,
                    "backend_call_count": 1, "requested_model": "mock-model",
                    "effective_model": "mock-model", "token_count_source": "gateway_estimate",
                })
                att_writer.write_row({
                    "run_id": run, "request_id": rid, "attempt_index": 0,
                    "destination_id": f"dest-{gw}", "started_at_utc": "2026-11-05T00:00:00.000Z",
                    "latency_ms": LAT[gw], "status": "ok", "http_status": 200,
                    "is_probe": False, "is_fallback": False,
                })
                idx += 1
            # 1 failure (timeout) — retained, never dropped.
            rid = ids.request_id(run, pair, idx)
            req_writer.write_row({
                "experiment_id": EXPERIMENT, "phase": PHASE, "run_id": run,
                "block_id": block, "pair_id": pair, "case_id": case, "request_id": rid,
                "gateway": gw, "scenario": "rq1_fixed_load",
                "backend_profile": "deterministic", "streaming": False, "concurrency": 1,
                "policy": "none", "feature_profile": "minimal", "window": "measured",
                "started_at_utc": "2026-11-05T00:00:00.000Z",
                # latency_ms intentionally EMPTY (missing != zero) on a timeout.
                "latency_ms": None, "status": "timeout",
                "http_status": None, "error_class": "deadline_exceeded",
                "timeout": True, "completion_valid": False,
                "requested_model": "mock-model", "effective_model": "mock-model",
            })
            att_writer.write_row({
                "run_id": run, "request_id": rid, "attempt_index": 0,
                "destination_id": f"dest-{gw}", "started_at_utc": "2026-11-05T00:00:00.000Z",
                "latency_ms": None, "status": "timeout", "is_probe": False, "is_fallback": False,
            })
    return out_dir


# Known expectations for the tests.
EXPECTED = {
    "n_blocks": 3,
    "requests_per_run": 5,
    "ok_per_run": 4,
    "failed_per_run": 1,
    "melis_minus_direct_ms": 3.0,
    "litellm_minus_direct_ms": 6.0,
    "experiment_id": EXPERIMENT,
}


if __name__ == "__main__":
    target = sys.argv[1] if len(sys.argv) > 1 else os.path.join(INSTR, "..", "data", "examples")
    generate(target)
    print("wrote synthetic dataset to", os.path.abspath(target))
