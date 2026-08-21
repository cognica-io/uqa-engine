#!/usr/bin/env bash
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#
# Publish public Rust crates in dependency order. The default preflight checks
# crates that do not depend on unpublished workspace packages. A live registry
# upload requires an explicit --live flag and dry-runs every crate immediately
# before uploading it.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

live=0
cargo_args=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --live)
      live=1
      shift
      ;;
    --)
      shift
      cargo_args+=("$@")
      break
      ;;
    *)
      cargo_args+=("$1")
      shift
      ;;
  esac
done

crates=(
  uqa-pg-query
  uqa-core
  uqa-pg-wire
  uqa-analysis
  uqa-storage
  uqa-storage-redb
  uqa-storage-sqlite
  uqa-scoring
  uqa-fusion
  uqa-operators
  uqa-ml
  uqa-sql
  uqa-fdw
  uqa-graph
  uqa-joins
  uqa-execution
  uqa-planner
  uqa-engine
  uqa-client
  uqa-api
  uqa-cli
  uqa
)

bootstrap_crates=(
  uqa-pg-query
  uqa-core
  uqa-pg-wire
)

if (( live )); then
  echo "Live crates.io publish of ${#crates[@]} crates" >&2
  for crate in "${crates[@]}"; do
    cargo publish --dry-run -p "$crate" --locked "${cargo_args[@]+"${cargo_args[@]}"}"
    cargo publish -p "$crate" --locked "${cargo_args[@]+"${cargo_args[@]}"}"
  done
else
  echo "Dry-run crates.io preflight for ${#bootstrap_crates[@]} registry-independent crates" >&2
  for crate in "${bootstrap_crates[@]}"; do
    cargo publish --dry-run -p "$crate" --locked "${cargo_args[@]+"${cargo_args[@]}"}"
  done
fi
