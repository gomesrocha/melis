#!/usr/bin/env python3
"""Block decisions for phase=final.

A planned block is the analysis unit. A repeat is another attempt of that
same block. It is not an extra block and it does not increase N. The
primary mean uses at most one attempt of each planned block.
"""
from __future__ import annotations

import csv
import os
from collections import deque

import ids

DECISION_FIELDS = (
    "planned_block_index",
    "attempt",
    "block_id",
    "include_in_primary",
    "reason",
    "attribution",
    "gateways_started",
    "partial",
)

REASONS = {
    "ok": "included",
    "system": "system_under_test",
    "uncertain": "uncertain_held_out",
    "environmental": "environmental_excluded",
    "grave": "safety_stop_partial",
    "metric_missing": "metric_missing_stop",
}


def combine(classes: list[str]) -> str:
    """One decision for the attempt. Mixed system and environmental stays uncertain."""
    if not classes:
        return "metric_missing"
    if "grave" in classes:
        return "grave"
    if "metric_missing" in classes:
        return "metric_missing"
    if "environmental" in classes and "system" in classes:
        return "uncertain"
    if "environmental" in classes:
        return "environmental"
    if "uncertain" in classes:
        return "uncertain"
    if "system" in classes:
        return "system"
    return "ok"


class AttemptLedger:
    def __init__(self, max_repeats: int = 1):
        self.max_repeats = max_repeats
        self.repeats_used = 0
        self.stopped = False
        self.incomplete = False
        self.records = []

    def finish(self, planned_index, attempt, block_id, attribution, gateways_started, n_gateways):
        partial = attribution in ("grave", "metric_missing") or len(gateways_started) != n_gateways
        include = attribution in ("ok", "system") and not partial
        self.records.append({
            "planned_block_index": planned_index,
            "attempt": attempt,
            "block_id": block_id,
            "include_in_primary": include,
            "reason": REASONS[attribution],
            "attribution": attribution,
            "gateways_started": list(gateways_started),
            "partial": partial,
        })
        if attribution in ("grave", "metric_missing"):
            self.stopped = True
            self.incomplete = True
            return "stop"
        if attribution == "environmental":
            if attempt != "original" or self.repeats_used >= self.max_repeats:
                self.stopped = True
                self.incomplete = True
                return "stop"
            return "repeat"
        return "continue"

    def readmission_failed(self, planned_index):
        self.records.append({
            "planned_block_index": planned_index,
            "attempt": "repeat-1",
            "block_id": "",
            "include_in_primary": False,
            "reason": "readmission_failed",
            "attribution": "readmission_failed",
            "gateways_started": [],
            "partial": False,
        })
        self.stopped = True
        self.incomplete = True


def write_decisions(path: str, records: list[dict]) -> None:
    with open(path, "w", newline="", encoding="utf-8") as fh:
        writer = csv.DictWriter(fh, fieldnames=DECISION_FIELDS)
        writer.writeheader()
        for record in records:
            row = dict(record)
            row["include_in_primary"] = "true" if record["include_in_primary"] else "false"
            row["partial"] = "true" if record["partial"] else "false"
            row["gateways_started"] = ",".join(record["gateways_started"])
            writer.writerow(row)


def load_decisions(path: str):
    """Return None when the collection has no decision ledger.

    Two included attempts of one planned block are an error, not two blocks.
    """
    if not os.path.exists(path):
        return None
    with open(path, newline="", encoding="utf-8") as fh:
        rows = list(csv.DictReader(fh))
    primaries = {}
    for row in rows:
        row["include_in_primary"] = row.get("include_in_primary") == "true"
        row["partial"] = row.get("partial") == "true"
        if row["include_in_primary"]:
            key = row["planned_block_index"]
            if key in primaries:
                raise ValueError(
                    f"planned block {key} has two attempts in the primary estimate"
                )
            primaries[key] = row["block_id"]
    return rows


def drive(orders, identity, on_cell, readmission, decision_path, max_repeats=1):
    """Run planned orders. ``on_cell`` performs one gateway or refuses to start it.

    ``on_cell(index, attempt, gateway, position, block_id)`` returns
    ``{"started": bool, "decision": str}``. ``readmission`` returns True
    only for a new approved 60 s sample. This function does not sleep and
    does not open sockets.
    """
    ledger = AttemptLedger(max_repeats=max_repeats)
    queue = deque(("original", index, list(order)) for index, order in enumerate(orders))
    calls = []
    while queue and not ledger.stopped:
        attempt, index, order = queue.popleft()
        if attempt != "original":
            if not readmission():
                ledger.readmission_failed(index)
                break
            ledger.repeats_used += 1
        block_id = ids.block_id(
            identity["experiment_id"], identity["phase"], index,
            identity["collection_id"], attempt=attempt,
        )
        classes = []
        started = []
        for position, gateway in enumerate(order):
            outcome = on_cell(index, attempt, gateway, position, block_id)
            calls.append((index, attempt, gateway, outcome.get("started", False)))
            if not outcome.get("started", False):
                classes.append(outcome.get("decision") or "grave")
                break
            started.append(gateway)
            classes.append(outcome["decision"])
            if outcome["decision"] in ("grave", "metric_missing"):
                break
        action = ledger.finish(index, attempt, block_id, combine(classes), started, len(order))
        if action == "repeat":
            queue.appendleft(("repeat-1", index, order))
    if decision_path:
        write_decisions(decision_path, ledger.records)
    ledger.calls = calls
    return ledger
