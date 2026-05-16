# UQA-RS

Rust implementation of the Unified Query Algebra (UQA): a multi-paradigm database engine that unifies relational, text retrieval, vector search, graph query, and geospatial paradigms under a single algebraic structure.

This is a port of the Python reference implementation at [`cognica-io/uqa`](https://github.com/cognica-io/uqa). The theoretical foundation is set out in five papers (see `docs/papers/` in the upstream repo); the formal contract this Rust port preserves is documented in [`docs/plans/0001-uqa-python-to-rust-port.md`](docs/plans/0001-uqa-python-to-rust-port.md).


## Status

The Rust port covers all eleven phases of the master plan, plus a post-phase parity sweep (Round 2) that brings the SQL surface and engine API up to one-to-one parity with the Python reference. Working slices in each crate:

* **Algebra and storage** (`uqa-core`, `uqa-storage`) — Boolean posting-list algebra with property tests for the 11 axioms, in-memory and SQLCipher-backed SQLite document/inverted/vector indexes, schema-neutral compressed SQLite containers with zstd or LZ4 codecs, crash-safe persistence, `_scoring_params` catalog table for Bayesian calibration.
* **Scoring and fusion** (`uqa-scoring`, `uqa-fusion`) — BM25, Bayesian BM25, WAND/BMW, multi-field, query features, learned and attention fusion, parameter learner. BMW pruning bound now folds `block_max` over the remaining blocks so no candidate is wrongly skipped.
* **Operators** (`uqa-operators`) — Boolean, hybrid, primitive, multi-stage, progressive-fusion, sparse, hierarchical. `PathSegment` / `PathExpr` live in `uqa-core` so any storage-layer trait can use them.
* **ML** (`uqa-ml`) — serializable `DeepModel` specs, deep-fusion CPU inference, analytical `deep_learn` training, feature-batch `deep_predict`, and optional Apple MLX support through the official `mlx-c` API behind the `mlx` feature.
* **Graph** (`uqa-graph`) — `MemoryGraphStore` and `SQLiteGraphStore`, RPQ NFA/DFA, full openCypher front-end (lexer, AST, recursive-descent parser), read + mutating executors, centrality, message passing, path index, embeddings, versioned store with delta rollback, temporal traversal. `GraphStore::vertices` / `edges` snapshot accessors complete the trait.
* **Joins** (`uqa-joins`) — relational, text-similarity (Jaccard), vector-similarity, hybrid, graph-driven, cross-paradigm.
* **SQL** (`uqa-sql`, `uqa-engine`) — libpg_query-backed parser, CREATE/INSERT/SELECT/UPDATE/DELETE, JOINs (incl. LATERAL), GROUP BY, GROUPING SETS / ROLLUP / CUBE, window functions with ROWS/RANGE FRAMEs, recursive CTEs, sequences (`CREATE SEQUENCE` / `nextval` / `currval` / `setval`), CHECK / FOREIGN KEY / DEFAULT validators, DROP CASCADE, UPDATE FROM / DELETE USING, scalar / `IN(SELECT)` / `EXISTS` subqueries, PREPARE / EXECUTE / DEALLOCATE, CTAS, MERGE, EXPLAIN, ORDER BY ... NULLS FIRST/LAST, SET search_path, CREATE TABLE AS SELECT, CREATE ANALYZER / SET TABLE ANALYZER DDL, CREATE FOREIGN SERVER / FOREIGN TABLE DDL, table functions (`generate_series`, `unnest`, `regexp_split_to_table`, `json_each`, `json_array_elements`, `rpq(expr, start, graph)`), `information_schema` + `pg_catalog` views, function registry hooks for `text_match`, `bayesian_match`, `knn_match`, `fuse_log_odds`, `multi_field_match`, `staged_retrieval`, `attention`, `learned_fusion`, `calibrated_vector_match`, `uqa_highlight`, `uqa_facets`, `graph_*`, `traverse_match`, `temporal_traverse`, `deep_predict`, `deep_learn`.
* **Engine API** (`uqa-engine`) — schema-aware table store, `SQLite` / SQLCipher / compressed SQLite catalog restore via `Engine::open`, `Engine::open_encrypted`, `Engine::open_compressed`, and `Engine::open_compressed_encrypted`, hash-join optimizer, `uqa-ml` model JSON persistence and SQL adapters, named graph workspaces, `Engine::run_cypher` for CREATE/MERGE/SET/DELETE/UNWIND Cypher, `apply_graph_delta`, `build_path_index` / `drop_path_index` / `get_path_index`, scoring-params save/load round-tripped through the catalog, `begin / commit / rollback / savepoint / rollback_to_savepoint / close` shortcuts wired to the backing storage transaction, `transaction` / `sql_batch` helpers for grouped writes, `search_path` + `tables_in_schema` accessors, `set_variable` driver behind SQL `SET search_path`.
* **API surface** (`uqa-fdw`, `uqa-api`, `uqa-cli`) — pushdown FDW handler trait + memory implementation, fluent `QueryBuilder` covering text / vector / hybrid / Bayesian / fusion / graph / RPQ / highlight / facets / `EXPLAIN`, interactive `usql` REPL with meta commands (`\dt`, `\describe`, `\stats`, `\dg`, `\dfs`, `\dft`, `\da`, `\timing`, `\expanded`, `\open`, `\new`, `\run`).
* **Parity and benchmarks** — golden-file SQL harness, `criterion` benches for posting-list ops, BM25/Bayesian BM25 scoring, KNN, RPQ, end-to-end SQL text match, multi-term WAND territory, and inner join. ~105 integration test groups exercise the end-to-end pipeline.

The master plan in [`docs/plans/0001-uqa-python-to-rust-port.md`](docs/plans/0001-uqa-python-to-rust-port.md) remains the source of truth for what each phase ships and what is explicitly deferred (e.g. DPccp join enumeration, the 2x-vs-Python performance gate measurement on a 1M-doc corpus).

## Build

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo doc --workspace --no-deps
cargo deny --workspace check     # cargo install cargo-deny --locked
```

`uqa-ml` builds with the CPU backend by default. The optional `mlx` feature links directly to Apple's official `mlx-c` system library; install `mlx` and `mlx-c` and expose their library directory through `MLX_C_LIB_DIR`, `MLX_LIB_DIR`, `HOMEBREW_PREFIX`, or the default Homebrew prefixes.

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

For an interactive prompt, build the CLI with `cargo run -p uqa-cli --bin usql` and pipe a SQL script in or type at the `usql>` prompt.

## Examples

Four runnable examples live under `crates/uqa-engine/examples/`:

```sh
cargo run -p uqa-engine --example text_search
cargo run -p uqa-engine --example hybrid_search
cargo run -p uqa-engine --example sqlcipher_encrypted_catalog
cargo run -p uqa-engine --example compressed_encrypted_catalog
```

`text_search` walks a CREATE -> INSERT -> SELECT pipeline through `text_match`. `hybrid_search` fuses text and vector signals via log-odds (Paper 4) using `Engine::hybrid_search`. `sqlcipher_encrypted_catalog` creates an encrypted catalog with `Engine::open_encrypted`, reopens it with the same key, and verifies wrong-key/plaintext opens fail. `compressed_encrypted_catalog` uses the schema-neutral compressed VFS path, where chunks are compressed with the selected codec before encryption.

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

The `relevance` bench replays the BEIR-style fixture under every declared scorer and asserts NDCG@K and MAP@K stay above the floor in `tests/parity/beir_fixture.json`. Reference numbers measured on Apple silicon live in [`docs/design/performance.md`](docs/design/performance.md).

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

See [HISTORY.md](HISTORY.md) for what is in each release and what is still open.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for the development gates, test conventions, and PR guidelines.

## License

AGPL-3.0-only. See [LICENSE](LICENSE).
