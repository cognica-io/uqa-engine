#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Match C setjmp/longjmp to the active Rust Emscripten unwind ABI."""

import os
import re
import subprocess


def c_flags(version: str) -> str:
    match = re.match(r"rustc (\d+)\.(\d+)\.\d+(?:[- ].*)?$", version.strip())
    if match is None:
        raise ValueError(f"unrecognized Rust compiler version: {version.strip()!r}")
    # Rust 1.93 changed its shipped Emscripten std from JS to Wasm unwinding:
    # https://github.com/rust-lang/rust/pull/147224
    if tuple(map(int, match.groups())) >= (1, 93):
        return "-fwasm-exceptions"
    return ""


if __name__ == "__main__":
    version = subprocess.check_output(
        [os.environ.get("RUSTC", "rustc"), "--version"], text=True
    )
    print(c_flags(version))
