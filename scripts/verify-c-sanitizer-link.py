#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Exercise the configured Rust/C/C++ sanitizer runtime before building libuqa."""

from __future__ import annotations

import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile


HEADER = """/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */
"""
NATIVE = HEADER + "int native_increment(int value) { return value + 1; }\n"
RUST = """extern "C" { fn native_increment(value: i32) -> i32; }
#[no_mangle]
pub unsafe extern "C" fn probe(value: *const i32) -> i32 {
    native_increment(value.read())
}
"""
CLIENT = HEADER + """#include <limits.h>
#include <stdlib.h>
#ifdef __cplusplus
extern "C"
#endif
int probe(const int *value);
int main(int argc, char **argv) {
    (void)argv;
    int value = argc > 1 ? INT_MAX : 41;
    if (argc > 2) {
        int *allocation = (int *)malloc(sizeof(int));
        if (allocation == NULL) return 3;
        value = probe(allocation + 1);
        free(allocation);
    }
    return probe(&value) == 42 ? 0 : 2;
}
"""


def main() -> None:
    cc = shlex.split(os.environ["CC"])
    cflags = shlex.split(os.environ["CFLAGS"])
    with tempfile.TemporaryDirectory(prefix="uqa-sanitizer-link-") as temporary:
        directory = Path(temporary)
        for filename, source in (("native.c", NATIVE), ("lib.rs", RUST), ("client.c", CLIENT)):
            (directory / filename).write_text(source, encoding="utf-8")
        native = directory / "native.o"
        library = directory / ("libprobe.dylib" if sys.platform == "darwin" else "libprobe.so")
        subprocess.run([*cc, *cflags, "-O1", "-g", "-c", str(directory / "native.c"),
                        "-o", str(native)], check=True)
        subprocess.run(["rustc", "+nightly", "--edition=2021", "--crate-type=cdylib",
                        *shlex.split(os.environ["RUSTFLAGS"]), str(directory / "lib.rs"),
                        f"-Clink-arg={native}", "-o", str(library)], check=True)
        for standard, compiler, flags in (
            ("c11", cc, cflags),
            ("c++17", shlex.split(os.environ["CXX"]), shlex.split(os.environ["CXXFLAGS"])),
        ):
            client = directory / f"client-{standard}"
            language = "c++" if standard == "c++17" else "c"
            subprocess.run([*compiler, *flags, "-O1", "-g", "-x", language,
                            f"-std={standard}", str(directory / "client.c"), "-x", "none",
                            str(library), f"-Wl,-rpath,{directory}", "-o", str(client)], check=True)
            for name, args, diagnostic in (
                ("valid", [], None),
                ("native UBSan", ["overflow"], b"signed integer overflow"),
                ("Rust ASan", ["overflow", "bounds"], b"AddressSanitizer: heap-buffer-overflow"),
            ):
                result = subprocess.run([str(client), *args], cwd=directory, capture_output=True)
                valid = result.returncode == 0 if diagnostic is None else (
                    result.returncode != 0 and diagnostic in result.stderr)
                if not valid:
                    sys.stderr.buffer.write(result.stdout + result.stderr)
                    raise RuntimeError(f"{standard} {name} failed (exit {result.returncode})")
                print(f"Passed {standard}: {name}", flush=True)


if __name__ == "__main__":
    main()
