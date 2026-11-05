#!/usr/bin/env python3
"""Common client harness for the three-path gateway comparison.

Client-side measurements (the same client for all paths) plus, when available,
backend-side call counting from the deterministic mock (/__count). Distinguishes:
  - http_error        : a non-2xx HTTP response
  - transport_error   : connection/timeout failures (no HTTP response)
  - invalid_response  : 2xx but not a usable OpenAI-compatible chat/SSE body
  - ok                : a functionally valid response

This is a SMOKE / PILOT harness, not the final evaluation. Every run records the
artifact id (Melis commit), gateway versions/digests, config, and limits passed in.
No warm-up/repetition policy is fixed here; the final protocol will define that.

Usage:
  python bench_client.py \
      --path direct|melis|litellm --url http://127.0.0.1:7070 \
      --model mock-model --mode json|sse --n 20 \
      --count-url http://127.0.0.1:7070/__count   # optional (deterministic only)
      --label smoke --out results.jsonl --meta meta.json
"""
import argparse
import json
import statistics
import sys
import time
import urllib.error
import urllib.request


def post(url, payload, timeout, stream):
    data = json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(
        url, data=data, headers={"Content-Type": "application/json"}, method="POST"
    )
    t0 = time.perf_counter()
    first_byte = None
    try:
        resp = urllib.request.urlopen(req, timeout=timeout)
    except urllib.error.HTTPError as e:
        return {"outcome": "http_error", "status": e.code, "ms": (time.perf_counter() - t0) * 1000}
    except Exception as e:
        return {"outcome": "transport_error", "error": str(e), "ms": (time.perf_counter() - t0) * 1000}

    status = resp.status
    body = b""
    if stream:
        # Read chunk-by-chunk; record time-to-first-byte and total.
        while True:
            chunk = resp.read1(4096) if hasattr(resp, "read1") else resp.read(4096)
            if not chunk:
                break
            if first_byte is None:
                first_byte = (time.perf_counter() - t0) * 1000
            body += chunk
    else:
        body = resp.read()
    total_ms = (time.perf_counter() - t0) * 1000

    ok, reason = validate(body, stream)
    return {
        "outcome": "ok" if ok else "invalid_response",
        "status": status,
        "ms": total_ms,
        "ttfb_ms": first_byte,
        "reason": None if ok else reason,
        "bytes": len(body),
    }


def validate(body, stream):
    try:
        text = body.decode("utf-8", "replace")
    except Exception:
        return False, "undecodable"
    if stream:
        if "data:" not in text:
            return False, "no sse data lines"
        if "[DONE]" not in text:
            return False, "no [DONE] terminator"
        return True, None
    try:
        obj = json.loads(text)
    except Exception:
        return False, "not json"
    try:
        c = obj["choices"][0]["message"]["content"]
        return (isinstance(c, str) and len(c) > 0), None if c else "empty content"
    except Exception:
        return False, "missing choices[0].message.content"


def get_json(url, timeout=5):
    try:
        with urllib.request.urlopen(url, timeout=timeout) as r:
            return json.loads(r.read())
    except Exception:
        return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--path", required=True)
    ap.add_argument("--url", required=True, help="base url, e.g. http://127.0.0.1:7071")
    ap.add_argument("--model", required=True)
    ap.add_argument("--mode", choices=["json", "sse"], default="json")
    ap.add_argument("--n", type=int, default=20)
    ap.add_argument("--max-tokens", type=int, default=16)
    ap.add_argument("--timeout", type=float, default=600)
    ap.add_argument("--count-url", default=None)
    ap.add_argument("--label", default="smoke")
    ap.add_argument("--out", default=None)
    ap.add_argument("--meta", default=None)
    args = ap.parse_args()

    if args.label not in ("smoke", "pilot"):
        print("WARNING: this harness is for smoke/pilot only, not final results", file=sys.stderr)

    chat_url = args.url.rstrip("/") + "/v1/chat/completions"
    stream = args.mode == "sse"

    # Reset backend counters if a deterministic count endpoint is provided.
    reset_url = args.count_url.replace("/__count", "/__reset") if args.count_url else None
    if reset_url:
        try:
            urllib.request.urlopen(urllib.request.Request(reset_url, data=b"{}", method="POST"), timeout=5).read()
        except Exception:
            pass

    rows = []
    for i in range(args.n):
        payload = {
            "model": args.model,
            "messages": [{"role": "user", "content": f"bench probe {i}"}],
            "temperature": 0,
            "max_tokens": args.max_tokens,
            "stream": stream,
        }
        r = post(chat_url, payload, args.timeout, stream)
        r["i"] = i
        rows.append(r)

    backend_counts = get_json(args.count_url) if args.count_url else None

    oks = [r["ms"] for r in rows if r["outcome"] == "ok"]
    summary = {
        "label": args.label,
        "path": args.path,
        "mode": args.mode,
        "model": args.model,
        "n": args.n,
        "ok": sum(1 for r in rows if r["outcome"] == "ok"),
        "http_error": sum(1 for r in rows if r["outcome"] == "http_error"),
        "transport_error": sum(1 for r in rows if r["outcome"] == "transport_error"),
        "invalid_response": sum(1 for r in rows if r["outcome"] == "invalid_response"),
        "latency_ms": {
            "min": round(min(oks), 3) if oks else None,
            "median": round(statistics.median(oks), 3) if oks else None,
            "p95": round(sorted(oks)[int(len(oks) * 0.95) - 1], 3) if len(oks) >= 20 else None,
            "max": round(max(oks), 3) if oks else None,
            "mean": round(statistics.fmean(oks), 3) if oks else None,
        },
        "backend_counts": backend_counts,
        "artifact": load_meta(args.meta),
        "note": "SMOKE/PILOT — not a final result; latency is client-side wall time",
    }
    print(json.dumps(summary, indent=2))
    if args.out:
        with open(args.out, "a") as f:
            for r in rows:
                f.write(json.dumps({**r, "path": args.path, "mode": args.mode, "label": args.label}) + "\n")


def load_meta(path):
    if not path:
        return None
    try:
        with open(path) as f:
            return json.load(f)
    except Exception:
        return None


if __name__ == "__main__":
    main()
