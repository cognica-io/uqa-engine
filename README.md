# UQA-RS

UQA-RS is the Rust workspace for the Unified Query Algebra (UQA): a multi-paradigm database engine that brings relational SQL, text retrieval, vector search, graph traversal, geospatial indexing, probabilistic scoring, and learned fusion under one execution model.

The workspace is organized as an embeddable engine plus reusable crates for storage, SQL compilation, scoring, graph processing, ML, APIs, and the `usql` interactive shell.

## Status

The current implementation covers the main UQA stack end to end. Working slices in each crate:

- **Algebra and storage** (`uqa-core`, `uqa-storage`) — Boolean posting-list algebra with property tests for the 11 axioms, in-memory storage, persistent document/inverted/vector indexes, SQLCipher-backed catalogs, schema-neutral compressed containers with zstd or LZ4 codecs, crash-safe persistence, `_scoring_params` catalog table for Bayesian calibration.
- **Scoring and fusion** (`uqa-scoring`, `uqa-fusion`) — BM25, Bayesian BM25, WAND/BMW, multi-field, query features, learned and attention fusion, parameter learner. BMW pruning bound now folds `block_max` over the remaining blocks so no candidate is wrongly skipped.
- **Operators** (`uqa-operators`) — Boolean, hybrid, primitive, multi-stage, progressive-fusion, sparse, hierarchical. `PathSegment` / `PathExpr` live in `uqa-core` so any storage-layer trait can use them.
- **ML** (`uqa-ml`) — serializable `DeepModel` specs, deep-fusion CPU inference with dense, CNN, RNN, LSTM, graph, pooling, and attention layers, analytical `deep_learn` training, feature-batch `deep_predict`, and optional Apple MLX support through the official `mlx-c` API behind the `mlx` feature.
- **Graph** (`uqa-graph`) — `MemoryGraphStore` and `SQLiteGraphStore`, RPQ NFA/DFA, full openCypher front-end (lexer, AST, recursive-descent parser), read + mutating executors, centrality, message passing, path index, embeddings, versioned store with delta rollback, temporal traversal. `GraphStore::vertices` / `edges` snapshot accessors complete the trait.
- **Joins** (`uqa-joins`) — relational, text-similarity (Jaccard), vector-similarity, hybrid, graph-driven, cross-paradigm.
- **SQL** (`uqa-sql`, `uqa-engine`) — libpg*query-backed parser, CREATE/INSERT/SELECT/UPDATE/DELETE, JOINs (incl. LATERAL), GROUP BY, GROUPING SETS / ROLLUP / CUBE, window functions with ROWS/RANGE FRAMEs, recursive CTEs, sequences (`CREATE SEQUENCE` / `nextval` / `currval` / `setval`), CHECK / FOREIGN KEY / DEFAULT validators, DROP CASCADE, UPDATE FROM / DELETE USING, scalar / `IN(SELECT)` / `EXISTS` subqueries, PREPARE / EXECUTE / DEALLOCATE, CTAS, MERGE, EXPLAIN, ORDER BY ... NULLS FIRST/LAST, SET search_path, CREATE TABLE AS SELECT, CREATE ANALYZER / SET TABLE ANALYZER DDL, CREATE FOREIGN SERVER / FOREIGN TABLE DDL, table functions (`generate_series`, `unnest`, `regexp_split_to_table`, `json_each`, `json_array_elements`, `rpq(expr, start, graph)`), `information_schema` + `pg_catalog` views, function registry hooks for `text_match`, `bayesian_match`, `knn_match`, `fuse_log_odds`, `multi_field_match`, `staged_retrieval`, `attention`, `learned_fusion`, `calibrated_vector_match`, `uqa_highlight`, `uqa_facets`, `graph*\*`, `traverse_match`, `temporal_traverse`, `deep_predict`, `deep_learn`.
- **Engine API** (`uqa-engine`) — schema-aware table store, persistent catalog restore via `Engine::open`, encrypted restore via `Engine::open_encrypted`, compressed restore via `Engine::open_compressed`, compressed-encrypted restore via `Engine::open_compressed_encrypted`, lazy attach to persisted inverted/vector indexes on reopen, hash-join optimizer, `uqa-ml` model JSON persistence and SQL adapters, named graph workspaces, `Engine::run_cypher` for CREATE/MERGE/SET/DELETE/UNWIND Cypher, `apply_graph_delta`, `build_path_index` / `drop_path_index` / `get_path_index`, scoring-params save/load round-tripped through the catalog, `begin / commit / rollback / savepoint / rollback_to_savepoint / close` shortcuts wired to the backing storage transaction, `transaction` / `sql_batch` helpers for grouped writes, `search_path` + `tables_in_schema` accessors, `set_variable` driver behind SQL `SET search_path`.
- **API surface** (`uqa-fdw`, `uqa-api`, `uqa-cli`) — pushdown FDW handler trait + memory implementation, fluent `QueryBuilder` covering text / vector / hybrid / Bayesian / fusion / graph / RPQ / highlight / facets / `EXPLAIN`, interactive `usql` REPL with meta commands (`\dt`, `\d`, `\di`, `\dF`, `\dS`, `\dg`, `\ds`, `\timing`, `\x`, `\open`, `\new`, `\where`, `\history`, `\run`), prompt history, suggestions, table/column/function completion, and case-insensitive SQL syntax highlighting.
- **Wire protocol** (`uqa-pg-wire`) — network-independent PostgreSQL v3 frontend decoders and backend encoders for startup, SSL/GSSENC negotiation, authentication, parse/bind/execute/describe/close/sync/terminate, row descriptions, data rows, command completion, errors, notices, and ready-for-query status.
- **Tests and benchmarks** — golden-file SQL harness, `criterion` benches for posting-list ops, BM25/Bayesian BM25 scoring, KNN, RPQ, end-to-end SQL text match, multi-term WAND territory, and inner join. Integration test groups exercise the end-to-end pipeline across storage, SQL, graph, scoring, ML, and CLI behavior.

