#!/usr/bin/env python3
"""Real parallel CLOSED-LOOP load generator for the three-path comparison.

Unlike the sequential pilot runner, here `concurrency` is a REAL number of
simultaneous in-flight requests, not a recorded factor. It is the same client
for the direct, Melis and LiteLLM paths (only the base URL differs).

Closed-loop model
-----------------
For each (block, mode, concurrency) cell we start `concurrency` worker threads.
Each worker runs a loop: claim the next request index from a shared atomic
counter, issue ONE request, record it, repeat — until the cell's request budget
is exhausted. So at any moment up to `concurrency` requests are in flight
(closed loop: a worker only starts its next request after its previous one
completes). This is the standard closed-loop generator; open-loop arrival
scheduling is a separate future mode (documented, not implemented here).

Connection pool & reuse policy (documented)
-------------------------------------------
* Each worker owns ONE persistent HTTP/1.1 connection (`http.client`) to the
  target and REUSES it across all of its requests (keep-alive). So a cell uses
  exactly `concurrency` connections — matching the number of simultaneous
  clients, which is what we want to measure.
* On any transport error the worker reconnects (a fresh connection) before its
  next request, so one broken connection does not poison the rest of the cell.
* Streaming (SSE) responses are fully drained per request before the connection
  is reused, so framing never bleeds across requests.
* The SAME connection policy is used for all three paths.

Measurements (client-side)
---------------------------
Per request: wall-clock latency to completion; for SSE also TTFB (first HTTP
byte) and first-content (first SSE event with delta content) as DISTINCT values.
Classification: ok / http_error / transport_error / invalid_response / timeout.
Rows are written through the versioned instrumentation (schema 1.0.0). A block
is the independent unit; the requests within a cell are nested observations.

Backend-side overlap (optional)
-------------------------------
If `--inflight-url` is given (deterministic backend `/__inflight`), the cell
records the backend-observed PEAK simultaneous in-flight requests, proving that
concurrency>1 actually overlaps at the backend (see also the Rust overlap test).

PILOT/SMOKE ONLY. Raw data stays local. Stdlib only.
"""
from __future__ import annotations

import argparse
import http.client
import json
import os
import sys
import threading
import time
import urllib.request
from datetime import datetime, timezone
from urllib.parse import urlparse

HARNESS = os.path.dirname(os.path.abspath(__file__))
INSTR = os.path.join(os.path.dirname(HARNESS), "instrumentation")
sys.path.insert(0, INSTR)

import ids  # noqa: E402
from csv_writer import CsvTableWriter  # noqa: E402
from schemas import SCHEMA_VERSION  # noqa: E402


def _now_utc():
    dt = datetime.now(timezone.utc)
    return dt.strftime("%Y-%m-%dT%H:%M:%S.") + f"{dt.microsecond // 1000:03d}Z"


def _get_json(url, timeout=5):
    try:
        with urllib.request.urlopen(url, timeout=timeout) as r:
            return json.loads(r.read())
    except Exception:
        return None


def _validate(body: bytes, stream: bool):
    try:
        text = body.decode("utf-8", "replace")
    except Exception:
        return False, "undecodable"
    if stream:
        if "data:" not in text:
            return False, "no_sse_data"
        if "[DONE]" not in text:
            return False, "no_done_terminator"
        return True, None
    try:
        obj = json.loads(text)
        c = obj["choices"][0]["message"]["content"]
        return (isinstance(c, str) and len(c) > 0), (None if c else "empty_content")
    except Exception:
        return False, "bad_json_or_shape"


