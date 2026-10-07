#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

from __future__ import annotations

import copy
import importlib.util
import json
import os
import pathlib
import subprocess
import tempfile
import unittest
from unittest import mock


ROOT = pathlib.Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("release_recovery", ROOT / "scripts/resolve-release-recovery.py")
RECOVERY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RECOVERY)
REPOSITORY = "cognica-io/uqa-engine"
TAG = "v0.5.1"
COMMIT = "a" * 40


def fixture():
    run = {"id": 123, "run_attempt": 1, "status": "completed", "conclusion": "failure",
           "event": "push", "path": ".github/workflows/release.yml",
           "head_branch": TAG, "head_sha": COMMIT,
           "head_repository": {"full_name": REPOSITORY}}
    jobs = [{"name": name, "conclusion": "success"} for name in sorted(RECOVERY.BUILD_JOBS)]
    artifacts = [{"name": name, "expired": False, "workflow_run": {"id": 123, "head_sha": COMMIT}}
                 for name in sorted(RECOVERY.PACKAGE_ARTIFACTS | {f"release-benchmarks-{TAG}-123-1"})]
    return run, jobs, artifacts


class RecoverySourceTest(unittest.TestCase):
    def test_only_complete_successful_original_builds_qualify(self):
        run, jobs, artifacts = fixture()
        RECOVERY.validate(run, TAG, COMMIT, REPOSITORY, jobs, artifacts)
        for index in range(len(jobs)):
            for conclusion in ("failure", "skipped", "cancelled", None):
                with self.subTest(job=jobs[index]["name"], conclusion=conclusion):
                    changed = copy.deepcopy(jobs)
                    changed[index]["conclusion"] = conclusion
                    with self.assertRaisesRegex(ValueError, "must have passed"):
                        RECOVERY.validate(run, TAG, COMMIT, REPOSITORY, changed, artifacts)

    def test_missing_and_new_failed_builds_are_not_omitted(self):
        run, jobs, artifacts = fixture()
        for changed in (jobs[1:], jobs + [{"name": "python / future platform", "conclusion": "failure"}]):
            with self.assertRaises(ValueError):
                RECOVERY.validate(run, TAG, COMMIT, REPOSITORY, changed, artifacts)

    def test_origin_must_match_the_same_repository_tag_and_commit(self):
        run, jobs, artifacts = fixture()
        for key, value in (("status", "in_progress"), ("conclusion", "success"),
                           ("event", "pull_request"), ("path", ".github/workflows/other.yml"),
                           ("head_branch", "main"), ("head_sha", "b" * 40),
                           ("head_repository", {"full_name": "other/repository"})):
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, "does not qualify"):
                RECOVERY.validate({**run, key: value}, TAG, COMMIT, REPOSITORY, jobs, artifacts)

    def test_every_artifact_must_be_unexpired_and_from_the_original_run(self):
        run, jobs, artifacts = fixture()
        for index in range(len(artifacts)):
            for replacement in (None, {**artifacts[index], "expired": True},
                                {**artifacts[index], "workflow_run": {"id": 456, "head_sha": COMMIT}},
                                {**artifacts[index], "workflow_run": {"id": 123, "head_sha": "b" * 40}}):
                changed = artifacts[:index] + artifacts[index + 1:]
                if replacement:
                    changed.append(replacement)
                with self.subTest(artifact=artifacts[index]["name"]), self.assertRaisesRegex(ValueError, "artifacts"):
                    RECOVERY.validate(run, TAG, COMMIT, REPOSITORY, jobs, changed)

    def test_inventory_pagination_keeps_late_failures(self):
        with mock.patch.object(RECOVERY, "api", side_effect=[
            {"total_count": 2, "jobs": [{"conclusion": "success"}]},
            {"total_count": 2, "jobs": [{"conclusion": "failure"}]},
        ]):
            self.assertEqual(RECOVERY.pages("jobs", "jobs")[-1]["conclusion"], "failure")
        with mock.patch.object(RECOVERY, "api", return_value={"total_count": 1, "jobs": []}):
            with self.assertRaisesRegex(RuntimeError, "incomplete"):
                RECOVERY.pages("jobs", "jobs")

    def test_main_resolves_the_immutable_annotated_tag_and_keeps_original_artifacts(self):
        run, jobs, artifacts = fixture()
        with tempfile.TemporaryDirectory() as temporary:
            output = pathlib.Path(temporary) / "output"
            with mock.patch.dict(os.environ, {"GITHUB_REPOSITORY": REPOSITORY, "GITHUB_OUTPUT": str(output)}), \
                 mock.patch.object(RECOVERY.pathlib.Path, "read_text", return_value='version = "0.5.1"\n'), \
                 mock.patch.object(RECOVERY, "publication_complete", return_value=False), \
                 mock.patch.object(RECOVERY, "pages", side_effect=[jobs, artifacts]), \
                 mock.patch.object(RECOVERY, "api", side_effect=[
                     {"workflow_runs": [run]}, {"object": {"type": "tag", "sha": "b" * 40}},
                     {"object": {"type": "commit", "sha": COMMIT}},
                 ]) as api:
                RECOVERY.main()
            self.assertEqual(output.read_text(), f"ready=true\ntag={TAG}\ncommit={COMMIT}\nrun_id=123\n")
            self.assertIn(f"/git/tags/{'b' * 40}", api.call_args.args[0])

    def test_active_successful_absent_or_already_recovered_runs_do_not_start_publication(self):
        run, _, _ = fixture()
        for runs, completed in (([], False), ([{**run, "status": "in_progress"}], False),
                                ([{**run, "conclusion": "success"}], False), ([run], True)):
            with tempfile.TemporaryDirectory() as temporary:
                output = pathlib.Path(temporary) / "output"
                with mock.patch.dict(os.environ, {"GITHUB_REPOSITORY": REPOSITORY, "GITHUB_OUTPUT": str(output)}), \
                     mock.patch.object(RECOVERY.pathlib.Path, "read_text", return_value='version = "0.5.1"\n'), \
                     mock.patch.object(RECOVERY, "api", return_value={"workflow_runs": runs}) as api, \
                     mock.patch.object(RECOVERY, "publication_complete", return_value=completed):
                    RECOVERY.main()
                self.assertFalse(output.exists())
                api.assert_called_once()


