#!/usr/bin/env python3
"""Measured resource sampler -> resources.csv (schema 1.0.0).

Samples ACTUAL resource usage of the experiment containers at a fixed interval
while a run executes, and writes one `resources.csv` row per (run, component,
sample). Values are MEASURED from the container runtime / cgroup, never inferred
from the configured compose limits.

Sources (documented in the manifest's measurement_origin):
  * cpu_seconds_total     : cgroup `cpu.stat` usage_usec / 1e6 (monotonic counter),
                            read via `docker exec <c> cat /sys/fs/cgroup/cpu.stat`.
                            Fallback: not available -> empty (missing != zero).
  * memory_rss_bytes      : cgroup `memory.current` (bytes), via docker exec.
  * memory_working_set_bytes : memory.current - inactive_file (k8s-style working set)
                            when memory.stat is readable; else empty.
  * network_rx/tx_bytes   : `docker stats --no-stream` NetIO (per-container).
  * gpu_*                 : left empty here (no GPU attributed per-container).

Component label is derived from the container name suffix (backend/melis/litellm
/redis). The sampler runs in a background thread; call stop() to finish.

Usage (standalone, sample for ~N seconds):
  python resource_sampler.py --run-id run-x --out-dir ../data/pilot \
      --containers gateway-comparison-backend-1 gateway-comparison-melis-1 \
      --interval 0.5 --duration 10

Stdlib only (shells out to `docker`).
"""
from __future__ import annotations

import argparse
import os
import subprocess
import sys
import threading
import time
from datetime import datetime, timezone

HARNESS = os.path.dirname(os.path.abspath(__file__))
INSTR = os.path.join(os.path.dirname(HARNESS), "instrumentation")
sys.path.insert(0, INSTR)

from csv_writer import CsvTableWriter  # noqa: E402


def _now_utc():
    dt = datetime.now(timezone.utc)
    return dt.strftime("%Y-%m-%dT%H:%M:%S.") + f"{dt.microsecond // 1000:03d}Z"


def _component_of(container: str) -> str:
    for key in ("backend", "melis", "litellm", "redis"):
        if key in container:
            return key
    return "other"


def _docker_exec_cat(container: str, path: str):
    try:
        out = subprocess.run(["docker", "exec", container, "cat", path],
                             capture_output=True, text=True, timeout=5)
        return out.stdout if out.returncode == 0 else None
    except (OSError, subprocess.SubprocessError):
        return None


def _cpu_seconds_total(container: str):
    txt = _docker_exec_cat(container, "/sys/fs/cgroup/cpu.stat")
    if not txt:
        return None
    for line in txt.splitlines():
        if line.startswith("usage_usec"):
            try:
                return int(line.split()[1]) / 1_000_000.0
            except (ValueError, IndexError):
                return None
    return None


def _mem_current(container: str):
    txt = _docker_exec_cat(container, "/sys/fs/cgroup/memory.current")
    if not txt:
        return None
    try:
        return int(txt.strip())
    except ValueError:
        return None


def _mem_working_set(container: str, rss):
    # working_set = memory.current - inactive_file (common k8s definition).
    txt = _docker_exec_cat(container, "/sys/fs/cgroup/memory.stat")
    if not txt or rss is None:
        return None
    inactive_file = None
    for line in txt.splitlines():
        if line.startswith("inactive_file "):
            try:
                inactive_file = int(line.split()[1])
            except (ValueError, IndexError):
                inactive_file = None
    if inactive_file is None:
        return None
    return max(0, rss - inactive_file)


def _net_io_bytes(stats_line: str):
    # docker stats NetIO is like "60.9kB / 75.7kB"; parse to bytes (rx, tx).
    def to_bytes(tok):
        tok = tok.strip()
        units = {"B": 1, "kB": 1000, "MB": 1000**2, "GB": 1000**3,
                 "KiB": 1024, "MiB": 1024**2, "GiB": 1024**3}
        for u in sorted(units, key=len, reverse=True):
            if tok.endswith(u):
                try:
                    return int(float(tok[:-len(u)]) * units[u])
                except ValueError:
                    return None
        return None
    try:
        rx, tx = stats_line.split("/")
        return to_bytes(rx), to_bytes(tx)
    except ValueError:
        return None, None


