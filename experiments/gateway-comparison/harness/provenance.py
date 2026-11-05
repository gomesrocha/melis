#!/usr/bin/env python3
"""Separate an execution checkout from the image that served it.

An image id is the local engine id. A registry digest is a different field
and may be empty. Neither field is a git commit. A source commit is recorded
only from an image label or from a build this process invoked on a clean
checkout. Missing metadata stays null.
"""
from __future__ import annotations

import hashlib
import json
import os
import subprocess
from typing import Optional

LABEL_REVISION = "org.opencontainers.image.revision"
BUILD_INPUTS = ("Dockerfile", "Cargo.toml", "Cargo.lock", "src")


def sha256_file(path: str) -> Optional[str]:
    try:
        digest = hashlib.sha256()
        with open(path, "rb") as fh:
            for chunk in iter(lambda: fh.read(65536), b""):
                digest.update(chunk)
        return digest.hexdigest()
    except OSError:
        return None


def image_identity_from_inspect(meta: Optional[dict]) -> dict:
    """Copy identity fields. Do not invent a digest or a revision label."""
    empty = {
        "image_id": None,
        "repo_digests": [],
        "architecture": None,
        "os": None,
        "created": None,
        "labels": None,
        "label_revision": None,
    }
    if not meta:
        return empty
    config = meta.get("Config") or {}
    labels = config.get("Labels")
    revision = None
    if isinstance(labels, dict):
        revision = labels.get(LABEL_REVISION)
    else:
        labels = None
    return {
        "image_id": meta.get("Id"),
        "repo_digests": list(meta.get("RepoDigests") or []),
        "architecture": meta.get("Architecture"),
        "os": meta.get("Os"),
        "created": meta.get("Created"),
        "labels": labels,
        "label_revision": revision,
    }


def build_source(*, invoked_build: bool, execution_clean: bool,
                  execution_commit: Optional[str], label_revision: Optional[str],
                  image_created: Optional[str] = None,
                  process_started: Optional[str] = None) -> dict:
    """Return a source commit only when the evidence kind names where it came from.

    Requesting a build is not enough. A cached image has no revision label and
    a Created timestamp from before this process; that image does not inherit
    the execution commit.
    """
    if label_revision:
        return {
            "source_commit": label_revision,
            "evidence_kind": "image_label",
            "invoked_during_this_process": bool(invoked_build),
        }
    created_by_this_process = bool(
        image_created and process_started and image_created >= process_started
    )
    if invoked_build and execution_clean and execution_commit and created_by_this_process:
        return {
            "source_commit": execution_commit,
            "evidence_kind": "invoked_from_clean_checkout",
            "invoked_during_this_process": True,
        }
    return {
        "source_commit": None,
        "evidence_kind": "none",
        "invoked_during_this_process": bool(invoked_build and created_by_this_process),
    }


def equivalence_from_numstat(left: str, right: str, numstat: Optional[str],
                             paths: tuple[str, ...]) -> dict:
    """Interpret `git diff --numstat`. None means the comparison was not run."""
    if numstat is None:
        rows = [
            {"path": path, "identical": None, "added": None, "removed": None}
            for path in paths
        ]
        return {"left": left, "right": right, "compared": False, "paths": rows}
    changed = {}
    for line in numstat.splitlines():
        if not line.strip():
            continue
        added, removed, path = line.split("\t", 2)
        changed[path] = (added, removed)
    rows = []
    for path in paths:
        # A directory input matches any numstat path under it.
        hits = [
            (name, values) for name, values in changed.items()
            if name == path or name.startswith(path.rstrip("/") + "/")
        ]
        if not hits:
            rows.append({"path": path, "identical": True, "added": 0, "removed": 0})
        else:
            rows.append({
                "path": path,
                "identical": False,
                "added": sum(int(item[1][0]) for item in hits if item[1][0].isdigit()),
                "removed": sum(int(item[1][1]) for item in hits if item[1][1].isdigit()),
            })
    return {
        "left": left,
        "right": right,
        "compared": True,
        "identical": all(row["identical"] for row in rows),
        "paths": rows,
    }


def git_diff_numstat(repo: str, left: str, right: str, paths: tuple[str, ...]) -> Optional[str]:
    try:
        proc = subprocess.run(
            ["git", "-C", repo, "diff", "--numstat", left, right, "--", *paths],
            capture_output=True, text=True, timeout=30, check=False,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if proc.returncode != 0:
        return None
    return proc.stdout


def assemble_provenance(*, collection_id: str, execution: dict, images: dict,
                        build: dict, harness_hashes: dict, compose_files: dict,
                        config_hashes: dict, equivalence: Optional[dict] = None,
                        original_manifest_sha256: Optional[str] = None,
                        notes: Optional[list] = None) -> dict:
    if build.get("source_commit") and build.get("evidence_kind") in (None, "", "none"):
        raise ValueError("refusing a source commit that has no evidence kind")
    if original_manifest_sha256 is not None and not collection_id:
        raise ValueError("a manifest hash needs a collection_id")
    return {
        "collection_id": collection_id,
        "original_manifest_sha256": original_manifest_sha256,
        "replaces_manifest": False,
        "execution_checkout": {
            "commit": execution.get("commit"),
            "clean": execution.get("clean"),
            "dirty_files": list(execution.get("dirty_files") or []),
        },
        "images": images,
        "image_build": build,
        "file_equivalence": equivalence,
        "harness_hashes": harness_hashes,
        "compose_files": compose_files,
        "config_hashes": config_hashes,
        "notes": list(notes or []),
    }


def historical_supplement(**kwargs) -> dict:
    """Sidecar for a collection whose manifest is already frozen."""
    if not kwargs.get("collection_id"):
        raise ValueError("collection_id is required")
    digest = kwargs.get("original_manifest_sha256")
    if not digest or len(digest) != 64:
        raise ValueError("original_manifest_sha256 must be the full hex digest")
    record = assemble_provenance(**kwargs)
    record["role"] = "historical_supplement"
    return record


def write_json(record: dict, path: str) -> str:
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    with open(path, "w", encoding="utf-8") as fh:
        json.dump(record, fh, indent=2, ensure_ascii=False, sort_keys=True)
        fh.write("\n")
    return path
