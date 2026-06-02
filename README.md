# UQA-RS

UQA-RS is the Rust workspace for the Unified Query Algebra (UQA): a multi-paradigm database engine that brings relational SQL, text retrieval, vector search, graph traversal, geospatial indexing, probabilistic scoring, learned fusion, and embeddable storage under one execution model.

The workspace is organized as an embeddable engine plus reusable crates for storage, SQL compilation, scoring, graph processing, ML, APIs, PostgreSQL wire-protocol primitives, and the `usql` interactive shell.

## Status

The current implementation covers the main UQA stack end to end. Working slices in each crate:

- **Algebra and storage** (`uqa-core`, `uqa-storage`, `uqa-storage-sqlite`): Boolean posting-list algebra with property tests for the 11 axioms, in-memory storage, SQLite-backed document/inverted/vector indexes, persistent catalogs, SQLCipher catalogs, compressed SQLite containers with zstd or LZ4 codecs, and backend-neutral KeyValue catalog, document, inverted-index, and vector-index implementations. `uqa-storage-sqlite` provides the physical SQLite `KeyValueStore`.
- **Scoring and fusion** (`uqa-scoring`, `uqa-fusion`): BM25, Bayesian BM25, WAND/BMW, calibration metrics, external priors, multi-field scoring, query features, log-odds fusion, learned fusion, attention fusion, and parameter learning. BMW pruning folds `block_max` over remaining blocks so later-block candidates are not skipped.
- **Operators** (`uqa-operators`): Boolean, hybrid, primitive, multi-stage, progressive-fusion, sparse, hierarchical, and deep-fusion operators. `PathSegment` and `PathExpr` live in `uqa-core` so storage-layer traits can use them without depending on higher crates.
- **ML** (`uqa-ml`): Serializable `DeepModel` specs, CPU deep-fusion inference, analytical `deep_learn` training, feature-batch `deep_predict`, and optional Apple MLX support through the official `mlx-c` API behind the `mlx` feature.
- **Graph** (`uqa-graph`): `MemoryGraphStore` and `SQLiteGraphStore`, named graphs, RPQ NFA/DFA, openCypher lexer/AST/parser, read and mutating Cypher executors, centrality, message passing, path indexes, embeddings, versioned store deltas, rollback, and temporal traversal.
- **Joins and planning** (`uqa-joins`, `uqa-planner`, `uqa-execution`): Relational, text-similarity, vector-similarity, hybrid, graph-driven, and cross-paradigm joins; cardinality and cost models; DPccp join enumeration; optimizer rewrites; and Volcano-style physical execution.
- **SQL** (`uqa-sql`, `uqa-engine`): A `libpg_query` backed PostgreSQL-oriented compiler and engine for CREATE/ALTER/DROP, INSERT/UPDATE/DELETE, `RETURNING`, `ON CONFLICT`, CTAS, MERGE, JOINs including LATERAL, recursive CTEs, windows, grouping sets, sequences, views, schemas, `search_path`, `SHOW`, PostgreSQL 17 runtime defaults, `DISCARD`, `TRUNCATE`, JSON/JSONB, arrays, temporal types, numeric types, `BYTEA`, constraints, referential actions, analyzer DDL, foreign server/table DDL, Rust-backed scalar/table/aggregate function registration, `information_schema`, and `pg_catalog` virtual views.
- **SQL retrieval and graph functions** (`uqa-sql`, `uqa-engine`): `text_match`, `fts_match`, `bayesian_match`, `bayesian_match_with_prior`, `knn_match`, `fuse_log_odds`, `multi_field_match`, `staged_retrieval`, `attention`, `learned_fusion`, `calibrated_vector_match`, `sparse_threshold`, `score_bm25`, `score_bayesian_bm25`, `uqa_highlight`, `uqa_facets`, `graph_pagerank`, `graph_hits`, `graph_betweenness`, `graph_traverse`, `graph_neighbors`, `graph_edges`, `graph_create`, `graph_drop`, `traverse_match`, `temporal_traverse`, `rpq`, `deep_predict`, and `deep_learn`.
- **Engine API** (`uqa-engine`): `Engine::open`, encrypted and compressed open variants, `Engine::from_persistent_backends`, schema-aware table storage, persistent catalog restore, lazy attach to persisted inverted/vector indexes, TENSOR chunk indexing, hash-join optimization, model persistence, named graph workspaces, `Engine::run_cypher`, graph deltas, path-index lifecycle, scoring-parameter persistence, transaction helpers, search-path accessors, prepared statements, cancellation, and `sql_batch`.
- **API surface** (`uqa-fdw`, `uqa-api`, `uqa-cli`): Pushdown FDW handler traits, a fluent `QueryBuilder` for text/vector/hybrid/Bayesian/fusion/graph/RPQ/highlight/facet/ML/explain flows, and the `usql` REPL with script execution, persistent history, suggestions, completion, highlighting, and meta commands.
- **Wire protocol** (`uqa-pg-wire`): Network-independent PostgreSQL v3 frontend decoders and backend encoders for startup, SSL/GSSENC negotiation, authentication, parse/bind/execute/describe/close/sync/terminate, row descriptions, data rows, command completion, errors, notices, and ready-for-query status.
- **Tests and benchmarks**: Golden SQL fixtures, BEIR-style relevance gates, PostgreSQL and Apache AGE compatibility matrices, SQLite KeyValue backend coverage, fuzz targets, and Criterion benches across storage, scoring, operators, fusion, planner, SQL, graph, and relevance workloads.

