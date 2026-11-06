#!/usr/bin/env python3
"""Development pilot for RQ3. Five episodes per scenario.

``direct`` tries the primary once. ``aligned_failover`` tries the fallback
destination once after a primary failure and does not retry the same
destination. That is the Melis OpenAI behavior already in the tree: a 429
moves to another provider instead of sleeping on Retry-After. It is not
LiteLLM ``num_retries``. This script does not label either client as a gateway.
"""
from __future__ import annotations

import json
import os
import time
import urllib.error
import urllib.request

EPISODES_PER_SCENARIO = 5
SEED = 20261106
DEADLINE_MS = 1000

SCENARIOS = {
    "healthy": [],
    "rq3_429_retry_after": [{
        "destination": "primary", "from_call": 1, "to_call": 2,
        "status": 429, "retry_after_s": 1,
    }],
    "rq3_5xx_transient": [{
        "destination": "primary", "from_call": 1, "to_call": 2, "status": 503,
    }],
    "rq3_timeout": [{
        "destination": "primary", "from_call": 1, "to_call": 1, "status": 200, "delay_ms": 1500,
    }],
    "rq3_primary_down_fallback": [{
        "destination": "primary", "from_call": 1, "to_call": 9, "status": 503,
    }],
    "rq3_none_healthy_then_recovery": [{
        "destination": "primary", "from_call": 1, "to_call": 1, "status": 503,
    }, {
        "destination": "fallback", "from_call": 1, "to_call": 1, "status": 503,
    }],
}


def _post(url, payload, headers=None, timeout=2.0):
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
    except (TimeoutError, urllib.error.URLError):
        code = 0
    return code, (time.perf_counter() - started) * 1000.0


def _schedule(base, rules):
    _post(base + "/__schedule", {"rules": rules}, timeout=2.0)


def _calls(base):
    with urllib.request.urlopen(base + "/__calls", timeout=2.0) as resp:
        return json.loads(resp.read().decode())["calls"]


def run_episode(base, scenario, client, deadline_ms=DEADLINE_MS):
    """One episode. State is reset by replacing the schedule."""
    _schedule(base, SCENARIOS[scenario])
    started = time.perf_counter()
    events = [{"relative_ms": 0.0, "event_kind": "scheduled", "event_type": "agenda_set"}]
    outcome = _post(
        base + "/v1/chat/completions",
        {"model": "mock-model", "messages": [{"role": "user", "content": "ping"}]},
        headers={"X-Experiment-Destination": "primary"},
        timeout=deadline_ms / 1000.0,
    )
    events.append({
        "relative_ms": (time.perf_counter() - started) * 1000.0,
        "event_kind": "observed", "event_type": "primary", "state": str(outcome[0]),
    })
    code, elapsed = outcome
    used_fallback = False
    if client == "aligned_failover" and code != 200:
        code, elapsed = _post(
            base + "/v1/chat/completions",
            {"model": "mock-model", "messages": [{"role": "user", "content": "ping"}]},
            headers={"X-Experiment-Destination": "fallback"},
            timeout=max(0.05, (deadline_ms - elapsed) / 1000.0),
        )
        used_fallback = True
        events.append({
            "relative_ms": (time.perf_counter() - started) * 1000.0,
            "event_kind": "observed", "event_type": "fallback", "state": str(code),
        })
    total_ms = (time.perf_counter() - started) * 1000.0
    # A delayed 200 that misses the deadline is not a success inside it.
    within = code == 200 and total_ms <= deadline_ms
    after = code == 200 and not within
    recovery_observed = None
    recovery_censored = scenario == "rq3_timeout" and not within
    if scenario == "rq3_none_healthy_then_recovery" and not within:
        probe, _probe_ms = _post(
            base + "/v1/chat/completions",
            {"model": "mock-model", "messages": [{"role": "user", "content": "probe"}]},
            headers={"X-Experiment-Destination": "primary"},
            timeout=deadline_ms / 1000.0,
        )
        recovery_observed = probe == 200
        recovery_censored = not recovery_observed
        events.append({
            "relative_ms": (time.perf_counter() - started) * 1000.0,
            "event_kind": "observed", "event_type": "recovery_probe", "state": str(probe),
        })
    calls = _calls(base)
    return {
        "client": client,
        "scenario": scenario,
        "success_within_deadline": within,
        "success_after_deadline": after,
        "failed": code != 200,
        "recovery_observed": recovery_observed,
        "recovery_censored": recovery_censored,
        "backend_call_count": len(calls),
        "attempt_count_scope": "per_attempt",
        "used_fallback": used_fallback,
        "events": events,
        "calls": calls,
    }


def main(base):
    rows = []
    for scenario in SCENARIOS:
        for episode in range(EPISODES_PER_SCENARIO):
            # Balance which client goes first inside the pair.
            order = ["direct", "aligned_failover"]
            if episode % 2:
                order = list(reversed(order))
            for position, client in enumerate(order):
                row = run_episode(base, scenario, client)
                row["episode_index"] = episode
                row["order_in_pair"] = position
                row["seed"] = SEED
                rows.append(row)
    return rows


if __name__ == "__main__":
    raise SystemExit("import and call main(base); this module does not bind a port")