class CompletedPublicationTest(unittest.TestCase):
    def test_marker_requires_a_successful_publication_workflow_in_the_same_repository(self):
        release = {"body": f"notes\n<!-- uqa-release-publication:456:{TAG}:123 -->"}
        publication = {"status": "completed", "conclusion": "success",
                       "path": ".github/workflows/release-recovery.yml",
                       "head_repository": {"full_name": REPOSITORY}}
        response = subprocess.CompletedProcess([], 0, json.dumps(release), "")
        with mock.patch.object(RECOVERY.subprocess, "run", return_value=response), \
             mock.patch.object(RECOVERY, "api", return_value=publication):
            self.assertTrue(RECOVERY.publication_complete(REPOSITORY, TAG, 123))
            self.assertFalse(RECOVERY.publication_complete(REPOSITORY, TAG, 999))
            self.assertFalse(RECOVERY.publication_complete(REPOSITORY, "v0.5.2", 123))
        for key, value in (("status", "in_progress"), ("conclusion", "failure"),
                           ("path", ".github/workflows/other.yml"),
                           ("head_repository", {"full_name": "other/repository"})):
            with self.subTest(key=key), mock.patch.object(RECOVERY.subprocess, "run", return_value=response), \
                 mock.patch.object(RECOVERY, "api", return_value={**publication, key: value}):
                self.assertFalse(RECOVERY.publication_complete(REPOSITORY, TAG, 123))

    def test_only_not_found_is_an_absent_release(self):
        for status in (404, 403, 429, 500):
            response = subprocess.CompletedProcess([], 1, json.dumps({"status": str(status)}), "registry error")
            with self.subTest(status=status), mock.patch.object(RECOVERY.subprocess, "run", return_value=response):
                if status == 404:
                    self.assertFalse(RECOVERY.publication_complete(REPOSITORY, TAG, 123))
                else:
                    with self.assertRaises(RuntimeError):
                        RECOVERY.publication_complete(REPOSITORY, TAG, 123)


if __name__ == "__main__":
    unittest.main()
