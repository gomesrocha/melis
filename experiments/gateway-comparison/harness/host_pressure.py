#!/usr/bin/env python3
"""Read host memory and swap activity. Does not change sysctl or stop stacks.

Occupied swap by itself is not thrashing. Thrashing evidence is swap-in or
swap-out increasing while available memory is low.
"""
from __future__ import annotations

import os
import time


def parse_meminfo(text: str) -> dict:
    out = {}
    for line in text.splitlines():
        if ":" not in line:
            continue
        key, rest = line.split(":", 1)
        parts = rest.split()
        if not parts:
            continue
        out[key] = int(parts[0]) * (1024 if len(parts) > 1 and parts[1] == "kB" else 1)
    return out


def parse_vmstat(text: str) -> dict:
    out = {}
    for line in text.splitlines():
        parts = line.split()
        if len(parts) == 2 and parts[1].isdigit():
            out[parts[0]] = int(parts[1])
    return out


def read_pressure(root: str = "") -> dict:
    """``root`` is empty on the host. Tests pass a fixture directory."""
    mem_path = os.path.join(root, "proc/meminfo") if root else "/proc/meminfo"
    vm_path = os.path.join(root, "proc/vmstat") if root else "/proc/vmstat"
    load_path = os.path.join(root, "proc/loadavg") if root else "/proc/loadavg"
    mem = parse_meminfo(open(mem_path, encoding="utf-8").read())
    vm = parse_vmstat(open(vm_path, encoding="utf-8").read())
    load = open(load_path, encoding="utf-8").read().split()
    return {
        "mem_available_bytes": mem.get("MemAvailable"),
        "mem_total_bytes": mem.get("MemTotal"),
        "swap_free_bytes": mem.get("SwapFree"),
        "swap_total_bytes": mem.get("SwapTotal"),
        "pswpin": vm.get("pswpin"),
        "pswpout": vm.get("pswpout"),
        "load1": float(load[0]) if load else None,
    }


def pressure_delta(before: dict, after: dict) -> dict:
    return {
        "pswpin": (after.get("pswpin") or 0) - (before.get("pswpin") or 0),
        "pswpout": (after.get("pswpout") or 0) - (before.get("pswpout") or 0),
        "mem_available_bytes": after.get("mem_available_bytes"),
        "swap_free_bytes": after.get("swap_free_bytes"),
        "swap_total_bytes": after.get("swap_total_bytes"),
        "load1": after.get("load1"),
    }


# A handful of pages on a host whose swap is already full is not thrashing.
# 1024 pages is 4 MiB during the sample. Below that, with memory still
# available, the run records the counters and continues.
RELEVANT_SWAP_PAGES = 1024


def inadequate(delta: dict, min_available_bytes: int = 4 * 1024 ** 3) -> str:
    """Return a reason to refuse a long pilot, or an empty string.

    Swap that is merely full is not thrashing. A few pages moving while
    several gigabytes stay available is recorded, not a refusal. Low
    available memory refuses on its own. Large swap movement refuses too.
    """
    available = delta.get("mem_available_bytes")
    if available is None:
        return "MemAvailable unreadable"
    if available < min_available_bytes:
        return f"MemAvailable {available} bytes is below {min_available_bytes}"
    moved = (delta.get("pswpin") or 0) + (delta.get("pswpout") or 0)
    if moved >= RELEVANT_SWAP_PAGES:
        return (
            f"swap activity during the sample (in={delta.get('pswpin')}, "
            f"out={delta.get('pswpout')})"
        )
    return ""


def sample_delta(seconds: float = 5.0) -> dict:
    before = read_pressure()
    time.sleep(seconds)
    after = read_pressure()
    delta = pressure_delta(before, after)
    delta["reason"] = inadequate(delta)
    delta["before"] = before
    delta["after"] = after
    return delta
