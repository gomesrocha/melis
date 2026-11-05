#!/usr/bin/env python3
"""Measured resource sampler -> resources.csv (schema 1.1.0).

Samples container resource usage from the Docker Engine stats API over the
local Unix socket the Docker CLI already uses. It does not exec a shell or
`cat` inside the container, so a distroless image does not erase the CPU
counter. It does not publish the Docker socket or change host permissions.

Memory columns are different definitions and are never substituted for each
other:

  * memory_cgroup_current_bytes — Docker `memory_stats.usage`
    (cgroup `memory.current`, includes cache). Not RSS.
  * memory_working_set_bytes — usage minus `inactive_file` (Kubernetes
    working set), only when both fields exist.
  * memory_rss_bytes — only `memory_stats.stats.rss`. On cgroup v2 this
    field is absent; `anon` is a different quantity and stays unrecorded
    rather than being written as RSS.

CPU is `cpu_stats.cpu_usage.total_usage` nanoseconds converted to seconds.
A CPU percentage is not turned into this counter.

Each container sample keeps the engine's own read timestamp, the duration of
that read, and the effective interval since the previous sample of the same
run and component. Measures taken apart do not share one artificial timestamp.

The sampler writes only while a run id is active (`set_run` / `clear_run`),
so samples belong to one execution cell.

Stdlib only.
"""
from __future__ import annotations

import argparse
import json
import os
import queue
import socket
import subprocess
import sys
import threading
import time
from datetime import datetime, timezone
from urllib.parse import quote

HARNESS = os.path.dirname(os.path.abspath(__file__))
INSTR = os.path.join(os.path.dirname(HARNESS), "instrumentation")
sys.path.insert(0, INSTR)

from csv_writer import CsvTableWriter  # noqa: E402

CPU_METHOD = "cpu_stats.cpu_usage.total_usage_ns/1e9"
CGROUP_METHOD = "memory_stats.usage (cgroup memory.current, not RSS)"
WORKING_SET_METHOD = (
    "memory_stats.usage - memory_stats.stats.inactive_file (kubernetes working set)"
)
RSS_METHOD = "memory_stats.stats.rss (cgroup rss field)"
NETWORK_METHOD = "sum(networks.*.rx_bytes/tx_bytes)"
HOST_CPU_METHOD = "proc_stat_nonidle_jiffies/CLK_TCK"
DOCKER_SOURCE = "docker_engine_stats"
HOST_SOURCE = "procfs"


def _now_utc():
    dt = datetime.now(timezone.utc)
    return dt.strftime("%Y-%m-%dT%H:%M:%S.") + f"{dt.microsecond // 1000:03d}Z"


def normalize_engine_timestamp(raw, fallback):
    """Normalize a Docker `read` timestamp to millisecond UTC. Never invent one."""
    if not isinstance(raw, str) or not raw:
        return fallback
    try:
        s = raw.strip().replace("Z", "+00:00")
        if "." in s:
            head, frac = s.split(".", 1)
            digits = []
            i = 0
            while i < len(frac) and frac[i].isdigit():
                digits.append(frac[i])
                i += 1
            frac_digits = ("".join(digits) + "000000")[:6]
            s = f"{head}.{frac_digits}{frac[i:]}"
        dt = datetime.fromisoformat(s)
        if dt.tzinfo is None:
            dt = dt.replace(tzinfo=timezone.utc)
        dt = dt.astimezone(timezone.utc)
        return dt.strftime("%Y-%m-%dT%H:%M:%S.") + f"{dt.microsecond // 1000:03d}Z"
    except (ValueError, OSError):
        return fallback


def interval_fields(prev_mono, now_mono, configured):
    """Effective interval and lag. The first sample has neither (missing != 0)."""
    if prev_mono is None:
        return None, None
    effective = now_mono - prev_mono
    return round(effective, 6), round(effective - configured, 6)


