#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from fdw_ci import affected, fingerprint, proof, select, validate_proof


class ForeignHandlerScopeTests(unittest.TestCase):
    def test_owner_adapters_shared_values_and_feature_inputs_are_selected(self):
        for path in (
            "crates/uqa-fdw/src/duckdb.rs", "crates/uqa-fdw/src/arrow_ipc.rs",
            "crates/uqa-fdw/tests/memory_handler.rs", "crates/uqa-core/src/types/value.rs",
            "crates/uqa-engine/src/fdw.rs",
            "crates/uqa-engine/src/capabilities/foreign_creation.rs",
            "crates/uqa-engine/src/tests/catalog/foreign_tables/runtime.rs",
            "crates/uqa-execution/src/catalog/foreign/restoration.rs",
            "crates/uqa-execution/src/catalog/security/foreign_authorization.rs",
            "crates/uqa-execution/src/schema/foreign_server_removal/targets.rs",
            "crates/uqa-execution/src/schema/foreign_wrapper_removal.rs",
            "crates/uqa-engine/src/capabilities/foreign_catalog.rs",
            "crates/uqa-engine/src/capabilities/foreign_table_alteration.rs",
            "crates/uqa-sql/src/compiler/foreign_servers.rs",
            "crates/uqa-sql/src/catalog/foreign_wrapper/tests.rs",
            "crates/uqa-sql/src/catalog/foreign_relations.rs",
            "crates/uqa-storage-sqlite/src/mvcc/native/foreign_servers.rs",
            "crates/uqa-storage-redb/tests/store_contract/foreign_servers.rs",
            "crates/uqa-execution/src/query/block/foreign/predicates.rs",
            "crates/uqa-execution/src/schema/foreign_definitions/migration.rs",
            "crates/uqa-sql/src/ast/ranges.rs", "crates/uqa-sql/src/compiler/foreign_tables.rs",
            "crates/uqa-sql/src/schema/foreign_tables/dependencies.rs",
            "crates/uqa-engine/tests/sql_foreign_ddl/availability.rs",
            "crates/uqa-engine/tests/sql_information_schema/foreign_table_ownership.rs",
            "Cargo.lock", "Cargo.toml", "crates/uqa-node/Cargo.toml",
            "rust-toolchain.toml", ".cargo/config.toml", "scripts/check-fdw-feature-isolation.py",
            ".github/workflows/fdw-features.yml",
        ):
            with self.subTest(path=path):
                self.assertTrue(affected(path))

    def test_unrelated_crates_range_input_and_progress_do_not_select_six_builds(self):
        for path in (
            "crates/uqa-sql/src/expr/range/input.rs",
            "crates/uqa-sql/src/expr/scalar_math.rs",
            "crates/uqa-execution/src/mutation/referential/references.rs",
            "crates/uqa-sql/src/schema/foreign_keys.rs",
            "crates/uqa-sql/src/schema/constraint_changes/foreign_key_target/tests.rs",
            "crates/uqa-engine/tests/pg18_semantics/range_input.rs",
            "crates/uqa-analysis/src/nori.rs", "crates/uqa-scoring/src/bm25.rs",
            "crates/uqa-storage-redb/src/lib.rs", "crates/uqa-graph/src/lib.rs",
            "docs/plans/0018-extension-progress.md", "HISTORY.md", "README.md",
            "scripts/fdw_ci.py", "scripts/tests/test_fdw_ci.py",
        ):
            with self.subTest(path=path):
                self.assertFalse(affected(path))


class ForeignHandlerHistoryTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.name", "CI fixture")
        self.git("config", "user.email", "ci@example.invalid")
        self.base = self.commit("crates/uqa-fdw/src/lib.rs", "initial\n")

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.root, stderr=subprocess.PIPE).decode().strip()

    def commit(self, path, content):
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content, encoding="utf-8")
        self.git("add", ".")
        self.git("-c", "commit.gpgsign=false", "commit", "-qm", "Update fixture")
        return self.git("rev-parse", "HEAD")

    def pr(self, base, revision):
        return select(self.root, {"action": "ready_for_review", "pull_request": {
            "draft": False, "base": {"sha": base}}}, "pull_request", revision)

    def test_product_pushes_wait_until_the_final_ready_transition(self):
        first = self.commit("crates/uqa-fdw/src/lib.rs", "first correction\n")
        final = self.commit("crates/uqa-fdw/src/lib.rs", "reviewed correction\n")
        for revision in (first, final):
            for action in ("opened", "synchronize", "reopened", "edited", "labeled"):
                for draft in (True, False):
                    with self.subTest(action=action, draft=draft, revision=revision):
                        event = {"action": action, "pull_request": {
                            "draft": draft, "base": {"sha": self.base}}}
                        self.assertFalse(select(self.root, event, "pull_request", revision)["required"])
        accepted = self.pr(self.base, final)
        self.assertTrue(accepted["required"])
        self.assertEqual(accepted["fingerprint"], fingerprint(self.root, final))
        self.assertNotEqual(accepted["fingerprint"], fingerprint(self.root, first))

    def test_a_draft_does_not_admit_validation_even_with_a_ready_event(self):
        revision = self.commit("crates/uqa-fdw/src/lib.rs", "unfinished\n")
        event = {"action": "ready_for_review", "pull_request": {
            "draft": True, "base": {"sha": self.base}}}
        self.assertFalse(select(self.root, event, "pull_request", revision)["required"])

    def test_event_only_changes_do_not_repeat_unchanged_feature_commands(self):
        path = ".github/workflows/fdw-features.yml"
        workflow = "name: Foreign handlers\non:\n  pull_request:\n\npermissions:\n  contents: read\nenv:\n  CARGO_INCREMENTAL: '0'\njobs:\n  features:\n    run: cargo test -p uqa-fdw\n"
        base = self.commit(path, workflow)
        ready = workflow.replace("  pull_request:\n", "  pull_request:\n    types: [ready_for_review]\n")
        revision = self.commit(path, ready)
        self.assertFalse(self.pr(base, revision)["required"])
        revision = self.commit("scripts/fdw_ci.py", "selector policy\n")
        self.assertFalse(self.pr(base, revision)["required"])
        revision = self.commit("crates/uqa-fdw/src/lib.rs", "product correction\n")
        self.assertEqual(self.pr(base, revision)["paths"], ["crates/uqa-fdw/src/lib.rs"])

    def test_commands_environment_and_unreadable_workflows_still_require_validation(self):
        path = ".github/workflows/fdw-features.yml"
        workflow = "name: Foreign handlers\non:\n  pull_request:\npermissions:\n  contents: read\nenv:\n  CARGO_INCREMENTAL: '0'\njobs:\n  features:\n    run: cargo test -p uqa-fdw\n"
        base = self.commit(path, workflow)
        for changed in (workflow.replace("cargo test", "cargo clippy"),
                        workflow.replace("CARGO_INCREMENTAL: '0'", "CARGO_INCREMENTAL: '1'"),
                        workflow.replace("on:", "'on':"), ""):
            with self.subTest(changed=changed):
                revision = self.commit(path, changed)
                self.assertTrue(self.pr(base, revision)["required"])

    def test_actual_readiness_change_keeps_the_previous_matrix_definition(self):
        root = Path(__file__).resolve().parents[2]
        current = (root / ".github/workflows/fdw-features.yml").read_text()
        previous = current.replace("    types: [ready_for_review]\n", "")
        base = self.commit(".github/workflows/fdw-features.yml", previous)
        revision = self.commit(".github/workflows/fdw-features.yml", current)
        self.assertFalse(self.pr(base, revision)["required"])

    def test_pull_request_retains_earlier_product_change_after_documentation_commit(self):
        product = self.commit("crates/uqa-fdw/src/lib.rs", "corrected\n")
        docs = self.commit("docs/plans/progress.md", "qualified\n")
        result = self.pr(self.base, docs)
        self.assertTrue(result["required"])
        self.assertEqual(result["paths"], ["crates/uqa-fdw/src/lib.rs"])
        self.assertEqual(result["fingerprint"], fingerprint(self.root, product))

    def test_unrelated_source_does_not_start_foreign_matrix_on_pr_or_main(self):
        revision = self.commit("crates/uqa-sql/src/expr/range/input.rs", "range\n")
        self.assertFalse(self.pr(self.base, revision)["required"])
        self.assertFalse(select(self.root, {"before": self.base}, "push", revision)["required"])

    def test_deletion_and_rename_outside_owner_still_select_foreign_tests(self):
        self.git("mv", "crates/uqa-fdw/src/lib.rs", "moved.rs")
        self.git("-c", "commit.gpgsign=false", "commit", "-qm", "Move fixture")
        result = self.pr(self.base, self.git("rev-parse", "HEAD"))
        self.assertTrue(result["required"])
        self.assertIn("crates/uqa-fdw/src/lib.rs", result["paths"])

    def test_merge_tree_comparison_excludes_unrelated_changes_already_in_base(self):
        self.git("checkout", "-qb", "fix/fixture")
        self.commit("docs/plans/progress.md", "report\n")
        self.git("checkout", "main")
        base = self.commit("crates/uqa-core/src/types/value.rs", "new base\n")
        self.git("-c", "commit.gpgsign=false", "merge", "--no-ff", "-qm", "Merge fixture", "fix/fixture")
        self.assertFalse(self.pr(base, self.git("rev-parse", "HEAD"))["required"])

    def test_unknown_deleted_or_unavailable_baseline_keeps_full_validation(self):
        for event, name in (({}, "workflow_dispatch"), ({}, "push"),
                            ({"before": "0" * 40}, "push"),
                            ({"before": "f" * 40}, "push"),
                            ({"before": "--unsafe"}, "push")):
            with self.subTest(event=event):
                self.assertTrue(select(self.root, event, name, self.base)["required"])

    def test_successful_evidence_reuses_only_identical_product_inputs(self):
        digest = fingerprint(self.root, self.base)
        completed = proof(self.root, digest, self.base, "owner/repo", "123")
        for path in ("docs/plans/progress.md", "HISTORY.md", "README.md"):
            current = self.commit(path, "progress\n")
            self.assertEqual(fingerprint(self.root, current), digest)
            validate_proof(completed, fingerprint(self.root, current), "owner/repo")
        for path in ("Cargo.lock", "crates/uqa-fdw/src/lib.rs", "docs/manual/sql/02-ddl.md",
                     "tests/parity/pg18/foreign.json", ".github/workflows/fdw-features.yml"):
            current = self.commit(path, "changed input\n")
            with self.subTest(path=path), self.assertRaises(ValueError):
                validate_proof(completed, fingerprint(self.root, current), "owner/repo")

    def test_incomplete_foreign_run_or_wrong_repository_never_becomes_evidence(self):
        digest = fingerprint(self.root, self.base)
        completed = proof(self.root, digest, self.base, "owner/repo", "123")
        for replacement in ({"platforms": ["ubuntu-24.04"]}, {"configurations": ["memory"]},
                            {"run_id": "0"}, {"revision": "missing"}, {"repository": "other/repo"}):
            with self.subTest(replacement=replacement), self.assertRaises(ValueError):
                validate_proof({**completed, **replacement}, digest, "owner/repo")
        with self.assertRaises(ValueError):
            proof(self.root, "wrong fingerprint", self.base, "owner/repo", "123")


