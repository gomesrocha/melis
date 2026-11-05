#!/usr/bin/env python3
"""Deterministic test that the closed-loop generator produces REAL overlap.

Starts the stdlib mock backend in-process with each chat request HELD OPEN for a
fixed time, then drives it with `conc_load.run_cell` at concurrency 1 and 8 and
reads the backend-observed PEAK simultaneous in-flight requests via /__inflight.

Expectations (known by construction, hence deterministic):
  * concurrency=1  -> peak_inflight == 1 (no overlap in a closed loop).
  * concurrency=8  -> peak_inflight  > 1 and <= 8 (real overlap); with a long
    enough hold vs request count it reaches 8.

This proves `concurrency` is a real simultaneity knob, not just metadata. It is a
HARNESS self-test on synthetic traffic, NOT experimental evidence.

Run: python test_concurrency.py   (stdlib only)
"""
from __future__ import annotations

import os
import sys
import threading
import time
import unittest
import urllib.request
from http.server import ThreadingHTTPServer

HARNESS = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, HARNESS)
BACKEND = os.path.join(os.path.dirname(HARNESS), "backend-deterministic")
sys.path.insert(0, BACKEND)

import conc_load  # noqa: E402


def _start_backend(hold_ms):
    # Import the mock backend module and start it on an ephemeral port with HOLD.
    os.environ["MOCK_HOLD_MS"] = str(hold_ms)
    import importlib
    import mock_openai
    importlib.reload(mock_openai)  # pick up MOCK_HOLD_MS
    srv = ThreadingHTTPServer(("127.0.0.1", 0), mock_openai.Handler)
    port = srv.socket.getsockname()[1]
    t = threading.Thread(target=srv.serve_forever, daemon=True)
    t.start()
    # Wait until it answers.
    base = f"http://127.0.0.1:{port}"
    for _ in range(50):
        try:
            urllib.request.urlopen(base + "/health", timeout=1).read()
            break
        except Exception:
            time.sleep(0.05)
    return srv, base


def _peak(base):
    import json
    with urllib.request.urlopen(base + "/__inflight", timeout=5) as r:
        return json.loads(r.read())["peak_inflight"]


def _reset(base):
    urllib.request.urlopen(urllib.request.Request(base + "/__reset", data=b"{}", method="POST"),
                           timeout=5).read()


class TestConcurrencyOverlap(unittest.TestCase):
    def test_concurrency_produces_real_overlap(self):
        # Each request is held open 150 ms so overlap is clearly observable.
        srv, base = _start_backend(hold_ms=150)
        try:
            # concurrency = 1 -> no overlap.
            _reset(base)
            conc_load.run_cell(base, "mock-model", "json", concurrency=1,
                               n_requests=4, max_tokens=16, timeout=30)
            peak1 = _peak(base)
            self.assertEqual(peak1, 1, f"closed-loop c=1 must not overlap (peak={peak1})")

            # concurrency = 8 -> real overlap (> 1, up to 8).
            _reset(base)
            conc_load.run_cell(base, "mock-model", "json", concurrency=8,
                               n_requests=24, max_tokens=16, timeout=30)
            peak8 = _peak(base)
            self.assertGreater(peak8, 1, f"c=8 must overlap at the backend (peak={peak8})")
            self.assertLessEqual(peak8, 8, f"peak cannot exceed worker count (peak={peak8})")
            print(f"OVERLAP: peak_inflight c=1 -> {peak1}, c=8 -> {peak8}")
        finally:
            srv.shutdown()
            srv.server_close()

    def test_results_classify_and_count(self):
        srv, base = _start_backend(hold_ms=0)
        try:
            results, wall = conc_load.run_cell(base, "mock-model", "json", concurrency=4,
                                               n_requests=20, max_tokens=16, timeout=30)
            self.assertEqual(len(results), 20)
            self.assertTrue(all(r is not None for r in results))
            oks = sum(1 for r in results if r["status"] == "ok")
            self.assertEqual(oks, 20, "all deterministic requests should be ok")
            self.assertGreater(wall, 0)
        finally:
            srv.shutdown()
            srv.server_close()


if __name__ == "__main__":
    unittest.main(verbosity=2)
