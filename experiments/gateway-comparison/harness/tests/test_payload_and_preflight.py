#!/usr/bin/env python3
"""Payload allowlist and Melis default-config detection. Not experimental evidence."""
from __future__ import annotations

import os
import sys
import unittest

HARNESS = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, HARNESS)

import host_pressure  # noqa: E402
import melis_preflight  # noqa: E402
import payload_equiv  # noqa: E402


class TestPayload(unittest.TestCase):
    def test_absent_stream_false_is_the_only_allowance(self):
        left = {
            "model": "mock-model",
            "messages": [{"role": "user", "content": "equivalence probe"}],
            "temperature": 0, "max_tokens": 16, "stream": False,
        }
        right = {
            "model": "mock-model",
            "messages": [{"role": "user", "content": "equivalence probe"}],
            "temperature": 0, "max_tokens": 16,
        }
        report = payload_equiv.compare_bodies([left, right])
        self.assertTrue(report["ok"], report)
        self.assertNotEqual(report["raw_sha256"][0], report["raw_sha256"][1])
        self.assertEqual(report["effective"][0], report["effective"][1])

    def test_other_omissions_fail(self):
        left = {"model": "mock-model", "messages": [{"role": "user", "content": "a"}],
                "temperature": 0, "max_tokens": 16, "stream": False}
        dropped_message = {"model": "mock-model", "messages": [],
                           "temperature": 0, "max_tokens": 16, "stream": False}
        extra = dict(left, user="someone")
        self.assertFalse(payload_equiv.compare_bodies([left, dropped_message])["ok"])
        self.assertFalse(payload_equiv.compare_bodies([left, extra])["ok"])
        changed = dict(left, temperature=1)
        self.assertFalse(payload_equiv.compare_bodies([left, changed])["ok"])


class TestPreflight(unittest.TestCase):
    def test_defaults_marker_aborts(self):
        log = "Failed to load config.yaml: redis.cluster_urls must contain at least one URL. Using defaults.\n"
        self.assertTrue(melis_preflight.startup_rejected_config(log))
        with self.assertRaises(RuntimeError):
            melis_preflight.assert_mounted_config(log)
        melis_preflight.assert_mounted_config("Melis AI Gateway starting\nListening on 0.0.0.0:7071\n")

    def test_model_mismatch_aborts(self):
        with self.assertRaises(RuntimeError):
            melis_preflight.assert_seen_model([{"model": "other"}], "mock-model")
        melis_preflight.assert_seen_model([{"model": "mock-model"}], "mock-model")


class TestHostPressure(unittest.TestCase):
    def test_full_swap_without_activity_is_not_thrashing(self):
        quiet = {
            "mem_available_bytes": 8 * 1024 ** 3,
            "pswpin": 0, "pswpout": 0,
            "swap_free_bytes": 0, "swap_total_bytes": 8 * 1024 ** 3,
        }
        self.assertEqual(host_pressure.inadequate(quiet), "")

    def test_swap_activity_or_low_memory_refuses(self):
        moving = {"mem_available_bytes": 8 * 1024 ** 3, "pswpin": 3, "pswpout": 0}
        low = {"mem_available_bytes": 1024, "pswpin": 0, "pswpout": 0}
        self.assertIn("swap activity", host_pressure.inadequate(moving))
        self.assertIn("MemAvailable", host_pressure.inadequate(low))


if __name__ == "__main__":
    unittest.main(verbosity=2)
