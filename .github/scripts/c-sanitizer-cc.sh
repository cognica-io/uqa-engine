#!/usr/bin/env bash
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

set -euo pipefail

# OpenSSL's own UBSan configuration excludes its function-pointer type erasure.
# https://github.com/openssl/openssl/commit/5f0dab5e74b7cdb1b51217044966dc3927824a4d
# Apply the maintainer-approved exception only to the unchanged vendored build.
case "$PWD" in
  */openssl-build/build/src)
    exec "${UQA_SANITIZER_CC:-clang}" "$@" -DPEDANTIC -fno-sanitize=function
    ;;
  *)
    exec "${UQA_SANITIZER_CC:-clang}" "$@"
    ;;
esac
