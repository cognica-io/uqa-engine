#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

from __future__ import annotations

import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("c_validation", ROOT / "scripts/c_validation.py")
SCOPE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SCOPE)


def workflow(kind, *, selection=False):
    jobs = "consumers native-sanitizers" if kind == "bindings" else "package"
    result = "name: C validation\non:\n  pull_request:\n    types: [ready_for_review]\npermissions:\n  contents: read\nenv:\n  CARGO_INCREMENTAL: '0'\njobs:\n"
    if selection:
        result += "  scope:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: python3 scripts/c_validation.py\n\n"
    for name in jobs.split():
        result += "  " + name + ":\n"
        if selection:
            result += "    needs: scope\n    if: needs.scope.outputs.required == 'true'\n"
        result += "    runs-on: ubuntu-24.04\n    strategy:\n      matrix:\n        features: [minimal, default]\n    steps:\n      - run: cargo test --locked\n\n"
    return result.encode()


class OwnershipTests(unittest.TestCase):
    def test_engine_runtime_and_rust_tests_do_not_schedule_c_matrices(self):
        for kind in SCOPE.WORKFLOWS:
            for path in ("crates/uqa-analysis/src/tokenizer/stream.rs",
                         "crates/uqa-storage/src/mvcc/overlay.rs",
                         "crates/uqa-storage-sqlite/src/inverted_index/native.rs",
                         "crates/uqa-engine/src/session.rs",
                         "crates/uqa-core/src/value/tests.rs",
                         "crates/uqa-core/src/value/tests/rows.rs",
                         "scripts/c_validation.py", "scripts/tests/test_c_validation.py",
                         "HISTORY.md", "docs/design/c-bindings.md"):
                with self.subTest(kind=kind, path=path):
                    self.assertFalse(SCOPE.affected(kind, path))

    def test_shared_abi_and_build_inputs_schedule_both_surfaces(self):
        for kind in SCOPE.WORKFLOWS:
            for path in ("crates/uqa-ffi/src/engine.rs", "crates/uqa-ffi/include/uqa/uqa.h",
                         "crates/uqa-ffi/src/tests/allocator.rs", "examples/c/transactions.c",
                         "Cargo.toml", "Cargo.lock", "build.rs", "rust-toolchain", "rust-toolchain.toml",
                         "crates/uqa-pg-query/build.rs", "crates/uqa-sql/build.rs",
                         "crates/uqa-storage/Cargo.toml", ".cargo/config.toml"):
                with self.subTest(kind=kind, path=path):
                    self.assertTrue(SCOPE.affected(kind, path))

    def test_c_values_arrow_and_sanitizers_are_binding_inputs(self):
        for path in ("crates/uqa-core/src/value.rs", "crates/uqa-api/src/arrow/arrays.rs",
                     "crates/uqa-storage/src/test_support/fixtures.rs",
                     "crates/uqa-storage/src/test_support/tests.rs",
                     "crates/uqa-storage/src/test_support/tests/fixtures.rs",
                     "scripts/verify-c-clients.py", ".github/scripts/c-sanitizer-cc.sh"):
            with self.subTest(path=path):
                self.assertTrue(SCOPE.affected("bindings", path))
                self.assertFalse(SCOPE.affected("packages", path))

    def test_distribution_and_license_inputs_schedule_packages(self):
        for path in ("scripts/build-c-bindings.py", "scripts/c-bindings-metadata.py",
                     "scripts/c-bindings-release.py", "scripts/c_bindings_licenses.py",
                     "scripts/c-bindings/licenses/sources.json", "scripts/verify-c-package.py",
                     "scripts/tests/test_c_bindings_package.py", "LICENSE", "NOTICE"):
            with self.subTest(path=path):
                self.assertTrue(SCOPE.affected("packages", path))
                self.assertFalse(SCOPE.affected("bindings", path))

    def test_selection_wiring_does_not_change_execution_contract(self):
        for kind in SCOPE.WORKFLOWS:
            self.assertEqual(SCOPE.execution_definition(kind, workflow(kind)),
                             SCOPE.execution_definition(kind, workflow(kind, selection=True)))

    def test_commands_features_and_environments_remain_execution_inputs(self):
        for kind in SCOPE.WORKFLOWS:
            original = workflow(kind)
            expected = SCOPE.execution_definition(kind, original)
            for before, after in ((b"cargo test --locked", b"cargo test --release --locked"),
                                  (b"[minimal, default]", b"[minimal, arrow]"),
                                  (b"CARGO_INCREMENTAL: '0'", b"CARGO_INCREMENTAL: '1'"),
                                  (b"ubuntu-24.04", b"macos-14")):
                with self.subTest(kind=kind, change=after):
                    self.assertNotEqual(expected, SCOPE.execution_definition(kind, original.replace(before, after)))

    def test_unknown_workflow_shape_cannot_claim_equivalent_execution(self):
        for contents in (b"", b"name: incomplete\non:\n", b"name: missing jobs\non:\n  push:\npermissions:\n  contents: read\n"):
            with self.assertRaises(ValueError):
                SCOPE.execution_definition("bindings", contents)
        with self.assertRaises(ValueError):
            SCOPE.execution_definition("bindings", workflow("bindings").replace(b"  consumers:", b"  renamed:"))


class GitSelectionTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.git("init", "-q")
        self.git("config", "user.name", "Fixture")
        self.git("config", "user.email", "fixture@example.invalid")
        for kind, path in SCOPE.WORKFLOWS.items():
            self.write(path, workflow(kind))
        self.write("README.md", b"original\n")
        self.base = self.commit()

    def tearDown(self):
        self.temporary.cleanup()

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.root, stderr=subprocess.PIPE).decode().strip()

    def write(self, name, contents):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(contents)

    def commit(self):
        self.git("add", "-A")
        self.git("commit", "-qm", "Fixture update")
        return self.git("rev-parse", "HEAD")

    def event(self, *, draft=False, action="ready_for_review"):
        return {"action": action, "pull_request": {"draft": draft, "base": {"sha": self.base}}}

    def select(self, kind="bindings", event=None, event_name="pull_request", **kwargs):
        return SCOPE.select(self.root, kind, self.event() if event is None else event,
                            event_name, self.git("rev-parse", "HEAD"), **kwargs)

    def test_runtime_only_ready_candidate_skips_both_matrices(self):
        self.write("crates/uqa-storage/src/mvcc/overlay.rs", b"runtime\n")
        self.commit()
        for kind in SCOPE.WORKFLOWS:
            self.assertFalse(self.select(kind)["required"])

    def test_ready_binding_source_and_main_push_require_validation(self):
        self.write("crates/uqa-ffi/src/value.rs", b"binding\n")
        self.commit()
        for kind in SCOPE.WORKFLOWS:
            self.assertTrue(self.select(kind)["required"])
            self.assertTrue(self.select(kind, {"ref": "refs/heads/main", "before": self.base}, "push")["required"])

    def test_main_runtime_change_uses_same_ownership_selection(self):
        self.write("crates/uqa-engine/src/session.rs", b"runtime\n")
        self.commit()
        for kind in SCOPE.WORKFLOWS:
            self.assertFalse(self.select(kind, {"ref": "refs/heads/main", "before": self.base}, "push")["required"])

    def test_deleted_and_renamed_binding_inputs_cannot_disappear_from_diff(self):
        self.write("crates/uqa-ffi/include/uqa/removed.h", b"header\n")
        self.base = self.commit()
        self.git("mv", "crates/uqa-ffi/include/uqa/removed.h", "moved-header.h")
        self.commit()
        for kind in SCOPE.WORKFLOWS:
            result = self.select(kind)
            self.assertTrue(result["required"])
            self.assertIn("crates/uqa-ffi/include/uqa/removed.h", result["paths"])

    def test_unknown_missing_and_zero_bases_fail_closed(self):
        for base in (None, "", "0" * 40, "a" * 40, "not-a-revision"):
            event = self.event()
            event["pull_request"]["base"]["sha"] = base
            for kind in SCOPE.WORKFLOWS:
                self.assertTrue(self.select(kind, event)["required"])

    def test_draft_and_non_ready_updates_wait_for_the_completed_batch(self):
        for event in (self.event(draft=True), self.event(action="synchronize"), self.event(action="opened")):
            self.assertFalse(self.select(event=event)["required"])

    def test_explicit_full_calls_and_releases_require_every_configuration(self):
        for kind in SCOPE.WORKFLOWS:
            self.assertTrue(self.select(kind, self.event(draft=True), force_all=True)["required"])
            self.assertTrue(self.select(kind, {}, "workflow_dispatch")["required"])
            self.assertTrue(self.select(kind, {"ref": "refs/tags/v1.2.3", "before": self.base}, "push")["required"])

    def test_policy_only_wiring_changes_run_policy_checks_without_recompiling_c(self):
        for kind, path in SCOPE.WORKFLOWS.items():
            self.write(path, workflow(kind, selection=True))
        self.write("scripts/c_validation.py", b"selection\n")
        self.commit()
        for kind in SCOPE.WORKFLOWS:
            self.assertFalse(self.select(kind)["required"])

    def test_actual_workflow_execution_changes_select_the_affected_surface(self):
        for kind, path in SCOPE.WORKFLOWS.items():
            with self.subTest(kind=kind):
                self.write(path, workflow(kind).replace(b"cargo test --locked", b"cargo test --release --locked"))
                self.commit()
                self.assertTrue(self.select(kind)["required"])

    def test_missing_workflow_history_requires_validation(self):
        self.write(SCOPE.WORKFLOWS["bindings"], b"unrecognized workflow\n")
        self.commit()
        self.assertTrue(self.select()["required"])


class WorkflowContractTests(unittest.TestCase):
    def test_scopes_gate_all_heavy_jobs_and_full_callers_are_preserved(self):
        for kind, path in SCOPE.WORKFLOWS.items():
            source = (ROOT / path).read_text()
            self.assertIn("force_all:\n", source)
            self.assertIn("        default: true\n", source)
            self.assertIn("C_VALIDATION_FORCE_ALL: ${{ inputs.force_all }}", source)
            self.assertIn("run: python3 scripts/c_validation.py --kind " + kind, source)
            self.assertIn("-p test_c_validation.py", source)
            self.assertEqual(source.count("    needs: scope\n"), len(SCOPE.JOBS[kind]))
            self.assertEqual(source.count("    if: needs.scope.outputs.required == 'true'\n"), len(SCOPE.JOBS[kind]))
            self.assertNotIn("    paths:\n", source)
            SCOPE.execution_definition(kind, source.encode())


if __name__ == "__main__":
    unittest.main()
