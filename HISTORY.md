# History

All notable changes to `uqa-rs` are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Encrypted catalogs:** `Engine::open_encrypted(path, key)` opens SQLCipher-backed catalogs through the same restore path as plaintext catalogs. The `sqlcipher_encrypted_catalog` example covers create, reopen, and wrong-key behavior.
- **Compressed catalogs:** `uqa-storage` registers a schema-neutral `uqa_compressed` SQLite VFS that stores byte-addressed SQLite pages as zstd or LZ4 chunks. `Engine::open_compressed` and `Engine::open_compressed_encrypted` expose compressed and compressed-encrypted catalog paths.
- **SQLite KeyValue backend:** `uqa-storage` now includes backend-neutral `KeyValueStore`, `KeyValueCatalog`, `KeyValueDocumentStore`, `KeyValueInvertedIndex`, and `KeyValueVectorIndex` implementations. `uqa-storage-sqlite` provides the physical SQLite `_key_value` store, and `Engine::from_persistent_backends` opens engines from either relational SQLite or KeyValue facade objects.
- **Standalone ML crate:** `uqa-ml` owns deep-model specs, training data, analytical `deep_learn`, feature-batch `deep_predict`, CPU inference, and the `MLBackend` trait. Dense, CNN1D, CNN2D, RNN, LSTM, graph, pooling, global pooling, softmax, batch norm, dropout, and attention layers are covered by the deep-fusion executor.
- **Apple MLX backend:** the optional `mlx` feature links to Apple's `mlx-c` API and exposes `MLXBackend` behind the same backend trait as CPU inference.
- **PostgreSQL wire protocol crate:** `uqa-pg-wire` provides network-independent PostgreSQL v3 startup/frontend decoders and backend encoders for SSL/GSSENC negotiation, authentication, parse/bind/execute/describe/close/sync/terminate, row descriptions, data rows, command completion, errors, notices, and ready-for-query status.
- **PostgreSQL compatibility matrix:** CI-grade tests now cover schemas, `search_path`, catalog views, JSON/JSONB operators, arrays, temporal types, prepared statements, sequences, views, DML, MERGE, CTAS, ALTER TABLE, TRUNCATE, SHOW, DISCARD, and DROP lifecycle behavior. The compatibility surface was also checked against a live PostgreSQL 17.10 container for shared SQL behavior.
- **AGE-style Cypher table function:** SQL can call `cypher('graph', $$ ... $$)` or `ag_catalog.cypher(...)` with an `AS (col agtype, ...)` record definition to execute Cypher against named graph workspaces and return table-function rows.
- **AGE graph lifecycle aliases:** `create_graph(name)` and `drop_graph(name [, cascade])` are available as Apache AGE-compatible aliases. `drop_graph(name, false)` rejects non-empty graphs, while `drop_graph(name, true)` drops graph data.
- **Tensor embeddings:** SQL supports `TENSOR(N)` columns and `SQLParam::tensor`. Tensor KNN scores each row against its best chunk, trains IVF with chunk counts, and preserves tensor IVF indexes across SQLite reopen.
- **Referential actions:** foreign keys now enforce `ON DELETE` and `ON UPDATE` actions including `CASCADE`, `SET NULL`, `SET DEFAULT`, `RESTRICT`, and `NO ACTION`, including self-referential cascades and `MERGE` interactions.
- **Readline `usql`:** TTY sessions now use readline editing, persistent history, history hints, backslash-command completion, SQL keyword highlighting, live table/foreign-table/column completion, and SQL function completion from the `uqa-sql` registry.
- **Automatic column statistics refresh:** DML and schema changes invalidate table statistics; `Engine::column_stats`, planning paths, and `usql \ds` recompute them lazily. `ANALYZE` remains available for eager refresh.
- **Python catalog migration utility:** `usql migrate-python-db <source> <destination>` and `\migrate-python-db` migrate Python UQA catalogs into Rust UQA catalogs, including documents, indexes, analyzers, graphs, models, foreign definitions, scoring parameters, and column statistics.
- **Benchmark parity workloads:** Criterion targets now cover Python-parity SQL workloads, graph workloads, planner statistics/selectivity paths, graph SQL, compiler and execution loops, fusion, operators, storage, scoring, calibration, relevance, and BEIR calibration surfaces.
- **Rust-backed SQL function registration:** `Engine` can register Rust scalar, table, and aggregate implementations as SQL functions. Registered scalars evaluate in projections and filters, registered table functions run in `FROM` with aliases, and registered aggregates participate in `GROUP BY`; unordered registered aggregates stream inputs into per-group state, while ordered registered aggregates spill sorted temp-file runs once the in-memory run buffer is exceeded.

