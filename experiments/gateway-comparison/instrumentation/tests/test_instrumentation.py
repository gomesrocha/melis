#!/usr/bin/env python3
"""Small tests for the instrumentation scripts, using SYNTHETIC known-result data.

These tests exercise the writer, validator, and aggregation on artificial data
whose outcome is known by construction. The synthetic data is NOT experimental
evidence (see make_synthetic.py).

Run: python -m unittest discover -s . -p 'test_*.py'  (from this tests/ dir)
or:  python test_instrumentation.py

Stdlib only (unittest).
"""
from __future__ import annotations

import csv
import os
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
INSTR = os.path.dirname(HERE)
sys.path.insert(0, INSTR)
sys.path.insert(0, HERE)

import aggregate  # noqa: E402
import ids  # noqa: E402
import validate  # noqa: E402
from csv_writer import CsvTableWriter, SchemaError  # noqa: E402
from make_synthetic import EXPECTED, generate  # noqa: E402
from schemas import SCHEMA_VERSION, header  # noqa: E402


def read_csv(path):
    with open(path, newline="", encoding="utf-8") as fh:
        rows = list(csv.reader(fh))
    return rows[0], rows[1:]


class TestIds(unittest.TestCase):
    def test_pair_id_is_gateway_independent(self):
        b = ids.block_id("exp", "smoke", 0)
        c = ids.case_id("exp", "fam", 1)
        # pair_id does NOT take a gateway argument -> same across gateways.
        p1 = ids.pair_id(b, c, "s", False, 1)
        p2 = ids.pair_id(b, c, "s", False, 1)
        self.assertEqual(p1, p2)
        # Different cell -> different pair.
        self.assertNotEqual(p1, ids.pair_id(b, c, "s", True, 1))

    def test_ids_are_deterministic(self):
        self.assertEqual(ids.experiment_id("a", "1", "1"), ids.experiment_id("a", "1", "1"))


class TestCsvWriter(unittest.TestCase):
    def test_missing_is_empty_not_zero(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "requests.csv")
            w = CsvTableWriter("requests", path)
            w.write_row({
                "experiment_id": "e", "phase": "smoke", "run_id": "r", "block_id": "b",
                "pair_id": "p", "case_id": "c", "request_id": "q", "gateway": "direct",
                "scenario": "s", "backend_profile": "deterministic", "streaming": False,
                "concurrency": 1, "started_at_utc": "2026-11-05T00:00:00.000Z",
                "status": "timeout", "timeout": True, "completion_valid": False,
                "latency_ms": None,  # missing
            })
            h, rows = read_csv(path)
            cell = rows[0][h.index("latency_ms")]
            self.assertEqual(cell, "", "missing latency must be empty, not 0")
            # A genuine zero is preserved as 0.
            w.write_row({
                "experiment_id": "e", "phase": "smoke", "run_id": "r", "block_id": "b",
                "pair_id": "p", "case_id": "c", "request_id": "q2", "gateway": "direct",
                "scenario": "s", "backend_profile": "deterministic", "streaming": False,
                "concurrency": 1, "started_at_utc": "2026-11-05T00:00:00.000Z",
                "status": "ok", "timeout": False, "completion_valid": True,
                "scheduled_delay_ms": 0,
            })
            h, rows = read_csv(path)
            self.assertEqual(rows[1][h.index("scheduled_delay_ms")], "0.0")

    def test_required_field_raises(self):
        with tempfile.TemporaryDirectory() as d:
            w = CsvTableWriter("requests", os.path.join(d, "requests.csv"))
            with self.assertRaises(SchemaError):
                w.write_row({"experiment_id": "e"})  # missing many required

    def test_enum_rejected(self):
        with tempfile.TemporaryDirectory() as d:
            w = CsvTableWriter("requests", os.path.join(d, "requests.csv"))
            with self.assertRaises(SchemaError):
                w.write_row({
                    "experiment_id": "e", "phase": "smoke", "run_id": "r", "block_id": "b",
                    "pair_id": "p", "case_id": "c", "request_id": "q", "gateway": "NOPE",
                    "scenario": "s", "backend_profile": "deterministic", "streaming": False,
                    "concurrency": 1, "started_at_utc": "t", "status": "ok",
                    "timeout": False, "completion_valid": True,
                })

    def test_schema_version_autostamped(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "runs.csv")
            w = CsvTableWriter("runs", path)
            w.write_row({
                "experiment_id": "e", "phase": "smoke", "run_id": "r", "block_id": "b",
                "gateway": "direct", "scenario": "s", "backend_profile": "deterministic",
                "streaming": False, "concurrency": 1, "n_requests": 1, "n_ok": 1, "n_failed": 0,
            })
            h, rows = read_csv(path)
            self.assertEqual(rows[0][h.index("schema_version")], SCHEMA_VERSION)


