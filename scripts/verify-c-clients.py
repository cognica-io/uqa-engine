#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Verify the native C/C++ ABI fixtures against a selected shared or static library."""

from __future__ import annotations

import argparse
import hashlib
import os
from pathlib import Path
import shlex
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "crates/uqa-ffi/tests/native"
BACKENDS = ("memory", "sqlite_native", "sqlite_kv", "redb")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--library", required=True, type=Path)
    parser.add_argument("--include", type=Path, default=ROOT / "crates/uqa-ffi/include")
    parser.add_argument("--static", action="store_true")
    parser.add_argument("--link-args", default="", help="Native dependencies for static linkage")
    parser.add_argument("--sanitize", action="store_true", help="Instrument C/C++ with ASan/UBSan")
    parser.add_argument("--fixture", action="append", help="Select a fixture filename; default: all")
    args = parser.parse_args()
    library = args.library.resolve(strict=True)
    fixtures = sorted(FIXTURES.glob("*.c"))
    if args.fixture:
        selected = set(args.fixture)
        known = {path.name for path in fixtures}
        if selected - known:
            parser.error(f"Unknown fixtures: {sorted(selected - known)}")
        fixtures = [path for path in fixtures if path.name in selected]
    executions = 0
    with tempfile.TemporaryDirectory(prefix="uqa-c-clients-") as temporary:
        output = Path(temporary)
        for language, standard, compiler in (
            ("c", "c11", os.environ.get("CC", "cc")),
            ("c++", "c++17", os.environ.get("CXX", "c++")),
        ):
            for fixture in fixtures:
                executable = output / f"{fixture.stem}-{standard}"
                command = [*shlex.split(compiler), "-x", language, f"-std={standard}",
                           "-Wall", "-Wextra", "-Werror", "-pedantic", "-pthread",
                           "-I", str(args.include)]
                if not args.static:
                    command += ["-DUQA_USE_SHARED"]
                if args.sanitize:
                    command += ["-fsanitize=address,undefined", "-fno-omit-frame-pointer"]
                command += [str(fixture), "-x", "none", str(library), "-lm",
                            *shlex.split(args.link_args)]
                if not args.static:
                    command += [f"-Wl,-rpath,{library.parent}"]
                command += ["-o", str(executable)]
                subprocess.run(command, check=True)
                for backend in BACKENDS:
                    directory = output / f"{fixture.stem}-{standard}-{backend}"
                    directory.mkdir()
                    env = dict(os.environ, UQA_TEST_STORAGE=backend,
                               UQA_TEST_DATABASE_DIR=str(directory), TMPDIR=str(directory),
                               TMP=str(directory), TEMP=str(directory), SQLITE_TMPDIR=str(directory))
                    subprocess.run([str(executable)], env=env, check=True)
                    executions += 1
                    print(f"Passed {fixture.name}: {standard}, {backend}", flush=True)
    with library.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    print(f"Passed {executions} native client executions; {library.name} sha256={digest}")


if __name__ == "__main__":
    main()