def interpret_docker_stats(payload):
    """Map one Docker stats JSON object onto the separated resource columns.

    `anon`, cgroup usage, and a MemUsage string are never written as RSS.
    """
    out = {
        "cpu_seconds_total": None,
        "cpu_method": None,
        "memory_cgroup_current_bytes": None,
        "memory_cgroup_method": None,
        "memory_working_set_bytes": None,
        "memory_working_set_method": None,
        "memory_rss_bytes": None,
        "memory_rss_method": None,
        "network_rx_bytes": None,
        "network_tx_bytes": None,
        "network_method": None,
        "engine_read": None,
        "source": DOCKER_SOURCE,
    }
    if not isinstance(payload, dict):
        return out
    out["engine_read"] = payload.get("read")
    cpu_usage = (payload.get("cpu_stats") or {}).get("cpu_usage") or {}
    total = cpu_usage.get("total_usage")
    if isinstance(total, int) and not isinstance(total, bool):
        out["cpu_seconds_total"] = total / 1_000_000_000.0
        out["cpu_method"] = CPU_METHOD
    mem = payload.get("memory_stats") or {}
    usage = mem.get("usage")
    if isinstance(usage, int) and not isinstance(usage, bool):
        out["memory_cgroup_current_bytes"] = usage
        out["memory_cgroup_method"] = CGROUP_METHOD
    stats = mem.get("stats") or {}
    inactive = stats.get("inactive_file") if isinstance(stats, dict) else None
    if (
        isinstance(usage, int) and not isinstance(usage, bool)
        and isinstance(inactive, int) and not isinstance(inactive, bool)
    ):
        out["memory_working_set_bytes"] = max(0, usage - inactive)
        out["memory_working_set_method"] = WORKING_SET_METHOD
    if isinstance(stats, dict) and "rss" in stats and isinstance(stats.get("rss"), int) \
            and not isinstance(stats.get("rss"), bool):
        out["memory_rss_bytes"] = stats["rss"]
        out["memory_rss_method"] = RSS_METHOD
    nets = payload.get("networks") or {}
    if isinstance(nets, dict) and nets:
        rx = tx = 0
        complete = True
        for iface in nets.values():
            if not isinstance(iface, dict) or "rx_bytes" not in iface or "tx_bytes" not in iface:
                complete = False
                break
            rx += int(iface["rx_bytes"])
            tx += int(iface["tx_bytes"])
        if complete:
            out["network_rx_bytes"] = rx
            out["network_tx_bytes"] = tx
            out["network_method"] = NETWORK_METHOD
    return out


def interpret_host_cpu(proc_stat_text):
    """Host CPU counter from /proc/stat. Memory is not filled from this source."""
    for line in (proc_stat_text or "").splitlines():
        if line.startswith("cpu "):
            parts = line.split()
            # user nice system idle iowait irq softirq steal guest guest_nice
            nums = []
            for tok in parts[1:]:
                try:
                    nums.append(int(tok))
                except ValueError:
                    return None
            if len(nums) < 4:
                return None
            # guest/guest_nice are already included in user/nice; do not add them again.
            busy_idx = (0, 1, 2, 5, 6, 7)
            non_idle = sum(nums[i] for i in busy_idx if i < len(nums))
            clk = os.sysconf(os.sysconf_names["SC_CLK_TCK"]) if hasattr(os, "sysconf") else 100
            return non_idle / float(clk)
    return None


def _component_of(container: str) -> str:
    name = container.lower()
    for key in ("backend", "melis", "litellm", "redis"):
        if key in name:
            return key
    return "other"


def resolve_docker_socket():
    """Unix socket the local Docker CLI uses. Does not change permissions."""
    env = os.environ.get("DOCKER_HOST", "")
    if env.startswith("unix://"):
        return env[len("unix://"):]
    try:
        out = subprocess.run(
            ["docker", "context", "inspect", "--format", "{{.Endpoints.docker.Host}}"],
            capture_output=True, text=True, timeout=5, check=False,
        )
        host = (out.stdout or "").strip()
        if out.returncode == 0 and host.startswith("unix://"):
            return host[len("unix://"):]
    except (OSError, subprocess.SubprocessError):
        pass
    return "/var/run/docker.sock"


