#!/usr/bin/env python3
"""Authorized RQ3 final collection. The five-episode pilot is not an input.

Gateway episodes are paired Melis/LiteLLM. Direct is a separate backend
control and is not added to that paired N. A zero-call gateway episode stops
the collection; completed rows stay on disk and are not retried.
"""
from __future__ import annotations

import hashlib
import json
import os
import random
import subprocess
import sys
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)

import rq3_gateway_pilot as pilot  # noqa: E402

FINAL_EPISODES = 20
SEED = 20261106


def gateway_orders(seed=SEED, n=FINAL_EPISODES):
    """Ten episodes with each gateway first. The seed fixes the sequence."""
    if n % 2:
        raise ValueError("n must be even so each order occurs equally often")
    rng = random.Random(seed)
    flags = [0] * (n // 2) + [1] * (n // 2)
    rng.shuffle(flags)
    return [("melis", "litellm") if flag == 0 else ("litellm", "melis") for flag in flags]


def _sha256(path):
    return hashlib.sha256(open(path, "rb").read()).hexdigest()


REPO = os.path.dirname(os.path.dirname(ROOT))


def verify_design(protocol):
    if protocol.get("execution_authorized") is not True:
        raise SystemExit("refusing: protocol does not authorize execution")
    if protocol.get("episodes_per_scenario") != FINAL_EPISODES:
        raise SystemExit("refusing: episode count is not the authorized 20")
    expected = [list(order) for order in gateway_orders()]
    if protocol.get("gateway_order") != expected:
        raise SystemExit("refusing: gateway order differs from the seeded sequence")
    if protocol.get("deadline_ms") != pilot.DEADLINE_MS:
        raise SystemExit("refusing: deadline differs from the piloted 2500 ms")
    for name, rel in protocol["frozen_files"].items():
        digest = _sha256(os.path.join(ROOT, rel))
        if digest != protocol["frozen_sha256"][name]:
            raise SystemExit(f"refusing: {name} hash differs from the protocol")


def verify_commit(protocol):
    named = protocol["harness_commit"]
    paths = [
        os.path.join("experiments/gateway-comparison", rel)
        for rel in protocol["frozen_files"].values()
    ]
    diff = subprocess.run(
        ["git", "diff", "--exit-code", named, "HEAD", "--", *paths],
        cwd=REPO, capture_output=True,
    )
    if diff.returncode != 0:
        raise SystemExit("refusing: frozen files differ from the recorded harness commit")


def _check(protocol):
    verify_design(protocol)
    verify_commit(protocol)


def confirm_runtime(protocol):
    """Image ids and the files mounted in the containers, before any episode."""
    expected = {
        "gwcomp-rq3-melis": protocol["images"]["melis_id"],
        "gwcomp-rq3-litellm": protocol["images"]["litellm_id"],
    }
    for name, image_id in expected.items():
        actual = subprocess.check_output(
            ["docker", "inspect", "-f", "{{.Image}}", name], text=True,
        ).strip()
        if actual != image_id:
            raise SystemExit(f"refusing: {name} image {actual} is not {image_id}")
    mounted = (
        ("gwcomp-rq3-melis", "/app/config.yaml", "melis_config"),
        ("gwcomp-rq3-melis", "/etc/melis/routes.yaml", "melis_routes"),
        ("gwcomp-rq3-litellm", "/app/config.yaml", "litellm_config"),
    )
    for container, path, key in mounted:
        payload = subprocess.check_output(["docker", "exec", container, "cat", path])
        digest = hashlib.sha256(payload).hexdigest()
        if digest != protocol["frozen_sha256"][key]:
            raise SystemExit(f"refusing: {container}:{path} is not the frozen file")
    for port in (17170, 17172):
        with urllib.request.urlopen(f"http://127.0.0.1:{port}/health", timeout=2) as resp:
            if resp.status != 200:
                raise SystemExit(f"refusing: mock on {port} is not ready")


def _preflight():
    rows = []
    for client in ("melis", "litellm", "direct"):
        row = pilot.run_client(client, "healthy")
        row["role"] = "preflight"
        rows.append(row)
        if row["http_status"] != 200 or row["backend_call_count"] < 1:
            raise SystemExit(f"refusing: preflight for {client} did not reach the backend")
    return rows


def _row(client, scenario, episode, position, role):
    row = pilot.run_client(client, scenario)
    row["episode_index"] = episode
    row["order_in_pair"] = position
    row["role"] = role
    if row["backend_call_count"] == 0:
        row["stop_reason"] = "zero_backend_calls"
    return row


def run(protocol_path, out_dir):
    protocol = json.load(open(protocol_path, encoding="utf-8"))
    _check(protocol)
    os.makedirs(out_dir, exist_ok=True)
    pilot.ensure_containers()
    confirm_runtime(protocol)
    preflight = _preflight()
    json.dump(preflight, open(os.path.join(out_dir, "preflight.json"), "w", encoding="utf-8"), indent=2)
    started = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=REPO, text=True).strip()
    json.dump({
        "collection_id": protocol["collection_id"],
        "phase": "final",
        "started_head": started,
        "harness_commit": protocol["harness_commit"],
        "protocol_sha256": _sha256(protocol_path),
        "images": protocol["images"],
    }, open(os.path.join(out_dir, "manifest-start.json"), "w", encoding="utf-8"), indent=2)
    dest = os.path.join(out_dir, "episodes.jsonl")
    if os.path.exists(dest) and os.path.getsize(dest):
        raise SystemExit("refusing: final episode log already exists")
    for scenario in pilot.SCENARIOS:
        for episode, order in enumerate(protocol["gateway_order"]):
            for position, client in enumerate(order):
                row = _row(client, scenario, episode, position, "gateway_pair")
                with open(dest, "a", encoding="utf-8") as fh:
                    fh.write(json.dumps(row) + "\n")
                print(scenario, episode, client, row["http_status"],
                      "within", row["success_within_deadline"],
                      "calls", row["backend_call_count"], flush=True)
                if row.get("stop_reason"):
                    raise SystemExit("stopping: an episode reached no backend call")
        for episode in range(FINAL_EPISODES):
            row = _row("direct", scenario, episode, 0, "backend_control")
            with open(dest, "a", encoding="utf-8") as fh:
                fh.write(json.dumps(row) + "\n")
            print(scenario, episode, "direct", row["http_status"],
                  "within", row["success_within_deadline"],
                  "calls", row["backend_call_count"], flush=True)
            if row.get("stop_reason"):
                raise SystemExit("stopping: an episode reached no backend call")
    return dest


if __name__ == "__main__":
    raise SystemExit("import run after the protocol, mocks and containers are ready")
