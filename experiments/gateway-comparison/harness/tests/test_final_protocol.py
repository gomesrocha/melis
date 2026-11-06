#!/usr/bin/env python3
"""Phase=final rules, with a simulated clock and synthetic rows.

Nothing here is a collection. Rows are written under a temporary directory
and marked SYNTHETIC. They are not confirmatory evidence.
"""
from __future__ import annotations

import io
import json
import os
import sys
import tempfile
import threading
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
HARNESS = os.path.dirname(HERE)
ROOT = os.path.dirname(HARNESS)
INSTR = os.path.join(ROOT, "instrumentation")
sys.path.insert(0, HARNESS)
sys.path.insert(0, INSTR)

import admission  # noqa: E402
import aggregate  # noqa: E402
import block_rules  # noqa: E402
import conc_load  # noqa: E402
import final_protocol  # noqa: E402
import ids  # noqa: E402
import run_comparative_pilot  # noqa: E402
from csv_writer import CsvTableWriter  # noqa: E402
from manifest import build_manifest  # noqa: E402
from validate import validate_dir  # noqa: E402

PROTOCOL = os.path.join(ROOT, "protocol", "rq1-final-candidate.json")
AMENDMENT = os.path.join(ROOT, "protocol", "rq1-final-amendment-1.1.0.json")
GIB = 1024 ** 3
GATES = ["direct", "melis", "litellm"]


class Clock:
    def __init__(self):
        self.t = 1000.0

    def __call__(self):
        return self.t

    def sleep(self, seconds):
        self.t += seconds


def _quiet():
    return {
        "mem_available_bytes": 10 * GIB,
        "pswpin": 100,
        "pswpout": 100,
        "load1": 0.4,
    }


def _cpu_summary():
    return {
        "per_cpu": [{
            "cpu": "cpu0", "utilization": 0.2, "iowait_fraction": 0.01, "total_jiffies": 100,
        }],
        "aggregate": {
            "cpu": "cpu", "utilization": 0.2, "iowait_fraction": 0.01, "total_jiffies": 100,
        },
        "role": "recorded",
    }


def _volumes(path):
    return {
        "csv": {"path": path, "free_bytes": 30 * GIB, "total_bytes": 100 * GIB},
        "docker": {"path": "/var/lib/docker", "free_bytes": 30 * GIB, "total_bytes": 100 * GIB},
    }


def _cpu_reader():
    calls = {"n": 0}

    def read():
        calls["n"] += 1
        n = calls["n"]
        row = {
            "user": 5 * n, "nice": 0, "system": n, "idle": 40 * n, "iowait": n,
            "irq": 0, "softirq": 0, "steal": 0,
        }
        return [{"cpu": "cpu", **row}, {"cpu": "cpu0", **row}]

    return read


def _identity():
    return {
        "experiment_id": "exp-synthetic-final",
        "phase": "final",
        "collection_id": "col-synthetic-final",
    }


def _request(experiment, collection, phase, block, gateway, status="ok", latency=10.0):
    case = "case-synthetic-0000"
    pair = ids.pair_id(block, case, "rq1_fixed_load", False, 1, collection)
    run = ids.run_id(experiment, phase, gateway, block, "rq1_fixed_load", False, 1, 0, collection)
    return {
        "experiment_id": experiment,
        "collection_id": collection,
        "phase": phase,
        "run_id": run,
        "block_id": block,
        "pair_id": pair,
        "case_id": case,
        "request_id": ids.request_id(run, pair, 0),
        "gateway": gateway,
        "scenario": "rq1_fixed_load",
        "backend_profile": "deterministic",
        "streaming": False,
        "concurrency": 1,
        "policy": "none",
        "feature_profile": "minimal",
        "window": "measured",
        "started_at_utc": "2026-11-06T00:00:00.000Z",
        "latency_ms": latency,
        "status": status,
        "http_status": 200 if status == "ok" else None,
        "timeout": False,
        "completion_valid": status == "ok",
    }