def _docker_stats(containers):
    """One-shot docker stats -> {name: {"net": netio_str, "mem": memusage_str}}.

    `docker stats` is a MEASURED source that works even for distroless images
    (no shell), unlike `docker exec cat /sys/fs/cgroup/...`. MemUsage is the
    used side of "used / limit"; NetIO is "rx / tx".
    """
    try:
        out = subprocess.run(
            ["docker", "stats", "--no-stream", "--format",
             "{{.Name}}\t{{.NetIO}}\t{{.MemUsage}}", *containers],
            capture_output=True, text=True, timeout=10)
        res = {}
        if out.returncode == 0:
            for line in out.stdout.splitlines():
                parts = line.split("\t")
                if len(parts) == 3:
                    res[parts[0]] = {"net": parts[1], "mem": parts[2]}
        return res
    except (OSError, subprocess.SubprocessError):
        return {}


def _bytes_from(tok: str):
    tok = tok.strip()
    units = {"B": 1, "kB": 1000, "MB": 1000**2, "GB": 1000**3,
             "KiB": 1024, "MiB": 1024**2, "GiB": 1024**3}
    for u in sorted(units, key=len, reverse=True):
        if tok.endswith(u):
            try:
                return int(float(tok[:-len(u)]) * units[u])
            except ValueError:
                return None
    return None


def _mem_used_from_stats(mem_str: str):
    # "10.58MiB / 256MiB" -> used bytes.
    try:
        used = mem_str.split("/")[0]
        return _bytes_from(used)
    except (ValueError, AttributeError):
        return None


class ResourceSampler:
    def __init__(self, run_id, out_dir, containers, interval=0.5):
        self.run_id = run_id
        self.containers = containers
        self.interval = interval
        self.writer = CsvTableWriter("resources", os.path.join(out_dir, "resources.csv"))
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._loop, daemon=True)
        self._t0 = None
        self.samples = 0

    def start(self):
        self._t0 = time.perf_counter()
        self._thread.start()
        return self

    def _loop(self):
        while not self._stop.is_set():
            ts = _now_utc()
            elapsed = time.perf_counter() - self._t0
            stats = _docker_stats(self.containers)
            for c in self.containers:
                st = stats.get(c, {})
                # Memory: prefer cgroup memory.current (exec); fall back to the
                # MEASURED docker-stats MemUsage for distroless images (no shell).
                rss = _mem_current(c)
                if rss is None:
                    rss = _mem_used_from_stats(st.get("mem", ""))
                rx, tx = _net_io_bytes(st.get("net", "")) if st.get("net") else (None, None)
                self.writer.write_row({
                    "run_id": self.run_id,
                    "timestamp_utc": ts,
                    "elapsed_s": round(elapsed, 3),
                    "component": _component_of(c),
                    # cpu_seconds_total is a COUNTER: only from cgroup cpu.stat.
                    # docker-stats CPU% is an instantaneous gauge, not this counter,
                    # so when cgroup is unreadable (distroless) we leave it EMPTY
                    # rather than fabricate a counter from a percentage.
                    "cpu_seconds_total": _cpu_seconds_total(c),
                    "memory_rss_bytes": rss,
                    "memory_working_set_bytes": _mem_working_set(c, rss),
                    "network_rx_bytes": rx,
                    "network_tx_bytes": tx,
                    # GPU not attributed per-container here -> empty (missing != zero).
                })
                self.samples += 1
            self._stop.wait(self.interval)

    def stop(self):
        self._stop.set()
        self._thread.join(timeout=10)
        return self.samples


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--run-id", required=True)
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--containers", nargs="+", required=True)
    ap.add_argument("--interval", type=float, default=0.5)
    ap.add_argument("--duration", type=float, default=10)
    args = ap.parse_args(argv)
    s = ResourceSampler(args.run_id, args.out_dir, args.containers, args.interval).start()
    time.sleep(args.duration)
    n = s.stop()
    print(f"wrote {n} resources.csv rows for run {args.run_id}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
