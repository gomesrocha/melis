#!/usr/bin/env python3
"""RQ3 pilot through Melis and LiteLLM. Direct calls the primary mock only.

Provider timeout is 1 second on both gateways. The logical deadline is 2500 ms,
long enough for one timed-out attempt and one fallback attempt. num_retries
stays 0 on LiteLLM. Melis moves to the next configured provider on 429, 5xx,
and timeout; it does not sleep on Retry-After. Containers are restarted before
each gateway episode so circuit-breaker state does not carry over.
"""
from __future__ import annotations

import http.client
import json
import os
import socket
import subprocess
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
PRIMARY = "http://127.0.0.1:17170"
FALLBACK = "http://127.0.0.1:17172"
MELIS = "http://127.0.0.1:17081/v1/chat/completions"
LITELLM = "http://127.0.0.1:14010/v1/chat/completions"
DEADLINE_MS = 2500
EPISODES = 5
SEED = 20261106
IMAGE_MELIS = "sha256:79adb313b5060df96c6e5218bb108a289cd7d17c1f1fa08df9b221a6232c43bb"
IMAGE_LITE = "ghcr.io/berriai/litellm@sha256:6c82d338a60e7b273ae46bf1d8db814d2856ae010f96c44eeadde574d3893f76"

SCENARIOS = {
    "healthy": ([], []),
    "rq3_429_retry_after": ([{
        "destination": "primary", "from_call": 1, "to_call": 2,
        "status": 429, "retry_after_s": 1,
    }], []),
    "rq3_5xx_transient": ([{
        "destination": "primary", "from_call": 1, "to_call": 2, "status": 503,
    }], []),
    "rq3_timeout": ([{
        "destination": "primary", "from_call": 1, "to_call": 1, "status": 200, "delay_ms": 1500,
    }], []),
    "rq3_primary_down_fallback": ([{
        "destination": "primary", "from_call": 1, "to_call": 9, "status": 503,
    }], []),
    "rq3_none_healthy_then_recovery": ([{
        "destination": "primary", "from_call": 1, "to_call": 1, "status": 503,
    }], [{
        "destination": "primary", "from_call": 1, "to_call": 1, "status": 503,
    }]),
}


def _post(url, payload, timeout, headers=None):
    data = json.dumps(payload).encode()
    req = urllib.request.Request(url, data=data, method="POST")
    req.add_header("Content-Type", "application/json")
    for key, value in (headers or {}).items():
        req.add_header(key, value)
    started = time.perf_counter()
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            resp.read()
            code = resp.status
    except urllib.error.HTTPError as exc:
        exc.read()
        code = exc.code
        kind = "http"
    except TimeoutError:
        code, kind = 0, "timeout"
    except urllib.error.URLError as exc:
        code = 0
        kind = "timeout" if isinstance(getattr(exc, "reason", None), TimeoutError) else "transport"
    except http.client.RemoteDisconnected:
        code, kind = 0, "transport"
    else:
        kind = "http"
    return code, (time.perf_counter() - started) * 1000.0, kind


def _schedule(base, rules):
    _post(base + "/__schedule", {"rules": rules}, timeout=2)


def _calls(base):
    with urllib.request.urlopen(base + "/__calls", timeout=2) as resp:
        return json.loads(resp.read().decode())["calls"]


def _wait_port(port, seconds):
    deadline = time.perf_counter() + seconds
    while time.perf_counter() < deadline:
        try:
            with socket.create_connection(("127.0.0.1", port), 0.3):
                return True
        except OSError:
            time.sleep(0.3)
    return False