## Storage and Persistence

`Engine::open(path)` opens a persistent UQA catalog. The built-in persistent backend is SQLite-based today, with SQLCipher and compressed-container variants exposed through `Engine::open_encrypted`, `Engine::open_compressed`, and `Engine::open_compressed_encrypted`; the engine talks through `PersistentStorageBackend` so storage construction stays behind a backend boundary.

Persistent catalogs store table schemas, documents, GIN-style inverted indexes, vector records, IVF metadata, named analyzers, named graphs, scoring parameters, foreign server/table definitions, model specs, and column statistics. Reopen paths attach to persisted inverted/vector index metadata lazily instead of rebuilding search indexes just because a database was opened.

Statistics are invalidated by table writes and schema changes, then recomputed lazily when SQL planning, `Engine::column_stats`, or `usql \ds` needs them. `ANALYZE` is still available when callers want eager refresh.

## SQL, Graph, and Retrieval

The SQL surface is PostgreSQL-oriented: libpg_query parsing, `information_schema` / `pg_catalog` virtual views, `search_path`, `MERGE`, CTAS, recursive CTEs, window frames, grouping sets, sequences, `PREPARE` / `EXECUTE`, subqueries, LATERAL joins, referential actions, foreign table DDL, analyzer DDL, and table functions including `generate_series`, JSON expansion, `rpq`, and Apache AGE-compatible `cypher`.

Retrieval functions run inside SQL predicates and projections: `text_match`, `fts_match`, `bayesian_match`, `knn_match`, `fuse_log_odds`, `multi_field_match`, `staged_retrieval`, `attention`, `learned_fusion`, `calibrated_vector_match`, `sparse_threshold`, `score_bm25`, `score_bayesian_bm25`, `uqa_highlight`, `uqa_facets`, `graph_pagerank`, `graph_traverse`, `graph_neighbors`, `traverse_match`, `temporal_traverse`, `deep_predict`, and `deep_learn`.

Graph support is available through both graph APIs and SQL. `Engine::run_cypher` executes read/write Cypher over named graph workspaces, while SQL can call `cypher('graph', $$ ... $$)` or `ag_catalog.cypher(...)` as a table function. RPQ traversal, path indexes, graph deltas, temporal traversal, PageRank, HITS, Betweenness, embeddings, and message passing live in `uqa-graph`.

## ML

`uqa-ml` owns model specs, training data types, inference backends, and deep-fusion execution. CPU inference is always available and supports dense, CNN1D, CNN2D, RNN, LSTM, graph propagation, convolution, pooling, batch norm, dropout, global pooling, softmax, and attention layers.

`deep_learn` performs analytical training for a named model from tabular feature/label data, and `deep_predict` scores rows or explicit feature batches with persisted model specs. With the optional `mlx` feature, `uqa-ml` can use Apple's MLX C API for backend operations while keeping the same `MLBackend` trait boundary.

## Protocol and Embedding

`uqa-engine` is the direct embedded API. `uqa-api` wraps common read flows in a fluent `QueryBuilder`, and `uqa-fdw` provides pushdown-oriented foreign data wrapper traits for DuckDB, Arrow IPC, and in-memory handlers.

`uqa-pg-wire` is intentionally network-independent. It parses and encodes PostgreSQL wire messages but leaves sockets, tasks, TLS policy, authentication storage, query planning, and SQL execution to the embedding server.

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
     (2, 'web app', 'forms and routing')",
    &[],
)?;
let result = engine.sql(
    "SELECT id, _score FROM notes \
     WHERE text_match(body, 'tokio') \
     ORDER BY _score DESC LIMIT 5",
    &[],
)?;
```

For the interactive shell, run:

```sh
cargo run -p uqa-cli --bin usql
```

To install the shell locally:

```sh
cargo install --locked --path crates/uqa-cli --bin usql
```

`usql --db <path>` opens a persistent UQA catalog, `-c "SELECT ..."` executes a command and exits, and positional `.sql` files run before the REPL. Backslash commands such as `\?`, `\dt`, `\d`, `\di`, `\dF`, `\dS`, `\dg`, `\ds`, `\x`, `\o`, `\timing`, `\where`, `\history`, and `\reset` inspect or control the session. On a TTY the shell uses readline editing with persistent prompt history, history suggestions, table/column/function completion, and case-insensitive ANSI SQL syntax highlighting; UQA function completions come from `uqa-sql`'s function registry rather than a duplicated CLI list.

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

The workspace is split into small crates by execution boundary:

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
  uqa-pg-wire      PostgreSQL wire protocol primitives
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
