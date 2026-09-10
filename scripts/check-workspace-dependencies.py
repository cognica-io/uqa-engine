#!/usr/bin/env python3
"""Check crate boundaries against the working tree or the Git index."""

from __future__ import annotations

import difflib
import argparse
import json
import pathlib
import subprocess
import sys
import tempfile


ROOT = pathlib.Path(__file__).resolve().parents[1]
POLICY_PATH = ROOT / "scripts" / "workspace-dependency-policy.json"


def cargo_metadata(root: pathlib.Path = ROOT) -> dict[str, object]:
    result = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked", "--offline"],
        cwd=root,
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


def boundary_errors(actual: dict[str, list[str]], policy: dict[str, object]) -> list[str]:
    errors = []
    for owner, allowed in sorted(policy.get("transitive_dependency_boundaries", {}).items()):
        if owner not in actual:
            errors.append(f"Dependency boundary names missing crate {owner}")
            continue
        pending = [(owner, [owner])]
        visited = set()
        while pending:
            node, path = pending.pop()
            if node in visited:
                continue
            visited.add(node)
            for dependency in actual.get(node, []):
                chain = path + [dependency]
                if dependency in path:
                    errors.append("Dependency cycle: " + " -> ".join(chain))
                elif dependency not in allowed:
                    errors.append(f"{owner} crosses its crate boundary: " + " -> ".join(chain))
                else:
                    pending.append((dependency, chain))
    return errors


def check(policy: dict[str, object], metadata: dict[str, object]) -> int:
    if policy.get("schema_version") != 1:
        print(f"Unsupported dependency policy schema: {policy.get('schema_version')}", file=sys.stderr)
        return 2

    expected = policy["runtime_workspace_dependencies"]
    actual = runtime_workspace_dependencies(metadata)
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

    for error in boundary_errors(actual, policy):
        failed = True
        print(error, file=sys.stderr)

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


def index_bytes(root: pathlib.Path, path: str) -> bytes:
    return subprocess.check_output(["git", "show", f":{path}"], cwd=root)


def check_staged(root: pathlib.Path) -> int:
    unmerged = subprocess.check_output(["git", "ls-files", "--unmerged", "-z"], cwd=root)
    if unmerged:
        print("Resolve index conflicts before checking crate dependencies.", file=sys.stderr)
        return 1
    paths = subprocess.check_output(["git", "ls-files", "-z"], cwd=root).decode().split("\0")
    policy = json.loads(index_bytes(root, "scripts/workspace-dependency-policy.json"))
    # Cargo discovers targets by path; source contents do not affect dependency metadata.
    # Materialize index manifests and target placeholders so unstaged edits cannot change the result.
    with tempfile.TemporaryDirectory(prefix="uqa-dependency-index-") as directory:
        snapshot = pathlib.Path(directory)
        for path in filter(None, paths):
            name = pathlib.PurePosixPath(path).name
            if name not in {"Cargo.toml", "Cargo.lock"} and not path.endswith(".rs"):
                continue
            destination = snapshot / path
            destination.parent.mkdir(parents=True, exist_ok=True)
            if name in {"Cargo.toml", "Cargo.lock"}:
                destination.write_bytes(index_bytes(root, path))
            else:
                destination.touch()
        return check(policy, cargo_metadata(snapshot))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--staged", action="store_true", help="validate the exact Git index")
    args = parser.parse_args()
    try:
        if args.staged:
            return check_staged(ROOT)
        policy = json.loads(POLICY_PATH.read_text(encoding="utf-8"))
        return check(policy, cargo_metadata())
    except subprocess.CalledProcessError as error:
        detail = error.stderr
        if isinstance(detail, bytes):
            detail = detail.decode(errors="replace")
        print(detail or str(error), file=sys.stderr)
        return 1
    except (OSError, ValueError, KeyError) as error:
        print(f"Cannot validate crate dependencies: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
