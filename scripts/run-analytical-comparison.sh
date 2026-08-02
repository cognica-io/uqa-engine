#!/usr/bin/env bash
set -euo pipefail

workspace_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$workspace_root"

cargo bench -p uqa-engine --bench analytical_comparison --locked -- --noplot
python3 scripts/check-analytical-benchmark.py \
  --criterion-root target/criterion \
  --output target/benchmark-runs/analytical-comparison.json
