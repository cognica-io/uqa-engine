#!/usr/bin/env python3
# Unified Query Algebra
# Copyright (c) 2023-2026 Cognica, Inc.
"""Compile and execute the five public C examples through an actual native library."""

from __future__ import annotations

import argparse
import hashlib
import os
from pathlib import Path
import shlex
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parents[1]
SCENARIOS = (
    "unified_search", "vector_knn", "graph_cypher", "storage_transactions", "extensibility",
)
BACKENDS = ("memory", "sqlite-native", "sqlite-kv", "redb")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--library", required=True, type=Path)
    parser.add_argument("--include", type=Path, default=ROOT / "crates/uqa-ffi/include")
    parser.add_argument("--static", action="store_true")
    parser.add_argument("--link-args", default="", help="Native dependencies for static linkage")
    parser.add_argument("--sanitize", action="store_true", help="Instrument C/C++ consumers with ASan/UBSan")
    args = parser.parse_args()
    library = args.library.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="uqa-c-examples-") as temporary:
        output = Path(temporary)
        executions = 0
        for language, standard, compiler in (
            ("c", "c11", os.environ.get("CC", "cc")),
            ("c++", "c++17", os.environ.get("CXX", "c++")),
        ):
            for scenario in SCENARIOS:
                executable = output / f"{scenario}-{standard}"
                command = [*shlex.split(compiler), "-x", language, f"-std={standard}",
                           "-Wall", "-Wextra", "-Werror", "-pedantic", "-I", str(args.include)]
                if not args.static:
                    command += ["-DUQA_USE_SHARED"]
                if args.sanitize:
                    command += ["-fsanitize=address,undefined", "-fno-omit-frame-pointer"]
                command += [str(ROOT / "examples/c" / f"{scenario}.c"), "-x", "none",
                            str(library), "-lm", "-pthread", *shlex.split(args.link_args)]
                if not args.static:
                    command += [f"-Wl,-rpath,{library.parent}"]
                command += ["-o", str(executable)]
                subprocess.run(command, check=True)
                for backend in BACKENDS:
                    if backend == "memory" and scenario == "storage_transactions":
                        continue  # This scenario requires close/reopen and independent sessions.
                    directory = output / f"{scenario}-{standard}-{backend}"
                    directory.mkdir()
                    command = [str(executable)]
                    if backend != "memory":
                        command += [backend, str(directory / "database.uqa")]
                    env = dict(os.environ, TMPDIR=str(directory), TMP=str(directory),
                               TEMP=str(directory), SQLITE_TMPDIR=str(directory))
                    subprocess.run(command, env=env, check=True)
                    executions += 1
                    print(f"Passed {scenario}: {standard}, {backend}", flush=True)
    with library.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    print(f"Passed {executions} example executions; {library.name} sha256={digest}")


if __name__ == "__main__":
    main()
