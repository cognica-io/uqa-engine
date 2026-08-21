#!/usr/bin/env bash
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#
# Create crates.io package archives for every publishable workspace crate in
# one Cargo invocation so unpublished workspace dependencies resolve locally.
# Then check the license contents of every archive that was produced.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

allow_dirty=()
if [[ "${1:-}" == "--allow-dirty" ]]; then
  allow_dirty=(--allow-dirty)
  shift
fi

package_rows="$(
  cargo metadata --locked --no-deps --format-version 1 | python3 -c '
import json
import sys

metadata = json.load(sys.stdin)
workspace_ids = set(metadata["workspace_members"])

def is_publishable(package):
    publish = package.get("publish")
    if publish is False or publish == []:
        return False
    return package["id"] in workspace_ids

for package in metadata["packages"]:
    if not is_publishable(package):
        continue
    print("{}\t{}".format(package["name"], package["version"]))
'
)"

if [[ -z "$package_rows" ]]; then
  echo "no publishable crates found" >&2
  exit 1
fi

package_args=()
archives=()
while IFS=$'\t' read -r package version; do
  [[ -n "$package" ]] || continue
  package_args+=(-p "$package")
  archives+=("target/package/${package}-${version}.crate")
done <<EOF
$package_rows
EOF

cargo package --no-verify --locked "${allow_dirty[@]}" "${package_args[@]}"

for archive in "${archives[@]}"; do
  if [[ ! -f "$archive" ]]; then
    echo "cargo package did not produce $archive" >&2
    exit 1
  fi
done

python3 scripts/check-release-licenses.py "${archives[@]}"