class Conn:
    """One persistent keep-alive connection, reused across a worker's requests."""

    def __init__(self, base_url: str, timeout: float):
        self.p = urlparse(base_url)
        self.timeout = timeout
        self._c = None

    def _connect(self):
        host, port = self.p.hostname, self.p.port or 80
        self._c = http.client.HTTPConnection(host, port, timeout=self.timeout)

    def post(self, path: str, payload: dict, stream: bool):
        """Issue one request on the reused connection; reconnect on transport error.

        Returns a dict of measured fields (never raises for HTTP/transport errors).
        """
        data = json.dumps(payload).encode("utf-8")
        started = _now_utc()
        t0 = time.perf_counter()
        ttfb = None
        first_content = None
        if self._c is None:
            self._connect()
        try:
            self._c.putrequest("POST", path, skip_host=False, skip_accept_encoding=True)
            self._c.putheader("Content-Type", "application/json")
            if stream:
                self._c.putheader("Accept", "text/event-stream")
            self._c.putheader("Content-Length", str(len(data)))
            self._c.putheader("Connection", "keep-alive")
            self._c.endheaders()
            self._c.send(data)
            resp = self._c.getresponse()
        except Exception as e:  # transport error: drop + reconnect next time
            try:
                self._c.close()
            except Exception:
                pass
            self._c = None
            is_to = isinstance(e, (TimeoutError,)) or "timed out" in str(e).lower()
            return dict(status=("timeout" if is_to else "transport_error"),
                        http_status=None, latency_ms=(time.perf_counter() - t0) * 1000,
                        ttfb_ms=None, first_content_ms=None, timeout=is_to,
                        completion_valid=False,
                        error_class=("deadline_exceeded" if is_to else "transport"),
                        started_at=started)
        status = resp.status
        body = b""
        try:
            if stream:
                while True:
                    chunk = resp.read(4096)
                    if not chunk:
                        break
                    if ttfb is None:
                        ttfb = (time.perf_counter() - t0) * 1000
                    body += chunk
                    if first_content is None and b'"content"' in body and b"delta" in body:
                        first_content = (time.perf_counter() - t0) * 1000
            else:
                body = resp.read()
        except Exception as e:
            try:
                self._c.close()
            except Exception:
                pass
            self._c = None
            return dict(status="transport_error", http_status=status,
                        latency_ms=(time.perf_counter() - t0) * 1000, ttfb_ms=ttfb,
                        first_content_ms=first_content, timeout=False,
                        completion_valid=False, error_class="stream_read", started_at=started)
        latency = (time.perf_counter() - t0) * 1000
        if status >= 400:
            return dict(status="http_error", http_status=status, latency_ms=latency,
                        ttfb_ms=ttfb, first_content_ms=first_content, timeout=False,
                        completion_valid=False, error_class=f"http_{status}", started_at=started)
        ok, reason = _validate(body, stream)
        return dict(status=("ok" if ok else "invalid_response"), http_status=status,
                    latency_ms=latency, ttfb_ms=ttfb, first_content_ms=first_content,
                    timeout=False, completion_valid=ok,
                    error_class=(None if ok else reason), started_at=started)

    def close(self):
        if self._c is not None:
            try:
                self._c.close()
            except Exception:
                pass
            self._c = None


def run_cell(base_url, model, mode, concurrency, n_requests, max_tokens, timeout,
             deadline_mono=None):
    """Run one closed-loop cell with `concurrency` workers; return list of result dicts.

    `n_requests` is a cap. When `deadline_mono` is set, workers also stop once
    that monotonic deadline is reached. In-flight requests are still recorded.
    Configured concurrency is the worker count, not the observed backend peak.
    """
    stream = mode == "sse"
    path = urlparse(base_url).path.rstrip("/") + "/v1/chat/completions"
    if path == "/v1/chat/completions":
        path = "/v1/chat/completions"
    results = [None] * n_requests
    next_idx = {"i": 0}
    lock = threading.Lock()

    def worker():
        conn = Conn(base_url, timeout)
        try:
            while True:
                if deadline_mono is not None and time.perf_counter() >= deadline_mono:
                    return
                with lock:
                    i = next_idx["i"]
                    if i >= n_requests:
                        return
                    next_idx["i"] = i + 1
                payload = {
                    "model": model,
                    "messages": [{"role": "user", "content": f"conc probe {i}"}],
                    "temperature": 0,
                    "max_tokens": max_tokens,
                    "stream": stream,
                }
                r = conn.post(path, payload, stream)
                r["i"] = i
                results[i] = r
        finally:
            conn.close()

    threads = [threading.Thread(target=worker, daemon=True) for _ in range(concurrency)]
    t0 = time.perf_counter()
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    wall = time.perf_counter() - t0
    finished = [r for r in results if r is not None]
    return finished, wall


