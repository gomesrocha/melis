#!/usr/bin/env python3
"""RQ1 pilot runner (deterministic profile) → schema-compliant requests.csv.

PILOT ONLY. Produces `phase=pilot` raw data to (a) verify the instrumentation
end-to-end on real containers and (b) estimate variability/duration for the
final-collection sizing. Pilot data NEVER enters the confirmatory set.

It reuses the per-request measurement logic (client wall clock; TTFB and first
SSE content for streaming; ok/http_error/transport_error/invalid_response
classification) and writes rows through the versioned instrumentation
(csv_writer + ids + schemas), so the output validates against schema 1.0.0.

Design notes / honesty:
  * A block is the independent repetition unit; the N requests per cell are
    NESTED observations (not independent reps).
  * Run ONE gateway at a time: the caller starts/stops the gateway containers.
    This script only sends requests to whatever is already up on the given URL.
  * latency_ms = wall time to full completion; ttfb_ms = first HTTP byte (SSE);
    first_content_ms = first SSE event carrying delta content. For JSON only
    latency_ms is populated (ttfb/first_content stay EMPTY — missing != zero).
  * backend_call_count is read from the deterministic /__count DELTA around each
    request (reset is coarse; we snapshot before/after per request).
  * A fast-but-invalid reply is NOT counted ok (completion_valid=false).

Usage (per gateway, after it is up):
  python run_pilot_rq1.py --gateway melis --url http://127.0.0.1:7071 \
      --count-url http://127.0.0.1:7070/__count \
      --blocks 5 --reqs-per-cell 30 --concurrency 1 8 --modes json sse \
      --out-dir ../data/pilot --experiment-name rq1-deterministic-pilot \
      --repo ../.. --phase pilot

Stdlib only.
"""
from __future__ import annotations

import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone

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


def _one_request(chat_url, payload, timeout, stream):
    """Return a dict of measured fields for a single logical request."""
    data = json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(
        chat_url, data=data, headers={"Content-Type": "application/json"}, method="POST"
    )
    t0 = time.perf_counter()
    started_at = _now_utc()
    ttfb = None
    first_content = None
    try:
        resp = urllib.request.urlopen(req, timeout=timeout)
    except urllib.error.HTTPError as e:
        return dict(status="http_error", http_status=e.code, latency_ms=(time.perf_counter() - t0) * 1000,
                    ttfb_ms=None, first_content_ms=None, timeout=False, completion_valid=False,
                    error_class=f"http_{e.code}", started_at=started_at)
    except (urllib.error.URLError, TimeoutError) as e:
        is_to = isinstance(e, TimeoutError) or "timed out" in str(getattr(e, "reason", e)).lower()
        return dict(status=("timeout" if is_to else "transport_error"), http_status=None,
                    latency_ms=(time.perf_counter() - t0) * 1000, ttfb_ms=None, first_content_ms=None,
                    timeout=is_to, completion_valid=False,
                    error_class=("deadline_exceeded" if is_to else "transport"), started_at=started_at)

    http_status = resp.status
    body = b""
    if stream:
        while True:
            chunk = resp.read1(4096) if hasattr(resp, "read1") else resp.read(4096)
            if not chunk:
                break
            if ttfb is None:
                ttfb = (time.perf_counter() - t0) * 1000
            body += chunk
            if first_content is None:
                # First SSE event that carries delta content (distinct from TTFB).
                txt = body.decode("utf-8", "replace")
                if '"content"' in txt and "delta" in txt:
                    first_content = (time.perf_counter() - t0) * 1000
    else:
        body = resp.read()
    latency = (time.perf_counter() - t0) * 1000
    ok, reason = _validate(body, stream)
    return dict(status=("ok" if ok else "invalid_response"), http_status=http_status,
                latency_ms=latency, ttfb_ms=ttfb, first_content_ms=first_content,
                timeout=False, completion_valid=ok,
                error_class=(None if ok else reason), started_at=started_at)


