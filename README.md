# UQA-RS

Rust implementation of the Unified Query Algebra (UQA): a multi-paradigm database engine that unifies relational, text retrieval, vector search, graph query, and geospatial paradigms under a single algebraic structure.

This is a port of the Python reference implementation at [`cognica-io/uqa`](https://github.com/cognica-io/uqa). The theoretical foundation is set out in five papers (see `docs/papers/` in the upstream repo); the formal contract this Rust port preserves is documented in [`docs/plans/0001-uqa-python-to-rust-port.md`](docs/plans/0001-uqa-python-to-rust-port.md).

## Status

The Rust port covers all eleven phases of the master plan, with one or more
working slices in each crate:

* **Algebra and storage** (`uqa-core`, `uqa-storage`) — Boolean posting-list
  algebra with property tests for the 11 axioms, in-memory and SQLite-backed
  document/inverted/vector indexes with crash-safe persistence.
* **Scoring and fusion** (`uqa-scoring`, `uqa-fusion`) — BM25, Bayesian BM25,
  WAND/BMW, multi-field, query features, learned and attention fusion,
  parameter learner.
* **Operators** (`uqa-operators`) — Boolean, hybrid, primitive, multi-stage,
  progressive-fusion, sparse, hierarchical, deep-fusion (with Propagate /
  Conv / Pool / Attention graph layers).
* **Graph** (`uqa-graph`) — `MemoryGraphStore`, RPQ NFA/DFA, full openCypher
  front-end (lexer, AST, recursive-descent parser), read + mutating
  executors, centrality, message passing, path index, embeddings,
  versioned store with delta rollback, temporal traversal.
* **Joins** (`uqa-joins`) — relational, text-similarity (Jaccard),
  vector-similarity, hybrid, graph-driven, cross-paradigm.
* **SQL** (`uqa-sql`, `uqa-engine`) — libpg_query-backed parser,
  CREATE/INSERT/SELECT/UPDATE/DELETE, JOINs, GROUP BY, window functions,
  CTEs (recursive), function registry hooks for `text_match`, `knn_match`,
  `fuse_log_odds`, `multi_field_match`, `staged_retrieval`, `graph_*`,
  `deep_predict`.
* **API surface** (`uqa-fdw`, `uqa-api`, `uqa-cli`) — pushdown FDW handler
  trait + memory implementation, fluent `QueryBuilder`, interactive `usql`
  REPL with meta commands.
* **Parity and benchmarks** — golden-file SQL harness, `criterion` benches
  for posting-list ops, BM25/Bayesian BM25 scoring, KNN, RPQ, end-to-end SQL
  text match, multi-term WAND territory, and inner join.

The master plan in [`docs/plans/0001-uqa-python-to-rust-port.md`](docs/plans/0001-uqa-python-to-rust-port.md)
remains the source of truth for what each phase ships and what is
explicitly deferred (e.g. DPccp join enumeration, the 2x-vs-Python
performance gate measurement on a 1M-doc corpus).

## Build

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo doc --workspace --no-deps
cargo deny --workspace check     # cargo install cargo-deny --locked
```

## Quickstart

```rust
use uqa_engine::Engine;

let engine = Engine::new();
engine.sql(
    "CREATE TABLE notes (id INTEGER PRIMARY KEY, title TEXT, body TEXT)",
    &[],
)?;
engine.sql(
    "INSERT INTO notes (id, title, body) VALUES \
     (1, 'rust async', 'futures and tokio'), \
     (2, 'python web', 'flask and django')",
    &[],
)?;
let result = engine.sql(
    "SELECT id, _score FROM notes \
     WHERE text_match(body, 'tokio') \
     ORDER BY _score DESC LIMIT 5",
    &[],
)?;
```

For an interactive prompt, build the CLI with `cargo run -p uqa-cli --bin
usql` and pipe a SQL script in or type at the `usql>` prompt.

## Examples

Two runnable examples live under `crates/uqa-engine/examples/`:

```sh
cargo run -p uqa-engine --example text_search
cargo run -p uqa-engine --example hybrid_search
```

`text_search` walks a CREATE -> INSERT -> SELECT pipeline through
`text_match`. `hybrid_search` fuses text and vector signals via
log-odds (Paper 4) using `Engine::hybrid_search`.

## Benchmarks

Criterion benches live under each crate's `benches/` directory:

```sh
cargo bench -p uqa-core    --bench posting_list
cargo bench -p uqa-scoring --bench bm25
cargo bench -p uqa-scoring --bench calibration
cargo bench -p uqa-storage --bench spatial
cargo bench -p uqa-engine  --bench sql_e2e
cargo bench -p uqa-engine  --bench sql_1m
cargo bench -p uqa-engine  --bench knn
cargo bench -p uqa-engine  --bench join
cargo bench -p uqa-engine  --bench relevance
cargo bench -p uqa-graph   --bench rpq
```

The `relevance` bench replays the BEIR-style fixture under every
declared scorer and asserts NDCG@K and MAP@K stay above the floor in
`tests/parity/beir_fixture.json`. Reference numbers measured on
Apple silicon live in [`docs/design/performance.md`](docs/design/performance.md).

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

## Release notes

See [CHANGELOG.md](CHANGELOG.md) for what is in each release and what
is still open.

## License

AGPL-3.0-only. See [LICENSE](LICENSE).