def execution_backend_calls(before_total, after_total):
    """Execution-level counter delta.

    A decrease means the counter was reset or the two reads are not the same
    series. That is ambiguous, so the result is missing rather than a guessed
    per-request count.
    """
    if before_total is None or after_total is None:
        return None
    if after_total < before_total:
        return None
    return int(after_total - before_total)


def _count_total(count_url):
    payload = _get_json(count_url)
    if not isinstance(payload, dict):
        return None
    try:
        return int(payload.get("chat", 0)) + int(payload.get("chat_stream", 0))
    except (TypeError, ValueError):
        return None


def reset_backend(inflight_url):
    reset = inflight_url.replace("/__inflight", "/__reset")
    try:
        urllib.request.urlopen(
            urllib.request.Request(reset, data=b"{}", method="POST"), timeout=5,
        ).read()
        return True
    except Exception:
        return False


def read_peak(inflight_url):
    payload = _get_json(inflight_url)
    if not isinstance(payload, dict) or "peak_inflight" not in payload:
        return None
    try:
        return int(payload["peak_inflight"])
    except (TypeError, ValueError):
        return None


class IdRegistry:
    """Rejects id collisions inside one output directory."""

    def __init__(self):
        self.request_ids = set()
        self.run_ids = set()

    def claim_run(self, run_id):
        if run_id in self.run_ids:
            raise RuntimeError(f"duplicate run_id {run_id}")
        self.run_ids.add(run_id)

    def claim_request(self, request_id):
        if request_id in self.request_ids:
            raise RuntimeError(f"duplicate request_id {request_id}")
        self.request_ids.add(request_id)


def execute_cell(cell):
    """Run one gateway cell and append raw rows.

    `cell` is a dict. Warm-up, when requested, is a separate run id and is not
    part of the measured window. Backend call counts are recorded on
    execution_facts only. Per-request backend_call_count stays empty.
    """
    mode = cell["mode"]
    stream = mode == "sse"
    conc = cell["concurrency"]
    block = cell["block_id"]
    case = cell["case_id"]
    scenario = cell.get("scenario", "rq1_fixed_load")
    pair = ids.pair_id(block, case, scenario, stream, conc)
    attempt = int(cell.get("attempt", 0))
    measured_run = ids.run_id(
        cell["experiment_id"], cell["phase"], cell["gateway"], block,
        scenario, stream, conc, attempt,
    )
    registry = cell["registry"]
    registry.claim_run(measured_run)
    req_writer = cell["req_writer"]
    fact_writer = cell["fact_writer"]
    feature = cell.get("feature_profile", "minimal")
    policy = cell.get("policy", "none")
    timeout = cell["timeout"]
    max_tokens = cell["max_tokens"]
    cap = cell["max_requests"]
    inflight = cell.get("inflight_url")
    count_url = cell.get("count_url")
    sampler = cell.get("sampler")

    warmup_s = float(cell.get("warmup_s") or 0)
    if warmup_s > 0:
        warm_run = ids.run_id(
            cell["experiment_id"], cell["phase"], cell["gateway"], block,
            "rq1_warmup", stream, conc, attempt + 1,
        )
        registry.claim_run(warm_run)
        warm_pair = ids.pair_id(block, case, "rq1_warmup", stream, conc)
        tw = time.perf_counter()
        warm, warm_wall = run_cell(
            cell["url"], cell["model"], mode, conc, cap, max_tokens, timeout,
            deadline_mono=tw + warmup_s,
        )
        _write_requests(
            req_writer, registry, warm, cell, warm_run, block, warm_pair, case,
            "rq1_warmup", stream, conc, feature, policy, "warmup",
        )
        fact_writer.write_row(_fact_row(
            cell, warm_run, block, "rq1_warmup", stream, conc, feature, policy,
            "warmup", warmup_s=None, duration_s=round(warm_wall, 3),
            backend_call_count=None, peak=None, reset_ok=False,
        ))

    reset_ok = reset_backend(inflight) if inflight else False
    before = _count_total(count_url) if count_url else None
    if sampler is not None:
        sampler.set_run(measured_run)
    t0 = time.perf_counter()
    deadline = (t0 + float(cell["measure_s"])) if cell.get("measure_s") else None
    try:
        results, wall = run_cell(
            cell["url"], cell["model"], mode, conc, cap, max_tokens, timeout,
            deadline_mono=deadline,
        )
    finally:
        if sampler is not None:
            sampler.clear_run()
    after = _count_total(count_url) if count_url else None
    peak = read_peak(inflight) if reset_ok else None
    calls = execution_backend_calls(before, after)
    _write_requests(
        req_writer, registry, results, cell, measured_run, block, pair, case,
        scenario, stream, conc, feature, policy, "measured",
    )
    fact_writer.write_row(_fact_row(
        cell, measured_run, block, scenario, stream, conc, feature, policy,
        "measured", warmup_s=warmup_s, duration_s=round(wall, 3),
        backend_call_count=calls, peak=peak, reset_ok=reset_ok,
    ))
    oks = sum(1 for r in results if r["status"] == "ok")
    return {
        "run_id": measured_run,
        "gateway": cell["gateway"],
        "mode": mode,
        "concurrency": conc,
        "ok": oks,
        "n": len(results),
        "wall_s": round(wall, 3),
        "backend_peak_inflight": peak,
        "backend_call_count": calls,
        "order_in_block": cell.get("order_in_block"),
    }


