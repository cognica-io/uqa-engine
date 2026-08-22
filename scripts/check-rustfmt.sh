#!/usr/bin/env bash
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

cargo fmt --all -- --check
