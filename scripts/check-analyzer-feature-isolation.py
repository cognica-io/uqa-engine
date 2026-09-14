#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Compile each runtime morphology configuration and verify its exact data dependencies."""

from pathlib import Path
import subprocess


ROOT = Path(__file__).resolve().parents[1]
PACKAGES = ("uqa-analysis", "uqa-engine", "uqa", "uqa-cli", "uqa-python", "uqa-node", "uqa-wasm")


def verify_dependencies(package, features):
    command = ["cargo", "tree", "-p", package, "--no-default-features",
               "--edges", "normal", "--prefix", "none", "--locked"]
    if features:
        command += ["--features", ",".join(features)]
    result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, check=True)
    packages = {line.split()[0] for line in result.stdout.splitlines() if line.strip()}
    actual = {language for language in ("nori", "kuromoji") if f"uqa-{language}-data" in packages}
    if actual != set(features):
        raise RuntimeError(f"{package} features {features} select unexpected data crates: {sorted(actual)}")
    print(f"{package} features {','.join(features) or 'none'}: runtime data {','.join(sorted(actual)) or 'none'}")


def main():
    for features in ((), ("nori",), ("kuromoji",), ("nori", "kuromoji")):
        for package in PACKAGES:
            verify_dependencies(package, features)
        check = ["cargo", "clippy", "-p", "uqa-analysis", "--no-default-features", "--all-targets", "--locked"]
        if features:
            check += ["--features", ",".join(features)]
        subprocess.run(check + ["--", "-D", "warnings"], cwd=ROOT, check=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