def run(args):
    experiment_id = ids.experiment_id(args.experiment_name, "1.0", SCHEMA_VERSION)
    collection = ids.collection_id(experiment_id, ids.new_collection_nonce())
    os.makedirs(args.out_dir, exist_ok=True)
    req_writer = CsvTableWriter("requests", os.path.join(args.out_dir, "requests.csv"))
    att_writer = CsvTableWriter("attempts", os.path.join(args.out_dir, "attempts.csv"))

    chat_url = args.url.rstrip("/") + "/v1/chat/completions"
    totals = {"ok": 0, "fail": 0, "n": 0}
    t_start = time.perf_counter()

    for b in range(args.blocks):
        block = ids.block_id(experiment_id, args.phase, b, collection)
        case = ids.case_id(experiment_id, "rq1-probe", 0)
        for mode in args.modes:
            stream = mode == "sse"
            for conc in args.concurrency:
                pair = ids.pair_id(block, case, "rq1_fixed_load", stream, conc, collection)
                run_id = ids.run_id(experiment_id, args.phase, args.gateway, block,
                                    "rq1_fixed_load", stream, conc, 0, collection)
                for i in range(args.reqs_per_cell):
                    payload = {
                        "model": args.model,
                        "messages": [{"role": "user", "content": f"rq1 pilot probe {i}"}],
                        "temperature": 0,
                        "max_tokens": args.max_tokens,
                        "stream": stream,
                    }
                    before = _get_json(args.count_url) if args.count_url else None
                    r = _one_request(chat_url, payload, args.timeout, stream)
                    after = _get_json(args.count_url) if args.count_url else None
                    bcount = None
                    if before is not None and after is not None:
                        bcount = sum(after.values()) - sum(before.values())
                    request_id = ids.request_id(run_id, pair, i)
                    req_writer.write_row({
                        "experiment_id": experiment_id, "collection_id": collection,
                        "phase": args.phase, "run_id": run_id,
                        "block_id": block, "pair_id": pair, "case_id": case, "request_id": request_id,
                        "gateway": args.gateway, "scenario": "rq1_fixed_load",
                        "backend_profile": "deterministic", "streaming": stream, "concurrency": conc,
                        "started_at_utc": r["started_at"],
                        "latency_ms": round(r["latency_ms"], 3),
                        "ttfb_ms": round(r["ttfb_ms"], 3) if r["ttfb_ms"] is not None else None,
                        "first_content_ms": round(r["first_content_ms"], 3) if r["first_content_ms"] is not None else None,
                        "status": r["status"], "http_status": r["http_status"],
                        "error_class": r["error_class"], "timeout": r["timeout"],
                        "completion_valid": r["completion_valid"],
                        "backend_call_count": bcount,
                        "requested_model": args.model, "effective_model": args.model,
                        "token_count_source": "gateway_estimate",
                    })
                    att_writer.write_row({
                        "collection_id": collection, "run_id": run_id,
                        "request_id": request_id, "attempt_index": 0,
                        "destination_id": f"dest-{args.gateway}", "started_at_utc": r["started_at"],
                        "latency_ms": round(r["latency_ms"], 3), "status": r["status"],
                        "http_status": r["http_status"], "is_probe": False, "is_fallback": False,
                    })
                    totals["n"] += 1
                    totals["ok" if r["status"] == "ok" else "fail"] += 1

    elapsed = time.perf_counter() - t_start
    summary = {
        "experiment_id": experiment_id, "phase": args.phase, "gateway": args.gateway,
        "schema_version": SCHEMA_VERSION, "totals": totals,
        "elapsed_s": round(elapsed, 2), "out_dir": os.path.abspath(args.out_dir),
        "note": "PILOT — not a final result; latency is client-side wall time",
    }
    print(json.dumps(summary, indent=2))
    return summary


def main(argv=None):
    ap = argparse.ArgumentParser(description="RQ1 pilot runner -> requests.csv (phase=pilot).")
    ap.add_argument("--gateway", required=True, choices=["direct", "melis", "litellm"])
    ap.add_argument("--url", required=True)
    ap.add_argument("--count-url", default=None)
    ap.add_argument("--model", default="mock-model")
    ap.add_argument("--blocks", type=int, default=5)
    ap.add_argument("--reqs-per-cell", type=int, default=30)
    ap.add_argument("--concurrency", type=int, nargs="+", default=[1])
    ap.add_argument("--modes", nargs="+", default=["json"], choices=["json", "sse"])
    ap.add_argument("--max-tokens", type=int, default=16)
    ap.add_argument("--timeout", type=float, default=60)
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--experiment-name", default="rq1-deterministic-pilot")
    ap.add_argument("--repo", default="../..")
    ap.add_argument("--phase", default="pilot", choices=["smoke", "pilot"])
    args = ap.parse_args(argv)
    if args.phase == "final":
        ap.error("this runner is for smoke/pilot only")
    run(args)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
