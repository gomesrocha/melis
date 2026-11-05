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


def run_cell(base_url, model, mode, concurrency, n_requests, max_tokens, timeout):
    """Run one closed-loop cell with `concurrency` workers; return list of result dicts."""
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
    return results, wall


def run(args):
    experiment_id = ids.experiment_id(args.experiment_name, "1.0", SCHEMA_VERSION)
    os.makedirs(args.out_dir, exist_ok=True)
    req_writer = CsvTableWriter("requests", os.path.join(args.out_dir, "requests.csv"))

    # Optional MEASURED resource sampling for the duration of this run. Rows go to
    # resources.csv keyed by a single run_id covering the whole generation (the
    # per-cell run_ids are in requests.csv; resources are sampled process-wide).
    sampler = None
    if getattr(args, "sample_containers", None):
        from resource_sampler import ResourceSampler
        sampler_run_id = ids.run_id(experiment_id, args.phase, args.gateway, "ALL",
                                    "rq1_fixed_load", False, 0)
        sampler = ResourceSampler(sampler_run_id, args.out_dir, args.sample_containers,
                                  interval=args.sample_interval).start()

    totals = {"ok": 0, "fail": 0, "n": 0}
    cells_summary = []
    for b in range(args.blocks):
        block = ids.block_id(experiment_id, args.phase, b)
        case = ids.case_id(experiment_id, "rq1-conc", 0)
        for mode in args.modes:
            stream = mode == "sse"
            for conc in args.concurrency:
                pair = ids.pair_id(block, case, "rq1_fixed_load", stream, conc)
                run_id = ids.run_id(experiment_id, args.phase, args.gateway, block,
                                    "rq1_fixed_load", stream, conc)
                # Reset backend inflight peak for this cell if available.
                if args.inflight_url:
                    try:
                        urllib.request.urlopen(urllib.request.Request(
                            args.inflight_url.replace("/__inflight", "/__reset"),
                            data=b"{}", method="POST"), timeout=5).read()
                    except Exception:
                        pass
                results, wall = run_cell(args.url, args.model, mode, conc,
                                         args.reqs_per_cell, args.max_tokens, args.timeout)
                peak = None
                if args.inflight_url:
                    j = _get_json(args.inflight_url)
                    peak = j.get("peak_inflight") if isinstance(j, dict) else None
                for r in results:
                    if r is None:
                        continue
                    request_id = ids.request_id(run_id, pair, r["i"])
                    req_writer.write_row({
                        "experiment_id": experiment_id, "phase": args.phase, "run_id": run_id,
                        "block_id": block, "pair_id": pair, "case_id": case,
                        "request_id": request_id, "gateway": args.gateway,
                        "scenario": "rq1_fixed_load", "backend_profile": args.backend_profile,
                        "streaming": stream, "concurrency": conc,
                        "started_at_utc": r["started_at"],
                        "latency_ms": round(r["latency_ms"], 3),
                        "ttfb_ms": round(r["ttfb_ms"], 3) if r.get("ttfb_ms") is not None else None,
                        "first_content_ms": round(r["first_content_ms"], 3) if r.get("first_content_ms") is not None else None,
                        "status": r["status"], "http_status": r.get("http_status"),
                        "error_class": r.get("error_class"), "timeout": r["timeout"],
                        "completion_valid": r["completion_valid"],
                        "requested_model": args.model, "effective_model": args.model,
                        "token_count_source": "gateway_estimate",
                    })
                    totals["n"] += 1
                    totals["ok" if r["status"] == "ok" else "fail"] += 1
                oks = sum(1 for r in results if r and r["status"] == "ok")
                cells_summary.append({
                    "block": b, "mode": mode, "concurrency": conc,
                    "requests": args.reqs_per_cell, "ok": oks, "wall_s": round(wall, 3),
                    "achieved_rps": round(oks / wall, 2) if wall > 0 else None,
                    "backend_peak_inflight": peak,
                })

    resource_samples = sampler.stop() if sampler is not None else 0
    summary = {
        "experiment_id": experiment_id, "phase": args.phase, "gateway": args.gateway,
        "schema_version": SCHEMA_VERSION, "totals": totals, "cells": cells_summary,
        "resource_samples": resource_samples,
        "note": "CLOSED-LOOP concurrent load; SMOKE/PILOT — not a final result",
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
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--experiment-name", default="rq1-concurrent")
    ap.add_argument("--phase", default="pilot", choices=["smoke", "pilot"])
    args = ap.parse_args(argv)
    run(args)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