class TestProtocolFile(unittest.TestCase):
    def test_file_matches_the_code_and_the_sidecar(self):
        document, digest = final_protocol.load(PROTOCOL)
        final_protocol.validate_document(document)
        self.assertEqual(document, final_protocol.candidate_document())
        with open(PROTOCOL + ".sha256", encoding="utf-8") as fh:
            sidecar = fh.read().split()[0]
        self.assertEqual(digest, sidecar)
        self.assertEqual(len(document["orders"]), 18)
        self.assertFalse(document["execution_authorized"])
        self.assertEqual(document["test_procedure"]["status"], "proposed_not_executed")
        self.assertTrue(document["test_procedure"]["not_computed_in_this_revision"])
        self.assertEqual(digest, final_protocol.PRESERVED_SHA256)

    def test_amendment_withdraws_the_load_threshold_and_keeps_the_old_file(self):
        amended, digest = final_protocol.load(AMENDMENT)
        final_protocol.validate_for_start(amended)
        self.assertEqual(amended, final_protocol.amended_document())
        with open(AMENDMENT + ".sha256", encoding="utf-8") as fh:
            self.assertEqual(digest, fh.read().split()[0])
        self.assertEqual(amended["amends_sha256"], final_protocol.PRESERVED_SHA256)
        self.assertNotIn("max_load1", amended["admission"])
        self.assertEqual(amended["admission"]["load1_role"], "diagnostic")
        self.assertFalse(amended["execution_authorized"])
        preserved, preserved_digest = final_protocol.load(PROTOCOL)
        self.assertEqual(preserved_digest, final_protocol.PRESERVED_SHA256)
        with self.assertRaises(final_protocol.ProtocolError):
            final_protocol.validate_for_start(preserved)
        broken = final_protocol.amended_document()
        broken["admission"]["max_load1"] = 2.0
        with self.assertRaises(final_protocol.ProtocolError):
            final_protocol.validate_for_start(broken)

    def test_a_lowered_threshold_is_rejected(self):
        document = final_protocol.candidate_document()
        document["admission"]["max_load1"] = 20
        with self.assertRaises(final_protocol.ProtocolError):
            final_protocol.validate_document(document)

    def test_incomplete_protocol_is_rejected(self):
        document = final_protocol.candidate_document()
        del document["orders"]
        with self.assertRaises(final_protocol.ProtocolError):
            final_protocol.validate_document(document)

    def test_existing_directory_and_argument_divergence(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(final_protocol.ProtocolError):
                final_protocol.refuse_existing_directory(directory)
        document, digest = final_protocol.load(PROTOCOL)
        bound = final_protocol.bind_hash(document, digest)

        class Args:
            phase = "final"
            blocks = 12
            permutation_repeats = 3
            seed = 20261105
            modes = ["json"]
            concurrency = [1]
            warmup_s = 20
            measure_s = 60
            backend_latency_ms = 0
            order = "permutations"
            model = "mock-model"
            experiment_name = "rq1-comparative"
            protocol_sha256 = digest

        self.assertIn("blocks", final_protocol.argument_mismatches(bound, Args()))
        blockers = final_protocol.start_blockers(
            Args(), {"clean": True}, True, {"ok": True},
        )
        self.assertIn("output_directory_exists", blockers)
        self.assertIn("operational_confirmation_missing", blockers)


class TestAdmission(unittest.TestCase):
    def _collect(self, reader, seconds=60.0):
        clock = Clock()
        return admission.collect(
            "inv-synthetic", "/tmp", seconds=seconds, interval=60.0,
            reader=reader, sleeper=clock.sleep,
            component_lister=lambda: ["gateway-comparison-backend-1\tup 2 hours"],
            clock=clock, cpu_reader=_cpu_reader(), volume_reader=_volumes,
        )

    def test_approved_refused_missing_and_stale(self):
        approved = self._collect(lambda: _quiet())
        self.assertTrue(approved["admitted"])
        self.assertGreaterEqual(approved["duration_s"], 60)
        self.assertEqual(approved["units"]["load1"], "loadavg_1min_diagnostic")
        self.assertFalse(approved["load_average"]["blocks_admission"])
        self.assertTrue(approved["cpu"]["per_cpu"])
        self.assertIn("csv", approved["volumes"])
        self.assertIn("docker", approved["volumes"])
        self.assertEqual(
            admission.accept_for_start(approved, "inv-synthetic", approved["finished_at_s"]),
            "",
        )

        noisy = dict(_quiet())
        noisy["load1"] = 11.0
        shared = self._collect(lambda: noisy)
        self.assertTrue(shared["admitted"])
        self.assertNotIn("load1", shared["reasons"])
        self.assertEqual(shared["load_average"]["load1_max"], 11.0)
        self.assertFalse(shared["load_average"]["blocks_admission"])
        tight = admission.judge(
            [noisy], None, 60.0, ["gateway-comparison-backend-1\tup"],
            cpu=_cpu_summary(),
            volumes={
                "csv": {"path": "/tmp", "free_bytes": 30 * GIB},
                "docker": {"path": "/var/lib/docker", "free_bytes": 1 * GIB},
            },
        )
        self.assertFalse(tight["admitted"])
        self.assertIn("docker_volume_free", tight["reasons"])
        self.assertNotIn("load1", tight["reasons"])

        missing = dict(_quiet())
        missing["mem_available_bytes"] = None
        blind = self._collect(lambda: missing)
        self.assertFalse(blind["admitted"])
        self.assertIn("mem_available_missing", blind["reasons"])

        self.assertEqual(
            admission.accept_for_start(approved, "other-invocation", approved["finished_at_s"]),
            "stale_invocation",
        )
        self.assertEqual(
            admission.accept_for_start(approved, "inv-synthetic", approved["finished_at_s"] + 120),
            "stale_age",
        )
        short = self._collect(lambda: _quiet(), seconds=5.0)
        self.assertIn("duration_short", short["reasons"])
        self.assertEqual(
            admission.accept_for_start(short, "inv-synthetic", short["finished_at_s"]),
            "not_the_60s_admission",
        )

    def test_unlisted_components_do_not_pass(self):
        record = admission.judge(
            [_quiet()], None, 60.0, None, cpu=_cpu_summary(), volumes=_volumes("/tmp"),
        )
        self.assertIn("components_unlisted", record["reasons"])
        self.assertFalse(record["admitted"])


class TestMonitorAndBlocks(unittest.TestCase):
    def test_monitor_decides_and_stops_only_for_safety(self):
        samples = [
            _quiet(),
            {"mem_available_bytes": int(0.5 * GIB), "pswpin": 100, "pswpout": 100, "load1": 15.0},
        ]

        def reader():
            return samples.pop(0)

        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, "monitor_decisions.jsonl")
            monitor = admission.Monitor(path, reader=reader, disk_reader=lambda: 30 * GIB)
            self.assertEqual(monitor.poll_once()["host"], "baseline")
            decided = monitor.poll_once()
            self.assertEqual(decided["host"], "grave")
            self.assertTrue(monitor.stop_event.is_set())
            with open(path, encoding="utf-8") as fh:
                self.assertIn('"host": "grave"', fh.read())

        # High load1 alone is not an in-run violation.
        before = _quiet()
        after = dict(_quiet())
        after["load1"] = 15.0
        healthy = admission.classify_interval(before, after, 30 * GIB, 0, final_protocol.MONITOR)
        self.assertEqual(healthy["host"], "ok")

        dropped = dict(_quiet())
        dropped["mem_available_bytes"] = 5 * GIB
        external = admission.classify_interval(_quiet(), dropped, 30 * GIB, 1024, final_protocol.MONITOR)
        self.assertEqual(
            admission.decide_cell(external, False),
            "environmental",
        )
        external["gateway_memory_delta_bytes"] = None
        self.assertEqual(admission.decide_cell(external, False), "uncertain")
        external["gateway_memory_delta_bytes"] = 4 * GIB
        external["host_mem_drop_bytes"] = 5 * GIB
        self.assertEqual(admission.decide_cell(external, False), "system")
        self.assertEqual(admission.decide_cell({"host": "ok"}, True), "system")

    def _drive(self, orders, outcomes, readmission):
        cursor = {"n": 0}

        def on_cell(index, attempt, gateway, position, block_id):
            decision = outcomes[cursor["n"]]
            cursor["n"] += 1
            if decision == "grave-before":
                return {"started": False, "decision": "grave"}
            return {"started": True, "decision": decision}

        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, "block_decisions.csv")
            ledger = block_rules.drive(
                orders, _identity(), on_cell, readmission, path, max_repeats=1,
            )
            rows = block_rules.load_decisions(path)
            return ledger, rows

    def test_environmental_finishes_the_permutation_and_repeats_once(self):
        outcomes = ["environmental", "ok", "ok", "ok", "ok", "ok"]
        ledger, rows = self._drive([GATES], outcomes, lambda: True)
        original = [call for call in ledger.calls if call[1] == "original"]
        repeat = [call for call in ledger.calls if call[1] == "repeat-1"]
        self.assertEqual([call[2] for call in original], GATES)
        self.assertEqual([call[2] for call in repeat], GATES)
        self.assertEqual(ledger.repeats_used, 1)
        included = [row for row in rows if row["include_in_primary"]]
        self.assertEqual(len(included), 1)
        self.assertEqual(included[0]["attempt"], "repeat-1")
        self.assertEqual(
            [row["reason"] for row in rows if row["attempt"] == "original"],
            ["environmental_excluded"],
        )

    def test_grave_stops_without_the_remaining_paths(self):
        ledger, rows = self._drive([GATES], ["ok", "grave-before"], lambda: True)
        self.assertEqual([call[2] for call in ledger.calls], ["direct", "melis"])
        self.assertFalse(ledger.calls[-1][3])
        self.assertTrue(ledger.stopped)
        self.assertTrue(ledger.incomplete)
        self.assertEqual(ledger.repeats_used, 0)
        self.assertEqual(rows[0]["reason"], "safety_stop_partial")
        self.assertTrue(rows[0]["partial"])

    def test_failed_readmission_and_second_violation_stop(self):
        outcomes = ["environmental", "ok", "ok"]
        ledger, rows = self._drive([GATES], outcomes, lambda: False)
        self.assertTrue(all(call[1] == "original" for call in ledger.calls))
        self.assertEqual(len(ledger.calls), 3)
        self.assertEqual(rows[-1]["reason"], "readmission_failed")
        self.assertTrue(ledger.incomplete)

        later = ["environmental", "ok", "ok", "ok", "ok", "ok", "environmental", "ok", "ok"]
        ledger, rows = self._drive([GATES, ["melis", "direct", "litellm"]], later, lambda: True)
        attempts = {(call[0], call[1]) for call in ledger.calls}
        self.assertIn((0, "repeat-1"), attempts)
        self.assertNotIn((1, "repeat-1"), attempts)
        self.assertTrue(ledger.incomplete)
        self.assertEqual(ledger.repeats_used, 1)

    def test_system_failure_is_kept_and_not_repeated(self):
        ledger, rows = self._drive([GATES], ["ok", "system", "ok"], lambda: True)
        self.assertEqual(len(ledger.calls), 3)
        self.assertEqual(ledger.repeats_used, 0)
        self.assertTrue(rows[0]["include_in_primary"])
        self.assertEqual(rows[0]["reason"], "system_under_test")

    def test_attempt_ids_are_not_counted_twice(self):
        experiment = "exp-synthetic-final"
        collection = "col-synthetic-final"
        original = ids.block_id(experiment, "final", 0, collection, attempt="original")
        repeat = ids.block_id(experiment, "final", 0, collection, attempt="repeat-1")
        self.assertNotEqual(original, repeat)
        self.assertIn("repeat-1", repeat)
        with tempfile.TemporaryDirectory() as directory:
            with open(os.path.join(directory, "SYNTHETIC.txt"), "w", encoding="utf-8") as fh:
                fh.write("synthetic phase=final fixture; not a collection\n")
            writer = CsvTableWriter("requests", os.path.join(directory, "requests.csv"))
            for block, latencies in (
                (original, {"direct": 10.0, "melis": 19.0, "litellm": 10.0}),
                (repeat, {"direct": 10.0, "melis": 11.0, "litellm": 10.0}),
            ):
                for gateway, latency in latencies.items():
                    writer.write_row(_request(
                        experiment, collection, "final", block, gateway, latency=latency,
                    ))
            writer.close()
            block_rules.write_decisions(os.path.join(directory, "block_decisions.csv"), [
                {
                    "planned_block_index": 0, "attempt": "original", "block_id": original,
                    "include_in_primary": False, "reason": "environmental_excluded",
                    "attribution": "environmental", "gateways_started": GATES, "partial": False,
                },
                {
                    "planned_block_index": 0, "attempt": "repeat-1", "block_id": repeat,
                    "include_in_primary": True, "reason": "included",
                    "attribution": "ok", "gateways_started": GATES, "partial": False,
                },
            ])
            built = aggregate.build(
                directory, os.path.join(directory, "runs.csv"),
                os.path.join(directory, "comparisons.csv"),
                experiment, "final", 20261105,
            )
            with open(os.path.join(directory, "comparisons.csv"), encoding="utf-8") as fh:
                comparisons = fh.read()
            self.assertIn(",final,", comparisons)
            self.assertNotIn("confirmatory", comparisons)
            melis = [
                row for row in built["pairing"]["excluded"]
            ]
            self.assertTrue(any(row["block_id"] == original for row in melis))
            text = comparisons.splitlines()
            header = text[0].split(",")
            estimate_at = header.index("estimate")
            n_at = header.index("n")
            phase_at = header.index("phase")
            contrast_at = header.index("contrast")
            for line in text[1:]:
                cells = line.split(",")
                self.assertEqual(cells[phase_at], "final")
                self.assertEqual(cells[n_at], "1")
                if cells[contrast_at] == "melis_minus_direct":
                    self.assertEqual(cells[estimate_at], "1.0")
            report = validate_dir(directory)
            self.assertTrue(report["ok"], report["errors"])
            manifest = build_manifest(
                experiment, "final", os.path.dirname(os.path.dirname(ROOT)),
                image_digests={}, models={}, config_files={},
            )
            self.assertEqual(manifest["phase"], "final")
            self.assertIn("SYNTHETIC.txt", os.listdir(directory))

    def test_gateway_failure_remains_a_system_result(self):
        experiment = "exp-synthetic-final"
        collection = "col-synthetic-final"
        block = ids.block_id(experiment, "final", 1, collection)
        with tempfile.TemporaryDirectory() as directory:
            with open(os.path.join(directory, "SYNTHETIC.txt"), "w", encoding="utf-8") as fh:
                fh.write("synthetic phase=final fixture; not a collection\n")
            writer = CsvTableWriter("requests", os.path.join(directory, "requests.csv"))
            writer.write_row(_request(experiment, collection, "final", block, "direct"))
            writer.write_row(_request(experiment, collection, "final", block, "melis", latency=12.0))
            writer.write_row(_request(
                experiment, collection, "final", block, "litellm",
                status="transport_error", latency=1.0,
            ))
            writer.close()
            block_rules.write_decisions(os.path.join(directory, "block_decisions.csv"), [{
                "planned_block_index": 1, "attempt": "original", "block_id": block,
                "include_in_primary": True, "reason": "system_under_test",
                "attribution": "system", "gateways_started": GATES, "partial": False,
            }])
            built = aggregate.build(directory, None, None, experiment, "final", 1)
            reasons = [row["reason"] for row in built["pairing"]["excluded"]]
            self.assertIn("system_under_test", reasons)
            self.assertNotIn("environmental_excluded", reasons)
            with open(os.path.join(directory, "requests.csv"), encoding="utf-8") as fh:
                raw = fh.read()
            self.assertIn("litellm", raw)
            self.assertIn("transport_error", raw)