def docker_api_get(sock_path, path, timeout=3.0):
    """HTTP/1.0 GET over a Unix socket. Returns (json, error)."""
    try:
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.settimeout(timeout)
        sock.connect(sock_path)
        req = f"GET {path} HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        sock.sendall(req.encode("ascii"))
        chunks = []
        while True:
            try:
                blob = sock.recv(65536)
            except socket.timeout:
                break
            if not blob:
                break
            chunks.append(blob)
        sock.close()
    except OSError as exc:
        return None, f"socket:{exc.__class__.__name__}"
    raw = b"".join(chunks)
    head, sep, body = raw.partition(b"\r\n\r\n")
    if not sep:
        return None, "short_response"
    status = head.split(b"\r\n", 1)[0].decode("latin1", "replace")
    if " 200 " not in status:
        return None, status[:120]
    try:
        return json.loads(body.decode("utf-8")), None
    except (UnicodeError, json.JSONDecodeError):
        return None, "bad_json"


def fetch_container_stats(sock_path, container, timeout=3.0):
    """One stats snapshot for one container, with its own timestamps."""
    started = _now_utc()
    t0 = time.perf_counter()
    path = f"/v1.44/containers/{quote(container, safe='')}/stats?stream=false"
    payload, err = docker_api_get(sock_path, path, timeout=timeout)
    duration_ms = (time.perf_counter() - t0) * 1000.0
    received_mono = time.perf_counter()
    received = _now_utc()
    if err or payload is None:
        return {
            "container": container,
            "component": _component_of(container),
            "ok": False,
            "collection_error": err or "empty",
            "collection_started_at_utc": started,
            "collection_duration_ms": round(duration_ms, 3),
            "received_mono": received_mono,
            "timestamp_utc": received,
            "source": None,
            "cpu_seconds_total": None,
            "cpu_method": None,
            "memory_cgroup_current_bytes": None,
            "memory_cgroup_method": None,
            "memory_working_set_bytes": None,
            "memory_working_set_method": None,
            "memory_rss_bytes": None,
            "memory_rss_method": None,
            "network_rx_bytes": None,
            "network_tx_bytes": None,
            "network_method": None,
        }
    parsed = interpret_docker_stats(payload)
    return {
        "container": container,
        "component": _component_of(container),
        "ok": True,
        "collection_error": None,
        "collection_started_at_utc": started,
        "collection_duration_ms": round(duration_ms, 3),
        "received_mono": received_mono,
        "timestamp_utc": normalize_engine_timestamp(parsed.get("engine_read"), received),
        **{k: parsed[k] for k in parsed if k != "engine_read"},
    }


def read_host_cpu_sample():
    started = _now_utc()
    t0 = time.perf_counter()
    try:
        with open("/proc/stat", encoding="utf-8") as fh:
            text = fh.read(4096)
        seconds = interpret_host_cpu(text)
        err = None if seconds is not None else "unparsed_proc_stat"
    except OSError as exc:
        seconds = None
        err = f"procfs:{exc.__class__.__name__}"
    return {
        "component": "host",
        "ok": seconds is not None,
        "collection_error": err,
        "collection_started_at_utc": started,
        "collection_duration_ms": round((time.perf_counter() - t0) * 1000.0, 3),
        "received_mono": time.perf_counter(),
        "timestamp_utc": _now_utc(),
        "source": HOST_SOURCE if seconds is not None else None,
        "cpu_seconds_total": seconds,
        "cpu_method": HOST_CPU_METHOD if seconds is not None else None,
        "memory_cgroup_current_bytes": None,
        "memory_cgroup_method": None,
        "memory_working_set_bytes": None,
        "memory_working_set_method": None,
        "memory_rss_bytes": None,
        "memory_rss_method": None,
        "network_rx_bytes": None,
        "network_tx_bytes": None,
        "network_method": None,
    }


