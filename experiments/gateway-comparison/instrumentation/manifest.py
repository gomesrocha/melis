#!/usr/bin/env python3
"""Build a reproducibility manifest for an experiment run directory.

The manifest records everything needed to tie a set of CSVs to the exact
software, configuration, and environment that produced them (PROTOCOL.md
"Manifesto com commit limpo, digests completos, modelos, configuração, seeds,
hardware, hashes e origem das medidas").

Any software change that affects results requires a NEW manifest; previous data
is preserved and only the impacted cells are repeated. Manifests are append-only
artifacts — never edit one after data is collected; create a new one.

What it captures
----------------
* ``clean_commit``: the artifact commit AND whether the tree was clean (dirty
  trees are flagged; a dirty tree invalidates reproducibility claims).
* ``image_digests``: pinned container digests (passed in; not invented).
* ``models``: model names/versions per path.
* ``config_hashes`` / ``dataset_hashes``: SHA-256 of the exact config/corpus
  files used, so a reviewer can detect drift.
* ``seeds``: scheduling/corpus seeds.
* ``hardware``: host facts (CPU count, memory, kernel, GPU when observable).
* ``measurement_origin``: where each measure comes from (client wall clock,
  deterministic backend counter, cgroup stats, provider usage, ...). This makes
  explicit which numbers are observed vs estimated.
* ``schema_version`` and ``field_limitations``: unobservable fields are listed
  here, not fabricated in the CSVs.

Stdlib only.
"""
from __future__ import annotations

import hashlib
import json
import os
import platform
import subprocess
from datetime import datetime, timezone
from typing import Optional

from schemas import SCHEMA_VERSION


def _utc_now() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.") + \
        f"{datetime.now(timezone.utc).microsecond // 1000:03d}Z"


def sha256_file(path: str) -> Optional[str]:
    try:
        h = hashlib.sha256()
        with open(path, "rb") as fh:
            for chunk in iter(lambda: fh.read(65536), b""):
                h.update(chunk)
        return "sha256:" + h.hexdigest()
    except OSError:
        return None


def _git(repo: str, *args) -> Optional[str]:
    try:
        out = subprocess.run(
            ["git", "-C", repo, *args],
            capture_output=True, text=True, timeout=15, check=False,
        )
        if out.returncode != 0:
            return None
        return out.stdout.strip()
    except (OSError, subprocess.SubprocessError):
        return None


def _git_raw(repo: str, *args) -> Optional[str]:
    """Like _git but does NOT strip output (preserves porcelain leading spaces)."""
    try:
        out = subprocess.run(
            ["git", "-C", repo, *args],
            capture_output=True, text=True, timeout=15, check=False,
        )
        if out.returncode != 0:
            return None
        return out.stdout
    except (OSError, subprocess.SubprocessError):
        return None


def git_state(repo: str) -> dict:
    """Commit + cleanliness. A dirty tree is reported, not hidden."""
    commit = _git(repo, "rev-parse", "HEAD")
    status = _git_raw(repo, "status", "--porcelain")
    # Treat an untouched WIP in bedrock.rs (intentionally excluded from commits)
    # as a known exception but STILL report it, so reproducibility is honest.
    # Porcelain v1: two status columns, a space, then the path. We split on the
    # first run of whitespace after the status field to recover the path exactly
    # (robust whether or not the first status column is a space).
    dirty_files = []
    if status:
        for ln in status.splitlines():
            if not ln:
                continue
            # Porcelain v1: 2 status columns + a space + path.
            path = ln[3:] if len(ln) > 3 else ln.strip()
            dirty_files.append(path)
    return {
        "commit": commit,
        "clean": (status or "").strip() == "",
        "dirty_files": dirty_files,
    }


def hardware_facts() -> dict:
    facts = {
        "platform": platform.platform(),
        "machine": platform.machine(),
        "processor": platform.processor(),
        "python": platform.python_version(),
        "cpu_count": os.cpu_count(),
    }
    # Best-effort memory + GPU; absence stays absent (not zero).
    try:
        facts["mem_total_bytes"] = os.sysconf("SC_PAGE_SIZE") * os.sysconf("SC_PHYS_PAGES")
    except (ValueError, OSError, AttributeError):
        facts["mem_total_bytes"] = None
    nvidia = subprocess.run(
        ["nvidia-smi", "--query-gpu=name,driver_version,memory.total",
         "--format=csv,noheader"],
        capture_output=True, text=True, check=False,
    ) if _which("nvidia-smi") else None
    facts["gpu"] = nvidia.stdout.strip() if nvidia and nvidia.returncode == 0 else None
    return facts


def _which(prog: str) -> bool:
    for d in os.environ.get("PATH", "").split(os.pathsep):
        if os.path.isfile(os.path.join(d, prog)) and os.access(os.path.join(d, prog), os.X_OK):
            return True
    return False


def build_manifest(
    experiment_id: str,
    phase: str,
    repo: str,
    image_digests: dict,
    models: dict,
    config_files: dict,
    dataset_files: Optional[dict] = None,
    seeds: Optional[dict] = None,
    measurement_origin: Optional[dict] = None,
    field_limitations: Optional[dict] = None,
    extra: Optional[dict] = None,
) -> dict:
    """Assemble the manifest dict. Hashes are computed from file paths provided."""
    config_hashes = {name: sha256_file(path) for name, path in (config_files or {}).items()}
    dataset_hashes = {name: sha256_file(path) for name, path in (dataset_files or {}).items()}
    manifest = {
        "schema_version": SCHEMA_VERSION,
        "generated_at_utc": _utc_now(),
        "experiment_id": experiment_id,
        "phase": phase,
        "artifact": git_state(repo),
        "image_digests": image_digests,
        "models": models,
        "config_hashes": config_hashes,
        "dataset_hashes": dataset_hashes,
        "seeds": seeds or {},
        "hardware": hardware_facts(),
        "measurement_origin": measurement_origin or {},
        "field_limitations": field_limitations or {},
    }
    if extra:
        manifest["extra"] = extra
    return manifest


def write_manifest(manifest: dict, out_path: str):
    os.makedirs(os.path.dirname(os.path.abspath(out_path)), exist_ok=True)
    with open(out_path, "w", encoding="utf-8") as fh:
        json.dump(manifest, fh, indent=2, ensure_ascii=False, sort_keys=True)
        fh.write("\n")
    return out_path
