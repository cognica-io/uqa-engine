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
from performance_change_scope import (requires_dependency_check, requires_regex_build,
                                      requires_regression_build, requires_revision_build)


class PerformanceChangeScopeTest(unittest.TestCase):
    def test_documentation_and_controlled_host_changes_do_not_rebuild_rust(self):
        self.assertFalse(requires_regression_build([
            "benchmarks/regressions/controlled-runner.md", "docs/plans/README.md",
            "scripts/controlled_runner_host.py", "scripts/run-controlled-performance.py",
            "scripts/tests/test_controlled_runner_build.py", ".github/workflows/performance-checks.yml",
            "scripts/performance_change_scope.py", "scripts/tests/test_performance_change_scope.py",
        ]))

    def test_product_fixtures_and_execution_configuration_still_select_regressions(self):
        for path in ["crates/uqa-engine/src/queries.rs", "crates/uqa-sql/Cargo.toml",
                     "crates/uqa-sql/src/tests/fixture.md", "Cargo.toml", "Cargo.lock", "rust-toolchain.toml",
                     ".config/nextest.toml", ".github/actions/setup-nextest/action.yml",
                     ".github/workflows/performance-platform.yml", "scripts/run-performance-checks.py",
                     "scripts/tests/test_performance_checks.py", "benchmarks/regressions/manifest.json",
                     "benchmarks/regressions/new workload/input.bin"]:
            with self.subTest(path=path):
                self.assertTrue(requires_regression_build(["docs/README.md", path]))

    def test_null_delimited_deleted_or_renamed_inputs_remain_visible(self):
        script = Path(__file__).resolve().parents[1] / "performance_change_scope.py"
        result = subprocess.run([sys.executable, str(script)],
                                input=b"docs/old name.md\0crates/uqa-sql/src/removed file.rs\0",
                                capture_output=True, check=True)
        self.assertEqual(result.stdout, b"required=true\n")
        self.assertFalse(requires_regression_build([]))

    def test_auxiliary_checks_select_only_their_actual_inputs(self):
        for select in [requires_regex_build, requires_dependency_check]:
            self.assertFalse(select(["docs/README.md", "crates/uqa-sql/src/binding.rs",
                                     "scripts/performance_change_scope.py",
                                     ".github/actions/select-validation-inputs/action.yml"]))
            for path in ["Cargo.toml", "Cargo.lock"]:
                with self.subTest(selector=select.__name__, path=path):
                    self.assertTrue(select([path]))
        for path in ["crates/uqa-pg-regex/src/lib.rs", "rust-toolchain.toml",
                     "scripts/emscripten-flags.py", ".github/workflows/regular-expression-checks.yml"]:
            self.assertTrue(requires_regex_build([path]))
        for path in ["crates/uqa-sql/Cargo.toml", "deny.toml",
                     ".github/workflows/dependency-checks.yml"]:
            self.assertTrue(requires_dependency_check([path]))
        self.assertFalse(requires_regex_build(["crates/uqa-sql/Cargo.toml"]))
        self.assertFalse(requires_dependency_check(["crates/uqa-pg-regex/src/lib.rs"]))


