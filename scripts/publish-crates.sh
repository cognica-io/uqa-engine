#!/usr/bin/env bash
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#
# Publish public Rust crates in dependency order. The default preflight checks
# crates that do not depend on unpublished workspace packages. A live registry
# upload requires an explicit --live flag and dry-runs every crate immediately
# before uploading it. Pass --retry-rate-limits to wait for and retry crates.io
# new-crate rate limits without repeating the successful dry run.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

live=0
start_at=""
retry_rate_limits=0
cargo_args=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --live)
      live=1
      shift
      ;;
    --start-at)
      if [[ $# -lt 2 ]]; then
        echo "--start-at requires a crate name" >&2
        exit 2
      fi
      start_at="$2"
      shift 2
      ;;
    --retry-rate-limits)
      retry_rate_limits=1
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

retry_epoch() {
  local retry_at="$1"
  local epoch

  if epoch="$(date -u -d "$retry_at" +%s 2>/dev/null)"; then
    printf '%s\n' "$epoch"
  elif epoch="$(date -j -u -f "%a, %d %b %Y %H:%M:%S GMT" "$retry_at" +%s 2>/dev/null)"; then
    printf '%s\n' "$epoch"
  else
    return 1
  fi
}

wait_for_rate_limit() {
  local retry_at="$1"
  local target_epoch
  local now_epoch
  local remaining
  local wait_seconds

  if ! target_epoch="$(retry_epoch "$retry_at")"; then
    echo "Could not parse crates.io retry time: $retry_at" >&2
    return 1
  fi

  while :; do
    now_epoch="$(date -u +%s)"
    remaining=$((target_epoch - now_epoch))
    if (( remaining <= 0 )); then
      return 0
    fi
    wait_seconds="$remaining"
    if (( wait_seconds > 60 )); then
      wait_seconds=60
    fi
    echo "crates.io rate limit: retrying after $retry_at ($remaining seconds remaining)" >&2
    sleep "$wait_seconds"
  done
}

publish_live() {
  local crate="$1"
  local publish_log
  local publish_status
  local retry_at

  if (( ! retry_rate_limits )); then
    cargo publish -p "$crate" --locked "${cargo_args[@]+"${cargo_args[@]}"}"
    return
  fi

  publish_log="$(mktemp "${TMPDIR:-/tmp}/uqa-publish.XXXXXX")"
  while :; do
    if cargo publish -p "$crate" --locked "${cargo_args[@]+"${cargo_args[@]}"}" 2>&1 | tee "$publish_log"; then
      rm -f "$publish_log"
      return 0
    else
      publish_status=$?
    fi

    if ! grep -Fq "published too many new crates" "$publish_log"; then
      rm -f "$publish_log"
      return "$publish_status"
    fi

    retry_at="$(sed -n 's/.*Please try again after \(.* GMT\) and see .*/\1/p' "$publish_log" | tail -n 1)"
    if [[ -z "$retry_at" ]] || ! wait_for_rate_limit "$retry_at"; then
      rm -f "$publish_log"
      return "$publish_status"
    fi
  done
}

if (( live )); then
  if [[ -n "$start_at" ]]; then
    found=0
    for crate in "${crates[@]}"; do
      if [[ "$crate" == "$start_at" ]]; then
        found=1
        break
      fi
    done
    if (( ! found )); then
      echo "Unknown --start-at crate: $start_at" >&2
      exit 2
    fi
  fi

  echo "Live crates.io publish of ${#crates[@]} crates" >&2
  publishing=0
  if [[ -z "$start_at" ]]; then
    publishing=1
  fi
  for crate in "${crates[@]}"; do
    if (( ! publishing )); then
      if [[ "$crate" != "$start_at" ]]; then
        continue
      fi
      publishing=1
    fi
    cargo publish --dry-run -p "$crate" --locked "${cargo_args[@]+"${cargo_args[@]}"}"
    publish_live "$crate"
  done
else
  if (( retry_rate_limits )); then
    echo "--retry-rate-limits requires --live" >&2
    exit 2
  fi
  if [[ -n "$start_at" ]]; then
    echo "--start-at requires --live" >&2
    exit 2
  fi
  echo "Dry-run crates.io preflight for ${#bootstrap_crates[@]} registry-independent crates" >&2
  for crate in "${bootstrap_crates[@]}"; do
    cargo publish --dry-run -p "$crate" --locked "${cargo_args[@]+"${cargo_args[@]}"}"
  done
fi