## Storage and Persistence

`Engine::open(path)` opens a persistent UQA catalog. The default persistent backend is SQLite-based. SQLCipher and compressed-container variants are exposed through `Engine::open_encrypted`, `Engine::open_compressed`, and `Engine::open_compressed_encrypted`.

Persistent catalogs store table schemas, documents, postings, GIN-style inverted indexes, vector and tensor records, IVF metadata, named analyzers, named graphs, graph members, path indexes, scoring parameters, foreign server/table definitions, model specs, sequences, views, schemas, catalog indexes, and column statistics. Reopen paths attach to persisted inverted/vector index metadata lazily instead of rebuilding search indexes just because a database was opened.

`Engine::from_persistent_backends(catalog, backend)` is the storage-neutral constructor. The relational SQLite catalog path and the KeyValue path both plug into it through `CatalogFacade` and `PersistentStorageBackend`. `uqa-storage` includes `MemoryKeyValueStore`, `KeyValueCatalog`, and `KeyValueStorageBackend`; `uqa-storage-sqlite` includes `SQLiteKeyValueStore` and `SQLiteKeyValueStorage`.

Statistics are invalidated by table writes and schema changes, then recomputed lazily when SQL planning, `Engine::column_stats`, or `usql \ds` needs them. `ANALYZE` remains available for eager refresh.

## SQL, Graph, and Retrieval

The SQL surface is PostgreSQL-oriented: `libpg_query` parsing, schemas, `search_path`, `information_schema` and `pg_catalog` views, JSON/JSONB operators, arrays, temporal types, numeric types, `BYTEA`, constraints, referential actions, MERGE, CTAS, recursive CTEs, window frames, grouping sets, sequences, prepared statements, subqueries, LATERAL joins, foreign table DDL, analyzer DDL, and table functions including `generate_series`, `unnest`, `regexp_split_to_table`, `json_each`, `json_each_text`, `json_array_elements`, `rpq`, and Apache AGE-compatible `cypher`.

Runtime compatibility covers PostgreSQL-style `SHOW` and `pg_catalog.pg_settings` rows for `server_version`, `server_encoding`, `client_encoding`, `DateStyle`, `TimeZone`, and `search_path`. `SHOW server_version` reports `17.0-uqa`, and `SET` / `SHOW` runtime parameter lookup accepts case-insensitive parameter names while preserving session overrides.

Embedding applications can extend SQL with Rust implementations by calling `Engine::register_scalar_function`, `Engine::register_table_function`, or `Engine::register_aggregate_function`. Registered scalar functions participate in projection and filter evaluation, registered table functions run in `FROM` with table and column aliases, and registered aggregate functions participate in `GROUP BY`; unordered aggregates stream rows directly into per-group state, while `ORDER BY` aggregates spill sorted runs to temp files when the ordered input buffer exceeds the in-memory threshold.

