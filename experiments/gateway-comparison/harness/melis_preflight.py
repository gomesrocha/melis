#!/usr/bin/env python3
"""Detect a Melis process that rejected its config and kept running on defaults.

``main.rs`` prints ``Using defaults.`` and then listens. A health check does
not reveal that. Measurement must abort when this marker is present.
"""
from __future__ import annotations

MARKERS = (
    "Failed to load config.yaml",
    "Using defaults.",
)


def startup_rejected_config(log_text: str) -> bool:
    return any(marker in (log_text or "") for marker in MARKERS)


def assert_mounted_config(log_text: str) -> None:
    if startup_rejected_config(log_text):
        excerpt = " | ".join(
            line.strip() for line in (log_text or "").splitlines() if any(m in line for m in MARKERS)
        )
        raise RuntimeError(
            "Melis rejected the mounted configuration and loaded built-in defaults. "
            "The process can still listen, so health alone is not evidence. "
            f"Aborting before measurement. {excerpt}"
        )


def assert_seen_model(seen_entries, expected_model: str) -> None:
    models = {entry.get("model") for entry in seen_entries}
    if models != {expected_model}:
        raise RuntimeError(
            f"backend saw models {sorted(str(m) for m in models)}; expected only {expected_model!r}"
        )
