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


def main():
    for features in ((), ("nori",), ("kuromoji",), ("nori", "kuromoji")):
        command = ["cargo", "tree", "-p", "uqa-analysis", "--no-default-features",
                   "--edges", "normal", "--prefix", "none", "--locked"]
        if features:
            command += ["--features", ",".join(features)]
        result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, check=True)
        packages = {line.split()[0] for line in result.stdout.splitlines() if line.strip()}
        actual = {language for language in ("nori", "kuromoji") if f"uqa-{language}-data" in packages}
        if actual != set(features):
            raise RuntimeError(f"Analysis features {features} select unexpected data crates: {sorted(actual)}")
        check = ["cargo", "clippy", "-p", "uqa-analysis", "--no-default-features", "--lib", "--locked"]
        if features:
            check += ["--features", ",".join(features)]
        subprocess.run(check + ["--", "-D", "warnings"], cwd=ROOT, check=True)
        print(f"Analysis features {','.join(features) or 'none'}: runtime data {','.join(sorted(actual)) or 'none'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
