#!/usr/bin/env python3
"""Candidate protocol for a future phase=final collection.

The file is a candidate. ``execution_authorized`` stays false. A matching
hash and ``--confirm-operational-start`` do not authorize the collection;
that flag only confirms that the operator is allowed to start after the
conversation has authorized it. This module does not start containers or
send requests.
"""
from __future__ import annotations

import hashlib
import json
import os

import block_design
import provenance

PROTOCOL_VERSION = "1.0.0"
AMENDMENT_VERSION = "1.1.0"
PRESERVED_SHA256 = "d566da7ab4804d2313a6ca63deae17b2244841ecc57eddd1628b0861f638184e"
GiB = 1024 ** 3
IMAGE_ID = "sha256:79adb313b5060df96c6e5218bb108a289cd7d17c1f1fa08df9b221a6232c43bb"
IMAGE_BUILD_COMMIT = "7b1c4a8734c301344615adc8c213b2dc7a2fb085"
GATEWAYS = ["direct", "melis", "litellm"]
CONTRASTS = ["melis_minus_direct", "litellm_minus_direct", "melis_minus_litellm"]
# Observed on col-33439a62b8ab. Extrapolation only; not a new measurement.
SWITCH_S_PER_BLOCK = 25.937
SWITCH_SOURCE = "col-33439a62b8ab observed 25.937 s/block; not remeasured"

ADMISSION_V1 = {
    "duration_s": 60.0,
    "min_mem_available_bytes": 8 * GiB,
    "min_reading_mem_available_bytes": 6 * GiB,
    "max_swap_pages": 256,
    "max_load1": 2.0,
    "min_disk_free_bytes": 20 * GiB,
}

# Shared-host amendment. Load average is recorded and does not refuse the host.
ADMISSION = {
    "duration_s": 60.0,
    "min_mem_available_bytes": 8 * GiB,
    "min_reading_mem_available_bytes": 6 * GiB,
    "max_swap_pages": 256,
    "min_disk_free_bytes": 20 * GiB,
    "load1_role": "diagnostic",
    "cpu_record_required": True,
    "volumes_required": ["csv", "docker"],
    "scenario": "shared_host",
}

MONITOR_V1 = {
    "interval_s": 30.0,
    "environmental": {
        "min_mem_available_bytes": 6 * GiB,
        "max_swap_pages_per_interval": 1024,
        "min_disk_free_bytes": 10 * GiB,
    },
    "grave": {
        "min_mem_available_bytes": 1 * GiB,
        "max_swap_pages_per_interval": 65536,
        "min_disk_free_bytes": 1 * GiB,
    },
    # Share of the host MemAvailable drop explained by the active gateway
    # container. At or above this share the pressure stays a system result.
    "attribution_gateway_share": 0.5,
}

MONITOR = dict(MONITOR_V1)
MONITOR["load_average_excludes_block"] = False
MONITOR["latency_excludes_block"] = False

TEST_PROCEDURE = {
    "status": "proposed_not_executed",
    "tests": "two_sided_student_t_of_block_differences_against_zero",
    "family": list(CONTRASTS),
    "multiplicity": "holm",
    "family_alpha": 0.05,
    "intervals": "marginal_student_t_95",
    "intervals_are_not_simultaneous": True,
    "assumptions": [
        "block_differences_are_independent",
        "block_differences_are_drawn_from_a_normal_distribution",
        "the_unit_is_the_planned_block_not_the_request",
    ],
    "not_computed_in_this_revision": True,
}

REQUIRED = (
    "protocol_version", "role", "execution_authorized", "confirmatory_claim",
    "experiment_name", "phase", "blocks", "permutation_repeats", "seed",
    "modes", "concurrency", "warmup_s", "measure_s", "backend_latency_ms",
    "feature_profile", "order", "gateways", "model", "contrasts", "estimator",
    "test_procedure", "precision_target_ms", "precision_target_role",
    "melis_image_id", "image_build_commit", "admission", "monitor", "repeat",
    "orders", "operational_confirmation", "collection_directory_policy",
    "historical_host_refusal_not_an_admission",
)


class ProtocolError(ValueError):
    """The candidate protocol and the invocation do not agree."""