class TestImageAndDryRun(unittest.TestCase):
    def test_a_different_image_is_rejected(self):
        expected = final_protocol.IMAGE_ID
        final_protocol.require_container_image(expected, expected)
        with self.assertRaises(final_protocol.ProtocolError):
            final_protocol.require_container_image(expected, "sha256:" + "ab" * 32)
        with self.assertRaises(final_protocol.ProtocolError):
            final_protocol.require_container_image(expected, "gateway-comparison-melis:latest")
        self.assertFalse(final_protocol.reference_is_proof(
            "gateway-comparison-melis:latest", expected,
        ))

    def test_reuse_does_not_relabel_the_image_with_head(self):
        same = final_protocol.image_reuse(True, final_protocol.IMAGE_BUILD_COMMIT, "")
        self.assertTrue(same["ok"])
        self.assertFalse(same["image_labeled_with_harness_commit"])
        dirty = final_protocol.image_reuse(False, "abc", None)
        self.assertFalse(dirty["ok"])
        changed = final_protocol.image_reuse(True, "a" * 40, "1\t0\tsrc/main.rs\n")
        self.assertFalse(changed["ok"])
        self.assertEqual(changed["reason"], "build_inputs_differ")
        same_inputs = final_protocol.image_reuse(True, "b" * 40, "")
        self.assertTrue(same_inputs["ok"])
        self.assertEqual(same_inputs["image_source_commit"], final_protocol.IMAGE_BUILD_COMMIT)
        self.assertNotEqual(same_inputs["harness_commit"], final_protocol.IMAGE_BUILD_COMMIT)
        self.assertFalse(same_inputs["image_labeled_with_harness_commit"])

    def test_dry_run_prints_the_plan_and_starts_nothing(self):
        with open(AMENDMENT + ".sha256", encoding="utf-8") as fh:
            digest = fh.read().split()[0]
        out = os.path.join(tempfile.gettempdir(), "melis-final-dry-run-should-not-exist")
        if os.path.exists(out):
            os.rmdir(out)
        argv = [
            "--dry-run", "--phase", "final",
            "--protocol", AMENDMENT, "--protocol-sha256", digest,
            "--out-dir", out,
            "--blocks", "18", "--modes", "json", "--concurrency", "1",
            "--warmup-s", "20", "--measure-s", "60", "--backend-latency-ms", "0",
            "--order", "permutations", "--permutation-repeats", "3",
            "--seed", "20261105", "--max-load-seconds", "5200",
            "--model", "mock-model", "--experiment-name", "rq1-comparative",
        ]
        stdout = io.StringIO()
        old = sys.stdout
        sys.stdout = stdout
        try:
            code = run_comparative_pilot.main(argv)
        finally:
            sys.stdout = old
        self.assertEqual(code, 0)
        self.assertFalse(os.path.exists(out))
        estimate = json.loads(stdout.getvalue())["estimate"]
        self.assertEqual(estimate["cells"], 54)
        self.assertEqual(estimate["load_seconds"], 4320)
        self.assertEqual(estimate["switch_overhead_s_extrapolated"], 466.866)
        self.assertFalse(estimate["overhead_included_in_load_seconds"])
        self.assertFalse(estimate["load_started"])
        self.assertFalse(estimate["admission_ran"])
        self.assertFalse(estimate["image_checked_this_process"])
        self.assertFalse(estimate["execution_authorized_by_protocol"])

        bad = list(argv)
        bad[bad.index("18")] = "12"
        with self.assertRaises(SystemExit):
            run_comparative_pilot.main(bad)

    def test_safety_stop_admits_no_request(self):
        stop = threading.Event()
        stop.set()
        summary = conc_load.run_cell(
            "http://127.0.0.1:9", "mock-model", "json", 1, None, 16, 1.0,
            deadline_mono=time.perf_counter() + 30,
            stop_event=stop, collect=False, requested_duration_s=30,
        )
        self.assertEqual(summary["termination_reason"], "safety_stop")
        self.assertTrue(summary["truncated"])
        self.assertEqual(summary["n"], 0)
        self.assertEqual(summary["admitted"], 0)


if __name__ == "__main__":
    unittest.main(verbosity=2)