def _write_requests(writer, registry, results, cell, run_id, block, pair, case,
                    scenario, stream, conc, feature, policy, window):
    for r in results:
        request_id = ids.request_id(run_id, pair, r["i"])
        registry.claim_request(request_id)
        writer.write_row({
            "experiment_id": cell["experiment_id"],
            "phase": cell["phase"],
            "run_id": run_id,
            "block_id": block,
            "pair_id": pair,
            "case_id": case,
            "request_id": request_id,
            "gateway": cell["gateway"],
            "scenario": scenario,
            "backend_profile": cell["backend_profile"],
            "streaming": stream,
            "concurrency": conc,
            "policy": policy,
            "feature_profile": feature,
            "window": window,
            "started_at_utc": r["started_at"],
            "latency_ms": round(r["latency_ms"], 3),
            "ttfb_ms": round(r["ttfb_ms"], 3) if r.get("ttfb_ms") is not None else None,
            "first_content_ms": round(r["first_content_ms"], 3) if r.get("first_content_ms") is not None else None,
            "status": r["status"],
            "http_status": r.get("http_status"),
            "error_class": r.get("error_class"),
            "timeout": r["timeout"],
            "completion_valid": r["completion_valid"],
            "requested_model": cell["model"],
            "effective_model": cell["model"],
        })


def _fact_row(cell, run_id, block, scenario, stream, conc, feature, policy, window,
              warmup_s, duration_s, backend_call_count, peak, reset_ok):
    sampler = cell.get("sampler")
    return {
        "experiment_id": cell["experiment_id"],
        "phase": cell["phase"],
        "run_id": run_id,
        "block_id": block,
        "gateway": cell["gateway"],
        "scenario": scenario,
        "backend_profile": cell["backend_profile"],
        "streaming": stream,
        "concurrency": conc,
        "policy": policy,
        "feature_profile": feature,
        "window": window,
        "order_in_block": cell.get("order_in_block"),
        "seed": cell.get("seed"),
        "warmup_s": warmup_s,
        "duration_s": duration_s,
        "backend_call_count": backend_call_count,
        "backend_peak_inflight": peak if reset_ok else None,
        "count_scope": "execution" if backend_call_count is not None else None,
        "sampler_enabled": sampler is not None and window == "measured",
        "sampler_interval_s": getattr(sampler, "interval", None) if sampler and window == "measured" else None,
    }


