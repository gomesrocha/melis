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

_SEP = "|"


def _slug(value) -> str:
    s = str(value).strip().lower()
    return "".join(ch if ch.isalnum() or ch in "-._" else "-" for ch in s)


def _short_hash(*parts) -> str:
    h = hashlib.sha256(_SEP.join(str(p) for p in parts).encode("utf-8"))
    return h.hexdigest()[:12]


def experiment_id(name: str, protocol_version: str, schema_version: str) -> str:
    """Stable id for an experiment definition (name + protocol + schema)."""
    return f"exp-{_slug(name)}-{_short_hash(name, protocol_version, schema_version)}"


def block_id(experiment_id_: str, phase: str, block_index) -> str:
    """A block is an independent repetition unit."""
    return f"blk-{_slug(phase)}-{int(block_index):04d}-{_short_hash(experiment_id_, phase, block_index)}"


def pair_id(block_id_: str, case_id_: str, scenario: str, streaming: bool, concurrency) -> str:
    """Gateway-INDEPENDENT pairing key: the same across direct/melis/litellm.

    Two requests share a pair_id iff they belong to the same block, case,
    scenario, streaming mode, and concurrency — i.e. they are the paired
    observations compared across gateways.
    """
    return "pair-" + _short_hash(block_id_, case_id_, scenario, bool(streaming), concurrency)


def run_id(experiment_id_: str, phase: str, gateway: str, block_id_: str,
           scenario: str, streaming: bool, concurrency, attempt: int = 0) -> str:
    """A measured run: one gateway, one cell. ``attempt`` disambiguates reruns."""
    return "run-" + _short_hash(
        experiment_id_, phase, gateway, block_id_, scenario, bool(streaming), concurrency, attempt
    )


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
