#!/usr/bin/env python3
"""Exercise dependency checks against real Git indexes and commit hooks."""

from __future__ import annotations

import importlib.util
import json
import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
CHECKER = ROOT / "scripts/check-workspace-dependencies.py"


class StagedDependencyTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory(prefix="uqa-dependency-test-")
        self.addCleanup(self.directory.cleanup)
        self.root = pathlib.Path(self.directory.name)
        (self.root / "scripts").mkdir()
        shutil.copyfile(CHECKER, self.root / "scripts/check-workspace-dependencies.py")
        shutil.copyfile(ROOT / "scripts/install-git-hooks.sh", self.root / "scripts/install-git-hooks.sh")
        (self.root / ".githooks").mkdir()
        shutil.copyfile(ROOT / ".githooks/pre-commit", self.root / ".githooks/pre-commit")
        (self.root / ".githooks/pre-commit").chmod(0o755)
        (self.root / "Cargo.toml").write_text(
            '[workspace]\nmembers = ["core", "sql", "engine"]\nresolver = "2"\n'
        )
        for name in ("core", "sql", "engine"):
            package = self.root / name
            (package / "src").mkdir(parents=True)
            (package / "src/lib.rs").write_text("")
            manifest = (
                f'[package]\nname = "uqa-{name}"\nversion = "0.1.0"\nedition = "2021"\n'
            )
            if name != "core":
                manifest += '\n[dependencies]\nuqa-core = { path = "../core" }\n'
            (package / "Cargo.toml").write_text(manifest)
        self.sql_manifest = (self.root / "sql/Cargo.toml").read_text()
        self.policy = {
            "schema_version": 1,
            "dependency_budgets": {"uqa-sql": 2},
            "runtime_workspace_dependencies": {
                "uqa-core": [],
                "uqa-engine": ["uqa-core"],
                "uqa-sql": ["uqa-core"],
            },
            "transitive_dependency_boundaries": {"uqa-sql": ["uqa-core"]},
        }
        self.write_policy()
        self.command("git", "init", "-q")
        self.command("git", "config", "user.name", "Dependency test")
        self.command("git", "config", "user.email", "dependency-test@example.invalid")
        self.command("git", "config", "core.hooksPath", ".githooks")
        self.stage()

    def command(self, *args: str, check: bool = True) -> subprocess.CompletedProcess:
        result = subprocess.run(args, cwd=self.root, capture_output=True, text=True)
        if check:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def write_policy(self) -> None:
        (self.root / "scripts/workspace-dependency-policy.json").write_text(
            json.dumps(self.policy)
        )

    def stage(self) -> None:
        self.command("cargo", "generate-lockfile", "--offline")
        self.command("git", "add", ".")

    def check_index(self) -> subprocess.CompletedProcess:
        return self.command(
            sys.executable, "scripts/check-workspace-dependencies.py", "--staged", check=False
        )

    def add_engine_dependency(self, section: str = "") -> None:
        (self.root / "sql/Cargo.toml").write_text(
            self.sql_manifest + section + 'uqa-engine = { path = "../engine" }\n'
        )

    def test_valid_index_can_be_committed(self) -> None:
        result = self.command("git", "commit", "-m", "Validate crate ownership")
        self.assertIn("Workspace dependency policy OK", result.stdout + result.stderr)

    def test_unstaged_dependency_does_not_change_the_committed_graph(self) -> None:
        self.add_engine_dependency()
        result = self.check_index()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_unstaged_repair_cannot_hide_a_forbidden_staged_dependency(self) -> None:
        self.add_engine_dependency()
        self.stage()
        (self.root / "sql/Cargo.toml").write_text(self.sql_manifest)
        result = self.check_index()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("uqa-sql -> uqa-engine", result.stderr)

    def test_updating_the_edge_inventory_does_not_waive_a_boundary(self) -> None:
        self.add_engine_dependency()
        self.policy["runtime_workspace_dependencies"]["uqa-sql"].append("uqa-engine")
        self.write_policy()
        self.stage()
        result = self.check_index()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("crosses its crate boundary", result.stderr)

    def test_unstaged_policy_changes_cannot_waive_the_staged_boundary(self) -> None:
        self.add_engine_dependency()
        self.stage()
        self.policy["transitive_dependency_boundaries"]["uqa-sql"].append("uqa-engine")
        self.policy["runtime_workspace_dependencies"]["uqa-sql"].append("uqa-engine")
        self.write_policy()
        result = self.check_index()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("crosses its crate boundary", result.stderr)

    def test_build_dependencies_obey_the_same_boundary(self) -> None:
        self.add_engine_dependency("\n[build-dependencies]\n")
        self.stage()
        result = self.check_index()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("uqa-sql -> uqa-engine", result.stderr)

    def test_target_dependencies_are_checked_on_other_platforms(self) -> None:
        self.add_engine_dependency('\n[target.\'cfg(windows)\'.dependencies]\n')
        self.stage()
        result = self.check_index()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("uqa-sql -> uqa-engine", result.stderr)

    def test_installer_activates_the_versioned_hook_and_is_idempotent(self) -> None:
        self.command("git", "config", "--local", "--unset", "core.hooksPath")
        self.command("sh", "scripts/install-git-hooks.sh")
        self.command("sh", "scripts/install-git-hooks.sh")
        self.assertEqual(self.command("git", "config", "--local", "--get", "core.hooksPath").stdout.strip(), ".githooks")
        self.add_engine_dependency()
        self.stage()
        result = self.command("git", "commit", "-m", "Invalid dependency", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("uqa-sql -> uqa-engine", result.stderr)

    def test_installer_preserves_custom_hook_configuration(self) -> None:
        self.command("git", "config", "--local", "core.hooksPath", "custom-hooks")
        result = self.command("sh", "scripts/install-git-hooks.sh", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.command("git", "config", "--get", "core.hooksPath").stdout.strip(), "custom-hooks")

    def test_installer_preserves_an_existing_default_hook(self) -> None:
        self.command("git", "config", "--local", "--unset", "core.hooksPath")
        hook = self.root / ".git/hooks/pre-commit"
        hook.write_text("#!/bin/sh\nexit 0\n")
        result = self.command("sh", "scripts/install-git-hooks.sh", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(hook.read_text(), "#!/bin/sh\nexit 0\n")

    def test_hook_rejects_a_commit_with_a_forbidden_edge(self) -> None:
        self.add_engine_dependency()
        self.stage()
        result = self.command("git", "commit", "-m", "Invalid dependency", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("uqa-sql -> uqa-engine", result.stderr)
        self.assertNotEqual(self.command("git", "rev-parse", "--verify", "HEAD", check=False).returncode, 0)


class DependencyPathTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        spec = importlib.util.spec_from_file_location("dependency_checker", CHECKER)
        cls.checker = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.checker)

    def test_forbidden_transitive_dependency_reports_the_complete_path(self) -> None:
        graph = {"sql": ["shared"], "shared": ["engine"], "engine": []}
        policy = {"transitive_dependency_boundaries": {"sql": ["shared"]}}
        errors = self.checker.boundary_errors(graph, policy)
        self.assertEqual(len(errors), 1)
        self.assertIn("sql -> shared -> engine", errors[0])

    def test_cycle_is_rejected_even_when_all_edges_are_listed(self) -> None:
        graph = {"sql": ["shared"], "shared": ["sql"]}
        policy = {"transitive_dependency_boundaries": {"sql": ["sql", "shared"]}}
        errors = self.checker.boundary_errors(graph, policy)
        self.assertEqual(errors, ["Dependency cycle: sql -> shared -> sql"])


if __name__ == "__main__":
    unittest.main()
