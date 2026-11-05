#!/usr/bin/env python3
"""Deterministic OpenAI-compatible backend for gateway mediation experiments.

Reproducible, dependency-free (Python stdlib only). Supports:
  - POST /v1/chat/completions  (JSON and SSE, controlled by "stream")
  - POST /v1/embeddings        (deterministic vectors; for cache paths)
  - GET  /__count              (call counters, for backend-side call counting)
  - POST /__reset              (reset counters)
  - GET  /health

The response content is a deterministic function of the request (model + a hash of
the messages), so identical requests yield identical outputs. Latency is fixed (no
sleep by default) unless MOCK_LATENCY_MS is set, to keep overhead measurements about
the gateway, not the backend.

This is a measurement harness backend, NOT a model. It never calls out anywhere.

Env:
  MOCK_PORT        (default 7070)
  MOCK_LATENCY_MS  (default 0) fixed artificial latency per request
  MOCK_EMBED_DIM   (default 8)
"""
import hashlib
import json
import os
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(os.environ.get("MOCK_PORT", "7070"))
LATENCY_MS = int(os.environ.get("MOCK_LATENCY_MS", "0"))
EMBED_DIM = int(os.environ.get("MOCK_EMBED_DIM", "8"))

_counts = {"chat": 0, "chat_stream": 0, "embeddings": 0}
_lock = threading.Lock()

# Last-received chat payloads (bounded), to VERIFY that each gateway forwards an
# equivalent payload/parameters. This is measurement-harness bookkeeping, not part
# of the system under test. Only chat requests are recorded; secrets are never sent
# to this mock (dummy keys only), and Authorization headers are NOT stored.
_MAX_SEEN = 50
_seen = []  # list of {"path","model","stream","params":{...},"n_messages"}

# In-flight accounting, to VERIFY that concurrency>1 overlaps at the backend.
# `_inflight` is the current count; `_peak_inflight` is the max observed since the
# last /__reset. A request can be held open (HOLD_MS) so overlap is observable.
_inflight = 0
_peak_inflight = 0
HOLD_MS = int(os.environ.get("MOCK_HOLD_MS", "0"))


def _enter_inflight():
    global _inflight, _peak_inflight
    with _lock:
        _inflight += 1
        if _inflight > _peak_inflight:
            _peak_inflight = _inflight


def _exit_inflight():
    global _inflight
    with _lock:
        _inflight -= 1


def _record_seen(path, req):
    # Capture only the comparison-relevant fields (model + generation params +
    # whether streaming + message count). Never store full message content here.
    params = {k: req.get(k) for k in ("temperature", "max_tokens", "top_p", "stream")
              if k in req}
    entry = {
        "path": path,
        "model": req.get("model"),
        "stream": bool(req.get("stream", False)),
        "params": params,
        "n_messages": len(req.get("messages", []) or []),
    }
    with _lock:
        _seen.append(entry)
        if len(_seen) > _MAX_SEEN:
            del _seen[0:len(_seen) - _MAX_SEEN]


def _hash(s: str) -> int:
    return int(hashlib.sha256(s.encode("utf-8")).hexdigest()[:8], 16)


def _deterministic_reply(model: str, messages) -> str:
    # One short, stable "word" derived from the request; identical input => identical
    # output. Kept tiny so token counts are stable and comparable.
    basis = model + "|" + json.dumps(messages, sort_keys=True, ensure_ascii=False)
    return f"reply-{_hash(basis) % 100000:05d}"


