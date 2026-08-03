#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Build and counterbalance base/head analytical benchmark binaries."""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile


ROOT = pathlib.Path(__file__).resolve().parents[1]
BENCHMARK = "analytical_comparison"
WORKLOAD_IDENTITY_KEYS = (
    "workload",
    "rows",
    "seed",
    "work_mem",
    "generator",
    "schema_sql",
    "queries",
)


def run(*args: str, cwd: pathlib.Path = ROOT, env: dict[str, str] | None = None) -> None:
    subprocess.run(args, cwd=cwd, env=env, check=True)


def output(*args: str, cwd: pathlib.Path = ROOT) -> str:
    return subprocess.run(
        args,
        cwd=cwd,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()


def manifest_path(worktree: pathlib.Path) -> pathlib.Path:
    return worktree / "benchmarks" / "analytical" / "manifest.json"


def workload_identity(path: pathlib.Path) -> dict[str, object]:
    manifest = json.loads(path.read_text(encoding="utf-8"))
    return {key: manifest.get(key) for key in WORKLOAD_IDENTITY_KEYS}


def build_benchmark(worktree: pathlib.Path, target: pathlib.Path) -> pathlib.Path:
    environment = os.environ.copy()
    environment["CARGO_TARGET_DIR"] = str(target)
    process = subprocess.Popen(
        [
            "cargo",
            "bench",
            "--locked",
            "-p",
            "uqa-engine",
            "--bench",
            BENCHMARK,
            "--no-run",
            "--message-format=json-render-diagnostics",
        ],
        cwd=worktree,
        env=environment,
        stdout=subprocess.PIPE,
        text=True,
    )
    executable = None
    assert process.stdout is not None
    for line in process.stdout:
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            print(line, end="")
            continue
        if message.get("reason") == "compiler-message":
            rendered = message.get("message", {}).get("rendered")
            if rendered:
                print(rendered, end="", file=sys.stderr)
        target_info = message.get("target", {})
        if (
            message.get("reason") == "compiler-artifact"
            and target_info.get("name") == BENCHMARK
            and "bench" in target_info.get("kind", [])
            and message.get("executable")
        ):
            executable = pathlib.Path(message["executable"])
    return_code = process.wait()
    if return_code:
        raise subprocess.CalledProcessError(return_code, process.args)
    if executable is None:
        raise RuntimeError(f"Cargo did not report the {BENCHMARK} executable")
    return executable


def measure(
    executable: pathlib.Path,
    worktree: pathlib.Path,
    criterion_home: pathlib.Path,
    label: str,
) -> None:
    print(f"==> measuring {label}", flush=True)
    environment = os.environ.copy()
    environment["CRITERION_HOME"] = str(criterion_home)
    run(str(executable), "--noplot", cwd=worktree, env=environment)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("base_revision")
    args = parser.parse_args()
    base_revision = output("git", "rev-parse", "--verify", f"{args.base_revision}^{{commit}}")
    head_manifest_payload = json.loads(manifest_path(ROOT).read_text(encoding="utf-8"))
    pair_count = int(head_manifest_payload["regression_protocol"]["pairs"])
    if pair_count < 2 or pair_count % 2:
        raise RuntimeError("analytical regression requires an even pair count of at least two")
    criterion_parent = ROOT / "target" / "criterion" / "analytical-ab"
    report = ROOT / "target" / "benchmark-runs" / "analytical-comparison.json"
    build_target = ROOT / "target" / "analytical-ab-build"

    with tempfile.TemporaryDirectory(prefix="uqa-analytical-ab-") as temporary:
        temporary_path = pathlib.Path(temporary)
        base_worktree = temporary_path / "base"
        run("git", "worktree", "add", "--detach", str(base_worktree), base_revision)
        try:
            head_manifest = manifest_path(ROOT)
            base_manifest = manifest_path(base_worktree)
            if workload_identity(head_manifest) != workload_identity(base_manifest):
                raise RuntimeError("base and head analytical workload identities differ")

            print("==> building base benchmark", flush=True)
            base_built = build_benchmark(base_worktree, build_target)
            base_executable = temporary_path / "analytical-comparison-base"
            shutil.copy2(base_built, base_executable)

            print("==> building head benchmark", flush=True)
            head_built = build_benchmark(ROOT, build_target)
            head_executable = temporary_path / "analytical-comparison-head"
            shutil.copy2(head_built, head_executable)

            head_roots = [criterion_parent / f"head-{pair}" for pair in range(1, pair_count + 1)]
            base_roots = [criterion_parent / f"base-{pair}" for pair in range(1, pair_count + 1)]
            for index, (head_root, base_root) in enumerate(
                zip(head_roots, base_roots), start=1
            ):
                if index % 2:
                    measure(head_executable, ROOT, head_root, f"head pair {index}")
                    measure(base_executable, base_worktree, base_root, f"base pair {index}")
                else:
                    measure(base_executable, base_worktree, base_root, f"base pair {index}")
                    measure(head_executable, ROOT, head_root, f"head pair {index}")

            checker = [
                sys.executable,
                str(ROOT / "scripts" / "check-analytical-benchmark.py"),
                "--baseline-manifest",
                str(base_manifest),
                "--baseline-revision",
                base_revision,
                "--head-executable",
                str(head_executable),
                "--baseline-executable",
                str(base_executable),
                "--output",
                str(report),
            ]
            for head_root in head_roots:
                checker.extend(("--criterion-root", str(head_root)))
            for base_root in base_roots:
                checker.extend(("--baseline-criterion-root", str(base_root)))
            run(*checker)
        finally:
            subprocess.run(
                ["git", "worktree", "remove", "--force", str(base_worktree)],
                cwd=ROOT,
                check=False,
            )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        print(error, file=sys.stderr)
        raise SystemExit(2) from error