Graph support is available through both graph APIs and SQL. `Engine::run_cypher` executes read/write Cypher over named graph workspaces, while SQL can call `cypher` or `ag_catalog.cypher` as an Apache AGE-style table function with a required record definition:

```sql
SELECT *
FROM cypher('social', $$
    MATCH (n:Person)
    RETURN n.name AS name
$$) AS (name agtype);
```

`create_graph(name)` and `drop_graph(name [, cascade])` are available as Apache AGE-compatible aliases for graph lifecycle operations. `drop_graph(name, false)` rejects non-empty graphs; `drop_graph(name, true)` drops graph data. RPQ traversal, path indexes, graph deltas, temporal traversal, PageRank, HITS, Betweenness, graph edges, embeddings, and message passing live in `uqa-graph`.

Vector search supports both `VECTOR(N)` and `TENSOR(N)` columns. Tensors store multiple fixed-width embeddings per row, score KNN against the best chunk for that row, train IVF on chunk vectors rather than row count, and persist through SQLite reopen. `SQLParam::tensor` supplies tensor parameters from embedded callers.

## ML

`uqa-ml` owns model specs, training data types, inference backends, and deep-fusion execution. CPU inference is always available and supports dense, CNN1D, CNN2D, RNN, LSTM, graph propagation, convolution, pooling, batch norm, dropout, global pooling, softmax, and attention layers.

`deep_learn` performs analytical training for a named model from tabular feature/label data, and `deep_predict` scores rows or explicit feature batches with persisted model specs. With the optional `mlx` feature, `uqa-ml` can use Apple's MLX C API for backend operations while keeping the same `MLBackend` trait boundary.

## Protocol and Embedding

`uqa-engine` is the direct embedded API. `uqa-api` wraps common read flows in a fluent `QueryBuilder`, and `uqa-fdw` provides pushdown-oriented foreign data wrapper traits for DuckDB, Arrow IPC, and in-memory handlers.

`uqa-pg-wire` parses and encodes PostgreSQL wire messages, but it intentionally leaves sockets, task scheduling, TLS policy, authentication storage, query planning, and SQL execution to the embedding server.

## Build

```sh
cargo build --workspace --locked
cargo test --workspace --all-targets --locked
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo doc --workspace --no-deps
cargo deny --workspace check
cargo bench --workspace --no-run --locked
```

`uqa-ml` builds with the CPU backend by default. The optional `mlx` feature links directly to Apple's official `mlx-c` system library; install `mlx` and `mlx-c` and expose their library directory through `MLX_C_LIB_DIR`, `MLX_LIB_DIR`, `HOMEBREW_PREFIX`, or the default Homebrew prefixes.

## Quickstart

```rust
use uqa_engine::Engine;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = Engine::new();
    engine.sql(
        "CREATE TABLE notes (id INTEGER PRIMARY KEY, title TEXT, body TEXT)",
        &[],
    )?;
    engine.sql(
        "INSERT INTO notes (id, title, body) VALUES
         (1, 'rust async', 'futures and tokio'),
         (2, 'web app', 'forms and routing')",
        &[],
    )?;
    let result = engine.sql(
        "SELECT id, _score FROM notes
         WHERE text_match(body, 'tokio')
         ORDER BY _score DESC LIMIT 5",
        &[],
    )?;
    println!("{result:?}");
    Ok(())
}
```

For the interactive shell, run:

```sh
cargo run -p uqa-cli --bin usql
```

To install the shell locally:

```sh
cargo install --locked --path crates/uqa-cli --bin usql
```

`usql --db <path>` opens a persistent UQA catalog, `-c "SELECT ..."` executes a command and exits, and positional `.sql` files run before the REPL when stdin is interactive. Backslash commands include `\?`, `\dt`, `\d`, `\di`, `\dF`, `\dS`, `\dg`, `\ds`, `\x`, `\o`, `\timing`, `\reset`, `\where`, `\history`, `\run`, `\open`, `\new`, and `\migrate-python-db`. On a TTY the shell uses readline editing with persistent prompt history, history suggestions, table/column/function completion, and case-insensitive ANSI SQL syntax highlighting.

Python UQA catalogs can be migrated through either the CLI entrypoint or the REPL command:

