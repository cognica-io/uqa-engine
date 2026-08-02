# Benchmark Parity Port Plan

Status: Complete (2026-08-02)

## Objective

Preserve the benchmark surface of the Python `cognica-io/uqa` repository in
the Rust Criterion suites, with enough provenance to detect a missing source
file, a changed source snapshot, or a removed Rust benchmark surface.

## Pinned source contract

[`benchmarks/parity/manifest.json`](../../benchmarks/parity/manifest.json)
pins commit `59339400b209796b349f3a1d82a942379a662686` and the exact SHA-256 of
all 15 `bench_*.py` files in that snapshot. Those files contain 207 named test
cases. The manifest maps them to 148 Rust evidence tokens and records four
additional Rust-only tokens for BEIR calibration and engine WAND/BMW coverage.

The previous inventory incorrectly listed `bench_hybrid_fusion.py` and
`bench_beir_calibration.py` as Python source files. Neither exists in the
pinned snapshot. The Rust `beir_calibration` suite remains useful additional
coverage, but it is not presented as a direct source-file port.

## Completed coverage

- Calibration, scoring, multi-field scoring, external priors, fusion, posting
  lists, storage, compiler, execution, SQL, and planner benchmark surfaces are
  mapped to their owning Rust crates.
- Planner coverage includes join enumeration, histogram construction, and
  equality/range selectivity through the planner and engine suites.
- Graph coverage includes traversal, RPQ, named/temporal graphs, property
  indexing, centrality, message passing, incremental updates, cached pattern
  matching, and engine SQL dispatch.
- The graph index comparison now measures an actual immutable property index
  against a true vertex scan and asserts that both paths return the same IDs.
- `scripts/check-benchmark-parity.py` validates the fixed file set, manifest
  schema, unique paths/tokens, Rust source evidence, and optionally the source
  commit, file hashes, and named-case counts. CI runs the repository-local
  half of this check on every change.

This contract establishes benchmark-surface coverage, not equal algorithms,
fixtures, or latency between Python and Rust. Numerical regression claims use
versioned Rust fixtures and same-machine benchmark artifacts separately.

## Verification

```sh
python3 scripts/check-benchmark-parity.py
python3 scripts/check-benchmark-parity.py --source-root <pinned-uqa-checkout>
cargo fmt --all --check
cargo check --workspace --all-targets --locked
cargo bench --workspace --no-run --locked
```

The source-backed check currently reports 15 Python files, 207 named cases,
and 152 Rust evidence tokens. Any future source update must deliberately
change the pinned commit, every affected digest/case count, and its Rust
mapping in the same review.
