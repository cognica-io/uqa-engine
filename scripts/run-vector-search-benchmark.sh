#!/usr/bin/env bash
set -euo pipefail

workspace_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$workspace_root"

profile="${1:-${UQA_VECTOR_BENCH_PROFILE:-standard}}"
case "$profile" in
  smoke|standard|large) ;;
  *) printf 'unknown vector benchmark profile: %s (expected smoke, standard, or large)\n' "$profile" >&2; exit 2 ;;
esac

observations="$workspace_root/target/benchmark-runs/vector-search-observations-$profile.json"
report="$workspace_root/target/benchmark-runs/vector-search-$profile.json"
case "${CRITERION_HOME:-}" in
  "") criterion_root="$workspace_root/target/criterion" ;;
  /*) criterion_root="$CRITERION_HOME" ;;
  *) criterion_root="$workspace_root/$CRITERION_HOME" ;;
esac
export CRITERION_HOME="$criterion_root"
mkdir -p "$workspace_root/target/benchmark-runs"

UQA_VECTOR_BENCH_PROFILE="$profile" \
UQA_VECTOR_QUALITY_OBSERVATIONS="$observations" \
UQA_RETRIEVAL_BENCH_SUITE="sql-vector-search" \
  cargo bench -p uqa-engine --bench retrieval_workloads --locked -- \
  "sql_vector_search_query_batch/$profile" --noplot

python3 scripts/check-vector-search-benchmark.py \
  --observations "$observations" \
  --criterion-root "$criterion_root" \
  --output "$report"