```sh
cargo run -p uqa-cli --bin usql -- migrate-python-db ../uqa migrated.uqa
```

Inside `usql`, the equivalent command is:

```text
\migrate-python-db ../uqa migrated.uqa
```

## Examples

Runnable examples live under `crates/uqa-engine/examples/`:

```sh
cargo run -p uqa-engine --example text_search
cargo run -p uqa-engine --example hybrid_search
cargo run -p uqa-engine --example sqlcipher_encrypted_catalog
cargo run -p uqa-engine --example compressed_encrypted_catalog
```

`text_search` walks a CREATE, INSERT, SELECT pipeline through `text_match`. `hybrid_search` fuses text and vector signals via log odds. `sqlcipher_encrypted_catalog` creates an encrypted catalog with `Engine::open_encrypted`, reopens it with the same key, and verifies wrong-key/plaintext opens fail. `compressed_encrypted_catalog` uses the compressed VFS path, where chunks are compressed with the selected codec before encryption.

## Benchmarks

Criterion benches live under each crate's `benches/` directory. Use the full compile gate before changing benchmark surfaces:

```sh
cargo bench --workspace --no-run --locked
```

Common focused targets:

```sh
cargo bench -p uqa-core      --bench posting_list
cargo bench -p uqa-storage   --bench storage
cargo bench -p uqa-storage   --bench spatial
cargo bench -p uqa-scoring   --bench bm25
cargo bench -p uqa-scoring   --bench calibration
cargo bench -p uqa-scoring   --bench scoring
cargo bench -p uqa-scoring   --bench multi_field
cargo bench -p uqa-scoring   --bench external_prior
cargo bench -p uqa-scoring   --bench fusion_wand
cargo bench -p uqa-scoring   --bench beir_calibration
cargo bench -p uqa-fusion    --bench fusion
cargo bench -p uqa-operators --bench operators
cargo bench -p uqa-planner   --bench planner
cargo bench -p uqa-engine    --bench sql_e2e
cargo bench -p uqa-engine    --bench sql_1m
cargo bench -p uqa-engine    --bench sql_workloads
cargo bench -p uqa-engine    --bench graph_sql
cargo bench -p uqa-engine    --bench compiler
cargo bench -p uqa-engine    --bench execution
cargo bench -p uqa-engine    --bench knn
cargo bench -p uqa-engine    --bench join
cargo bench -p uqa-engine    --bench relevance
cargo bench -p uqa-graph     --bench rpq
cargo bench -p uqa-graph     --bench graph_workloads
```

The `relevance` and `beir_calibration` benches replay BEIR-style fixtures and assert ranking metrics stay above the declared floors. Reference numbers measured on Apple silicon live in [docs/design/performance.md](docs/design/performance.md).

## Layout

The workspace is split into small crates by execution boundary:

```text
crates/
  uqa-core            posting list, predicates, value types
  uqa-analysis        tokenizers, char/token filters, analyzers
  uqa-storage         document store, inverted index, IVF, B-tree, R*Tree, KeyValue
  uqa-storage-sqlite  SQLite-backed physical KeyValue store
  uqa-scoring         BM25, Bayesian BM25, WAND, BMW, calibration
  uqa-fusion          log odds, attention, learned fusion
  uqa-operators       Operator trait plus primitive, boolean, hybrid, aggregation
  uqa-graph           graph store, RPQ, Cypher, temporal graph operations
  uqa-joins           hash, outer, semi, sort-merge, cross-paradigm joins
  uqa-planner         cost model, cardinality, DPccp join enumeration
  uqa-execution       Volcano physical operators, Arrow batches
  uqa-sql             libpg_query-based PostgreSQL compiler
  uqa-pg-wire         PostgreSQL wire protocol primitives
  uqa-fdw             foreign data wrappers for DuckDB, Arrow IPC, memory
  uqa-engine          Engine struct, catalog restore, SQL, transactions
  uqa-api             fluent QueryBuilder
  uqa-cli             usql REPL
  uqa-ml              deep model specs, training, inference backends
```

## Release notes

See [HISTORY.md](HISTORY.md) for what is in each release and what is still open.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for the development gates, test conventions, and PR guidelines.

## License

AGPL-3.0-only. See [LICENSE](LICENSE).
