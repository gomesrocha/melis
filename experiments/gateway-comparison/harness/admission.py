#!/usr/bin/env python3
"""60-second host admission for phase=final.

The 5-second gate in host_pressure.inadequate is a complementary check.
It does not admit a final collection. An admission belongs to one
invocation and is not reused for a later start. This module does not
change sysctl, swap, or running stacks.
"""
from __future__ import annotations

import os
import time

import final_protocol
import host_pressure

UNITS = {
    "mem_available_bytes": "bytes",
    "swap_pages": "pages",
    "load1": "loadavg_1min",
    "disk_free_bytes": "bytes",
    "duration_s": "seconds",
}


def disk_free_bytes(path: str) -> int | None:
    try:
        st = os.statvfs(path)
    except OSError:
        return None
    return int(st.f_bavail * st.f_frsize)


def judge(readings, disk_free, duration_s, components, criteria=None) -> dict:
    """Decide from a finished sample. Missing metrics refuse."""
    criteria = criteria or final_protocol.ADMISSION
    reasons = []
    if duration_s is None or float(duration_s) < float(criteria["duration_s"]):
        reasons.append("duration_short")
    if not readings:
        reasons.append("no_readings")
    if components is None:
        reasons.append("components_unlisted")
    if disk_free is None:
        reasons.append("disk_free_missing")
    elif disk_free < criteria["min_disk_free_bytes"]:
        reasons.append("disk_free")

    mems = []
    loads = []
    swap_complete = bool(readings)
    for reading in readings:
        mem = reading.get("mem_available_bytes")
        load = reading.get("load1")
        if mem is None:
            reasons.append("mem_available_missing")
        else:
            mems.append(mem)
        if load is None:
            reasons.append("load1_missing")
        else:
            loads.append(load)
        if reading.get("pswpin") is None or reading.get("pswpout") is None:
            reasons.append("swap_missing")
            swap_complete = False
    if mems and min(mems) < criteria["min_mem_available_bytes"]:
        reasons.append("mem_available")
    if mems and any(value < criteria["min_reading_mem_available_bytes"] for value in mems):
        reasons.append("mem_reading_floor")
    if loads and any(value >= criteria["max_load1"] for value in loads):
        reasons.append("load1")
    pages = None
    if swap_complete and readings:
        pages = (
            (readings[-1]["pswpin"] - readings[0]["pswpin"])
            + (readings[-1]["pswpout"] - readings[0]["pswpout"])
        )
        if pages < 0:
            reasons.append("swap_counter_decreased")
        elif pages >= criteria["max_swap_pages"]:
            reasons.append("swap_pages")
    unique = list(dict.fromkeys(reasons))
    return {
        "admitted": not unique,
        "reasons": unique,
        "duration_s": duration_s,
        "readings": readings,
        "disk_free_bytes": disk_free,
        "swap_pages": pages,
        "components_already_up": components,
        "units": dict(UNITS),
        "criteria": criteria,
        "reusable": False,
        "replaces_5s_gate": False,
    }


def accept_for_start(record: dict, invocation_id: str, now_s: float, max_age_s: float = 60.0) -> str:
    """Return an empty string when this invocation may use the sample.

    A sample from another invocation, an old sample, a short sample, or a
    refusal is not a start. The 5-second gate fails here on duration.
    """
    if not record or record.get("invocation_id") != invocation_id:
        return "stale_invocation"
    finished = record.get("finished_at_s")
    if finished is None or now_s - float(finished) > max_age_s:
        return "stale_age"
    if float(record.get("duration_s") or 0) < float(final_protocol.ADMISSION["duration_s"]):
        return "not_the_60s_admission"
    if not record.get("admitted"):
        return "not_admitted"
    return ""


def collect(invocation_id: str, disk_path: str, seconds: float = 60.0, interval: float = 5.0,
            reader=None, sleeper=None, disk_reader=None, component_lister=None, clock=None) -> dict:
    """Sample until ``seconds`` have elapsed on ``clock``. Tests pass a fake clock."""
    reader = reader or host_pressure.read_pressure
    sleeper = sleeper or time.sleep
    disk_reader = disk_reader or disk_free_bytes
    clock = clock or time.time
    if component_lister is None:
        raise RuntimeError("component_lister is required so the sample names what is already up")
    readings = []
    started = clock()
    while True:
        readings.append(reader())
        elapsed = clock() - started
        if elapsed >= seconds:
            break
        sleeper(min(interval, seconds - elapsed))
    record = judge(readings, disk_reader(disk_path), clock() - started, component_lister())
    record["invocation_id"] = invocation_id
    record["started_at_s"] = started
    record["finished_at_s"] = clock()
    record["disk_path"] = disk_path
    return record


