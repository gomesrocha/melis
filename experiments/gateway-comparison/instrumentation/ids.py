#!/usr/bin/env python3
"""Stable, reproducible identifiers for experiment, run, block, case, pair, request.

Design goals
------------
* **Stable**: the same logical inputs always produce the same id, so reruns and
  independent reproductions line up. Ids are deterministic functions of their
  components (no wall-clock, no random component unless explicitly seeded).
* **Pairing**: ``pair_id`` is independent of the gateway, so the three paths
  (direct/melis/litellm) share a pairing key for the same (block, case, scenario,
  streaming, concurrency) cell. This encodes the protocol's paired design.
* **Nesting**: ``request_id`` is unique per logical request but derives from the
  block/case/pair so nested requests are never mistaken for independent units.

Stdlib only.
"""
from __future__ import annotations

import hashlib
import uuid

_SEP = "|"


def _slug(value) -> str:
    s = str(value).strip().lower()
    return "".join(ch if ch.isalnum() or ch in "-._" else "-" for ch in s)


def _short_hash(*parts) -> str:
    h = hashlib.sha256(_SEP.join(str(p) for p in parts).encode("utf-8"))
    return h.hexdigest()[:12]


def experiment_id(name: str, protocol_version: str, schema_version: str = "") -> str:
    """Stable id of the experiment definition (name + protocol).

    ``schema_version`` is accepted so older callers keep working. It is not
    part of the hash: a schema bump must not look like a new experiment.
    A collection is a separate id.
    """
    return f"exp-{_slug(name)}-{_short_hash(name, protocol_version)}"


def new_collection_nonce() -> str:
    """A new collection. The same nonce reproduces the same collection_id."""
    return uuid.uuid4().hex


def collection_id(experiment_id_: str, nonce: str) -> str:
    """Unique per collection. Reprocessing the same nonce preserves it.

    A new collection must pass a new nonce even when the seed and the
    definition match. Historical files are not rewritten; namespace them
    with ``historical_collection_namespace`` at analysis time.
    """
    if nonce is None or str(nonce).strip() == "":
        raise ValueError("collection nonce must be non-empty")
    return "col-" + _short_hash(experiment_id_, nonce)


def historical_collection_namespace(source_label: str) -> str:
    """Label for a directory written before collection_id existed.

    Does not modify the historical file. Callers that concatenate those
    directories apply this namespace to copies of the rows.
    """
    if not str(source_label).strip():
        raise ValueError("historical source label must be non-empty")
    return "hist-" + _short_hash("historical", source_label)


def apply_historical_namespace(rows, source_label: str):
    """Return copies whose ids cannot collide with another historical source.

    The input rows are not mutated. Block, run, pair and request ids are
    prefixed. Gateways that shared a pair inside the source still share it.
    """
    namespace = historical_collection_namespace(source_label)
    out = []
    for row in rows:
        copied = dict(row)
        copied["collection_id"] = namespace
        for key in ("block_id", "run_id", "pair_id", "request_id"):
            value = copied.get(key)
            if value:
                copied[key] = f"{namespace}:{value}"
        out.append(copied)
    return out


def block_id(experiment_id_: str, phase: str, block_index, collection_id_: str = "") -> str:
    """A block is an independent repetition unit inside one collection.

    Omitting ``collection_id_`` reproduces the pre-1.2.0 hash so historical
    ids can still be recomputed. New collections must pass it.
    """
    parts = [experiment_id_, phase, int(block_index)]
    if collection_id_:
        parts.append(collection_id_)
    return f"blk-{_slug(phase)}-{int(block_index):04d}-{_short_hash(*parts)}"


def pair_id(block_id_: str, case_id_: str, scenario: str, streaming: bool, concurrency,
            collection_id_: str = "") -> str:
    """Gateway-INDEPENDENT pairing key inside one collection.

    Two requests share a pair_id when they belong to the same collection,
    block, case, scenario, streaming mode and concurrency. A second
    collection does not share that key.
    """
    parts = [block_id_, case_id_, scenario, bool(streaming), concurrency]
    if collection_id_:
        parts.append(collection_id_)
    return "pair-" + _short_hash(*parts)


def run_id(experiment_id_: str, phase: str, gateway: str, block_id_: str,
           scenario: str, streaming: bool, concurrency, attempt: int = 0,
           collection_id_: str = "") -> str:
    """A measured run: one gateway, one cell, one collection."""
    parts = [
        experiment_id_, phase, gateway, block_id_, scenario,
        bool(streaming), concurrency, attempt,
    ]
    if collection_id_:
        parts.append(collection_id_)
    return "run-" + _short_hash(*parts)


def request_id(run_id_: str, pair_id_: str, index: int) -> str:
    """Unique logical request id; nested under its run and pair."""
    return f"req-{_short_hash(run_id_, pair_id_)}-{int(index):06d}"


def case_id(experiment_id_: str, family: str, index) -> str:
    """Logical case id, grouped by family/template to avoid pseudo-replication."""
    return f"case-{_slug(family)}-{int(index):04d}"


def episode_id(run_id_: str, index) -> str:
    """Independent resilience episode (reset state)."""
    return f"ep-{_short_hash(run_id_)}-{int(index):04d}"


def answer_id(case_id_: str, policy: str, generation_rep: int) -> str:
    """Id for a stored answer blob (one generation of one case under one policy)."""
    return f"ans-{_slug(case_id_)}-{_slug(policy)}-g{int(generation_rep):03d}"
