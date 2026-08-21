#!/usr/bin/env bash
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

packages="$(
  cargo metadata --no-deps --format-version 1 | python3 -c '
import json
import sys

metadata = json.load(sys.stdin)
members = set(metadata["workspace_members"])
for package in metadata["packages"]:
    if package["id"] in members and package["name"] != "uqa-pg-query":
        print(package["name"])
'
)"

if [[ -z "$packages" ]]; then
  echo "no rustfmt packages found" >&2
  exit 1
fi

args=()
while IFS= read -r package; do
  [[ -n "$package" ]] || continue
  args+=(-p "$package")
done <<EOF
$packages
EOF

cargo fmt "${args[@]}" -- --check
