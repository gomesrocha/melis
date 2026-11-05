#!/usr/bin/env python3
"""Comparative smoke/pilot runner for direct, Melis and LiteLLM.

Each block runs all three paths under the same mode and concurrency, in a
seeded rotation (`block_design.balanced_orders`). One gateway container is
running at a time. This script only stops or starts compose services named
melis, litellm, or backend in this project. It does not remove volumes and
does not touch containers outside the project.

Every invocation requires a new `--out-dir`. Raw CSVs stay in that directory.
Derived comparisons go to a new filename that includes the analysis id.

Stdlib only.
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import threading
import time
import urllib.request

HARNESS = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HARNESS)
INSTR = os.path.join(ROOT, "instrumentation")
REPO = os.path.dirname(os.path.dirname(ROOT))
sys.path.insert(0, HARNESS)
sys.path.insert(0, INSTR)

import block_design  # noqa: E402
import conc_load  # noqa: E402
import host_pressure  # noqa: E402
import ids  # noqa: E402
import melis_preflight  # noqa: E402
import payload_equiv  # noqa: E402
from csv_writer import CsvTableWriter  # noqa: E402
from manifest import build_manifest, git_state, sha256_file, write_manifest  # noqa: E402
from schemas import SCHEMA_VERSION  # noqa: E402

DET = os.path.join(ROOT, "docker-compose.deterministic.yml")
WINDOW = os.path.join(ROOT, "docker-compose.pilot-window.yml")
ALLOWED_SERVICES = {"backend", "melis", "litellm"}
TARGETS = {
    "direct": "http://127.0.0.1:7070",
    "melis": "http://127.0.0.1:7071",
    "litellm": "http://127.0.0.1:4000",
}
HEALTH = {
    "direct": ["http://127.0.0.1:7070/health"],
    "melis": ["http://127.0.0.1:7071/healthz"],
    "litellm": [
        "http://127.0.0.1:4000/health/liveliness",
        "http://127.0.0.1:4000/health",
    ],
}


def estimate(args):
    gateways = 3
    cells = args.blocks * len(args.modes) * len(args.concurrency) * gateways
    seconds = cells * ((args.warmup_s or 0) + (args.measure_s or 0))
    duration_mode = bool(args.measure_s)
    return {
        "cells": cells,
        "load_seconds": round(seconds, 1),
        "load_minutes": round(seconds / 60.0, 1),
        "duration_mode": duration_mode,
        "safety_cap": args.safety_cap,
        "backend_latency_ms": args.backend_latency_ms,
        "note": (
            "Load time is cells times the configured windows. It is not the "
            "effective duration, and it excludes gateway start and stop. "
            "A final grid of 30 blocks x 3 paths x 80 s is 120 minutes of load "
            "before switches. Duration mode does not stop at max_requests."
        ),
    }


def _compose(args, extra_files=(), profile=None):
    cmd = ["docker", "compose", "-f", DET]
    for path in extra_files:
        cmd.extend(["-f", path])
    if profile:
        cmd.extend(["--profile", profile])
    cmd.extend(args)
    proc = subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True)
    if proc.returncode != 0:
        raise RuntimeError(f"compose failed: {' '.join(cmd)}\n{proc.stderr[-2000:]}")
    return proc.stdout


_CONTAINER = {
    "backend": "gateway-comparison-backend-1",
    "melis": "gateway-comparison-melis-1",
    "litellm": "gateway-comparison-litellm-1",
}


def _stop(service):
    if service not in _CONTAINER:
        raise RuntimeError(f"refusing to stop service {service}")
    # Exact experiment container only. A missing container is not an error.
    subprocess.run(["docker", "stop", _CONTAINER[service]], cwd=ROOT, capture_output=True)


def _up(service, extra_files=()):
    if service not in ALLOWED_SERVICES:
        raise RuntimeError(f"refusing to start service {service}")
    profile = service if service in ("melis", "litellm") else None
    build_flag = []
    if service == "melis" and not getattr(_up, "build_melis", False):
        build_flag = ["--no-build"]
    _compose(["up", "-d", *build_flag, service], extra_files=extra_files, profile=profile)


def _wait_health(gateway, timeout=90):
    urls = HEALTH[gateway]
    deadline = time.time() + timeout
    last = ""
    while time.time() < deadline:
        for url in urls:
            try:
                with urllib.request.urlopen(url, timeout=3) as resp:
                    if 200 <= resp.status < 300:
                        return
            except Exception as exc:
                last = str(exc)
        time.sleep(0.5)
    raise RuntimeError(f"{gateway} did not become healthy at {urls}: {last}")


def _activate(gateway, extra_files):
    """Leave only the requested gateway running. The backend stays up."""
    if gateway == "direct":
        _stop("melis")
        _stop("litellm")
    elif gateway == "melis":
        _stop("litellm")
        _up("melis", extra_files)
    elif gateway == "litellm":
        _stop("melis")
        _up("litellm", extra_files)
    else:
        raise RuntimeError(gateway)
    _wait_health(gateway if gateway != "direct" else "direct")
    if gateway == "melis":
        melis_preflight.assert_mounted_config(_container_logs("gateway-comparison-melis-1"))
    if gateway != "direct":
        _wait_health("direct")


def _containers(gateway):
    names = ["gateway-comparison-backend-1"]
    if gateway == "melis":
        names.append("gateway-comparison-melis-1")
    elif gateway == "litellm":
        names.append("gateway-comparison-litellm-1")
    return names


def _post_json(url, payload, timeout=30):
    data = json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(url, data=data, method="POST")
    req.add_header("Content-Type", "application/json")
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        body = resp.read()
        return resp.status, body


def _record_pressure(out_dir, stop):
    path = os.path.join(out_dir, "host_pressure.csv")
    import csv
    from datetime import datetime, timezone
    with open(path, "w", newline="", encoding="utf-8") as fh:
        writer = csv.writer(fh)
        writer.writerow([
            "timestamp_utc", "mem_available_bytes", "swap_free_bytes",
            "swap_total_bytes", "pswpin", "pswpout", "load1",
        ])
        while not stop.is_set():
            try:
                sample = host_pressure.read_pressure()
            except OSError:
                sample = {}
            writer.writerow([
                datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3] + "Z",
                sample.get("mem_available_bytes"),
                sample.get("swap_free_bytes"),
                sample.get("swap_total_bytes"),
                sample.get("pswpin"),
                sample.get("pswpout"),
                sample.get("load1"),
            ])
            fh.flush()
            stop.wait(30)


def _container_logs(name):
    started = subprocess.run(
        ["docker", "inspect", "-f", "{{.State.StartedAt}}", name],
        capture_output=True, text=True,
    )
    since = started.stdout.strip() if started.returncode == 0 else ""
    cmd = ["docker", "logs"]
    if since:
        cmd.extend(["--since", since])
    cmd.append(name)
    proc = subprocess.run(cmd, capture_output=True, text=True)
    return (proc.stdout or "") + (proc.stderr or "")


def check_forwarded_payloads(extra, model="mock-model"):
    """Send one identical JSON body through each path and compare backend hashes.

    The deterministic backend stores a canonical SHA-256 and length, not the
    message text. A mismatch is recorded; it is not treated as success.
    """
    base = {
        "model": model,
        "messages": [{"role": "user", "content": "equivalence probe"}],
        "temperature": 0,
        "max_tokens": 16,
    }
    _post_json("http://127.0.0.1:7070/__reset", {})
    client = {}
    for gateway in ("direct", "melis", "litellm"):
        _activate(gateway, extra)
        if gateway == "melis":
            melis_preflight.assert_mounted_config(
                _container_logs("gateway-comparison-melis-1")
            )
        client[gateway] = {}
        for mode, stream in (("json", False), ("sse", True)):
            payload = dict(base, stream=stream)
            status, body = _post_json(TARGETS[gateway] + "/v1/chat/completions", payload)
            if stream:
                text = body.decode("utf-8", "replace")
                parts = []
                for line in text.splitlines():
                    if line.startswith("data:") and "[DONE]" not in line:
                        try:
                            delta = json.loads(line[5:].strip())["choices"][0]["delta"].get("content") or ""
                        except Exception:
                            delta = ""
                        parts.append(delta)
                content = "".join(parts)
                framing_ok = "data:" in text and "[DONE]" in text
            else:
                try:
                    content = json.loads(body)["choices"][0]["message"]["content"]
                    framing_ok = True
                except Exception:
                    content = None
                    framing_ok = False
            client[gateway][mode] = {
                "http_status": status,
                "content": content,
                "body_len": len(body),
                "framing_ok": framing_ok,
            }
    seen = json.loads(urllib.request.urlopen("http://127.0.0.1:7070/__seen", timeout=5).read())
    forwarded = seen.get("seen", [])
    melis_preflight.assert_seen_model(forwarded, model)

    def _same(mode):
        rows = [row for row in forwarded if bool(row.get("stream")) is (mode == "sse")]
        hashes = [row.get("body_sha256") for row in rows]
        return {
            "n": len(rows),
            "hashes_equal": len(hashes) == 3 and len(set(hashes)) == 1,
            "body_len": rows[0].get("body_len") if rows else None,
            "keys": rows[0].get("keys") if rows else None,
            "hashes": hashes,
        }

    replies_equal = all(
        len({client[gw][mode]["content"] for gw in client}) == 1
        for mode in ("json", "sse")
    )

    def _mode_entries(mode):
        return [row for row in forwarded if bool(row.get("stream")) is (mode == "sse")]

    compared = {
        mode: payload_equiv.compare_seen(_mode_entries(mode))
        for mode in ("json", "sse")
    }
    return {
        "client": client,
        "forwarded": forwarded,
        "json": _same("json"),
        "sse": _same("sse"),
        "effective": compared,
        "replies_equal": replies_equal,
        "note": "SSE on this mock uses Connection: close after [DONE]; JSON keeps the connection. That mode difference is in the backend, shared by all three paths. A missing stream key is equivalent to false only.",
    }


def _image_record(container):
    """Local image id, registry digest when the engine has one, and architecture.

    A locally built image often has no RepoDigest. That absence is recorded.
    The container's image id is not treated as a digest of HEAD by itself.
    """
    proc = subprocess.run(["docker", "inspect", container], capture_output=True, text=True)
    if proc.returncode != 0:
        return None
    info = json.loads(proc.stdout)[0]
    image = info.get("Image")
    img_proc = subprocess.run(["docker", "image", "inspect", image], capture_output=True, text=True)
    meta = json.loads(img_proc.stdout)[0] if img_proc.returncode == 0 else {}
    return {
        "container": container,
        "image_id": meta.get("Id") or image,
        "repo_digests": meta.get("RepoDigests") or [],
        "architecture": meta.get("Architecture"),
        "os": meta.get("Os"),
        "created": meta.get("Created"),
    }


def run(args):
    est = estimate(args)
    print(json.dumps({"estimate": est}, indent=2))
    if args.dry_estimate:
        return 0
    if est["load_seconds"] > args.max_load_seconds:
        raise SystemExit(
            f"refusing grid: configured load {est['load_seconds']}s "
            f"> --max-load-seconds {args.max_load_seconds}"
        )
    if args.require_clean:
        state = git_state(REPO)
        if not state["clean"]:
            raise SystemExit(
                "refusing to measure a dirty tree: "
                + ", ".join(state["dirty_files"] or ["unknown"])
            )
    if args.order == "permutations":
        if args.blocks != 6 * args.permutation_repeats:
            raise SystemExit(
                "permutation order needs blocks == 6 * repeats "
                f"({args.blocks} != {6 * args.permutation_repeats})"
            )
    if os.path.exists(args.out_dir):
        raise SystemExit(f"refusing existing output directory {args.out_dir}")
    os.makedirs(args.out_dir)
    if args.check_host:
        delta = host_pressure.sample_delta(5)
        with open(os.path.join(args.out_dir, "host_pressure_before.json"), "w", encoding="utf-8") as fh:
            json.dump(delta, fh, indent=2)
            fh.write("\n")
        if delta["reason"]:
            raise SystemExit("host inadequate for this pilot: " + delta["reason"])
    extra = [WINDOW] if args.backend_latency_ms == 100 else []
    if args.backend_latency_ms not in (0, 100):
        raise SystemExit("backend latency must be 0 (compose default) or 100 (pilot-window override)")

    experiment_id = ids.experiment_id(args.experiment_name, "1.0", SCHEMA_VERSION)
    nonce = args.collection_nonce or ids.new_collection_nonce()
    collection = ids.collection_id(experiment_id, nonce)
    req_writer = CsvTableWriter(
        "requests", os.path.join(args.out_dir, "requests.csv"), buffer_rows=32,
    )
    fact_writer = CsvTableWriter(
        "execution_facts", os.path.join(args.out_dir, "execution_facts.csv"),
    )
    registry = conc_load.IdRegistry()
    gates = ["direct", "melis", "litellm"]
    if args.order == "permutations":
        orders = block_design.permutation_orders(gates, args.permutation_repeats, args.seed)
    else:
        orders = block_design.balanced_orders(gates, args.blocks, args.seed)
    from resource_sampler import ResourceSampler
    sampler = None
    cells = []
    payload_report = None
    pressure_stop = threading.Event()
    pressure_thread = threading.Thread(
        target=_record_pressure, args=(args.out_dir, pressure_stop), daemon=True,
    )
    try:
        if args.check_host:
            pressure_thread.start()
        _up("backend", extra)
        _compose(["restart", "backend"])
        _wait_health("direct")
        # Open stats streams only after the backend container that will serve
        # this run exists. A stream opened before `up` can sit on the replaced
        # container and record nothing.
        _up.build_melis = bool(args.build_melis)
        if not args.no_sampler:
            sampler = ResourceSampler(
                args.out_dir, _containers("direct"), interval=args.sample_interval,
                collection_id=collection,
            ).start()
        payload_report = check_forwarded_payloads(extra, args.model) if args.check_payloads else None
        if payload_report is not None:
            with open(os.path.join(args.out_dir, "payload_check.json"), "w", encoding="utf-8") as fh:
                json.dump(payload_report, fh, indent=2)
                fh.write("\n")
            payload_report["byte_identical"] = (
                payload_report["json"]["hashes_equal"]
                and payload_report["sse"]["hashes_equal"]
            )
            payload_report["effective_ok"] = all(
                payload_report["effective"][mode]["ok"] for mode in ("json", "sse")
            )
            # Re-write after the summary flags. A byte mismatch is evidence, not a
            # reason to drop the smoke: the file records it and the manifest too.
            with open(os.path.join(args.out_dir, "payload_check.json"), "w", encoding="utf-8") as fh:
                json.dump(payload_report, fh, indent=2)
                fh.write("\n")
        if payload_report is not None and not payload_report.get("effective_ok"):
            raise RuntimeError(
                "preflight payload mismatch outside the stream-default allowlist: "
                + json.dumps(payload_report["effective"])
            )
        if payload_report is not None and not payload_report.get("replies_equal"):
            raise RuntimeError("preflight reconstructed replies differ across paths")
        for b, order in enumerate(orders):
            block = ids.block_id(experiment_id, args.phase, b, collection)
            case = ids.case_id(experiment_id, "rq1-conc", 0)
            for mode in args.modes:
                for conc in args.concurrency:
                    for position, gateway in enumerate(order):
                        _activate(gateway, extra)
                        if sampler is not None:
                            sampler.set_containers(_containers(gateway))
                        cells.append(conc_load.execute_cell({
                            "experiment_id": experiment_id,
                            "collection_id": collection,
                            "phase": args.phase,
                            "gateway": gateway,
                            "url": TARGETS[gateway],
                            "model": args.model,
                            "backend_profile": "deterministic",
                            "mode": mode,
                            "concurrency": conc,
                            "block_id": block,
                            "case_id": case,
                            "timeout": args.timeout,
                            "max_tokens": args.max_tokens,
                            "max_requests": args.max_requests,
                            "safety_cap": args.safety_cap,
                            "measure_s": args.measure_s,
                            "warmup_s": args.warmup_s,
                            "inflight_url": "http://127.0.0.1:7070/__inflight",
                            "count_url": "http://127.0.0.1:7070/__count",
                            "sampler": sampler,
                            "registry": registry,
                            "req_writer": req_writer,
                            "fact_writer": fact_writer,
                            "feature_profile": "minimal",
                            "policy": "none",
                            "seed": str(args.seed),
                            "order_in_block": position,
                        }))
    finally:
        pressure_stop.set()
        if pressure_thread.is_alive():
            pressure_thread.join(timeout=2)
        req_writer.close()
        fact_writer.close()
        if sampler is not None:
            sampler.stop()
        if extra:
            # Restore the zero-latency backend definition. No volume removal.
            try:
                _up("backend", extra_files=())
            except Exception as exc:
                print(f"WARN: could not restore backend latency: {exc}", file=sys.stderr)

    import aggregate
    analysis = aggregate.analysis_id()
    comparisons = os.path.join(args.out_dir, f"comparisons.{analysis}.csv")
    runs = os.path.join(args.out_dir, f"runs.{analysis}.csv")
    pairing = os.path.join(args.out_dir, f"pairing.{analysis}.json")
    built = aggregate.build(
        args.out_dir, runs, comparisons, experiment_id, args.phase, args.seed,
    )
    with open(pairing, "w", encoding="utf-8") as fh:
        json.dump({"analysis_id": analysis, "pairing": built["pairing"]}, fh, indent=2)
        fh.write("\n")
    manifest = build_manifest(
        experiment_id, args.phase, REPO,
        image_digests={
            "backend": _image_record("gateway-comparison-backend-1"),
            "melis": _image_record("gateway-comparison-melis-1"),
            "litellm": _image_record("gateway-comparison-litellm-1"),
        },
        models={"all_paths": args.model},
        config_files={
            "melis_config": os.path.join(ROOT, "melis", "config.deterministic.yaml"),
            "melis_routes": os.path.join(ROOT, "melis", "routes.deterministic.yaml"),
            "litellm_config": os.path.join(ROOT, "litellm", "config.deterministic.yaml"),
        },
        seeds={"block_order": args.seed},
        measurement_origin={
            "latency_ms": "client monotonic clock, full completion",
            "ttfb_ms": "first HTTP body byte on the client; SSE only",
            "first_content_ms": "first SSE event containing delta content; not a token time",
            "cpu_seconds_total": "docker engine cpu_stats.cpu_usage.total_usage nanoseconds / 1e9",
            "memory_cgroup_current_bytes": "docker engine memory_stats.usage (cgroup memory.current)",
            "memory_working_set_bytes": "usage - inactive_file",
            "memory_rss_bytes": "memory_stats.stats.rss only; empty on cgroup v2",
            "backend_call_count": "execution-level /__count delta; not per request",
            "backend_peak_inflight": "backend /__inflight peak after per-cell /__reset",
        },
        field_limitations={
            "phase": "smoke and pilot are exploratory, not confirmatory",
            "memory_rss_bytes": "cgroup v2 Docker stats have no rss field; anon is not substituted",
            "tokens": "SSE chunks are not tokenizer tokens",
            "melis_image": (
                "built from this checkout before the run"
                if args.build_melis
                else "started with --no-build; the image id is not evidence of HEAD"
            ),
            "duration": "duration_s is effective wall time including drain, not the configured window",
            "backend_latency_ms": "0 is the zero-latency mock; 100 is a different scenario and is not pooled",
        },
        extra={
            "orders": [list(order) for order in orders],
            "estimate": est,
            "cells": cells,
            "backend_latency_ms": args.backend_latency_ms,
            "analysis_id": analysis,
            "collection_id": collection,
            "collection_nonce": nonce,
            "feature_profile": "minimal",
            "order_design": args.order,
            "compose_files": [DET, *extra],
            "harness_hashes": {
                "conc_load": sha256_file(os.path.join(HARNESS, "conc_load.py")),
                "run_comparative_pilot": sha256_file(os.path.join(HARNESS, "run_comparative_pilot.py")),
                "mock_openai": sha256_file(os.path.join(ROOT, "backend-deterministic", "mock_openai.py")),
            },
            "payload_check": None if payload_report is None else {
                "replies_equal": payload_report.get("replies_equal"),
                "json_hashes_equal": payload_report["json"]["hashes_equal"],
                "sse_hashes_equal": payload_report["sse"]["hashes_equal"],
                "json_keys": payload_report["json"].get("keys"),
            },
        },
    )
    write_manifest(manifest, os.path.join(args.out_dir, "manifest.json"))
    import validate
    report = validate.validate_dir(args.out_dir)
    with open(os.path.join(args.out_dir, "validation_report.json"), "w", encoding="utf-8") as fh:
        json.dump(report, fh, indent=2)
        fh.write("\n")
    print(json.dumps({
        "out_dir": args.out_dir,
        "ok": report["ok"],
        "n_errors": len(report["errors"]),
        "analysis_id": analysis,
        "cells": len(cells),
        "incomplete_blocks": built["incomplete_blocks"],
    }, indent=2))
    for err in report["errors"]:
        print("ERROR:", err)
    return 0 if report["ok"] else 1


def main(argv=None):
    ap = argparse.ArgumentParser(description="Balanced three-path smoke/pilot")
    ap.add_argument("--out-dir", default=None)
    ap.add_argument("--phase", default="pilot", choices=["smoke", "pilot"])
    ap.add_argument("--blocks", type=int, default=1)
    ap.add_argument("--modes", nargs="+", default=["json", "sse"])
    ap.add_argument("--concurrency", type=int, nargs="+", default=[1, 4])
    ap.add_argument("--warmup-s", type=float, default=3)
    ap.add_argument("--measure-s", type=float, default=12)
    ap.add_argument("--max-requests", type=int, default=2000,
                    help="Planned cap only when --measure-s is omitted. Duration mode uses --safety-cap.")
    ap.add_argument("--safety-cap", type=int, default=conc_load.DEFAULT_SAFETY_CAP)
    ap.add_argument("--max-load-seconds", type=float, default=3600)
    ap.add_argument("--order", choices=["rotation", "permutations"], default="rotation")
    ap.add_argument("--permutation-repeats", type=int, default=2)
    ap.add_argument("--require-clean", action="store_true")
    ap.add_argument("--build-melis", action="store_true")
    ap.add_argument("--collection-nonce", default=None)
    ap.add_argument("--check-host", action="store_true")
    ap.add_argument("--backend-latency-ms", type=int, default=0)
    ap.add_argument("--sample-interval", type=float, default=1.0,
                    help="Requested sample interval. The Docker stats stream emits about 1s; "
                         "a smaller request is recorded as lag, not as achieved.")
    ap.add_argument("--no-sampler", action="store_true")
    ap.add_argument("--timeout", type=float, default=30)
    ap.add_argument("--max-tokens", type=int, default=16)
    ap.add_argument("--model", default="mock-model")
    ap.add_argument("--seed", type=int, default=20261105)
    ap.add_argument("--experiment-name", default="rq1-comparative")
    ap.add_argument("--dry-estimate", action="store_true")
    ap.add_argument("--skip-payload-check", action="store_true")
    args = ap.parse_args(argv)
    args.check_payloads = not args.skip_payload_check
    if not args.dry_estimate and not args.out_dir:
        raise SystemExit("--out-dir is required unless --dry-estimate")
    return run(args)


if __name__ == "__main__":
    raise SystemExit(main())
