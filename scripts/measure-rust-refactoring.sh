#!/usr/bin/env bash
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

if ! command -v cloc >/dev/null 2>&1; then
  echo "cloc is required to reproduce Rust code, comment, and blank-line totals" >&2
  exit 1
fi

cloc --include-lang=Rust crates
python3 scripts/check-rust-file-lines.py --root "$root" --report