def ensure_containers():
    subprocess.run(["docker", "rm", "-f", "gwcomp-rq3-melis", "gwcomp-rq3-litellm"],
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    subprocess.check_call([
        "docker", "run", "-d", "--name", "gwcomp-rq3-melis",
        "--network", "bridge", "--add-host", "host.docker.internal:host-gateway",
        "-p", "127.0.0.1:17081:7071",
        "-v", f"{ROOT}/melis/config.rq3-pilot.yaml:/app/config.yaml:ro",
        "-v", f"{ROOT}/melis/routes.rq3-pilot.yaml:/etc/melis/routes.yaml:ro",
        "-e", "MELIS_SERVER_PORT=7071", IMAGE_MELIS,
    ])
    subprocess.check_call([
        "docker", "run", "-d", "--name", "gwcomp-rq3-litellm",
        "--network", "bridge", "--add-host", "host.docker.internal:host-gateway",
        "--memory", "2g",
        "-p", "127.0.0.1:14010:4000",
        "-v", f"{ROOT}/litellm/config.rq3-pilot.yaml:/app/config.yaml:ro",
        IMAGE_LITE, "--config", "/app/config.yaml", "--port", "4000", "--num_workers", "1",
    ])
    if not _wait_port(17081, 20) or not _wait_port(14010, 60):
        raise SystemExit("a gateway did not open its port")


def _wait_litellm(seconds):
    deadline = time.perf_counter() + seconds
    while time.perf_counter() < deadline:
        try:
            with urllib.request.urlopen("http://127.0.0.1:14010/health/liveliness", timeout=1) as resp:
                if resp.status == 200:
                    return True
        except (urllib.error.URLError, TimeoutError, OSError):
            time.sleep(0.5)
    return False


def _wait_melis(seconds):
    before = subprocess.run(
        ["docker", "logs", "gwcomp-rq3-melis"], capture_output=True, text=True,
    )
    seen = (before.stdout + before.stderr).count("Listening on")
    deadline = time.perf_counter() + seconds
    while time.perf_counter() < deadline:
        logs = subprocess.run(
            ["docker", "logs", "gwcomp-rq3-melis"], capture_output=True, text=True,
        )
        if (logs.stdout + logs.stderr).count("Listening on") > seen:
            return True
        time.sleep(0.5)
    return False


def restart(name, port, seconds):
    subprocess.check_call(["docker", "restart", name], stdout=subprocess.DEVNULL)
    if name == "gwcomp-rq3-litellm":
        ready = _wait_litellm(seconds)
    else:
        ready = _wait_melis(max(seconds, 40))
    if not ready:
        raise SystemExit(f"{name} did not return")


def run_client(client, scenario):
    primary_rules, fallback_rules = SCENARIOS[scenario]
    _schedule(PRIMARY, primary_rules)
    _schedule(FALLBACK, fallback_rules)
    if client == "melis":
        restart("gwcomp-rq3-melis", 17081, 20)
        url, model = MELIS, "mock-model"
    elif client == "litellm":
        restart("gwcomp-rq3-litellm", 14010, 60)
        url, model = LITELLM, "mock-primary"
    else:
        url, model = PRIMARY + "/v1/chat/completions", "mock-model"
    started_at = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%fZ")
    started = time.perf_counter()
    code, _elapsed, kind = _post(
        url, {"model": model, "messages": [{"role": "user", "content": "ping"}]},
        timeout=DEADLINE_MS / 1000.0,
    )
    total_ms = (time.perf_counter() - started) * 1000.0
    primary_calls = _calls(PRIMARY)
    fallback_calls = _calls(FALLBACK)
    within = code == 200 and total_ms <= DEADLINE_MS
    recovery_observed = None
    recovery_censored = scenario == "rq3_timeout" and not within
    if scenario == "rq3_none_healthy_then_recovery" and not within:
        probe, _, _probe_kind = _post(
            PRIMARY + "/v1/chat/completions",
            {"model": "mock-model", "messages": [{"role": "user", "content": "probe"}]},
            timeout=1,
        )
        recovery_observed = probe == 200
        recovery_censored = not recovery_observed
    return {
        "client": client,
        "scenario": scenario,
        "started_at_utc": started_at,
        "error_kind": kind,
        "http_status": code,
        "latency_ms": round(total_ms, 3),
        "success_within_deadline": within,
        "success_after_deadline": code == 200 and not within,
        "failed": code != 200,
        "recovery_observed": recovery_observed,
        "recovery_censored": recovery_censored,
        "primary_calls": primary_calls,
        "fallback_calls": fallback_calls,
        "backend_call_count": len(primary_calls) + len(fallback_calls),
        "seed": SEED,
        "deadline_ms": DEADLINE_MS,
    }


def main(out_path=None):
    rows = []
    clients = ["direct", "melis", "litellm"]
    for scenario in SCENARIOS:
        for episode in range(EPISODES):
            order = clients[episode % 3:] + clients[:episode % 3]
            for position, client in enumerate(order):
                row = run_client(client, scenario)
                row["episode_index"] = episode
                row["order_in_pair"] = position
                rows.append(row)
                if out_path:
                    with open(out_path, "a", encoding="utf-8") as fh:
                        fh.write(json.dumps(row) + "\n")
                print(scenario, episode, client, row["http_status"],
                      "within", row["success_within_deadline"],
                      "calls", row["backend_call_count"], flush=True)
    return rows


if __name__ == "__main__":
    raise SystemExit("import main after the mocks and containers are up")
