#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

from pathlib import Path
import subprocess
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from performance_change_scope import requires_regression_build


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
