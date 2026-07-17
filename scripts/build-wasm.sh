#!/usr/bin/env bash
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#
# Build the browser (emscripten) WASM package into crates/uqa-wasm/js/.
#
# Requires emscripten (emcc) and the wasm32-unknown-emscripten Rust
# target:
#   brew install emscripten          # or emsdk
#   rustup target add wasm32-unknown-emscripten
#
# Usage: scripts/build-wasm.sh [--debug]

set -euo pipefail

cd "$(dirname "$0")/.."

if ! command -v emcc > /dev/null; then
    echo "error: emcc not found; install emscripten first" >&2
    exit 1
fi

PROFILE="release"
PROFILE_FLAG="--release"
if [[ "${1:-}" == "--debug" ]]; then
    PROFILE="debug"
    PROFILE_FLAG=""
fi

# The emscripten sysroot provides the libc headers bindgen needs when
# it parses C headers for the wasm target.
EM_SYSROOT="$(em-config CACHE)/sysroot"
export BINDGEN_EXTRA_CLANG_ARGS_wasm32_unknown_emscripten="--sysroot=${EM_SYSROOT} -fvisibility=default"

# Rust's emscripten target links with -fwasm-exceptions, so every C
# object (libpg_query's setjmp/longjmp error handling in particular)
# must be compiled in the same exception/SJLJ mode. This also shields
# the build from any host CFLAGS pointing at a native sysroot.
export CFLAGS_wasm32_unknown_emscripten="-fwasm-exceptions"

cargo build --target wasm32-unknown-emscripten -p uqa-wasm ${PROFILE_FLAG}

OUT_DIR="crates/uqa-wasm/js"
cp "target/wasm32-unknown-emscripten/${PROFILE}/uqa.js" "${OUT_DIR}/uqa.js"
cp "target/wasm32-unknown-emscripten/${PROFILE}/uqa.wasm" "${OUT_DIR}/uqa.wasm"

echo "built ${OUT_DIR}/uqa.js and ${OUT_DIR}/uqa.wasm (${PROFILE})"
