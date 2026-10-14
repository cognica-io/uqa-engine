#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Verify optional foreign handlers across native, test and browser dependency graphs."""

from pathlib import Path
import subprocess


ROOT = Path(__file__).resolve().parents[1]
NATIVE_PACKAGES = ("uqa-engine", "uqa", "uqa-cli", "uqa-python", "uqa-node", "uqa-api", "uqa-pg-server")
DEFAULT_PACKAGES = (*NATIVE_PACKAGES, "uqa-execution", "uqa-wasm")
EXTERNAL = {"uqa-fdw", "duckdb", "libduckdb-sys"}


def verify(package, *, default=True, features=(), target=None):
    args = ["cargo", "tree", "--locked", "--offline", "-p", package,
            "--edges", "normal,build,dev", "--prefix", "none", "--format", "{p}"]
    if not default:
        args += ["--no-default-features"]
    if features:
        args += ["--features", ",".join(features)]
    if target:
        args += ["--target", target]
    output = subprocess.check_output(args, cwd=ROOT, text=True)
    packages = {line.split()[0] for line in output.splitlines() if line.strip()}
    expected = {"uqa-fdw"} if package == "uqa-fdw" or features else set()
    if "duckdb" in features and target != "wasm32-unknown-emscripten":
        expected |= {"duckdb", "libduckdb-sys"}
    if packages & EXTERNAL != expected:
        raise RuntimeError(f"{package}: expected {sorted(expected)}, found {sorted(packages & EXTERNAL)}")
    print(f"{package}: defaults={default}, features={features}, target={target or 'native'}: OK")


def main():
    for package in DEFAULT_PACKAGES:
        verify(package)
        verify(package, default=False)
    for package in NATIVE_PACKAGES:
        verify(package, default=False, features=("fdw",))
        verify(package, default=False, features=("duckdb",))
    verify("uqa-execution", default=False, features=("fdw",))
    verify("uqa-engine", default=False, features=("uqa-execution/fdw",))
    verify("uqa-fdw")
    verify("uqa-fdw", default=False, features=("duckdb",))
    for package in ("uqa-fdw", "uqa-engine"):
        verify(package, default=False, target="wasm32-unknown-emscripten")
        verify(package, default=False, features=("duckdb",), target="wasm32-unknown-emscripten")
    verify("uqa-wasm", features=("fdw",), target="wasm32-unknown-emscripten")


if __name__ == "__main__":
    main()
