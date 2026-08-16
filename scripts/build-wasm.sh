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

# Emscripten 6 requires Python 3.10 or newer. Some developer environments put
# an older Xcode Python first even when the package manager installed a current
# interpreter for Emscripten, so select a compatible interpreter explicitly.
if [[ -n "${EMSDK_PYTHON:-}" ]]; then
    if ! "${EMSDK_PYTHON}" -c 'import sys; raise SystemExit(sys.version_info < (3, 10))'; then
        echo "error: EMSDK_PYTHON must point to Python 3.10 or newer" >&2
        exit 1
    fi
else
    for python_candidate in python3 python3.14 python3.13 python3.12 python3.11 python3.10; do
        if ! command -v "${python_candidate}" > /dev/null; then
            continue
        fi
        python_path="$(command -v "${python_candidate}")"
        if "${python_path}" -c 'import sys; raise SystemExit(sys.version_info < (3, 10))'; then
            export EMSDK_PYTHON="${python_path}"
            break
        fi
    done
    if [[ -z "${EMSDK_PYTHON:-}" ]]; then
        echo "error: emscripten requires Python 3.10 or newer" >&2
        exit 1
    fi
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

# Every C object must use the same exception and setjmp/longjmp mode as
# the Rust objects it is linked with. The precompiled standard library
# for wasm32-unknown-emscripten is built with emscripten's JavaScript
# exception handling, and rustc passes -sDISABLE_EXCEPTION_CATCHING=0 to
# match, so the C sources must stay on that default too.
#
# Compiling them with -fwasm-exceptions instead makes libpg_query emit
# __wasm_longjmp and __c_longjmp, which the link step cannot resolve;
# forcing wasm exceptions at link time to satisfy those then leaves the
# standard library's __cxa_find_matching_catch_*, __resumeException, and
# llvm_eh_typeid_for undefined. Selecting wasm exceptions for both sides
# would require rebuilding std with -Z build-std on nightly.
#
# Setting the target-scoped variable also shields the build from any host
# CFLAGS that point at a native sysroot.
export CFLAGS_wasm32_unknown_emscripten=""

cargo build --target wasm32-unknown-emscripten -p uqa-wasm ${PROFILE_FLAG}

OUT_DIR="crates/uqa-wasm/js"
cp "target/wasm32-unknown-emscripten/${PROFILE}/uqa.js" "${OUT_DIR}/uqa.js"
cp "target/wasm32-unknown-emscripten/${PROFILE}/uqa.wasm" "${OUT_DIR}/uqa.wasm"

echo "built ${OUT_DIR}/uqa.js and ${OUT_DIR}/uqa.wasm (${PROFILE})"
