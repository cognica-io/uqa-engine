#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Execute C feature configurations with a shared Cargo target directory per job."""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import shlex
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]
GROUPS = {
    "core": ("minimal", "arrow", "arrow-export", "arrow-export,arrow", "default"),
    "duckdb": ("duckdb", "arrow,duckdb", "arrow-export,duckdb", "arrow-export,arrow,duckdb"),
    "sanitizers": ("minimal", "arrow-export"),
}


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
    result = [
        [*cargo, "build", *common, "--profile", "test"],
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


def run(group: str, *, target: str | None = None) -> int:
    results = []
    for configuration in GROUPS[group]:
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
    parser.add_argument("--group", choices=GROUPS, required=True)
    args = parser.parse_args()
    target = None
    if args.group == "sanitizers":
        version = subprocess.check_output(["rustc", "+nightly", "-vV"], text=True)
        target = next(line.removeprefix("host: ") for line in version.splitlines()
                      if line.startswith("host: "))
    return run(args.group, target=target)


if __name__ == "__main__":
    raise SystemExit(main())
