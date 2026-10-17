#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Select foreign-handler changes and retain successful, source-matched CI evidence."""

from __future__ import annotations

import argparse
import fnmatch
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess


# Core supplies FDW values; SQL's declared types and the foreign adapters consume them.
INPUTS = (
    "Cargo.toml", "Cargo.lock", "crates/*/Cargo.toml", "rust-toolchain.toml", ".cargo/*",
    "crates/uqa-fdw/*", "crates/uqa-core/src/*",
    "crates/uqa-sql/src/ast.rs", "crates/uqa-sql/src/ast/*",
    "crates/*/src/fdw.rs", "crates/*/src/fdw/*",
    "crates/*/src/*/foreign.rs", "crates/*/src/*/foreign/*",
    "crates/*/src/*/foreign_table*", "crates/*/src/*/foreign_server*",
    "crates/*/src/*/foreign_wrapper*", "crates/*/src/*/foreign_creation*",
    "crates/*/src/*/foreign_definitions*", "crates/*/src/*/foreign_removal*",
    "crates/*/src/*/foreign_catalog*", "crates/*/src/*/foreign_authorization*",
    "crates/*/src/*/foreign_relations*",
    "crates/*/tests/*foreign_table*", "crates/*/tests/*foreign_server*",
    "crates/uqa-engine/src/tests/catalog/row_type_arrays.rs",
    "crates/uqa-engine/tests/sql_foreign_ddl*",
    "crates/uqa-engine/tests/sql_information_schema/foreign_table_ownership.rs",
    "crates/uqa-engine/tests/catalog/sql_rules/relation_dependencies.rs",
    "tests/parity/pg18/*foreign*",
    ".github/workflows/fdw-features.yml", ".github/scripts/free-linux-runner-disk.sh",
    "scripts/check-fdw-feature-isolation.py", "scripts/fdw_ci.py",
    "scripts/tests/test_fdw_ci.py",
)
PLATFORMS = ["ubuntu-24.04", "macos-14"]
CONFIGURATIONS = ["memory", "arrow", "duckdb"]
SHA = re.compile(r"[0-9a-f]{40}\Z")


def git(root: Path, *args: str) -> bytes:
    return subprocess.check_output(["git", *args], cwd=root, stderr=subprocess.PIPE)


def fingerprint(root: Path, revision: str) -> str:
    # Include every product, fixture, build and workflow input. Only status prose is excluded.
    digest = hashlib.sha256()
    for entry in git(root, "ls-tree", "-r", "-z", revision).split(b"\0"):
        if not entry:
            continue
        path = entry.split(b"\t", 1)[1]
        if path.startswith(b"docs/plans/") or path in (b"HISTORY.md", b"README.md"):
            continue
        digest.update(entry + b"\0")
    return digest.hexdigest()


def affected(path: str) -> bool:
    return any(fnmatch.fnmatchcase(path, pattern) for pattern in INPUTS)


def select(root: Path, event: dict, event_name: str, revision: str, *, fetch=False) -> dict:
    result = {"required": True, "fingerprint": fingerprint(root, revision),
              "revision": revision, "reason": "No reliable comparison; retain all configurations."}
    if event_name == "pull_request":
        if (event.get("action") != "ready_for_review"
                or event.get("pull_request", {}).get("draft", True)):
            result.update(required=False, reason="Wait for the final ready-for-review transition.")
            return result
        base = event.get("pull_request", {}).get("base", {}).get("sha")
    elif event_name == "push":
        base = event.get("before")
    else:
        return result
    if not isinstance(base, str) or not SHA.fullmatch(base) or base == "0" * 40:
        return result
    try:
        try:
            git(root, "cat-file", "-e", f"{base}^{{commit}}")
        except subprocess.CalledProcessError:
            if not fetch:
                return result
            git(root, "fetch", "--no-tags", "--depth=1", "origin", base)
        # checkout supplies the PR merge tree: base-to-merge excludes unrelated base changes.
        # Disabling rename detection retains both the removed and newly introduced paths.
        raw = git(root, "diff", "--name-only", "--no-renames", "-z", base, revision, "--")
    except subprocess.CalledProcessError:
        return result
    paths = [path.decode("utf-8", errors="surrogateescape") for path in raw.split(b"\0") if path]
    selected = sorted(path for path in paths if affected(path))
    result.update(required=bool(selected), paths=selected,
                  reason="Foreign-handler inputs changed." if selected else "No foreign-handler input changed.")
    return result


def proof(root: Path, expected: str, revision: str, repository: str, run: str) -> dict:
    if fingerprint(root, revision) != expected:
        raise ValueError("FDW qualification source changed between selection and completion")
    if not SHA.fullmatch(revision) or not run.isdecimal() or int(run) <= 0:
        raise ValueError("invalid FDW qualification source or run")
    return {"fingerprint": expected, "revision": revision, "repository": repository,
            "run_id": run, "platforms": PLATFORMS, "configurations": CONFIGURATIONS}


def validate_proof(value: dict, expected: str, repository: str) -> None:
    if (value.get("fingerprint") != expected or value.get("repository") != repository
            or value.get("platforms") != PLATFORMS or value.get("configurations") != CONFIGURATIONS
            or not SHA.fullmatch(str(value.get("revision", "")))
            or not str(value.get("run_id", "")).isdecimal() or int(value["run_id"]) <= 0):
        raise ValueError("cached FDW evidence does not qualify these inputs")


def output(name: str, value: str) -> None:
    if os.environ.get("GITHUB_OUTPUT"):
        with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as handle:
            handle.write(f"{name}={value}\n")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=["select", "record", "reuse"])
    parser.add_argument("--fingerprint")
    parser.add_argument("--proof", type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    revision = git(root, "rev-parse", "HEAD").decode().strip()
    repository = os.environ.get("GITHUB_REPOSITORY", "")
    if args.mode == "select":
        event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text(encoding="utf-8"))
        result = select(root, event, os.environ["GITHUB_EVENT_NAME"], revision, fetch=True)
        output("required", str(result["required"]).lower())
        output("fingerprint", result["fingerprint"])
        print(json.dumps(result, ensure_ascii=True))
    elif args.mode == "record":
        result = proof(root, args.fingerprint, revision, repository, os.environ["GITHUB_RUN_ID"])
        args.proof.parent.mkdir(parents=True, exist_ok=True)
        args.proof.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    else:
        result = json.loads(args.proof.read_text(encoding="utf-8"))
        validate_proof(result, args.fingerprint, repository)
        output("reused", "true")
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as handle:
            handle.write(f"FDW inputs are unchanged. Reusing all six successful configurations from "
                         f"[run {result['run_id']}](https://github.com/{repository}/actions/runs/{result['run_id']}) "
                         f"at `{result['revision']}`; input fingerprint `{args.fingerprint}`.\n")


if __name__ == "__main__":
    main()
