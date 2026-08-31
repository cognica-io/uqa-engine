#!/usr/bin/env bash
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
exec python3 "$root/scripts/check-rust-file-lines.py" --root "$root"
