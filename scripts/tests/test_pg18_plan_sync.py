#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

from __future__ import annotations

import copy
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
        states = {
            "M0": "partial",
            "M1": "verified",
            "M2": "verified",
            "M3": "explicitly_rejected",
            "M4": "partial",
            "M5": "partial",
            "M6": "not_audited",
        }
        self.manifest = self.manifest_with_states(states)

    @staticmethod
    def manifest_with_states(states: dict[str, str]) -> dict:
        return {
            "milestones": {
                milestone: {
                    "title": f"Milestone {milestone}",
                    "exit_gate": f"Exit gate for {milestone}",
                }
                for milestone in states
            },
            "milestone_items": {
                milestone: [f"item.{milestone.lower()}"] for milestone in states
            },
            "items": [
                {"id": f"item.{milestone.lower()}", "status": status}
                for milestone, status in states.items()
            ],
        }

    def test_milestone_statuses_are_derived_from_owned_items(self) -> None:
        self.assertEqual(
            RUN_DIFF.derive_milestone_statuses(self.manifest),
            {
                "M0": "in_progress",
                "M1": "complete",
                "M2": "complete",
                "M3": "in_progress",
                "M4": "in_progress",
                "M5": "in_progress",
                "M6": "not_started",
            },
        )

    def test_verified_and_not_audited_mix_is_in_progress(self) -> None:
        self.manifest["milestone_items"]["M4"].append("item.m4_pending")
        self.manifest["items"].append(
            {"id": "item.m4_pending", "status": "not_audited"}
        )
        self.manifest["items"][4]["status"] = "verified"

        statuses = RUN_DIFF.derive_milestone_statuses(self.manifest)

        self.assertEqual(statuses["M4"], "in_progress")

    def test_m6_requires_every_milestone_and_item_to_be_complete(self) -> None:
        all_verified = self.manifest_with_states(
            {f"M{index}": "verified" for index in range(7)}
        )
        self.assertEqual(
            RUN_DIFF.derive_milestone_statuses(all_verified)["M6"], "complete"
        )
        all_verified["items"][0]["status"] = "partial"
        self.assertEqual(
            RUN_DIFF.derive_milestone_statuses(all_verified)["M6"], "in_progress"
        )

    def test_complete_claim_must_equal_the_derived_m6_state(self) -> None:
        self.manifest["complete_compatibility_claim"] = False
        statuses = RUN_DIFF.derive_milestone_statuses(self.manifest)
        RUN_DIFF.validate_complete_compatibility_claim(self.manifest, statuses)
        self.manifest["complete_compatibility_claim"] = True
        with self.assertRaisesRegex(RuntimeError, "derived M6"):
            RUN_DIFF.validate_complete_compatibility_claim(self.manifest, statuses)
        self.manifest["complete_compatibility_claim"] = 0
        with self.assertRaisesRegex(RuntimeError, "derived M6"):
            RUN_DIFF.validate_complete_compatibility_claim(self.manifest, statuses)

        all_verified = self.manifest_with_states(
            {f"M{index}": "verified" for index in range(7)}
        )
        all_verified["complete_compatibility_claim"] = True
        RUN_DIFF.validate_complete_compatibility_claim(
            all_verified, RUN_DIFF.derive_milestone_statuses(all_verified)
        )
        snapshot = RUN_DIFF.render_manual_milestone_snapshot(all_verified)
        self.assertIn("in progress — none", snapshot)
        self.assertIn("not started — none", snapshot)

    def test_rendered_ledger_matches_the_manifest(self) -> None:
        ledger = RUN_DIFF.render_plan_status(self.manifest)
        source = f"# Plan\n\n{ledger}\n\n## Next section\n"

        RUN_DIFF.validate_plan_status(self.manifest, source)

    def test_status_drift_is_rejected(self) -> None:
        ledger = RUN_DIFF.render_plan_status(self.manifest)
        changed = copy.deepcopy(self.manifest)
        changed["items"][0]["status"] = "verified"

        with self.assertRaisesRegex(RuntimeError, "does not match manifest.json"):
            RUN_DIFF.validate_plan_status(changed, ledger)

    def test_milestone_move_and_exit_gate_drift_are_rejected(self) -> None:
        ledger = RUN_DIFF.render_plan_status(self.manifest)
        moved = copy.deepcopy(self.manifest)
        moved["milestone_items"]["M0"], moved["milestone_items"]["M1"] = (
            moved["milestone_items"]["M1"],
            moved["milestone_items"]["M0"],
        )
        with self.assertRaisesRegex(RuntimeError, "does not match manifest.json"):
            RUN_DIFF.validate_plan_status(moved, ledger)

        changed_gate = copy.deepcopy(self.manifest)
        changed_gate["milestones"]["M3"]["exit_gate"] = "A different exit gate"
        with self.assertRaisesRegex(RuntimeError, "does not match manifest.json"):
            RUN_DIFF.validate_plan_status(changed_gate, ledger)

    def test_missing_ledger_is_rejected(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "must contain one"):
            RUN_DIFF.validate_plan_status(self.manifest, "# Plan\n")

    def test_manual_snapshot_matches_derived_statuses(self) -> None:
        snapshot = RUN_DIFF.render_manual_milestone_snapshot(self.manifest)
        source = f"# Manual\n\n{snapshot}\n\n## Next section\n"

        RUN_DIFF.validate_manual_milestone_snapshot(self.manifest, source)

    def test_manual_status_drift_is_rejected(self) -> None:
        snapshot = RUN_DIFF.render_manual_milestone_snapshot(self.manifest)
        changed = copy.deepcopy(self.manifest)
        changed["items"][0]["status"] = "verified"

        with self.assertRaisesRegex(RuntimeError, "does not match manifest.json"):
            RUN_DIFF.validate_manual_milestone_snapshot(changed, snapshot)

    def test_every_item_has_exactly_one_owning_milestone(self) -> None:
        item_ids = {item["id"] for item in self.manifest["items"]}
        RUN_DIFF.validate_milestone_accounting(self.manifest, item_ids)

        duplicate = copy.deepcopy(self.manifest)
        duplicate["milestone_items"]["M1"].append("item.m0")
        with self.assertRaisesRegex(RuntimeError, "one owning milestone"):
            RUN_DIFF.validate_milestone_accounting(duplicate, item_ids)

        orphan = copy.deepcopy(self.manifest)
        orphan["milestone_items"]["M0"] = ["item.unknown"]
        with self.assertRaisesRegex(RuntimeError, "unknown items"):
            RUN_DIFF.validate_milestone_accounting(orphan, item_ids)

        missing = copy.deepcopy(self.manifest)
        missing["milestone_items"]["M0"] = []
        with self.assertRaisesRegex(RuntimeError, "no evidence items"):
            RUN_DIFF.validate_milestone_accounting(missing, item_ids)

    def test_orphan_item_is_rejected(self) -> None:
        item_ids = {item["id"] for item in self.manifest["items"]}
        item_ids.add("item.orphan")

        with self.assertRaisesRegex(RuntimeError, "orphan evidence items"):
            RUN_DIFF.validate_milestone_accounting(self.manifest, item_ids)


if __name__ == "__main__":
    unittest.main()