### Changed

- **Persistent restore path:** `Engine::open` now attaches to persisted GIN and IVF metadata without rebuilding indexes on database open, restores table doc-id watermarks via direct lookups, and lazy-loads column statistics on first use.
- **Storage construction boundary:** persistent engine restore now goes through `CatalogFacade` and `PersistentStorageBackend`, so new storage implementations can reuse the same table, graph, model, analyzer, statistics, and index restore path.
- **Model ownership:** engine and operator crates now keep only catalog persistence and SQL adapters for ML; model specs, training, and backend execution live in `uqa-ml`.
- **CLI storage wording:** `usql \open` and startup messaging describe persistent UQA storage rather than a single concrete backend.
- **Function registry reuse:** CLI completion and highlighting read UQA function names from `uqa-sql::registry::registered_names` instead of duplicating a hard-coded CLI list.
- **SQL point updates:** point UPDATE paths now use direct document replacement where possible while keeping FTS, vector, tensor, and KeyValue index state synchronized.
- **crossbeam-epoch advisory bump:** the lockfile moves `crossbeam-epoch` to 0.9.20 for RUSTSEC-2026-0204; the remaining `cargo deny` advisories require the pyo3 0.29 migration in `uqa-python`.

### Fixed

- **Persistent open latency:** large persistent catalogs no longer rebuild inverted/vector indexes or deserialize large statistics payloads during open.
- **Nested persistent writes:** SQLite-backed inverted and vector index writes use savepoints when they run inside outer engine transactions, avoiding transaction conflicts in SQL-managed index lifecycle tests.
- **Mixed SQL function predicates:** search-aware functions now compose with ordinary WHERE predicates instead of dropping the non-function filters during operator-tree lowering.
- **FTS index lifecycle:** GIN index creation and reopen paths now preserve existing posting data, avoid unintended backfill side effects when `CREATE INDEX IF NOT EXISTS` skips an existing index, and refresh every matching row when non-unique UPDATE predicates touch indexed text.
- **Tensor IVF lifecycle:** tensor IVF metadata now counts chunk vectors for training thresholds, keeps one result per row, rejects dimension mismatches, and restores persisted assignments after reopen.
- **PostgreSQL runtime parameter defaults:** `SHOW server_version` now reports `17.0-uqa`, PostgreSQL-compatible defaults are exposed for server/client encoding, `DateStyle`, and `TimeZone`, and runtime parameter lookup honors case-insensitive session overrides.
- **UPDATE document id handling:** UPDATE paths now skip stale document ids and keep table/index state consistent after row replacement.
- **Supply-chain license gate:** `deny.toml` now allows `BSL-1.0`, which is required by `rustyline` transitive dependencies and is accepted by the project's license policy.
- **Registered scalar functions in ORDER BY:** scored-match and row-projection queries materialise ORDER BY keys with the engine hook attached before entering the Volcano sort, so `Engine::register_scalar_function` UDFs work inside ORDER BY expressions instead of failing with `Unsupported`.
- **Text-match field validation:** `text_match`, `bayesian_match`, `fts_match`, `bayesian_match_with_prior`, and `multi_field_match` now reject unknown columns, columns without a text index, and computed-expression field arguments with actionable errors instead of silently matching zero rows; JSONPath `@@` matches and the `_all` pseudo-field keep their existing behavior.
- **Multi-field fusion no-match floor:** `multi_field_match` and `MultiFieldBayesianScorer` pad unmatched fields with the zero-evidence composite prior instead of 0.5, so matching an additional field can never rank a document below one that matched fewer fields on small corpora.
- **Integer column literal normalization:** integer-typed columns coerce parser Decimal literals (and finite floats) to `Value::Int` on INSERT and UPDATE, so literal-written and bind-written rows read back the same variant, in-memory and across persistent reopen.
- **Integer primary key updates relocate document ids:** updating an integer primary key moves the row to the doc id named by the new key value, keeping the value-to-doc-id fast path honest for unique-conflict checks and FOREIGN KEY validation (fixes the `ON UPDATE CASCADE`, ON CONFLICT update, and MERGE referenced-key failures).

