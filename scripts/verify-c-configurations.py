#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Execute C feature configurations with a shared Cargo target directory per job."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]
LINUX = "ubuntu-24.04"
MACOS = "macos-26-arm64"
CORE = ("minimal", "arrow", "arrow-export", "arrow-export,arrow", "default")
DUCKDB = ("duckdb", "arrow,duckdb", "arrow-export,duckdb", "arrow-export,arrow,duckdb")
GROUPS = {
    "core": CORE,
    "duckdb": DUCKDB,
    "consumers": CORE + DUCKDB,
    "sanitizers": ("minimal", "arrow-export"),
}


def job_matrices(platform: str, group: str) -> dict[str, list[dict[str, str]]]:
    if platform not in ("all", LINUX, MACOS) or group not in ("all", "core", "duckdb", "sanitizers"):
        raise ValueError(f"Unknown C validation selection: {platform}, {group}")
    consumers = []
    sanitizers = []
    platforms = (LINUX, MACOS) if platform == "all" else (platform,)
    for runner in platforms:
        if group != "sanitizers":
            groups = (group,)
            if group == "all":
                groups = ("consumers",) if runner == LINUX else ("core", "duckdb")
            for selected in groups:
                consumers.append({"os": runner, "group": selected})
        if group in ("all", "sanitizers"):
            configurations = GROUPS["sanitizers"] if runner == LINUX else ("all",)
            for configuration in configurations:
                sanitizers.append({"os": runner, "configuration": configuration})
    return {"consumers": consumers, "sanitizers": sanitizers}


def commands(configuration: str, *, target: str | None = None) -> list[list[str]]:
    features = ["--features", "test-support"]
    if configuration != "default":
        features += ["--no-default-features"]
    if configuration not in ("minimal", "default"):
        features += ["--features", configuration]
    cargo = ["cargo", "+nightly"] if target else ["cargo"]
    common = ["--locked", "-p", "uqa-ffi", *features]
    if target:
        common += ["-Zbuild-std", "--target", target]
    # With build-std, selecting the test target up front shares instrumented libtest
    # and its dependency graph between the native library and the Rust unit tests.
    build_targets = ["--lib", "--tests"] if target == "x86_64-unknown-linux-gnu" else []
    result = [
        [*cargo, "build", *common, "--profile", "test", *build_targets],
        [*cargo, "test", *common, "--lib"],
    ]
    if configuration == "minimal" and not target:
        result.append(["cargo", "test", "-p", "uqa-storage", "--locked", "--features",
                       "test-support", "--lib", "test_support::tests"])
    directory = Path(os.environ.get("CARGO_TARGET_DIR", str(ROOT / "target")))
    if target:
        directory /= target
    suffix = "dylib" if sys.platform == "darwin" else "so"
    library = str(directory / "debug" / f"libuqa.{suffix}")

    def verify(script: str, *, sanitize=False) -> list[str]:
        return [sys.executable, str(ROOT / "scripts" / script), "--library", library,
                *(["--sanitize"] if sanitize else [])]

    result += [
        verify("verify-c-clients.py", sanitize=bool(target)),
        verify("verify-c-examples.py", sanitize=bool(target)),
        verify("verify-c-xml-isolation.py", sanitize=bool(target)),
    ]
    if not target:
        result += [verify("verify-c-examples.py", sanitize=True),
                   verify("verify-c-clients.py", sanitize=True)]
    return result


def run(group: str, *, target: str | None = None, selected_configuration: str = "all") -> int:
    results = []
    configurations = GROUPS[group] if selected_configuration == "all" else (selected_configuration,)
    for configuration in configurations:
        print(f"::group::C configuration: {configuration}", flush=True)
        try:
            # A failed build/test stops its consumers, but never hides another configuration.
            for command in commands(configuration, target=target):
                print(shlex.join(command), flush=True)
                subprocess.run(command, cwd=ROOT, check=True)
        except (subprocess.CalledProcessError, OSError) as error:
            results.append((configuration, "FAIL"))
            print(f"::error::C configuration {configuration}: {error}", flush=True)
        else:
            results.append((configuration, "PASS"))
        finally:
            print("::endgroup::", flush=True)
    report = "\n".join([f"C configurations ({group})", "", "| Features | Result |",
                        "| --- | --- |", *(f"| {name} | {status} |" for name, status in results)])
    print(report, flush=True)
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as summary:
            summary.write(report + "\n")
    return int(any(status != "PASS" for _, status in results))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--matrix", nargs=2, metavar=("PLATFORM", "GROUP"),
                      help="Print the GitHub Actions job matrices for a workflow selection")
    mode.add_argument("--group", choices=GROUPS)
    parser.add_argument("--configuration", choices=("all", *GROUPS["sanitizers"]), default="all",
                        help="Select a native sanitizer configuration")
    args = parser.parse_args()
    if args.configuration != "all" and args.group != "sanitizers":
        parser.error("--configuration requires --group sanitizers")
    if args.matrix:
        try:
            matrices = job_matrices(*args.matrix)
        except ValueError as error:
            parser.error(str(error))
        print(json.dumps(matrices))
        return 0
    target = None
    if args.group == "sanitizers":
        version = subprocess.check_output(["rustc", "+nightly", "-vV"], text=True)
        target = next(line.removeprefix("host: ") for line in version.splitlines()
                      if line.startswith("host: "))
    return run(args.group, target=target, selected_configuration=args.configuration)


if __name__ == "__main__":
    raise SystemExit(main())
