# UQA-RS

Rust implementation of the Unified Query Algebra (UQA): a multi-paradigm database engine that unifies relational, text retrieval, vector search, graph query, and geospatial paradigms under a single algebraic structure.

This is a port of the Python reference implementation at [`cognica-io/uqa`](https://github.com/cognica-io/uqa). The theoretical foundation is set out in five papers (see `docs/papers/` in the upstream repo); the formal contract this Rust port preserves is documented in [`docs/plans/0001-uqa-python-to-rust-port.md`](docs/plans/0001-uqa-python-to-rust-port.md).

## Status

Early scaffolding. The Cargo workspace and `uqa-core` Boolean algebra are in place; everything else is stubbed out. See the master plan for the delivery roadmap.

## Build

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

## Layout

The workspace is split into small crates that mirror the Python package structure:

```
crates/
  uqa-core         posting list, predicates, value types
  uqa-analysis     tokenizers, char/token filters, analyzers
  uqa-storage      document store, inverted index, IVF, B-tree, R*Tree, catalog
  uqa-scoring      BM25, Bayesian BM25, WAND, BMW, calibration
  uqa-fusion       log-odds, attention, learned fusion
  uqa-operators    Operator trait + primitives, boolean, hybrid, aggregation
  uqa-graph        graph store, RPQ, Cypher (lexer/parser/AST/compiler)
  uqa-joins        hash, outer, semi, sort-merge, cross-paradigm joins
  uqa-planner      cost model, cardinality, DPccp join enumeration
  uqa-execution    Volcano physical operators, Arrow batches
  uqa-sql          libpg_query-based SQL compiler
  uqa-fdw          foreign data wrappers (DuckDB, Arrow IPC)
  uqa-engine       Engine struct, catalog restore, transactions
  uqa-api          fluent QueryBuilder
  uqa-cli          usql REPL
```

## License

AGPL-3.0-only. See [LICENSE](LICENSE).