def iter_stats_stream(sock_path, container, stop_event):
    """Yield parsed stats objects from the Engine stats stream.

    The daemon emits about one object per second. That cadence is a property
    of the API, not something this client can tighten. Each object is returned
    with the monotonic time it was completed and how long its bytes took to
    arrive once the chunk started.
    """
    path = f"/v1.44/containers/{quote(container, safe='')}/stats"
    while not stop_event.is_set():
        try:
            sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            sock.settimeout(1.0)
            sock.connect(sock_path)
            req = f"GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n"
            sock.sendall(req.encode("ascii"))
        except OSError:
            stop_event.wait(0.5)
            continue
        buf = b""
        header_done = False
        last_object = time.perf_counter()
        try:
            while not stop_event.is_set():
                # A recreated container can leave the parser waiting on a dead
                # chunk. Reconnect instead of staying silent.
                if time.perf_counter() - last_object > 3.0:
                    break
                try:
                    blob = sock.recv(65536)
                except socket.timeout:
                    continue
                if not blob:
                    break
                buf += blob
                if not header_done:
                    if b"\r\n\r\n" not in buf:
                        continue
                    head, buf = buf.split(b"\r\n\r\n", 1)
                    status = head.split(b"\r\n", 1)[0]
                    if b" 200 " not in status:
                        break
                    header_done = True
                while True:
                    nl = buf.find(b"\r\n")
                    if nl < 0:
                        break
                    try:
                        size = int(buf[:nl].split(b";", 1)[0].strip(), 16)
                    except ValueError:
                        buf = buf[nl + 2:]
                        continue
                    if size == 0:
                        break
                    start = nl + 2
                    if len(buf) < start + size + 2:
                        break
                    chunk = buf[start:start + size]
                    buf = buf[start + size + 2:]
                    t_done = time.perf_counter()
                    try:
                        payload = json.loads(chunk.decode("utf-8"))
                    except (UnicodeError, json.JSONDecodeError):
                        continue
                    last_object = t_done
                    yield payload, t_done
        finally:
            try:
                sock.close()
            except OSError:
                pass
        stop_event.wait(0.3)