def expected_orders():
    return [
        list(order) for order in block_design.permutation_orders(GATEWAYS, 3, 20261105)
    ]


def candidate_document() -> dict:
    """The candidate. Not an authorization to collect."""
    return {
        "protocol_version": PROTOCOL_VERSION,
        "role": "candidate",
        "execution_authorized": False,
        "confirmatory_claim": False,
        "experiment_name": "rq1-comparative",
        "phase": "final",
        "blocks": 18,
        "permutation_repeats": 3,
        "seed": 20261105,
        "modes": ["json"],
        "concurrency": [1],
        "warmup_s": 20,
        "measure_s": 60,
        "backend_latency_ms": 0,
        "feature_profile": "minimal",
        "order": "permutations",
        "gateways": list(GATEWAYS),
        "model": "mock-model",
        "contrasts": list(CONTRASTS),
        "estimator": "mean_of_paired_block_median_differences",
        "test_procedure": json.loads(json.dumps(TEST_PROCEDURE)),
        "precision_target_ms": 0.5,
        "precision_target_role": "candidate_planning_half_width",
        "melis_image_id": IMAGE_ID,
        "image_build_commit": IMAGE_BUILD_COMMIT,
        "image_id_is_not_a_registry_digest": True,
        "admission": json.loads(json.dumps(ADMISSION_V1)),
        "monitor": json.loads(json.dumps(MONITOR_V1)),
        "repeat": {
            "max_full_permutation_repeats": 1,
            "unit": "full_permutation",
            "repeat_one_path": False,
            "requires_new_admission": True,
            "on_second_violation": "stop_incomplete",
            "on_failed_readmission": "stop_incomplete",
            "does_not_increase_n": True,
        },
        "orders": expected_orders(),
        "operational_confirmation": {
            "flag": "--confirm-operational-start",
            "is_conversation_authorization": False,
            "required_to_start_load": True,
            "required_for_dry_run": False,
        },
        "collection_directory_policy": (
            "new directory and new collection_id; do not rename a pilot "
            "or reuse its rows as confirmatory evidence"
        ),
        "harness_commit_policy": (
            "The execution checkout must be clean. If HEAD differs from "
            "image_build_commit, Dockerfile, Cargo.toml, Cargo.lock and src "
            "must be identical to that commit. The image is not relabeled "
            "with HEAD."
        ),
        "historical_host_refusal_not_an_admission": {
            "sample_start": "2026-11-05T22:14:56Z",
            "sample_end": "2026-11-05T22:15:56Z",
            "reason": "load1",
            "load1_min": 10.46,
            "load1_max": 11.33,
            "threshold": 2.0,
            "stands": True,
            "reusable": False,
        },
    }


def dumps(document: dict) -> str:
    return json.dumps(document, indent=2, sort_keys=True) + "\n"


