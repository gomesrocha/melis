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
            pop = m[ch.index("population")]
            self.assertIn("concurrency=1", pop)
            self.assertIn("streaming=false", pop)
            self.assertIn("backend_profile=deterministic", pop)
            self.assertIn("policy=none", pop)
            self.assertIn("feature_profile=minimal", pop)
            self.assertNotIn("concurrency=8", pop)
            self.assertEqual(m[ch.index("evidence_role")], "exploratory")
            self.assertTrue(m[ch.index("analysis_id")].startswith("agg-1.1.0-"))

    def test_does_not_pool_concurrencies(self):
        with tempfile.TemporaryDirectory() as d:
            _write_known_grid(d)
            aggregate.build(
                d, None, os.path.join(d, "comparisons.csv"), None, None, seed=1,
            )
            ch, crows = read_csv(os.path.join(d, "comparisons.csv"))
            pops = {r[ch.index("population")] for r in crows}
            self.assertTrue(any("concurrency=1" in p for p in pops))
            self.assertTrue(any("concurrency=8" in p for p in pops))
            by_key = {
                (r[ch.index("contrast")], "8" if "concurrency=8" in r[ch.index("population")] else "1"): r
                for r in crows
            }
            c1 = by_key[("melis_minus_direct", "1")]
            c8 = by_key[("melis_minus_direct", "8")]
            self.assertAlmostEqual(float(c1[ch.index("estimate")]), 3.0, places=6)
            self.assertAlmostEqual(float(c8[ch.index("estimate")]), 9.0, places=6)
            self.assertEqual(c1[ch.index("n")], "1")
            self.assertEqual(c8[ch.index("n")], "1")

    def test_incomplete_pairing_is_excluded(self):
        with tempfile.TemporaryDirectory() as d:
            generate(d)
            # Drop litellm from block index 0 only. The other two blocks stay paired.
            path = os.path.join(d, "requests.csv")
            h, rows = read_csv(path)
            igw, iblock = h.index("gateway"), h.index("block_id")
            # block ids: keep rows that are not (litellm AND the first block).
            first_block = sorted({r[iblock] for r in rows})[0]
            kept = [r for r in rows if not (r[igw] == "litellm" and r[iblock] == first_block)]
            with open(path, "w", newline="", encoding="utf-8") as fh:
                wr = csv.writer(fh)
                wr.writerow(h)
                wr.writerows(kept)
            report = aggregate.pairing_report(aggregate._dict_rows(h, kept))
            self.assertEqual(len(report["incomplete"]), 1)
            self.assertEqual(len(report["complete"]), 2)
            aggregate.build(
                d, None, os.path.join(d, "comparisons.kept.csv"), None, None, seed=1,
            )
            ch, crows = read_csv(os.path.join(d, "comparisons.kept.csv"))
            melis = [r for r in crows if r[ch.index("contrast")] == "melis_minus_direct"]
            self.assertEqual(len(melis), 1)
            self.assertEqual(melis[0][ch.index("n")], "2")
            v = validate.validate_dir(d)
            self.assertFalse(v["ok"])
            self.assertTrue(any(e.startswith("pairing:") for e in v["errors"]), v["errors"])

    def test_duplicate_request_id_across_runs(self):
        with tempfile.TemporaryDirectory() as d:
            generate(d)
            path = os.path.join(d, "requests.csv")
            h, rows = read_csv(path)
            rows.append(list(rows[0]))
            # Same request_id, different run, still a duplicated id.
            rows[-1][h.index("run_id")] = "run-other"
            with open(path, "w", newline="", encoding="utf-8") as fh:
                wr = csv.writer(fh)
                wr.writerow(h)
                wr.writerows(rows)
            report = validate.validate_dir(d)
            self.assertFalse(report["ok"])
            self.assertTrue(any("duplicated" in e for e in report["errors"]), report["errors"])

    def test_smoke_rows_rejected_in_final_directory(self):
        with tempfile.TemporaryDirectory() as parent:
            d = os.path.join(parent, "final")
            generate(d)
            report = validate.validate_dir(d)
            self.assertFalse(report["ok"])
            self.assertTrue(any("does not belong in 'final'" in e for e in report["errors"]),
                            report["errors"])

    def test_warmup_does_not_enter_the_contrast(self):
        with tempfile.TemporaryDirectory() as d:
            generate(d)
            path = os.path.join(d, "requests.csv")
            h, rows = read_csv(path)
            extra = list(rows[0])
            extra[h.index("request_id")] = "req-warmup-only"
            extra[h.index("window")] = "warmup"
            extra[h.index("latency_ms")] = "100000.0"
            extra[h.index("gateway")] = "direct"
            with open(path, "a", newline="", encoding="utf-8") as fh:
                csv.writer(fh).writerow(extra)
            aggregate.build(
                d, None, os.path.join(d, "comparisons.csv"), None, None, seed=1,
            )
            ch, crows = read_csv(os.path.join(d, "comparisons.csv"))
            m = [r for r in crows if r[ch.index("contrast")] == "melis_minus_direct"][0]
            self.assertAlmostEqual(float(m[ch.index("estimate")]), 3.0, places=6)

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

    def test_aggregate_refuses_to_replace_an_existing_analysis(self):
        with tempfile.TemporaryDirectory() as d:
            generate(d)
            out = os.path.join(d, "comparisons.v1.csv")
            aggregate.main(["--data-dir", d, "--comparisons-out", out, "--seed", "1"])
            with self.assertRaises(SystemExit):
                aggregate.main(["--data-dir", d, "--comparisons-out", out, "--seed", "1"])


def _write_known_grid(out_dir):
    """One block, two concurrencies, three gateways, fixed latencies.

    c=1 overhead is +3 ms. c=8 overhead is +9 ms. Pooling them would not
    equal either value. Synthetic only.
    """
    os.makedirs(out_dir, exist_ok=True)
    exp = ids.experiment_id("synthetic-grid", "1.0", "1.1.0")
    block = ids.block_id(exp, "smoke", 0)
    case = ids.case_id(exp, "probe", 0)
    w = CsvTableWriter("requests", os.path.join(out_dir, "requests.csv"))
    lat = {
        (1, "direct"): 10.0, (1, "melis"): 13.0, (1, "litellm"): 16.0,
        (8, "direct"): 20.0, (8, "melis"): 29.0, (8, "litellm"): 40.0,
    }
    for conc in (1, 8):
        for gw in ("direct", "melis", "litellm"):
            pair = ids.pair_id(block, case, "rq1_fixed_load", False, conc)
            run = ids.run_id(exp, "smoke", gw, block, "rq1_fixed_load", False, conc)
            for i in range(4):
                w.write_row({
                    "experiment_id": exp, "phase": "smoke", "run_id": run,
                    "block_id": block, "pair_id": pair, "case_id": case,
                    "request_id": ids.request_id(run, pair, i),
                    "gateway": gw, "scenario": "rq1_fixed_load",
                    "backend_profile": "deterministic", "streaming": False,
                    "concurrency": conc, "policy": "none", "feature_profile": "minimal",
                    "window": "measured", "started_at_utc": "2026-11-05T00:00:00.000Z",
                    "latency_ms": lat[(conc, gw)], "status": "ok", "http_status": 200,
                    "timeout": False, "completion_valid": True,
                })


if __name__ == "__main__":
    unittest.main(verbosity=2)
