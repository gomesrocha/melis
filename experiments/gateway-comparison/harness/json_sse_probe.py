#!/usr/bin/env python3
"""Minimal timing-instrumented repro for the JSON vs SSE latency asymmetry.

Measures, per request, the distinct phases using a RAW socket so we can see
exactly when each stage happens (no urllib buffering hiding the timeline):

  t_connect   : TCP connect done
  t_sent      : request bytes flushed
  t_headers   : end of HTTP response headers seen (first useful byte)
  t_first_ctl : first body byte
  t_first_sse : first SSE event carrying delta content (SSE only; else empty)
  t_done      : full body received (completion)

It reuses ONE keep-alive connection for N requests (to separate connection
setup from per-request cost) and also offers a fresh-connection mode. It prints
per-phase medians so JSON vs SSE can be compared on FULL COMPLETION (t_done),
with the sub-phases shown so we attribute the gap to a stage with evidence.

Also toggles TCP_NODELAY on the client socket to test the Nagle/delayed-ACK
hypothesis directly (cause is NOT assumed; this is how we get evidence).

Usage:
  python json_sse_probe.py --url http://127.0.0.1:7071 --model mock-model \
      --n 30 --modes json sse --nodelay on off

Stdlib only.
"""
from __future__ import annotations

import argparse
import json
import socket
import statistics
import sys
import time
from urllib.parse import urlparse


def _median(xs):
    xs = [x for x in xs if x is not None]
    return round(statistics.median(xs), 3) if xs else None


def one_request(host, port, path, payload, stream, nodelay, reuse_state=None):
    """Perform one request on a raw socket.

    `reuse_state` is None (open a fresh connection) or a `(sock, leftover_bytes)`
    tuple from a previous keep-alive call. Returns `(timings, new_state_or_None)`.
    Any bytes read past this response's end are carried in the returned state so
    the next reused call starts cleanly (correct keep-alive framing).
    """
    body = json.dumps(payload).encode("utf-8")
    headers = (
        f"POST {path} HTTP/1.1\r\n"
        f"Host: {host}:{port}\r\n"
        "Content-Type: application/json\r\n"
        + ("Accept: text/event-stream\r\n" if stream else "")
        + f"Content-Length: {len(body)}\r\n"
        "Connection: keep-alive\r\n"
        "\r\n"
    ).encode("utf-8")

    t = {}
    t0 = time.perf_counter()
    if reuse_state is None:
        s = socket.create_connection((host, port), timeout=30)
        t["t_connect"] = (time.perf_counter() - t0) * 1000
        buf = b""
    else:
        s, buf = reuse_state
        t["t_connect"] = None  # reused
    if nodelay:
        s.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)

    s.sendall(headers + body)
    t["t_sent"] = (time.perf_counter() - t0) * 1000

    header_end = buf.find(b"\r\n\r\n")
    t_headers = None
    t_first = (time.perf_counter() - t0) * 1000 if buf else None
    t_first_sse = None
    s.settimeout(30)
    content_len = None
    body_start = header_end + 4 if header_end >= 0 else 0
    closed = False
    while True:
        # Do we already have a complete response in `buf`?
        if header_end >= 0:
            if t_headers is None:
                t_headers = (time.perf_counter() - t0) * 1000
                head = buf[:header_end].decode("latin1")
                for line in head.split("\r\n"):
                    if line.lower().startswith("content-length:"):
                        content_len = int(line.split(":", 1)[1].strip())
                if "connection: close" in head.lower():
                    closed = True
            if stream and t_first_sse is None and b'"content"' in buf[body_start:] \
                    and b"delta" in buf[body_start:]:
                t_first_sse = (time.perf_counter() - t0) * 1000
            if stream:
                done_at = buf.find(b"[DONE]", body_start)
                if done_at >= 0:
                    consumed = done_at + len(b"[DONE]")
                    break
            elif content_len is not None and len(buf) - body_start >= content_len:
                consumed = body_start + content_len
                break
        chunk = s.recv(4096)
        if not chunk:
            closed = True
            consumed = len(buf)
            break
        if t_first is None:
            t_first = (time.perf_counter() - t0) * 1000
        buf += chunk
        if header_end < 0:
            header_end = buf.find(b"\r\n\r\n")
            if header_end >= 0:
                body_start = header_end + 4
                # Record header arrival as soon as the terminator is seen, so a
                # response that fully lands in one recv still captures t_headers.
                t_headers = (time.perf_counter() - t0) * 1000
                head = buf[:header_end].decode("latin1")
                for line in head.split("\r\n"):
                    if line.lower().startswith("content-length:"):
                        content_len = int(line.split(":", 1)[1].strip())
                if "connection: close" in head.lower():
                    closed = True
    t["t_headers"] = t_headers
    t["t_first_byte"] = t_first
    t["t_first_sse"] = t_first_sse
    t["t_done"] = (time.perf_counter() - t0) * 1000
    leftover = buf[consumed:] if not closed else b""
    if closed:
        try:
            s.close()
        except OSError:
            pass
        return t, None
    return t, (s, leftover)


def run(url, model, n, mode, nodelay, reuse):
    p = urlparse(url)
    host, port = p.hostname, p.port or 80
    path = "/v1/chat/completions"
    stream = mode == "sse"
    rows = []
    state = None
    for i in range(n):
        payload = {
            "model": model,
            "messages": [{"role": "user", "content": f"probe {i}"}],
            "temperature": 0,
            "max_tokens": 16,
            "stream": stream,
        }
        t, state = one_request(host, port, path, payload, stream, nodelay,
                               reuse_state=state if reuse else None)
        if not reuse and state is not None:
            state[0].close()
            state = None
        rows.append(t)
    if state is not None:
        state[0].close()
    agg = {
        "mode": mode, "nodelay": nodelay, "reuse": reuse, "n": n,
        "t_connect_med": _median([r["t_connect"] for r in rows]),
        "t_sent_med": _median([r["t_sent"] for r in rows]),
        "t_headers_med": _median([r["t_headers"] for r in rows]),
        "t_first_byte_med": _median([r["t_first_byte"] for r in rows]),
        "t_first_sse_med": _median([r["t_first_sse"] for r in rows]),
        "t_done_med": _median([r["t_done"] for r in rows]),
    }
    return agg


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", required=True)
    ap.add_argument("--model", default="mock-model")
    ap.add_argument("--n", type=int, default=30)
    ap.add_argument("--modes", nargs="+", default=["json", "sse"], choices=["json", "sse"])
    ap.add_argument("--nodelay", nargs="+", default=["off"], choices=["on", "off"])
    ap.add_argument("--reuse", nargs="+", default=["reuse", "fresh"],
                    choices=["reuse", "fresh"])
    args = ap.parse_args(argv)
    out = []
    for mode in args.modes:
        for nd in args.nodelay:
            for ru in args.reuse:
                out.append(run(args.url, args.model, args.n, mode, nd == "on", ru == "reuse"))
    print(json.dumps(out, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