def classify_interval(before: dict, after: dict, disk_free, gateway_memory_delta, limits=None) -> dict:
    """One monitor interval. Grave is safety. It is not a statistical discard."""
    limits = limits or final_protocol.MONITOR
    missing = []
    for name in ("mem_available_bytes", "pswpin", "pswpout"):
        if after.get(name) is None or (name != "mem_available_bytes" and before.get(name) is None):
            missing.append(name)
    if disk_free is None:
        missing.append("disk_free_bytes")
    drop = None
    if before.get("mem_available_bytes") is not None and after.get("mem_available_bytes") is not None:
        drop = before["mem_available_bytes"] - after["mem_available_bytes"]
    if missing:
        return {
            "host": "metric_missing",
            "missing": missing,
            "gateway_memory_delta_bytes": gateway_memory_delta,
            "host_mem_drop_bytes": drop,
            "disk_free_bytes": disk_free,
            "decision": "metric_missing",
        }
    mem = after["mem_available_bytes"]
    pages = (after["pswpin"] - before["pswpin"]) + (after["pswpout"] - before["pswpout"])
    grave = limits["grave"]
    env = limits["environmental"]
    host = "ok"
    if (mem < grave["min_mem_available_bytes"] or disk_free < grave["min_disk_free_bytes"]
            or pages >= grave["max_swap_pages_per_interval"]):
        host = "grave"
    elif (mem < env["min_mem_available_bytes"] or disk_free < env["min_disk_free_bytes"]
            or pages >= env["max_swap_pages_per_interval"]):
        host = "environmental_threshold"
    return {
        "host": host,
        "mem_available_bytes": mem,
        "swap_pages": pages,
        "disk_free_bytes": disk_free,
        "host_mem_drop_bytes": drop,
        "gateway_memory_delta_bytes": gateway_memory_delta,
        "units": dict(UNITS),
        "interval_s": limits["interval_s"],
    }


class Monitor:
    """30 s sampler that records a decision. Tests call ``poll_once`` with fakes.

    Grave and a missing metric set ``stop_event`` so the cell stops admitting
    work. An environmental threshold does not, because that block still
    finishes its three paths.
    """

    def __init__(self, path, limits=None, reader=None, disk_reader=None,
                 memory_reader=None, sleeper=None):
        import json
        import threading
        self._json = json
        self._threading = threading
        self.path = path
        self.limits = limits or final_protocol.MONITOR
        self.reader = reader or host_pressure.read_pressure
        self.disk_reader = disk_reader
        self.memory_reader = memory_reader or (lambda: None)
        self.sleeper = sleeper or time.sleep
        self.latest = {"host": "ok", "gateway_memory_delta_bytes": None, "host_mem_drop_bytes": None}
        self.decisions = []
        self.stop_event = threading.Event()
        self._stop = threading.Event()
        self._previous = None
        self._thread = None

    def poll_once(self) -> dict:
        current = self.reader()
        if self._previous is None:
            self._previous = current
            return {"host": "baseline"}
        disk = None if self.disk_reader is None else self.disk_reader()
        interval = classify_interval(
            self._previous, current, disk, self.memory_reader(), self.limits,
        )
        self._previous = current
        self.latest = interval
        self.decisions.append(interval)
        if interval.get("host") in ("grave", "metric_missing") or interval.get("decision") == "metric_missing":
            self.stop_event.set()
        if self.path:
            with open(self.path, "a", encoding="utf-8") as fh:
                fh.write(self._json.dumps(interval) + "\n")
        return interval

    def run_until(self, samples: int) -> None:
        for _ in range(samples):
            if self._stop.is_set():
                return
            self.sleeper(self.limits["interval_s"])
            if self._stop.is_set():
                return
            self.poll_once()

    def start(self) -> None:
        self._previous = self.reader()
        self._thread = self._threading.Thread(target=self.run_until, args=(10_000,), daemon=True)
        self._thread.start()

    def stop(self) -> None:
        self._stop.set()
        if self._thread is not None:
            self._thread.join(timeout=2)


def decide_cell(interval: dict, cell_failed: bool, limits=None) -> str:
    """Attribute one cell. Latency is not an input."""
    limits = limits or final_protocol.MONITOR
    host = interval.get("host")
    if host == "metric_missing":
        return "metric_missing"
    if host == "grave":
        return "grave"
    if host == "ok":
        return "system" if cell_failed else "ok"
    gateway = interval.get("gateway_memory_delta_bytes")
    drop = interval.get("host_mem_drop_bytes")
    share = limits["attribution_gateway_share"]
    if gateway is None or drop is None or drop <= 0:
        return "uncertain"
    if gateway >= share * drop:
        return "system"
    return "environmental"