def sha256_text(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def load(path: str) -> tuple[dict, str]:
    with open(path, encoding="utf-8") as fh:
        raw = fh.read()
    try:
        document = json.loads(raw)
    except json.JSONDecodeError as exc:
        raise ProtocolError(f"protocol is not JSON: {exc}") from exc
    if not isinstance(document, dict):
        raise ProtocolError("protocol must be a JSON object")
    return document, sha256_text(raw)


def _common(document: dict) -> None:
    missing = [name for name in REQUIRED if name not in document]
    if missing:
        raise ProtocolError("protocol missing fields: " + ", ".join(missing))
    if document["role"] != "candidate":
        raise ProtocolError("protocol role must stay candidate")
    if document["execution_authorized"] is not False:
        raise ProtocolError("protocol file cannot authorize execution")
    if document["confirmatory_claim"] is not False:
        raise ProtocolError("protocol file cannot claim confirmatory evidence")
    if document["phase"] != "final":
        raise ProtocolError("protocol phase must be final")
    if document["test_procedure"] != TEST_PROCEDURE:
        raise ProtocolError("test procedure differs from the proposed procedure")
    if document["melis_image_id"] != IMAGE_ID:
        raise ProtocolError("protocol image id differs from the recorded local id")
    if document["image_build_commit"] != IMAGE_BUILD_COMMIT:
        raise ProtocolError("protocol build commit differs from the recorded build")
    if document["orders"] != expected_orders():
        raise ProtocolError("protocol block order differs from the seeded permutations")
    if document["repeat"]["max_full_permutation_repeats"] != 1:
        raise ProtocolError("repeat budget must stay one full permutation")
    if document["repeat"]["repeat_one_path"] is not False:
        raise ProtocolError("a single path must not be repeated")
    if document["historical_host_refusal_not_an_admission"]["reusable"] is not False:
        raise ProtocolError("the recorded host refusal is not an admission")
    if document["precision_target_role"] != "candidate_planning_half_width":
        raise ProtocolError("0.50 ms is a planning target, not an equivalence margin")


def validate_document(document: dict) -> None:
    """Validate the preserved 1.0.0 candidate. It is not the execution protocol."""
    _common(document)
    if document["protocol_version"] != PROTOCOL_VERSION:
        raise ProtocolError("protocol_version mismatch")
    if document["admission"] != ADMISSION_V1:
        raise ProtocolError("admission criteria differ from the recorded thresholds")
    if document["monitor"] != MONITOR_V1:
        raise ProtocolError("monitor limits differ from the recorded limits")


def amended_document() -> dict:
    """Shared-host amendment. Load average does not refuse the host or a block."""
    document = candidate_document()
    document["protocol_version"] = AMENDMENT_VERSION
    document["amends_sha256"] = PRESERVED_SHA256
    document["amendment"] = {
        "id": "shared-host-load-diagnostic",
        "scenario": "this machine in a shared environment",
        "withdrawn": ["admission.max_load1 as an automatic host refusal"],
        "load_average": "diagnostic_only",
        "blocks_not_excluded_for": ["high_load", "unfavorable_latency"],
        "safety_unchanged": ["memory", "disk"],
    }
    document["admission"] = json.loads(json.dumps(ADMISSION))
    document["monitor"] = json.loads(json.dumps(MONITOR))
    document["historical_host_refusal_not_an_admission"] = {
        "sample_start": "2026-11-05T22:14:56Z",
        "sample_end": "2026-11-05T22:15:56Z",
        "reason": "load1",
        "load1_min": 10.46,
        "load1_max": 11.33,
        "threshold": 2.0,
        "observation_preserved": True,
        "automatic_refusal_withdrawn": True,
        "stands": False,
        "reusable": False,
    }
    return document


def validate_for_start(document: dict) -> None:
    """The only protocol a final start may use. It does not authorize the run."""
    _common(document)
    if document["protocol_version"] != AMENDMENT_VERSION:
        raise ProtocolError("phase final requires amendment 1.1.0; 1.0.0 is preserved and is not the start protocol")
    if document.get("amends_sha256") != PRESERVED_SHA256:
        raise ProtocolError("amendment must name the preserved protocol hash")
    if document.get("amendment", {}).get("scenario") != "this machine in a shared environment":
        raise ProtocolError("amendment scenario must be this shared machine")
    if "max_load1" in document["admission"]:
        raise ProtocolError("load1 must not be an admission threshold")
    if document["admission"] != ADMISSION:
        raise ProtocolError("admission criteria differ from the shared-host amendment")
    if document["monitor"] != MONITOR:
        raise ProtocolError("monitor limits differ from the shared-host amendment")
    if document["monitor"].get("load_average_excludes_block") is not False:
        raise ProtocolError("high load must not exclude a block")
    if document["monitor"].get("latency_excludes_block") is not False:
        raise ProtocolError("unfavorable latency must not exclude a block")
    if document["historical_host_refusal_not_an_admission"].get("automatic_refusal_withdrawn") is not True:
        raise ProtocolError("the load1 refusal must be withdrawn by the amendment")


def argument_mismatches(document: dict, args) -> list[str]:
    checks = {
        "phase": args.phase,
        "blocks": args.blocks,
        "permutation_repeats": args.permutation_repeats,
        "seed": args.seed,
        "modes": list(args.modes),
        "concurrency": list(args.concurrency),
        "warmup_s": args.warmup_s,
        "measure_s": args.measure_s,
        "backend_latency_ms": args.backend_latency_ms,
        "order": args.order,
        "model": args.model,
        "experiment_name": args.experiment_name,
    }
    found = []
    for name, value in checks.items():
        if document[name] != value:
            found.append(name)
    if args.protocol_sha256 and args.protocol_sha256 != document.get("_sha256"):
        found.append("protocol_sha256")
    return found


def bind_hash(document: dict, digest: str) -> dict:
    bound = dict(document)
    bound["_sha256"] = digest
    return bound


def plan_fields(document: dict, digest: str) -> dict:
    cells = document["blocks"] * len(document["modes"]) * len(document["concurrency"]) * 3
    per_cell = document["warmup_s"] + document["measure_s"]
    load = cells * per_cell
    overhead = round(SWITCH_S_PER_BLOCK * document["blocks"], 3)
    return {
        "phase": "final",
        "protocol_sha256": digest,
        "protocol_version": document["protocol_version"],
        "cells": cells,
        "load_seconds": load,
        "switch_overhead_s_extrapolated": overhead,
        "switch_overhead_basis": SWITCH_SOURCE,
        "overhead_included_in_load_seconds": False,
        "melis_image_id": document["melis_image_id"],
        "image_build_commit": document["image_build_commit"],
        "image_checked_this_process": False,
        "admission_ran": False,
        "load_started": False,
        "execution_authorized_by_protocol": False,
        "confirmatory_claim": False,
        "note": (
            "Configured load is cells times the 20 s warmup and 60 s measure. "
            "Switch overhead is extrapolated from the variance pilot and is "
            "not included in load_seconds. Duration mode does not stop at "
            "max_requests."
        ),
    }


def start_blockers(args, git_state: dict, directory_exists: bool, image_reuse: dict) -> list[str]:
    """Reasons a real start must stop. Dry-run reports these and does not start."""
    found = []
    if directory_exists:
        found.append("output_directory_exists")
    if not getattr(args, "confirm_operational_start", False):
        found.append("operational_confirmation_missing")
    if not git_state.get("clean"):
        found.append("dirty_tree")
    elif not image_reuse.get("ok"):
        found.append(image_reuse.get("reason") or "image_reuse_refused")
    return found


def image_reuse(execution_clean: bool, head: str, numstat: str | None) -> dict:
    """A later harness commit may reuse the local image when build inputs match.

    The new commit is not written back onto the image.
    """
    if not execution_clean:
        return {"ok": False, "reason": "dirty_tree", "image_labeled_with_harness_commit": False}
    if head == IMAGE_BUILD_COMMIT:
        return {
            "ok": True,
            "reason": "same_checkout_as_build",
            "image_source_commit": IMAGE_BUILD_COMMIT,
            "harness_commit": head,
            "image_labeled_with_harness_commit": False,
        }
    compared = provenance.equivalence_from_numstat(
        IMAGE_BUILD_COMMIT, head, numstat, provenance.BUILD_INPUTS,
    )
    if not compared.get("compared") or not compared.get("identical"):
        return {
            "ok": False,
            "reason": "build_inputs_differ",
            "image_labeled_with_harness_commit": False,
            "comparison": compared,
        }
    return {
        "ok": True,
        "reason": "build_inputs_unchanged",
        "image_source_commit": IMAGE_BUILD_COMMIT,
        "harness_commit": head,
        "image_labeled_with_harness_commit": False,
        "comparison": compared,
    }


def require_container_image(expected_id: str, observed_id: str) -> None:
    """The container Image field must be the protocol's local id.

    A mutable tag or a different id is not accepted. The local id is not
    turned into a registry digest.
    """
    if not isinstance(observed_id, str) or not observed_id.startswith("sha256:"):
        raise ProtocolError(
            f"container image {observed_id!r} is not a local image id"
        )
    if observed_id != expected_id:
        raise ProtocolError(
            "container image does not match the protocol local id: "
            f"{observed_id} != {expected_id}"
        )


def reference_is_proof(reference: str, expected_id: str) -> bool:
    """A compose image reference proves the id only when it is that id."""
    return isinstance(reference, str) and reference == expected_id


def refuse_existing_directory(path: str) -> None:
    if path and os.path.exists(path):
        raise ProtocolError(f"refusing existing output directory {path}")
