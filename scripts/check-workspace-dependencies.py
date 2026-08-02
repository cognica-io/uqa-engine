#!/usr/bin/env python3
"""Reject unreviewed workspace dependency edges and budget growth."""

from __future__ import annotations

import difflib
import json
import pathlib
import subprocess
import sys


ROOT = pathlib.Path(__file__).resolve().parents[1]
POLICY_PATH = ROOT / "scripts" / "workspace-dependency-policy.json"


def cargo_metadata() -> dict[str, object]:
    result = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    return json.loads(result.stdout)


def runtime_workspace_dependencies(metadata: dict[str, object]) -> dict[str, list[str]]:
    packages = metadata["packages"]
    workspace_members = set(metadata["workspace_members"])
    workspace_packages = [package for package in packages if package["id"] in workspace_members]
    workspace_names = {package["name"] for package in workspace_packages}
    return {
        package["name"]: sorted(
            {
                dependency["name"]
                for dependency in package["dependencies"]
                if dependency["kind"] != "dev" and dependency["name"] in workspace_names
            }
        )
        for package in sorted(workspace_packages, key=lambda item: item["name"])
    }


def pretty(value: dict[str, list[str]]) -> list[str]:
    return json.dumps(value, indent=2, sort_keys=True).splitlines(keepends=True)


def main() -> int:
    policy = json.loads(POLICY_PATH.read_text(encoding="utf-8"))
    if policy.get("schema_version") != 1:
        print(f"Unsupported dependency policy schema: {policy.get('schema_version')}", file=sys.stderr)
        return 2

    expected = policy["runtime_workspace_dependencies"]
    actual = runtime_workspace_dependencies(cargo_metadata())
    failed = False
    if actual != expected:
        failed = True
        print("Workspace dependency policy changed:", file=sys.stderr)
        print(
            "".join(
                difflib.unified_diff(
                    pretty(expected),
                    pretty(actual),
                    fromfile=str(POLICY_PATH),
                    tofile="cargo metadata",
                )
            ),
            file=sys.stderr,
        )

    for crate, budget in sorted(policy["dependency_budgets"].items()):
        count = len(actual.get(crate, []))
        if count > budget:
            failed = True
            print(
                f"{crate} has {count} runtime workspace dependencies; budget is {budget}",
                file=sys.stderr,
            )

    if failed:
        print(
            "Update architecture first, then change the policy in the same review if the new edge is intentional.",
            file=sys.stderr,
        )
        return 1

    print(
        "Workspace dependency policy OK "
        f"({sum(map(len, actual.values()))} runtime edges across {len(actual)} crates)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
