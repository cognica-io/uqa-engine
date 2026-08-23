#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

from __future__ import annotations

import importlib.util
import pathlib
import sys
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "tests" / "parity" / "pg18" / "run_diff.py"
SPEC = importlib.util.spec_from_file_location("pg18_run_diff", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
RUN_DIFF = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = RUN_DIFF
SPEC.loader.exec_module(RUN_DIFF)


class PG18PlanSyncTest(unittest.TestCase):
    def setUp(self) -> None:
        self.manifest = {
            "milestones": {"M0": "in_progress", "M1": "complete"},
            "items": [
                {"id": "query.example", "status": "verified"},
                {"id": "catalog.example", "status": "partial"},
            ],
        }

    def test_rendered_ledger_matches_the_manifest(self) -> None:
        ledger = RUN_DIFF.render_plan_status(self.manifest)
        source = f"# Plan\n\n{ledger}\n\n## Next section\n"

        RUN_DIFF.validate_plan_status(self.manifest, source)

    def test_status_drift_is_rejected(self) -> None:
        ledger = RUN_DIFF.render_plan_status(self.manifest)
        changed = {
            **self.manifest,
            "milestones": {"M0": "complete", "M1": "complete"},
        }

        with self.assertRaisesRegex(RuntimeError, "does not match manifest.json"):
            RUN_DIFF.validate_plan_status(changed, ledger)

    def test_missing_ledger_is_rejected(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "must contain one"):
            RUN_DIFF.validate_plan_status(self.manifest, "# Plan\n")


if __name__ == "__main__":
    unittest.main()
