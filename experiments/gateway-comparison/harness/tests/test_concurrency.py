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
            # The peak belongs to the cell that just ran. After reset, c=1 must
            # not inherit the c=8 peak. Configured concurrency and observed peak
            # stay different measures: this does not require peak8 == 8.
            _reset(base)
            conc_load.run_cell(base, "mock-model", "json", concurrency=1,
                               n_requests=3, max_tokens=16, timeout=30)
            peak_after = _peak(base)
            self.assertEqual(peak_after, 1, f"reset must attribute the next cell (peak={peak_after})")
            print(f"OVERLAP: peak_inflight c=1 -> {peak1}, c=8 -> {peak8}, after reset c=1 -> {peak_after}")
        finally:
            srv.shutdown()
            srv.server_close()

    def test_results_classify_and_count(self):
        srv, base = _start_backend(hold_ms=0)
        try:
            summary = conc_load.run_cell(base, "mock-model", "json", concurrency=4,
                                          n_requests=20, max_tokens=16, timeout=30)
            results = summary["results"]
            self.assertEqual(len(results), 20)
            self.assertEqual(summary["termination_reason"], "request_cap")
            self.assertFalse(summary["truncated"])
            self.assertIsNone(summary["requested_duration_s"])
            oks = sum(1 for r in results if r["status"] == "ok")
            self.assertEqual(oks, 20, "all deterministic requests should be ok")
            self.assertGreater(summary["effective_duration_s"], 0)
            self.assertIsNone(conc_load.execution_backend_calls(20, 10))
            self.assertEqual(conc_load.execution_backend_calls(20, 28), 8)
            self.assertIsNone(conc_load.execution_backend_calls(None, 4))
        finally:
            srv.shutdown()
            srv.server_close()

    def test_backend_does_not_stall_json_on_reused_keepalive(self):
        """Regression guard for the JSON/SSE asymmetry artifact.

        The mock backend writes JSON headers then body as separate writes; without
        TCP_NODELAY on the server connection, Nagle holds the small body segment on
        a REUSED keep-alive connection until the client's delayed-ACK (~40ms),
        inflating JSON completion time. The backend's setup() sets TCP_NODELAY to
        prevent this. Here we drive several JSON requests on ONE reused connection
        via the raw-socket probe and assert the median completion is well under the
        ~40ms delayed-ACK floor. This catches a regression that drops NODELAY.
        """
        import json_sse_probe as probe
        srv, base = _start_backend(hold_ms=0)
        try:
            agg = probe.run(base, "mock-model", n=15, mode="json", nodelay=False, reuse=True)
            done = agg["t_done_med"]
            self.assertIsNotNone(done)
            # Delayed-ACK stalls are ~40ms; a healthy reused JSON path is a few ms.
            self.assertLess(
                done, 20.0,
                f"JSON reused-keepalive completion {done}ms suggests a Nagle/delayed-ACK "
                f"stall — the backend must keep TCP_NODELAY (see setup())",
            )
            print(f"NO-STALL: reused JSON completion median = {done}ms")
        finally:
            srv.shutdown()
            srv.server_close()

    def test_duration_window_is_not_cut_by_a_small_count(self):
        srv, base = _start_backend(hold_ms=0)
        try:
            summary = conc_load.run_cell(
                base, "mock-model", "json", concurrency=1, n_requests=None,
                max_tokens=16, timeout=30, deadline_mono=time.perf_counter() + 0.25,
                safety_cap=100000, requested_duration_s=0.25, collect=False,
            )
            self.assertEqual(summary["termination_reason"], "deadline")
            self.assertFalse(summary["truncated"])
            self.assertGreater(summary["n"], 40)
            self.assertGreaterEqual(summary["effective_duration_s"], 0.25)
            self.assertEqual(summary["requested_duration_s"], 0.25)
            self.assertNotEqual(summary["effective_duration_s"], summary["requested_duration_s"])
        finally:
            srv.shutdown()
            srv.server_close()

    def test_safety_cap_marks_the_window_truncated(self):
        srv, base = _start_backend(hold_ms=0)
        try:
            summary = conc_load.run_cell(
                base, "mock-model", "json", concurrency=1, n_requests=None,
                max_tokens=16, timeout=30, deadline_mono=time.perf_counter() + 2.0,
                safety_cap=5, requested_duration_s=2.0, collect=False,
            )
            self.assertEqual(summary["n"], 5)
            self.assertEqual(summary["termination_reason"], "safety_cap")
            self.assertTrue(summary["truncated"])
            self.assertLess(summary["effective_duration_s"], summary["requested_duration_s"])
        finally:
            srv.shutdown()
            srv.server_close()

    def test_inflight_requests_are_drained_after_the_deadline(self):
        srv, base = _start_backend(hold_ms=300)
        try:
            summary = conc_load.run_cell(
                base, "mock-model", "json", concurrency=2, n_requests=None,
                max_tokens=16, timeout=30, deadline_mono=time.perf_counter() + 0.05,
                safety_cap=100, requested_duration_s=0.05, collect=True,
            )
            self.assertEqual(summary["termination_reason"], "deadline")
            self.assertFalse(summary["truncated"])
            self.assertEqual(summary["n"], summary["admitted"])
            self.assertGreaterEqual(summary["admitted"], 1)
            self.assertLessEqual(summary["admitted"], 2)
            self.assertGreater(summary["effective_duration_s"], summary["requested_duration_s"])
            self.assertGreater(summary["drained"], 0)
        finally:
            srv.shutdown()
            srv.server_close()

    def test_absent_stream_key_defaults_to_json(self):
        import json
        srv, base = _start_backend(hold_ms=0)
        try:
            body = json.dumps({
                "model": "mock-model",
                "messages": [{"role": "user", "content": "no stream key"}],
                "temperature": 0, "max_tokens": 16,
            }).encode()
            req = urllib.request.Request(
                base + "/v1/chat/completions", data=body, method="POST",
            )
            req.add_header("Content-Type", "application/json")
            urllib.request.urlopen(req, timeout=5).read()
            seen = json.loads(urllib.request.urlopen(base + "/__seen", timeout=5).read())
            counts = json.loads(urllib.request.urlopen(base + "/__count", timeout=5).read())
            self.assertEqual(seen["seen"][0]["stream"], False)
            self.assertNotIn("stream", seen["seen"][0]["params"])
            self.assertEqual(counts["chat"], 1)
            self.assertEqual(counts["chat_stream"], 0)
        finally:
            srv.shutdown()
            srv.server_close()


if __name__ == "__main__":
    unittest.main(verbosity=2)
