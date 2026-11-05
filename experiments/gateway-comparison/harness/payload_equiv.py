#!/usr/bin/env python3
"""Raw and effective comparison of forwarded chat payloads.

The raw hash is the canonical JSON of the body the backend actually received.
The effective payload is a fixed set of fields. The only permitted
normalization is: a missing ``stream`` key means false, which is the
deterministic backend's default (``bool(req.get("stream", False))``).

Any other missing key, extra key, or value difference is a mismatch.
This module does not treat an arbitrary omission as equivalent.
"""
from __future__ import annotations

import hashlib
import json

EFFECTIVE_FIELDS = ("model", "messages", "temperature", "max_tokens", "top_p", "stream")
ALLOWLIST = ("absent_stream_means_false",)


def canonical_bytes(body: dict) -> bytes:
    text = json.dumps(body, sort_keys=True, ensure_ascii=False, separators=(",", ":"))
    return text.encode("utf-8")


def raw_sha256(body: dict) -> str:
    return hashlib.sha256(canonical_bytes(body)).hexdigest()


def effective_payload(body: dict) -> dict:
    return {
        "model": body.get("model"),
        "messages": body.get("messages"),
        "temperature": body.get("temperature"),
        "max_tokens": body.get("max_tokens"),
        "top_p": body.get("top_p"),
        "stream": bool(body.get("stream", False)),
    }


def only_absent_stream_false(left: dict, right: dict) -> bool:
    """True when the bodies differ only by a missing stream key whose default is false."""
    if effective_payload(left) != effective_payload(right):
        return False
    keys_left, keys_right = set(left), set(right)
    if keys_left == keys_right:
        return left == right
    if keys_left.symmetric_difference(keys_right) != {"stream"}:
        return False
    present = left if "stream" in left else right
    return present.get("stream") is False


def compare_seen(entries: list) -> dict:
    """Compare backend-recorded views. Message text is not required, only its hash.

    A raw-hash difference is accepted only when the key sets differ by
    ``stream`` and both effective streams are false.
    """
    if len(entries) < 2:
        return {"ok": False, "mismatches": [{"kind": "too_few"}], "raw_sha256": [], "effective": None}

    def eff(entry):
        return {
            "model": entry.get("model"),
            "messages_sha256": entry.get("messages_sha256"),
            "temperature": entry.get("temperature"),
            "max_tokens": entry.get("max_tokens"),
            "top_p": entry.get("top_p"),
            "stream": bool(entry.get("stream")),
        }

    base = eff(entries[0])
    mismatches = []
    for index, entry in enumerate(entries[1:], start=1):
        if eff(entry) != base:
            mismatches.append({"index": index, "kind": "effective"})
            continue
        if entry.get("body_sha256") == entries[0].get("body_sha256"):
            continue
        keys = set(entry.get("keys") or [])
        keys0 = set(entries[0].get("keys") or [])
        if keys.symmetric_difference(keys0) != {"stream"} or base["stream"] is not False:
            mismatches.append({"index": index, "kind": "raw_not_allowlisted"})
    return {
        "ok": not mismatches,
        "raw_sha256": [entry.get("body_sha256") for entry in entries],
        "effective": base,
        "allowlist": list(ALLOWLIST),
        "mismatches": mismatches,
    }


def compare_bodies(bodies: list) -> dict:
    """Compare one body per path. ``ok`` is false for any non-allowlisted difference."""
    if len(bodies) < 2:
        raise ValueError("need at least two bodies")
    raw = [raw_sha256(body) for body in bodies]
    effective = [effective_payload(body) for body in bodies]
    mismatches = []
    for index, body in enumerate(bodies[1:], start=1):
        if effective[index] != effective[0]:
            mismatches.append({"index": index, "kind": "effective"})
            continue
        if raw[index] != raw[0] and not only_absent_stream_false(bodies[0], body):
            mismatches.append({"index": index, "kind": "raw_not_allowlisted"})
    return {
        "ok": not mismatches,
        "raw_sha256": raw,
        "effective": effective,
        "allowlist": list(ALLOWLIST),
        "mismatches": mismatches,
    }
