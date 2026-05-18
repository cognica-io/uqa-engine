# Benchmark Parity Port Plan

## Objective

Port the remaining Python benchmark coverage from `/Users/jaepil/work/research/uqa/benchmarks` into the Rust `uqa-rs` Criterion benchmark suite without omitting named benchmark surfaces.

## Current Gap Summary

- `bench_hybrid_fusion.py` and `bench_beir_calibration.py` include real BEIR-style report paths, NDCG@10, MAP@10, Recall@10, ECE, Brier, LogLoss, Dense, BM25, RRF, Convex, and balanced log-odds fusion. The Rust benchmark currently exercises only a compact synthetic subset.
- `bench_planner.py` includes histogram construction and equality/range selectivity in addition to DPccp and greedy fallback. The Rust planner benchmark currently covers only join enumeration.
- `bench_graph_centrality.py` includes PageRank and HITS variants, weighted max path, subgraph-index cached pattern match, incremental remove, progressive fusion, and SQL graph function paths. The Rust graph benchmark currently covers broad graph operators but omits several variants and SQL paths.

## Implementation Tasks

1. Expand `crates/uqa-scoring/benches/beir_calibration.rs` to cover the full BEIR/hybrid benchmark surface:
   - Keep a deterministic built-in fixture for CI.
   - Add optional real BEIR fixture loading from `UQA_BENCH_BEIR_DIR` or the sibling Python data directory when available.
   - Report Dense, BM25, RRF, Convex, and Balanced methods.
   - Compute NDCG@10, MAP@10, Recall@10, ECE, Brier, and LogLoss.
   - Cover calibration sources analogous to distance gap, Bayesian BM25, and density prior.
2. Expand the planner benchmark coverage to include the missing histogram/analyze and equality/range selectivity SQL surfaces. The join-enumerator portions stay in `crates/uqa-planner/benches/planner.rs`; the SQL statistics surfaces live in `crates/uqa-engine/benches/sql_workloads.rs` because `uqa-engine` owns `ANALYZE` and predicate execution.
3. Expand graph benchmark coverage to include the missing graph centrality and named-graph variants:
   - PageRank high damping and low iteration variants.
   - HITS low iteration variant.
   - Weighted max path.
   - Subgraph-index cached pattern match.
   - Incremental remove vertex.
   - Centrality-style SQL function dispatch through the engine, in an engine benchmark to avoid a crate dependency cycle.
4. Update crate benchmark dependencies and manifests only where required by the new benchmark code.
5. Verify with:
   - `cargo fmt --all --check`
   - `cargo check --workspace --all-targets --locked`
   - `cargo bench --workspace --no-run --locked`

## Completion Audit

The work is complete only when every Python `bench_*.py` file maps to Rust Criterion benchmark functions or groups with evidence from manifests and source files, and all verification commands above pass.
