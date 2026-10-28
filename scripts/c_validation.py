#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Select C interface and distribution validation from their owned inputs."""

from __future__ import annotations

import argparse
import fnmatch
import json
import os
from pathlib import Path
import re
import subprocess


SHARED = (
    "Cargo.toml", "Cargo.lock", "build.rs", "crates/*/Cargo.toml", "crates/*/build.rs",
    "rust-toolchain", "rust-toolchain.toml",
    ".cargo/*", "crates/uqa-ffi/*", "examples/c/*",
    ".github/scripts/free-linux-runner-disk.sh",
)
BINDINGS = (
    "crates/uqa-core/src/*", "crates/uqa-api/src/*arrow*",
    "crates/*/src/test_support*",
    "scripts/verify-c-examples.py", "scripts/verify-c-clients.py",
    "scripts/verify-c-xml-isolation.py", "scripts/verify-c-sanitizer-link.py",
    ".github/scripts/c-sanitizer-cc.sh",
)
PACKAGES = (
    "scripts/*c-bindings*", "scripts/c_bindings_licenses.py",
    "scripts/verify-c-package.py", "scripts/tests/test_c_bindings_package.py",
    "scripts/check-release-licenses.py", "LICENSE*", "NOTICE*",
)
WORKFLOWS = {"bindings": ".github/workflows/c-bindings.yml",
             "packages": ".github/workflows/c-packages.yml"}
JOBS = {"bindings": {"consumers", "native-sanitizers"}, "packages": {"package"}}
SHA = re.compile(r"[0-9a-f]{40}\Z")


def affected(kind: str, path: str) -> bool:
    if path.startswith("crates/") and ("/tests/" in path or path.endswith("/tests.rs")):
        return path.startswith("crates/uqa-ffi/") or (
            kind == "bindings" and fnmatch.fnmatchcase(path, "crates/*/src/test_support*"))
    patterns = SHARED + (BINDINGS if kind == "bindings" else PACKAGES)
    return any(fnmatch.fnmatchcase(path, pattern) for pattern in patterns)


def execution_definition(kind: str, contents: bytes) -> bytes:
    """Ignore selection wiring only; commands, matrices, environments and tools remain inputs."""
    before, separator, remaining = contents.partition(b"\non:\n")
    if not separator:
        raise ValueError("workflow event declaration is unavailable")
    following = re.search(rb"(?m)^[A-Za-z_][A-Za-z0-9_-]*:", remaining)
    if following is None:
        raise ValueError("workflow execution definition is unavailable")
    execution = before + b"\n" + remaining[following.start():]
    prefix, separator, jobs = execution.partition(b"\njobs:\n")
    if not separator:
        raise ValueError("workflow jobs are unavailable")
    starts = list(re.finditer(rb"(?m)^  ([A-Za-z_][A-Za-z0-9_-]*):\n", jobs))
    names = {match[1].decode() for match in starts}
    if names - {"scope"} != JOBS[kind]:
        raise ValueError("workflow validation jobs changed")
    blocks = []
    for index, match in enumerate(starts):
        if match[1] == b"scope":
            continue
        block = jobs[match.start():starts[index + 1].start() if index + 1 < len(starts) else len(jobs)]
        lines = [line for line in block.splitlines(keepends=True)
                 if line not in (b"    needs: scope\n",
                                 b"    if: needs.scope.outputs.required == 'true'\n")]
        blocks.append(b"".join(lines).rstrip())
    return prefix + b"\njobs:\n" + b"\n\n".join(blocks) + b"\n"


def git(root: Path, *args: str) -> bytes:
    return subprocess.check_output(["git", *args], cwd=root, stderr=subprocess.PIPE)


def select(root: Path, kind: str, event: dict, event_name: str, revision: str,
           *, force_all=False, fetch=False) -> dict:
    result = {"required": True, "revision": revision,
              "reason": "No reliable comparison; retain complete validation."}
    if force_all:
        result["reason"] = "Explicit full validation requires every configuration."
        return result
    if event_name == "pull_request":
        if (event.get("action") != "ready_for_review"
                or event.get("pull_request", {}).get("draft", True)):
            result.update(required=False, reason="Wait for the completed review candidate.")
            return result
        base = event.get("pull_request", {}).get("base", {}).get("sha")
    elif event_name == "push" and event.get("ref", "").startswith("refs/heads/"):
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
        paths = [path.decode("utf-8", "surrogateescape") for path in
                 git(root, "diff", "--name-only", "--no-renames", "-z", base, revision, "--").split(b"\0")
                 if path]
        selected = [path for path in paths if affected(kind, path)]
        workflow = WORKFLOWS[kind]
        if workflow in paths:
            try:
                unchanged = execution_definition(kind, git(root, "show", f"{base}:{workflow}")) == \
                    execution_definition(kind, git(root, "show", f"{revision}:{workflow}"))
            except (subprocess.CalledProcessError, ValueError):
                unchanged = False
            if not unchanged:
                selected.append(workflow)
    except subprocess.CalledProcessError:
        return result
    result.update(required=bool(selected), paths=sorted(selected),
                  reason="C validation inputs changed." if selected else "No C validation input changed.")
    return result


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kind", choices=WORKFLOWS, required=True)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text(encoding="utf-8"))
    revision = git(root, "rev-parse", "HEAD").decode().strip()
    result = select(root, args.kind, event, os.environ.get("GITHUB_EVENT_NAME", ""), revision,
                    force_all=os.environ.get("C_VALIDATION_FORCE_ALL") == "true", fetch=True)
    if os.environ.get("GITHUB_OUTPUT"):
        with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as handle:
            handle.write("required=" + str(result["required"]).lower() + "\n")
    print(json.dumps(result, ensure_ascii=True))


if __name__ == "__main__":
    main()