class ResourceSampler:
    def __init__(self, out_dir, containers, interval=0.5, include_host=True, sock_path=None):
        self.containers = list(containers)
        self.interval = float(interval)
        self.include_host = include_host
        self.sock_path = sock_path or resolve_docker_socket()
        self.writer = CsvTableWriter("resources", os.path.join(out_dir, "resources.csv"))
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._loop, daemon=True)
        self._lock = threading.Lock()
        self._run_id = None
        self._cell_t0 = None
        self._sample_index = {}
        self._prev_mono = {}
        self._queue = queue.Queue()
        self._stream_threads = {}
        self.samples = 0
        self.errors = 0

    def set_containers(self, containers):
        with self._lock:
            self.containers = list(containers)
        self._ensure_streams()

    def set_run(self, run_id):
        """Attribute subsequent samples to this execution. Call before the cell.

        Interval baselines are cleared so a sample does not inherit the gap
        since the previous cell.
        """
        with self._lock:
            self._run_id = run_id
            self._cell_t0 = time.perf_counter()
            self._prev_mono.clear()

    def clear_run(self):
        """Stop attributing samples. In-flight reads finish without a run id and are dropped."""
        with self._lock:
            self._run_id = None
            self._cell_t0 = None

    def start(self):
        self._ensure_streams()
        self._thread.start()
        return self

    def _ensure_streams(self):
        for name in list(self.containers):
            if name in self._stream_threads:
                continue
            thread = threading.Thread(target=self._read_container, args=(name,), daemon=True)
            self._stream_threads[name] = thread
            thread.start()

    def _read_container(self, name):
        component = _component_of(name)
        while not self._stop.is_set():
            try:
                for payload, received_mono in iter_stats_stream(self.sock_path, name, self._stop):
                    parsed = interpret_docker_stats(payload)
                    received = _now_utc()
                    self._queue.put({
                        "container": name,
                        "component": component,
                        "ok": True,
                        "collection_error": None,
                        "collection_started_at_utc": received,
                        "collection_duration_ms": None,
                        "received_mono": received_mono,
                        "timestamp_utc": normalize_engine_timestamp(parsed.get("engine_read"), received),
                        **{k: parsed[k] for k in parsed if k != "engine_read"},
                    })
            except Exception as exc:
                self._queue.put({
                    "container": name,
                    "component": component,
                    "collection_error": f"stream:{exc.__class__.__name__}",
                    "collection_started_at_utc": _now_utc(),
                    "collection_duration_ms": None,
                    "received_mono": time.perf_counter(),
                    "timestamp_utc": _now_utc(),
                    "source": None,
                })
                self._stop.wait(0.5)

    def _claim(self, run_id, component, mono):
        with self._lock:
            key = (run_id, component)
            index = self._sample_index.get(key, 0)
            self._sample_index[key] = index + 1
            prev = self._prev_mono.get(key)
            self._prev_mono[key] = mono
            return index, *interval_fields(prev, mono, self.interval)

    def _write_sample(self, run_id, cell_t0, sample):
        index, effective, lag = self._claim(run_id, sample["component"], sample["received_mono"])
        elapsed = sample["received_mono"] - cell_t0
        self.writer.write_row({
            "run_id": run_id,
            "timestamp_utc": sample["timestamp_utc"],
            "elapsed_s": round(elapsed, 6),
            "component": sample["component"],
            "cpu_seconds_total": sample.get("cpu_seconds_total"),
            "memory_rss_bytes": sample.get("memory_rss_bytes"),
            "memory_working_set_bytes": sample.get("memory_working_set_bytes"),
            "network_rx_bytes": sample.get("network_rx_bytes"),
            "network_tx_bytes": sample.get("network_tx_bytes"),
            "sample_index": index,
            "collection_started_at_utc": sample.get("collection_started_at_utc"),
            "collection_duration_ms": sample.get("collection_duration_ms"),
            "configured_interval_s": self.interval,
            "effective_interval_s": effective,
            "sample_lag_s": lag,
            "source": sample.get("source"),
            "cpu_method": sample.get("cpu_method"),
            "memory_cgroup_current_bytes": sample.get("memory_cgroup_current_bytes"),
            "memory_cgroup_method": sample.get("memory_cgroup_method"),
            "memory_working_set_method": sample.get("memory_working_set_method"),
            "memory_rss_method": sample.get("memory_rss_method"),
            "network_method": sample.get("network_method"),
            "collection_error": sample.get("collection_error"),
        })
        self.samples += 1
        if sample.get("collection_error"):
            self.errors += 1

    def _accept(self, sample):
        with self._lock:
            run_id = self._run_id
            cell_t0 = self._cell_t0
            allowed = set(self.containers)
        if run_id is None or cell_t0 is None:
            return
        container = sample.get("container")
        if container is not None and container not in allowed:
            return
        self._write_sample(run_id, cell_t0, sample)

    def _loop(self):
        """Drain the stats streams. Host CPU is read on the configured interval.

        Container cadence is the Engine stream (~1s). A configured interval
        below that is recorded as lag; it is not pretended to have been met.
        """
        next_host = time.perf_counter()
        while not self._stop.is_set():
            try:
                sample = self._queue.get(timeout=0.2)
            except queue.Empty:
                sample = None
            if sample is not None:
                self._accept(sample)
            if self.include_host and time.perf_counter() >= next_host:
                self._accept(read_host_cpu_sample())
                next_host = time.perf_counter() + self.interval

    def stop(self):
        self._stop.set()
        self._thread.join(timeout=max(10.0, self.interval * 4))
        return self.samples


def main(argv=None):
    ap = argparse.ArgumentParser(description="Sample container stats into resources.csv")
    ap.add_argument("--run-id", required=True)
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--containers", nargs="+", required=True)
    ap.add_argument("--interval", type=float, default=0.5)
    ap.add_argument("--duration", type=float, default=10)
    ap.add_argument("--no-host", action="store_true")
    args = ap.parse_args(argv)
    sampler = ResourceSampler(
        args.out_dir, args.containers, interval=args.interval, include_host=not args.no_host,
    ).start()
    sampler.set_run(args.run_id)
    time.sleep(args.duration)
    sampler.clear_run()
    n = sampler.stop()
    print(f"wrote {n} resources.csv rows for run {args.run_id} errors={sampler.errors}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