class RevisionScopeTest(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.repository = Path(directory.name)
        self.git("init", "--initial-branch=main")
        self.git("config", "user.name", "Regression input test")
        self.git("config", "user.email", "regression@example.invalid")
        self.write("crates/example/src/lib.rs", "original\n")
        self.write("docs/README.md", "original\n")
        self.base = self.commit("Initial product")
        self.git("checkout", "-b", "fix/product")
        self.write("crates/example/src/lib.rs", "changed\n")
        self.before = self.commit("Product change")
        self.write("docs/README.md", "updated documentation\n")
        self.commit("Document completed checks")

    def git(self, *args):
        return subprocess.run(["git", "-C", str(self.repository), *args],
                              capture_output=True, check=True).stdout.decode().strip()

    def write(self, name, contents):
        path = self.repository / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(contents)

    def commit(self, message):
        self.git("add", ".")
        self.git("commit", "-m", message)
        return self.git("rev-parse", "HEAD")

    def required(self, before=True):
        return requires_revision_build(self.base, self.before if before else None,
                                       repository=self.repository)

    def merge_base(self):
        self.base = self.git("rev-parse", "HEAD")
        self.git("checkout", "fix/product")
        self.git("merge", "main", "--no-edit")

    def test_opened_product_pr_builds_but_documentation_update_keeps_prior_run(self):
        self.assertTrue(self.required(before=False))
        self.assertFalse(self.required())

    def test_changed_or_deleted_product_input_requires_new_run(self):
        self.write("crates/example/src/lib.rs", "next change\n")
        self.commit("Change product again")
        self.assertTrue(self.required())
        self.git("rm", "crates/example/src/lib.rs")
        self.commit("Remove product input")
        self.assertTrue(self.required())

    def test_renamed_product_to_documentation_requires_new_run(self):
        self.git("mv", "crates/example/src/lib.rs", "docs/old source.md")
        self.commit("Move source")
        self.assertTrue(self.required())

    def test_controller_only_base_merge_keeps_prior_run(self):
        self.git("checkout", "main")
        self.write("scripts/controlled_runner_host.py", "# controller\n")
        self.commit("Correct controlled controller")
        self.merge_base()
        self.assertFalse(self.required())

    def test_product_base_merge_requires_new_run(self):
        self.git("checkout", "main")
        self.write("crates/example/src/new.rs", "new base input\n")
        self.commit("Change base input")
        self.merge_base()
        self.assertTrue(self.required())

    def test_reverted_base_input_requires_new_run_even_when_final_tree_matches(self):
        self.git("checkout", "main")
        self.write("crates/example/src/new.rs", "new base input\n")
        changed = self.commit("Change base input")
        self.git("revert", "--no-edit", changed)
        self.merge_base()
        self.assertEqual(self.git("diff", "--name-only", self.before, "HEAD"), "docs/README.md")
        self.assertTrue(self.required())

    def test_unknown_or_invalid_previous_revision_is_conservative(self):
        for before in ["0" * 40, "f" * 40, "--stat", ""]:
            with self.subTest(before=before):
                self.assertTrue(requires_revision_build(self.base, before, repository=self.repository))
        self.assertTrue(requires_revision_build("0" * 40, repository=self.repository))

    def test_initial_documentation_only_pr_does_not_build(self):
        self.git("checkout", "main")
        self.write("docs/new.md", "new documentation\n")
        self.commit("Add documentation")
        self.assertFalse(self.required(before=False))

    def test_cli_compares_actual_checkout_with_previous_head(self):
        script = Path(__file__).resolve().parents[1] / "performance_change_scope.py"
        result = subprocess.run([sys.executable, str(script), "--base", self.base,
                                 "--before", self.before], cwd=self.repository,
                                capture_output=True, check=True)
        self.assertEqual(result.stdout, b"required=false\n")

    def test_auxiliary_selection_uses_the_same_head_and_base_history_guards(self):
        self.assertFalse(requires_revision_build(self.base, repository=self.repository,
                                                select=requires_regex_build))
        self.assertFalse(requires_revision_build(self.base, repository=self.repository,
                                                select=requires_dependency_check))
        self.git("checkout", "main")
        self.write("crates/uqa-pg-regex/src/lib.rs", "regex change\n")
        changed = self.commit("Change regex on base")
        self.git("revert", "--no-edit", changed)
        self.merge_base()
        self.assertTrue(requires_revision_build(self.base, self.before, repository=self.repository,
                                               select=requires_regex_build))
        self.assertFalse(requires_revision_build(self.base, self.before, repository=self.repository,
                                                select=requires_dependency_check))

    def test_dependency_manifest_change_is_selected_on_a_documented_pr(self):
        self.write("crates/example/Cargo.toml", "[package]\nname = 'example'\n")
        self.commit("Change native dependencies")
        self.assertTrue(requires_revision_build(self.base, self.before, repository=self.repository,
                                               select=requires_dependency_check))
        self.assertFalse(requires_revision_build(self.base, self.before, repository=self.repository,
                                                select=requires_regex_build))

    def test_cli_selects_an_auxiliary_input_set(self):
        script = Path(__file__).resolve().parents[1] / "performance_change_scope.py"
        result = subprocess.run([sys.executable, str(script), "--base", self.base,
                                 "--kind", "regex"], cwd=self.repository,
                                capture_output=True, check=True)
        self.assertEqual(result.stdout, b"required=false\n")