def _embedding(text: str):
    h = _hash(text)
    return [((h >> (i * 3)) & 0x7) / 7.0 for i in range(EMBED_DIM)]


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):  # silence default logging
        pass

    def _json(self, code, obj):
        body = json.dumps(obj).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/health":
            return self._json(200, {"status": "ok"})
        if self.path == "/__count":
            with _lock:
                return self._json(200, dict(_counts))
        if self.path == "/__seen":
            with _lock:
                return self._json(200, {"seen": list(_seen)})
        if self.path == "/__inflight":
            with _lock:
                return self._json(200, {"inflight": _inflight, "peak_inflight": _peak_inflight})
        return self._json(404, {"error": "not found"})

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        raw = self.rfile.read(length) if length else b"{}"
        try:
            req = json.loads(raw or b"{}")
        except Exception:
            return self._json(400, {"error": "invalid json"})

        if self.path == "/__reset":
            global _peak_inflight
            with _lock:
                for k in _counts:
                    _counts[k] = 0
                _seen.clear()
                _peak_inflight = _inflight  # keep current, reset the peak
            return self._json(200, {"status": "reset"})

        if LATENCY_MS:
            time.sleep(LATENCY_MS / 1000.0)

        if self.path == "/v1/embeddings":
            with _lock:
                _counts["embeddings"] += 1
            inp = req.get("input", "")
            inputs = inp if isinstance(inp, list) else [inp]
            data = [
                {"object": "embedding", "index": i, "embedding": _embedding(str(t))}
                for i, t in enumerate(inputs)
            ]
            return self._json(
                200,
                {
                    "object": "list",
                    "data": data,
                    "model": req.get("model", "mock-embed"),
                    "usage": {"prompt_tokens": 0, "total_tokens": 0},
                },
            )

        if self.path == "/v1/chat/completions":
            model = req.get("model", "mock-model")
            messages = req.get("messages", [])
            _record_seen(self.path, req)
            content = _deterministic_reply(model, messages)
            stream = bool(req.get("stream", False))
            # Enter in-flight accounting and optionally HOLD the request open so
            # concurrent overlap is observable at the backend. Per-request override
            # via "hold_ms" in the body; default from MOCK_HOLD_MS.
            hold_ms = int(req.get("hold_ms", HOLD_MS) or 0)
            _enter_inflight()
            try:
                if hold_ms:
                    time.sleep(hold_ms / 1000.0)
                if stream:
                    with _lock:
                        _counts["chat_stream"] += 1
                    return self._sse(model, content)
                with _lock:
                    _counts["chat"] += 1
                return self._json(
                    200,
                    {
                        "id": f"chatcmpl-{_hash(content):08x}",
                        "object": "chat.completion",
                        "created": 0,
                        "model": model,
                        "choices": [
                            {
                                "index": 0,
                                "finish_reason": "stop",
                                "message": {"role": "assistant", "content": content},
                            }
                        ],
                        "usage": {
                            "prompt_tokens": len(json.dumps(messages)) // 4,
                            "completion_tokens": len(content) // 4,
                            "total_tokens": (len(json.dumps(messages)) + len(content)) // 4,
                        },
                    },
                )
            finally:
                _exit_inflight()

        return self._json(404, {"error": "not found"})

    def _sse(self, model, content):
        # Close the connection at end-of-stream so HTTP/1.1 clients see EOF after
        # [DONE] (no Content-Length / chunked framing needed for a mock).
        self.close_connection = True
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "close")
        self.end_headers()
        # One token per word, then [DONE].
        for i, tok in enumerate(content.split("-")):
            chunk = {
                "id": f"chatcmpl-{_hash(content):08x}",
                "object": "chat.completion.chunk",
                "created": 0,
                "model": model,
                "choices": [
                    {"index": 0, "delta": {"content": ("" if i == 0 else "-") + tok}, "finish_reason": None}
                ],
            }
            self.wfile.write(f"data: {json.dumps(chunk)}\n\n".encode("utf-8"))
            self.wfile.flush()
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()


def main():
    srv = ThreadingHTTPServer(("0.0.0.0", PORT), Handler)
    print(f"deterministic mock backend on :{PORT} (latency={LATENCY_MS}ms dim={EMBED_DIM})", file=sys.stderr)
    srv.serve_forever()


if __name__ == "__main__":
    main()
