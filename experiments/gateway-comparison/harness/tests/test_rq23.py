#!/usr/bin/env python3
"""RQ2 rubric and RQ3 fault agenda. Synthetic. Not a final collection."""
from __future__ import annotations

import json
import os
import sys
import threading
import unittest
import urllib.error
import urllib.request
from http.server import ThreadingHTTPServer

HERE = os.path.dirname(os.path.abspath(__file__))
HARNESS = os.path.dirname(HERE)
ROOT = os.path.dirname(HARNESS)
sys.path.insert(0, HARNESS)
sys.path.insert(0, os.path.join(ROOT, "backend-deterministic"))

import mock_openai  # noqa: E402
import quality_checks  # noqa: E402


def _case():
    return {
        "critical_facts": [{
            "id": "berth",
            "equivalents": ["cais 14"],
            "contradictions": ["cais 2"],
        }],
        "other_facts": [{
            "id": "shift",
            "equivalents": ["turno da manha"],
            "contradictions": ["turno da noite"],
        }],
        "question_anchors": ["cais"],
        "decision_changing_inventions": ["cais 99"],
    }


class RubricTests(unittest.TestCase):
    def test_scores_do_not_overlap(self):
        case = _case()
        self.assertEqual(quality_checks.rubric(case, "O cais 14 no turno da manha.")["score"], 3)
        self.assertEqual(quality_checks.rubric(case, "O cais 14.")["rule"], "noncritical_omission")
        self.assertEqual(quality_checks.rubric(case, "O cais 14 no turno da noite.")["score"], 1)
        self.assertEqual(quality_checks.rubric(case, "O cais 2 no turno da manha.")["score"], 0)
        self.assertEqual(quality_checks.rubric(case, "Nada a declarar.")["score"], 0)
        self.assertEqual(quality_checks.rubric(case, "O cais 99.")["score"], 0)

    def test_negative_control_rejects_the_wrong_fact(self):
        case = _case()
        for item in quality_checks.negative_controls(case):
            self.assertEqual(quality_checks.rubric(case, item["answer"])["score"], item["must_score"])

    def test_context_channel_is_separate_from_the_answer(self):
        case = _case()
        forwarded = "O cais 14. turno da manha."
        answer = "O cais 14."
        context = quality_checks.channel_rows(case, forwarded, "forwarded_context")
        replied = quality_checks.channel_rows(case, answer, "answer")
        self.assertTrue(all(row["observed_present"] for row in context))
        by_id = {row["fact_id"]: row["observed_present"] for row in replied}
        self.assertTrue(by_id["berth"])
        self.assertFalse(by_id["shift"])

    def test_development_set_is_twenty_four_and_not_the_eval_families(self):
        with open(os.path.join(ROOT, "quality", "dev_cases.json"), encoding="utf-8") as fh:
            dev = json.load(fh)
        with open(os.path.join(ROOT, "quality", "eval_proposal.json"), encoding="utf-8") as fh:
            proposal = json.load(fh)
        self.assertEqual(len(dev["cases"]), 24)
        self.assertTrue(dev["not_evaluation"])
        self.assertEqual(dev["generation_reps_pilot"], 1)
        self.assertTrue(proposal["not_scored"])
        self.assertEqual(proposal["planned_case_count"], 48)
        overlap = set(dev["families"]) & set(proposal["families"])
        self.assertEqual(overlap, set())


class FaultTests(unittest.TestCase):
    def test_schedule_is_opt_in_and_counts_failures(self):
        mock_openai._rules.clear()
        mock_openai._calls.clear()
        server = ThreadingHTTPServer(("127.0.0.1", 0), mock_openai.Handler)
        port = server.server_address[1]
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            base = f"http://127.0.0.1:{port}"
            self._post(base, "/__schedule", {"rules": [{
                "destination": "primary", "from_call": 1, "to_call": 1,
                "status": 429, "retry_after_s": 1,
            }]})
            first = self._post(base, "/v1/chat/completions", {"model": "mock-model", "messages": []})
            self.assertEqual(first.status, 429)
            self.assertEqual(first.headers.get("Retry-After"), "1")
            second = self._post(base, "/v1/chat/completions", {"model": "mock-model", "messages": []})
            self.assertEqual(second.status, 200)
            calls = json.loads(self._get(base, "/__calls"))["calls"]
            self.assertEqual([call["status"] for call in calls], [429, 200])
            self._post(base, "/__reset", {})
            quiet = self._post(base, "/v1/chat/completions", {"model": "mock-model", "messages": []})
            self.assertEqual(quiet.status, 200)
        finally:
            server.shutdown()

    def _post(self, base, path, payload):
        data = json.dumps(payload).encode()
        req = urllib.request.Request(base + path, data=data, method="POST")
        req.add_header("Content-Type", "application/json")
        try:
            with urllib.request.urlopen(req, timeout=5) as resp:
                resp.read()
                return resp
        except urllib.error.HTTPError as exc:
            exc.read()
            return exc

    def _get(self, base, path):
        with urllib.request.urlopen(base + path, timeout=5) as resp:
            return resp.read().decode()


if __name__ == "__main__":
    unittest.main()
