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
EXTERNAL = {"uqa-fdw", "duckdb", "libduckdb-sys", "uqa-duckdb", "uqa-libduckdb-sys",
            "arrow-array", "arrow-ipc", "arrow-schema"}


def verify(package, *, default=True, features=(), target=None):
    args = ["cargo", "tree", "--locked", "--offline", "-p", package,
            "--edges", "normal,build,dev", "--prefix", "none", "--format", "{p}|{f}"]
    if not default:
        args += ["--no-default-features"]
    if features:
        args += ["--features", ",".join(features)]
    if target:
        args += ["--target", target]
    output = subprocess.check_output(args, cwd=ROOT, text=True)
    packages = {line.split()[0] for line in output.splitlines() if line.strip()}
    selected = {feature.rsplit("/", 1)[-1] for feature in features}
    handler_features = set()
    for line in output.splitlines():
        if line.startswith("uqa-fdw "):
            handler_features.update(line.split("|", 1)[1].removesuffix(" (*)").split(","))
    if handler_features & {"arrow", "duckdb"} != selected:
        raise RuntimeError(f"{package}: handler features {handler_features} differ from {selected}")
    expected = {"uqa-fdw"}
    native = target != "wasm32-unknown-emscripten"
    if "duckdb" in selected and native:
        expected |= {"uqa-duckdb", "uqa-libduckdb-sys", "arrow-array", "arrow-schema"}
    if ("arrow" in selected and native) or package == "uqa-api":
        # The fluent API already provides Arrow/Parquet result conversion independently of FDWs.
        expected |= {"arrow-array", "arrow-ipc", "arrow-schema"}
    if packages & EXTERNAL != expected:
        raise RuntimeError(f"{package}: expected {sorted(expected)}, found {sorted(packages & EXTERNAL)}")
    print(f"{package}: defaults={default}, features={features}, target={target or 'native'}: OK")


def main():
    for package in DEFAULT_PACKAGES:
        verify(package)
        verify(package, default=False)
    for package in (*NATIVE_PACKAGES, "uqa-fdw"):
        for features in (("arrow",), ("duckdb",), ("arrow", "duckdb")):
            verify(package, default=False, features=features)
    verify("uqa-engine", default=False, features=("uqa-fdw/arrow",))
    verify("uqa-fdw")
    for package in ("uqa-fdw", "uqa-engine"):
        for features in ((), ("arrow",), ("duckdb",), ("arrow", "duckdb")):
            verify(package, default=False, features=features, target="wasm32-unknown-emscripten")
    verify("uqa-wasm", target="wasm32-unknown-emscripten")


if __name__ == "__main__":
    main()