class TestValidate(unittest.TestCase):
    def _gen(self, d):
        generate(d)
        return d

    def test_clean_synthetic_passes(self):
        with tempfile.TemporaryDirectory() as d:
            self._gen(d)
            report = validate.validate_dir(d)
            self.assertTrue(report["ok"], report["errors"])

    def test_detects_duplicate_key(self):
        with tempfile.TemporaryDirectory() as d:
            self._gen(d)
            # Append a duplicate requests row (same run_id+request_id).
            path = os.path.join(d, "requests.csv")
            h, rows = read_csv(path)
            with open(path, "a", newline="", encoding="utf-8") as fh:
                csv.writer(fh).writerow(rows[0])
            report = validate.validate_dir(d)
            self.assertFalse(report["ok"])
            self.assertTrue(any("duplicate key" in e for e in report["errors"]))

    def test_detects_header_mismatch(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "requests.csv")
            with open(path, "w", newline="", encoding="utf-8") as fh:
                csv.writer(fh).writerow(["wrong", "header"])
                csv.writer(fh).writerow(["a", "b"])
            report = validate.validate_dir(d)
            self.assertFalse(report["ok"])
            self.assertTrue(any("header mismatch" in e for e in report["errors"]))

    def test_detects_dropped_failure_in_runs(self):
        """If runs.csv claims zero failures but requests.csv has one, flag it."""
        with tempfile.TemporaryDirectory() as d:
            self._gen(d)
            aggregate.build(d, os.path.join(d, "runs.csv"), None, None, None, 1)
            # Tamper: set n_failed to 0 (pretend failures were dropped).
            path = os.path.join(d, "runs.csv")
            h, rows = read_csv(path)
            fi = h.index("n_failed")
            ni = h.index("n_requests")
            for r in rows:
                r[fi] = "0"
                r[ni] = "4"  # also lie about total to hide the dropped failure
            with open(path, "w", newline="", encoding="utf-8") as fh:
                wr = csv.writer(fh)
                wr.writerow(h)
                wr.writerows(rows)
            report = validate.validate_dir(d)
            self.assertFalse(report["ok"])
            self.assertTrue(
                any("n_failed" in e or "n_requests" in e for e in report["errors"]),
                report["errors"],
            )


class TestAggregate(unittest.TestCase):
    def test_known_contrasts_and_counts(self):
        with tempfile.TemporaryDirectory() as d:
            generate(d)
            aggregate.build(
                d, os.path.join(d, "runs.csv"), os.path.join(d, "comparisons.csv"),
                None, None, seed=12345,
            )
            # runs.csv: every run has 5 requests, 4 ok, 1 failed.
            h, rows = read_csv(os.path.join(d, "runs.csv"))
            self.assertEqual(len(rows), 9, "3 blocks x 3 gateways = 9 runs")
            for r in rows:
                self.assertEqual(r[h.index("n_requests")], str(EXPECTED["requests_per_run"]))
                self.assertEqual(r[h.index("n_ok")], str(EXPECTED["ok_per_run"]))
                self.assertEqual(r[h.index("n_failed")], str(EXPECTED["failed_per_run"]))

            # comparisons.csv: paired overheads are exactly +3 and +6 ms, n=3 blocks.
            ch, crows = read_csv(os.path.join(d, "comparisons.csv"))
            by_contrast = {r[ch.index("contrast")]: r for r in crows}
            m = by_contrast["melis_minus_direct"]
            self.assertEqual(m[ch.index("independent_units")], "block")
            self.assertEqual(m[ch.index("n")], str(EXPECTED["n_blocks"]))
            self.assertAlmostEqual(float(m[ch.index("estimate")]),
                                   EXPECTED["melis_minus_direct_ms"], places=6)
            # bootstrap CI of a constant diff collapses to the point value.
            self.assertAlmostEqual(float(m[ch.index("ci_lower")]), 3.0, places=6)
            self.assertAlmostEqual(float(m[ch.index("ci_upper")]), 3.0, places=6)
            # contract-only fields empty; no fabricated p-values.
            self.assertEqual(m[ch.index("p_raw")], "")
            self.assertEqual(m[ch.index("p_adjusted")], "")

            ll = by_contrast["litellm_minus_direct"]
            self.assertAlmostEqual(float(ll[ch.index("estimate")]),
                                   EXPECTED["litellm_minus_direct_ms"], places=6)

    def test_aggregate_refuses_to_overwrite_raw(self):
        with tempfile.TemporaryDirectory() as d:
            generate(d)
            # main() raises SystemExit before writing when the output path would
            # clobber the raw requests.csv.
            with self.assertRaises(SystemExit):
                aggregate.main([
                    "--data-dir", d,
                    "--runs-out", os.path.join(d, "requests.csv"),  # == raw file
                ])


if __name__ == "__main__":
    unittest.main(verbosity=2)