class ForeignHandlerWorkflowTests(unittest.TestCase):
    def test_only_readiness_starts_the_pr_matrix_and_main_does_not_repeat_it(self):
        root = Path(__file__).resolve().parents[2]
        workflow = (root / ".github/workflows/fdw-features.yml").read_text()
        self.assertEqual(workflow.split("on:\n", 1)[1].split("\npermissions:", 1)[0].strip(),
                         "pull_request:\n    types: [ready_for_review]\n  workflow_call:")
        main = (root / ".github/workflows/ci.yml").read_text()
        self.assertNotIn("uses: ./.github/workflows/fdw-features.yml", main)
        self.assertNotIn("test-fdw-linux", main)
        full = (root / ".github/workflows/full-ci.yml").read_text()
        self.assertIn("uses: ./.github/workflows/fdw-features.yml", full)

    def test_reuse_is_exact_and_evidence_is_saved_only_after_the_complete_matrix(self):
        workflow = (Path(__file__).resolve().parents[2] / ".github/workflows/fdw-features.yml").read_text()
        self.assertNotIn('      - "crates/**"', workflow)
        self.assertIn("cancel-in-progress: false", workflow)
        self.assertIn("needs.scope.outputs.required == 'true' && needs.scope.outputs.reused != 'true'", workflow)
        self.assertNotIn("restore-keys:", workflow)
        self.assertIn("os: [ubuntu-24.04, macos-14]", workflow)
        self.assertIn("configuration: [memory, arrow, duckdb]", workflow)
        self.assertIn("fail-fast: false", workflow)
        qualify = workflow.split("  qualify:\n", 1)[1]
        self.assertIn("needs: [scope, features]", qualify)
        self.assertIn("if: needs.features.result == 'success'", qualify)
        self.assertIn("scripts/fdw_ci.py record", qualify)
        self.assertIn("actions/cache/save@v6.1.0", qualify)


if __name__ == "__main__":
    unittest.main()
