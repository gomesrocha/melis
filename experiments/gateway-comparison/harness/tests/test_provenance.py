#!/usr/bin/env python3
"""Synthetic fixtures for provenance. These records are not a real collection."""
from __future__ import annotations

import json
import os
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

import precision  # noqa: E402
import provenance  # noqa: E402


class TestImageAndBuild(unittest.TestCase):
    def test_missing_label_is_null_and_id_is_not_a_digest(self):
        meta = {
            "Id": "sha256:local",
            "RepoDigests": [],
            "Architecture": "amd64",
            "Os": "linux",
            "Created": "2026-11-05T20:45:29Z",
            "Config": {},
        }
        identity = provenance.image_identity_from_inspect(meta)
        self.assertEqual(identity["image_id"], "sha256:local")
        self.assertEqual(identity["repo_digests"], [])
        self.assertIsNone(identity["labels"])
        self.assertIsNone(identity["label_revision"])
        self.assertNotEqual(identity["image_id"], identity["repo_digests"])

    def test_label_revision_is_copied_and_not_replaced_by_head(self):
        meta = {
            "Id": "sha256:local",
            "RepoDigests": ["registry@sha256:abc"],
            "Config": {"Labels": {provenance.LABEL_REVISION: "aaa"}},
        }
        identity = provenance.image_identity_from_inspect(meta)
        built = provenance.build_source(
            invoked_build=True, execution_clean=True,
            execution_commit="bbb", label_revision=identity["label_revision"],
        )
        self.assertEqual(built["source_commit"], "aaa")
        self.assertEqual(built["evidence_kind"], "image_label")
        self.assertNotEqual(built["source_commit"], "bbb")

    def test_clean_invoked_build_uses_the_execution_commit_only_if_the_image_is_new(self):
        built = provenance.build_source(
            invoked_build=True, execution_clean=True,
            execution_commit="bbb", label_revision=None,
            image_created="2026-11-05T20:50:00Z",
            process_started="2026-11-05T20:45:00Z",
        )
        self.assertEqual(built["evidence_kind"], "invoked_from_clean_checkout")
        self.assertEqual(built["source_commit"], "bbb")

    def test_cached_image_does_not_inherit_the_execution_commit(self):
        built = provenance.build_source(
            invoked_build=True, execution_clean=True,
            execution_commit="86dc5ce7", label_revision=None,
            image_created="2026-11-05T20:45:29Z",
            process_started="2026-11-05T20:48:16Z",
        )
        self.assertIsNone(built["source_commit"])
        self.assertEqual(built["evidence_kind"], "none")
        self.assertFalse(built["invoked_during_this_process"])

    def test_no_build_and_no_label_has_no_source_commit(self):
        built = provenance.build_source(
            invoked_build=False, execution_clean=True,
            execution_commit="bbb", label_revision=None,
        )
        self.assertIsNone(built["source_commit"])
        self.assertEqual(built["evidence_kind"], "none")

    def test_dirty_tree_does_not_claim_the_execution_commit(self):
        built = provenance.build_source(
            invoked_build=True, execution_clean=False,
            execution_commit="bbb", label_revision=None,
        )
        self.assertIsNone(built["source_commit"])


class TestSupplement(unittest.TestCase):
    def test_historical_sidecar_keeps_the_manifest_hash_and_refuses_a_bare_commit(self):
        manifest = {"artifact": {"commit": "old", "clean": False}}
        raw = json.dumps(manifest, sort_keys=True).encode()
        digest = __import__("hashlib").sha256(raw).hexdigest()
        record = provenance.historical_supplement(
            collection_id="col-synthetic",
            original_manifest_sha256=digest,
            execution={"commit": "exec", "clean": True, "dirty_files": []},
            images={"melis": {"image_id": "sha256:local", "repo_digests": []}},
            build=provenance.build_source(
                invoked_build=False, execution_clean=True,
                execution_commit="exec", label_revision=None,
            ),
            harness_hashes={"runner": "abc"},
            compose_files={"deterministic": "def"},
            config_hashes={"melis": "123"},
            equivalence=provenance.equivalence_from_numstat(
                "parent", "exec", "", provenance.BUILD_INPUTS,
            ),
            notes=["synthetic fixture"],
        )
        self.assertEqual(record["collection_id"], "col-synthetic")
        self.assertEqual(record["original_manifest_sha256"], digest)
        self.assertFalse(record["replaces_manifest"])
        self.assertIsNone(record["image_build"]["source_commit"])
        self.assertTrue(record["file_equivalence"]["identical"])
        with self.assertRaises(ValueError):
            provenance.assemble_provenance(
                collection_id="col-synthetic",
                execution={"commit": "exec", "clean": True},
                images={},
                build={"source_commit": "invented", "evidence_kind": "none"},
                harness_hashes={}, compose_files={}, config_hashes={},
            )

    def test_unknown_diff_is_not_treated_as_identical(self):
        record = provenance.equivalence_from_numstat("a", "b", None, ("src",))
        self.assertFalse(record["compared"])
        self.assertIsNone(record["paths"][0]["identical"])

    def test_supplement_does_not_rewrite_the_manifest_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            manifest_path = os.path.join(tmp, "manifest.json")
            with open(manifest_path, "w", encoding="utf-8") as fh:
                fh.write('{"keep": true}\n')
            with open(manifest_path, "rb") as fh:
                before = fh.read()
            digest = __import__("hashlib").sha256(before).hexdigest()
            record = provenance.historical_supplement(
                collection_id="col-file",
                original_manifest_sha256=digest,
                execution={"commit": "exec", "clean": True, "dirty_files": []},
                images={},
                build={"source_commit": None, "evidence_kind": "none",
                       "invoked_during_this_process": False},
                harness_hashes={}, compose_files={}, config_hashes={},
            )
            provenance.write_json(record, os.path.join(tmp, "provenance.json"))
            with open(manifest_path, "rb") as fh:
                self.assertEqual(fh.read(), before)


class TestPrecision(unittest.TestCase):
    def test_known_quantiles(self):
        self.assertAlmostEqual(precision.t_ppf(0.975, 11), 2.200985160082949, places=3)
        self.assertAlmostEqual(precision.chi2_ppf(0.05, 11), 4.574813079322224, places=2)

    def test_planning_grid_balances_permutations_and_does_not_approve(self):
        rows = precision.planning_rows(
            contrast="melis_minus_direct", sample_sd_ms=0.1,
            pilot_blocks=12, observed_switch_s_per_block=26.0,
        )
        self.assertEqual([row["n_blocks"] for row in rows], [12, 18, 24, 30])
        self.assertTrue(all(row["permutation_repeats"] * 6 == row["n_blocks"] for row in rows))
        self.assertTrue(all(row["approved"] is False for row in rows))
        # The upper-sigma width is wider than the plug-in width.
        self.assertGreater(rows[0]["half_width_sigma_upper_ms"], rows[0]["half_width_plugin_ms"])
        self.assertEqual(rows[0]["configured_load_s"], 12 * 3 * 80)


if __name__ == "__main__":
    unittest.main()
