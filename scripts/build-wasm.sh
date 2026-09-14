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
# Usage: scripts/build-wasm.sh [--debug] [--no-default-features] [--features NAMES] [--output-dir DIR]

set -euo pipefail

cd "$(dirname "$0")/.."

wasm_profile="release"
wasm_build_args=(--release)
wasm_feature_args=()
wasm_output_dir="crates/uqa-wasm/js"
while (($#)); do
    case "$1" in
        --debug)
            wasm_profile="debug"
            wasm_build_args=()
            ;;
        --no-default-features)
            wasm_feature_args+=(--no-default-features)
            ;;
        --features)
            if [[ $# -lt 2 || -z "$2" || "$2" == --* ]]; then
                echo "error: --features requires Cargo feature names" >&2
                exit 2
            fi
            wasm_feature_args+=(--features "$2")
            shift
            ;;
        --output-dir)
            if [[ $# -lt 2 || -z "$2" || "$2" == --* ]]; then
                echo "error: --output-dir requires a directory" >&2
                exit 2
            fi
            wasm_output_dir="$2"
            shift
            ;;
        -h|--help)
            echo "usage: scripts/build-wasm.sh [--debug] [--no-default-features] [--features NAMES] [--output-dir DIR]"
            exit 0
            ;;
        *)
            echo "error: unknown argument: $1" >&2
            exit 2
            ;;
    esac
    shift
done

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

cargo build --locked --target wasm32-unknown-emscripten -p uqa-wasm \
    ${wasm_build_args[@]+"${wasm_build_args[@]}"} ${wasm_feature_args[@]+"${wasm_feature_args[@]}"}

wasm_target_dir="$(cargo metadata --locked --no-deps --format-version 1 | "${EMSDK_PYTHON}" -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')"
mkdir -p "${wasm_output_dir}"
cp "${wasm_target_dir}/wasm32-unknown-emscripten/${wasm_profile}/uqa.js" "${wasm_output_dir}/uqa.js"
cp "${wasm_target_dir}/wasm32-unknown-emscripten/${wasm_profile}/uqa.wasm" "${wasm_output_dir}/uqa.wasm"

echo "built ${wasm_output_dir}/uqa.js and ${wasm_output_dir}/uqa.wasm (${wasm_profile})"