def run(args):
    """Single-gateway driver.

    This runs every block of ONE gateway before any other gateway. It is a
    smoke/debug path, not the comparative block design. Comparative pilots use
    `run_comparative_pilot.py`, which interleaves the three paths inside a block.
    """
    if os.path.exists(args.out_dir) and os.listdir(args.out_dir):
        raise SystemExit(f"refusing to write into non-empty directory {args.out_dir}")
    experiment_id = ids.experiment_id(args.experiment_name, "1.0", SCHEMA_VERSION)
    os.makedirs(args.out_dir, exist_ok=True)
    req_writer = CsvTableWriter("requests", os.path.join(args.out_dir, "requests.csv"))
    fact_writer = CsvTableWriter("execution_facts", os.path.join(args.out_dir, "execution_facts.csv"))
    sampler = None
    if getattr(args, "sample_containers", None):
        from resource_sampler import ResourceSampler
        sampler = ResourceSampler(
            args.out_dir, args.sample_containers, interval=args.sample_interval,
        ).start()
    registry = IdRegistry()
    cells_summary = []
    count_url = None
    if args.inflight_url:
        count_url = args.inflight_url.replace("/__inflight", "/__count")
    try:
        for b in range(args.blocks):
            block = ids.block_id(experiment_id, args.phase, b)
            case = ids.case_id(experiment_id, "rq1-conc", 0)
            for mode in args.modes:
                for conc in args.concurrency:
                    summary = execute_cell({
                        "experiment_id": experiment_id,
                        "phase": args.phase,
                        "gateway": args.gateway,
                        "url": args.url,
                        "model": args.model,
                        "backend_profile": args.backend_profile,
                        "mode": mode,
                        "concurrency": conc,
                        "block_id": block,
                        "case_id": case,
                        "timeout": args.timeout,
                        "max_tokens": args.max_tokens,
                        "max_requests": args.reqs_per_cell,
                        "measure_s": getattr(args, "measure_s", None),
                        "warmup_s": getattr(args, "warmup_s", 0) or 0,
                        "inflight_url": args.inflight_url,
                        "count_url": count_url,
                        "sampler": sampler,
                        "registry": registry,
                        "req_writer": req_writer,
                        "fact_writer": fact_writer,
                        "feature_profile": "minimal",
                        "policy": "none",
                        "seed": str(getattr(args, "seed", "")),
                        "order_in_block": 0,
                    })
                    cells_summary.append(summary)
    finally:
        resource_samples = sampler.stop() if sampler is not None else 0
    summary = {
        "experiment_id": experiment_id,
        "phase": args.phase,
        "gateway": args.gateway,
        "schema_version": SCHEMA_VERSION,
        "cells": cells_summary,
        "resource_samples": resource_samples,
        "note": "SINGLE-GATEWAY closed loop. Not a comparative block and not a final result.",
    }
    print(json.dumps(summary, indent=2))
    return summary


def main(argv=None):
    ap = argparse.ArgumentParser(description="Real parallel closed-loop load generator -> requests.csv")
    ap.add_argument("--gateway", required=True, choices=["direct", "melis", "litellm"])
    ap.add_argument("--url", required=True)
    ap.add_argument("--backend-profile", default="deterministic", choices=["deterministic", "real"])
    ap.add_argument("--model", default="mock-model")
    ap.add_argument("--blocks", type=int, default=3)
    ap.add_argument("--reqs-per-cell", type=int, default=60)
    ap.add_argument("--concurrency", type=int, nargs="+", default=[1, 8])
    ap.add_argument("--modes", nargs="+", default=["json"], choices=["json", "sse"])
    ap.add_argument("--max-tokens", type=int, default=16)
    ap.add_argument("--timeout", type=float, default=60)
    ap.add_argument("--inflight-url", default=None, help="deterministic backend /__inflight (optional)")
    ap.add_argument("--sample-containers", nargs="+", default=None,
                    help="container names to sample MEASURED resources during the run")
    ap.add_argument("--sample-interval", type=float, default=0.5)
    ap.add_argument("--measure-s", type=float, default=None,
                    help="Measured-window seconds. With --reqs-per-cell, whichever limit comes first.")
    ap.add_argument("--warmup-s", type=float, default=0)
    ap.add_argument("--seed", default="0")
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--experiment-name", default="rq1-concurrent")
    ap.add_argument("--phase", default="pilot", choices=["smoke", "pilot"])
    args = ap.parse_args(argv)
    run(args)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