## [0.1.0] - 2026-05-09

Initial workspace release with the UQA engine, storage layer, SQL compiler, graph runtime, scoring/fusion stack, API crates, CLI, benchmarks, and CI gates.

### Added

- **uqa-core:** Boolean posting-list algebra, generalized posting lists, predicates, `Value`, `Payload`, `PostingEntry`, index statistics, and property tests for the eleven Boolean axioms.
- **uqa-analysis:** standard, whitespace, and CJK analyzers plus char filters, token filters, and analyzer registry support.
- **uqa-storage:** in-memory and SQLite-backed document stores, inverted index, IVF vector index, B-tree, R\*Tree, block-max metadata, catalog schema management, transactions, and catalog migrations.
- **uqa-scoring:** BM25, Bayesian BM25, WAND, BMW, multi-field scoring, parameter learning, calibration, and IR metrics including NDCG@K, MAP@K, DCG@K, and AP@K.
- **uqa-fusion:** confidence-scaled log-odds, scale-neutral mean log-odds, weighted log-odds, query-feature extraction, learned fusion, and attention fusion.
- **uqa-operators:** operator traits and primitives for term, vector, filter, score, Boolean composition, hybrid retrieval, aggregation, sparse retrieval, progressive fusion, hierarchical traversal, and deep-fusion integration.
- **uqa-graph:** `MemoryGraphStore`, openCypher lexer/parser/AST/executors, mutating Cypher writer, RPQ NFA/DFA evaluation, centrality algorithms, message passing, label/path/embedding indexes, temporal traversal, versioned store deltas, and cross-paradigm graph bridges.
- **uqa-joins:** relational joins, text-similarity joins, vector-similarity joins, hybrid joins, graph-driven joins, and cross-paradigm joins.
- **uqa-planner:** cost model, cardinality estimation, DPccp join enumeration, optimizer rewrites, and operator-tree planning support.
- **uqa-execution:** Volcano-style physical operators and row-batch execution pipelines.
- **uqa-sql:** libpg_query-backed SQL parser/compiler, CREATE/INSERT/SELECT/UPDATE/DELETE, JOIN, GROUP BY, window functions, recursive CTEs, search-aware function registry, SQL expression evaluation, and fuzz coverage.
- **uqa-engine:** schema-aware table store, persistent catalog restore, hash-join optimizer, saved model specs, named graph workspaces, SQL execution, transactions, `text_search` and `hybrid_search` examples, and concurrent-read smoke coverage.
- **uqa-fdw:** `ForeignServer`, `ForeignTable`, pushdown-oriented `FDWHandler`, and in-memory handler implementation.
- **uqa-api:** fluent `QueryBuilder` for common text, vector, hybrid, Bayesian, fusion, graph, RPQ, highlight, facet, ML, and explain flows.
- **uqa-cli:** `usql` with `--db`, `-c`, script-file execution, output redirection, expanded display, table/index/FDW/graph/statistics meta commands, and binary-level integration tests.
- **Testing and benchmarks:** SQL golden-file harness, BEIR-style relevance fixture, Criterion benches for posting-list operations, BM25/Bayesian BM25, calibration, KNN, RPQ, end-to-end SQL text match, 1M-row SQL path, joins, and relevance metrics.
- **CI:** GitHub Actions jobs for fmt, clippy, test, release build, doc, cargo-deny, and bench build across the supported Linux/macOS matrix.

### Expanded

