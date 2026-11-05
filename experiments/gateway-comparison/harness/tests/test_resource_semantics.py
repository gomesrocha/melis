#!/usr/bin/env python3
"""Sampler semantics on known Docker stats payloads. Not experimental evidence."""
from __future__ import annotations

import os
import sys
import unittest

HARNESS = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, HARNESS)

import block_design  # noqa: E402
import resource_sampler as rs  # noqa: E402


CGROUP_V2 = {
    "read": "2026-11-05T18:56:03.43045631Z",
    "cpu_stats": {"cpu_usage": {"total_usage": 1_500_000_000}, "online_cpus": 16},
    "memory_stats": {
        "usage": 20_000_000,
        "stats": {"anon": 11_000_000, "inactive_file": 2_000_000, "file": 3_000_000},
    },
    "networks": {"eth0": {"rx_bytes": 100, "tx_bytes": 40}},
}

CGROUP_V1 = {
    "read": "2026-11-05T18:56:04.10000000Z",
    "cpu_stats": {"cpu_usage": {"total_usage": 2_000_000_000}},
    "memory_stats": {
        "usage": 50_000_000,
        "stats": {"rss": 30_000_000, "cache": 20_000_000, "inactive_file": 5_000_000},
    },
    "networks": {"eth0": {"rx_bytes": 1, "tx_bytes": 2}},
}


class TestMemorySeparation(unittest.TestCase):
    def test_cgroup_v2_does_not_invent_rss(self):
        parsed = rs.interpret_docker_stats(CGROUP_V2)
        self.assertEqual(parsed["cpu_seconds_total"], 1.5)
        self.assertEqual(parsed["memory_cgroup_current_bytes"], 20_000_000)
        self.assertEqual(parsed["memory_working_set_bytes"], 18_000_000)
        self.assertIsNone(parsed["memory_rss_bytes"])
        self.assertIsNone(parsed["memory_rss_method"])
        self.assertNotIn("anon", (parsed["memory_rss_method"] or ""))
        self.assertEqual(parsed["network_rx_bytes"], 100)
        self.assertEqual(parsed["network_tx_bytes"], 40)
        self.assertIn("total_usage_ns", parsed["cpu_method"])

    def test_cgroup_v1_rss_is_recorded_as_rss(self):
        parsed = rs.interpret_docker_stats(CGROUP_V1)
        self.assertEqual(parsed["memory_rss_bytes"], 30_000_000)
        self.assertIn("rss", parsed["memory_rss_method"])
        self.assertNotEqual(parsed["memory_rss_bytes"], parsed["memory_cgroup_current_bytes"])
        self.assertEqual(parsed["memory_working_set_bytes"], 45_000_000)

    def test_cpu_percent_is_not_a_counter(self):
        parsed = rs.interpret_docker_stats({"cpu_stats": {}, "memory_stats": {"usage": 10}})
        self.assertIsNone(parsed["cpu_seconds_total"])
        self.assertIsNone(parsed["cpu_method"])
        self.assertEqual(parsed["memory_cgroup_current_bytes"], 10)
        self.assertIsNone(parsed["memory_rss_bytes"])

    def test_timestamps_follow_each_payload(self):
        a = rs.normalize_engine_timestamp(CGROUP_V2["read"], "fallback")
        b = rs.normalize_engine_timestamp(CGROUP_V1["read"], "fallback")
        self.assertNotEqual(a, b)
        self.assertTrue(a.endswith("Z") and b.endswith("Z"))
        self.assertEqual(rs.normalize_engine_timestamp(None, "fallback"), "fallback")

    def test_interval_lag_and_first_sample(self):
        self.assertEqual(rs.interval_fields(None, 10.0, 0.5), (None, None))
        effective, lag = rs.interval_fields(10.0, 10.8, 0.5)
        self.assertAlmostEqual(effective, 0.8)
        self.assertAlmostEqual(lag, 0.3)

    def test_host_cpu_does_not_double_count_guest(self):
        # 100 user (includes 10 guest), 0 nice, 0 system, 50 idle, 0 iowait,
        # 0 irq, 0 softirq, 0 steal, 10 guest, 0 guest_nice.
        text = "cpu  100 0 0 50 0 0 0 0 10 0\n"
        # busy = user+nice+system+irq+softirq+steal = 100, not 110
        self.assertAlmostEqual(rs.interpret_host_cpu(text), 100 / os.sysconf("SC_CLK_TCK"))


class TestBlockDesign(unittest.TestCase):
    def test_three_blocks_cover_every_position(self):
        orders = block_design.balanced_orders(
            ["direct", "melis", "litellm"], n_blocks=3, seed=20261105,
        )
        self.assertEqual(len(orders), 3)
        self.assertTrue(block_design.positions_balanced(orders))
        # A block never repeats a gateway, and the three paths share the block.
        for order in orders:
            self.assertEqual(set(order), {"direct", "melis", "litellm"})

    def test_one_gateway_first_is_not_required(self):
        orders = block_design.balanced_orders(
            ["direct", "melis", "litellm"], n_blocks=3, seed=7,
        )
        firsts = [order[0] for order in orders]
        self.assertEqual(len(set(firsts)), 3)


if __name__ == "__main__":
    unittest.main(verbosity=2)
