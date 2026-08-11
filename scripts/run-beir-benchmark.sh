#!/usr/bin/env bash
set -euo pipefail

workspace_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$workspace_root"

case "${1:-}" in
  "") force_prepare=false ;;
  --force) force_prepare=true ;;
  *) printf 'usage: %s [--force]\n' "$0" >&2; exit 2 ;;
esac

manifest="${UQA_BEIR_BENCH_MANIFEST:-$workspace_root/benchmarks/beir/manifest.json}"
data_dir="${UQA_BEIR_DATA_DIR:-$workspace_root/target/benchmark-runs/beir-data}"
cache_dir="${UQA_BEIR_CACHE_DIR:-$workspace_root/target/benchmark-runs/beir-cache}"
observations="$workspace_root/target/benchmark-runs/beir-observations.json"
report="$workspace_root/target/benchmark-runs/beir-report.json"
case "${CRITERION_HOME:-}" in
  "") criterion_root="$workspace_root/target/criterion" ;;
  /*) criterion_root="$CRITERION_HOME" ;;
  *) criterion_root="$workspace_root/$CRITERION_HOME" ;;
esac
export CRITERION_HOME="$criterion_root"
mkdir -p "$workspace_root/target/benchmark-runs"

prepare_command=(
  python3 scripts/prepare-beir-benchmark.py
  --manifest "$manifest"
  --cache "$cache_dir"
  --output "$data_dir"
)
if [[ "$force_prepare" == true ]]; then
  prepare_command+=(--force)
fi
"${prepare_command[@]}"

UQA_RETRIEVAL_BENCH_SUITE="beir" \
UQA_BEIR_BENCH_MANIFEST="$manifest" \
UQA_BEIR_DATA_DIR="$data_dir" \
UQA_BEIR_OBSERVATIONS="$observations" \
  cargo bench -p uqa-engine --bench retrieval_workloads --locked -- \
  "beir_hybrid_query_batch/scifact" --noplot

python3 scripts/check-beir-benchmark.py \
  --manifest "$manifest" \
  --observations "$observations" \
  --criterion-root "$criterion_root" \
  --output "$report"