- **SQL compatibility:** sequences, CTAS, `PREPARE` / `EXECUTE` / `DEALLOCATE`, standalone `VALUES`, CHECK constraints, default values, foreign key validation, DROP CASCADE/RESTRICT, UPDATE FROM, DELETE USING, scalar subqueries, `IN(SELECT)`, `EXISTS`, LATERAL joins, grouping sets, ROLLUP, CUBE, explicit window frames, table functions, `information_schema`, `pg_catalog`, analyzer DDL, foreign server/table DDL, `MERGE`, NULL ordering controls, `SET search_path`, and `EXPLAIN SELECT`.
- **Search and graph SQL:** `generate_series`, `unnest`, `regexp_split_to_table`, `json_each`, `json_array_elements`, `create_analyzer`, `drop_analyzer`, `list_analyzers`, `set_table_analyzer`, `rpq(expr, start, graph)`, graph functions, traversal functions, multi-field retrieval, staged retrieval, fusion functions, highlight, and facets.
- **Optimizer wiring:** single-table `SELECT ... WHERE ...` now lowers supported search-aware predicates into an operator tree, applies algebraic, graph-aware, index-scan, filter-pushdown, and fusion-reordering rewrites, then executes through the engine-backed operator-tree driver. Unsupported shapes continue through the direct SQL path.
- **Engine API:** graph helpers, path-index lifecycle, scoring-parameter save/load/drop, analyzer management, transaction shortcuts, savepoints, `close`, sequence hooks, subquery hooks, model deletion, `search_path`, schema listing, and table listing.
- **Storage API:** `DocumentStore` gained bulk field reads, value existence checks, path evaluation, full iteration, and `eval_path_in_document`; `PathSegment` and `PathExpr` moved into `uqa-core`.
- **Graph API:** `GraphStore::vertices` and `GraphStore::edges` expose snapshot accessors for downstream executors and metadata views.
- **QueryBuilder:** fluent calls for attention fusion, learned fusion, calibrated vector match, Bayesian match, RPQ, traversal, temporal traversal, highlight, facets, `deep_learn`, and `explain`, plus `uqa_api::query(engine, table)`.
- **CLI meta commands:** `\stats`, `\dg` / `\graphs`, `\dfs`, `\dft`, and `\da` / `\analyzers` expose engine introspection in the shell.

### Changed

- **Vector indexing:** vector fields and `CREATE INDEX ... USING ivf` use IVF as the primary backend instead of a brute-force SQLite vector scan.
- **Index DDL compatibility:** `USING hnsw` is accepted as an alias for the IVF backend and is stored in the catalog as `ivf`.
- **Release notes:** `CHANGELOG.md` was renamed to `HISTORY.md`.

### Fixed

- **BMW pruning:** `BlockMaxWANDScorer` now folds `block_max` over all remaining blocks for each term, preventing candidates with later-block contributions from being pruned incorrectly.
- **NOT LIKE / NOT ILIKE:** SQL compilation now negates these operators correctly.
- **Window frame defaults:** implicit and explicit window frames are distinguished so `OVER ()`, `OVER (ORDER BY x)`, and explicit `ROWS` / `RANGE` frames use the correct row set.
- **Virtual catalog views:** single-table search-aware planning skips schema-qualified and virtual-view names so `information_schema` and `pg_catalog` dispatch correctly.
- **ORDER BY NULL placement:** sorting honors PostgreSQL defaults and explicit `NULLS FIRST` / `NULLS LAST`.
- **Grouping sets:** aggregators emit NULL for inactive grouping columns instead of erroring on non-active groups.
- **SQLite IVF durability:** persisted IVF centroids and assignments restore on `Engine::open`, so vector indexes are not rebuilt from raw vectors on every reopen.
- **Vector semantics:** IVF keeps original vectors beside normalized vectors and scores candidates against the original values, preserving cosine-threshold behavior.
- **Vector DDL lifecycle:** vector-index metadata is cleared or rebuilt when vector columns are dropped or renamed.

### Tooling

- `cargo clippy --workspace --all-targets -- -D warnings` was clean on the release matrix.
- `cargo doc --workspace --no-deps` was clean with rustdoc warnings denied.
- `cargo test --workspace --all-targets --locked` covered the workspace integration suite.
- `cargo build --workspace --release --locked` was clean.
