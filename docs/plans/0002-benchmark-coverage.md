# Benchmark Coverage Contract

Status: Complete (2026-08-03)

## Objective

Keep every Rust benchmark entrypoint visible in a machine-checked inventory and attach representative semantic evidence tokens to each surface. The contract detects an unregistered benchmark, a stale manifest entry, or the accidental removal of a named workload from an existing suite.

## Coverage contract

[`benchmarks/coverage/manifest.json`](../../benchmarks/coverage/manifest.json) lists every top-level `crates/*/benches/*.rs` entrypoint. Each entry records tokens for benchmark groups, named cases, or benchmark functions that express the behavior the suite is expected to retain.

The inventory covers carrier operations, posting lists, calibration, scoring, fusion, multi-field retrieval, external priors, storage, vector and spatial indexes, SQL compilation and execution, planning, graph workloads, RPQs, top-K pruning, relevance, and analytical comparisons.

[`scripts/check-benchmark-coverage.py`](../../scripts/check-benchmark-coverage.py) discovers benchmark entrypoints from the current workspace, requires the manifest to match that set exactly, rejects duplicate or unsafe paths, and checks that every evidence token is still present in its declared Rust source. CI runs this contract on every change.

This is a surface-coverage contract. It does not claim equivalent algorithms, fixtures, throughput, or latency across systems. Numerical regression and comparative performance claims require versioned fixtures, executable hashes, same-machine artifacts, and the separate methodology in [`performance.md`](../design/performance.md).

## Verification

```sh
python3 scripts/check-benchmark-coverage.py
cargo fmt --all --check
cargo check --workspace --all-targets --locked
cargo bench --workspace --no-run --locked
```

A new benchmark entrypoint must add a manifest entry in the same review. A renamed or removed workload must update its evidence tokens deliberately.
