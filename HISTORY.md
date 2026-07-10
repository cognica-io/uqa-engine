# History

All notable changes to `uqa-rs` are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **PL/pgSQL and SQL-language routines:** `CREATE [OR REPLACE] FUNCTION` / `CREATE PROCEDURE` (`LANGUAGE plpgsql` and `LANGUAGE sql`, including SQL-standard `BEGIN ATOMIC` and `RETURN expr` bodies), `DROP FUNCTION` / `DROP PROCEDURE`, `DO` blocks, and `CALL` with OUT/INOUT parameters. The interpreter covers nested/labeled blocks, `DECLARE` (DEFAULT, `CONSTANT`, `NOT NULL`, `%TYPE`, RECORD), assignments, `SELECT INTO [STRICT]`, `PERFORM`, `IF`/`CASE`, all loop forms with `EXIT`/`CONTINUE [WHEN]`, `RETURN` / `RETURN NEXT` / `RETURN QUERY [EXECUTE]`, `RAISE` with `%` formatting into a notice sink, exception handlers with `SQLERRM`/`SQLSTATE`, dynamic `EXECUTE ... [INTO] [USING]`, `GET DIAGNOSTICS ROW_COUNT`, and `FOUND`. Routines participate in expressions, `FROM` (SETOF / `RETURNS TABLE`), aggregates, and views; definitions persist across `Engine::open` and surface in `pg_catalog.pg_proc` / `information_schema.routines`. Recursion is guarded by a configurable depth limit that raises `stack depth limit exceeded` (54001).
- **Apache AGE agtype compatibility:** the `cypher()` table function now renders byte-exact AGE 1.6.0 agtype text (`{"id": ..., "label": ..., "properties": {...}}::vertex`, edges with `start_id`/`end_id`, `[...]::path`, JSONB key ordering, PostgreSQL `float8out` float formatting), allocates AGE graphids (`label_id << 48 | sequence`, user labels from 3, per-graph label registry persisted through the catalog), coerces declared record columns like AGE casts, and validates graph names (`>= 3` chars, leading letter/underscore, AGE error texts). The Cypher executor matches verified AGE semantics for arithmetic (truncating division, `^` float power, division-by-zero errors, `sqrt`/`log` domain nulls, `% 0` quirks), the agtype total order in `ORDER BY` and `min`/`max`, three-valued logic, end-exclusive list slices, end-inclusive `range()`, comprehensions, string/entity functions, `exists()`, path variables, `OPTIONAL MATCH` padding, and null-skipping aggregates. Covered by an `age_agtype_compat` matrix asserting container-captured ground truth verbatim.
- **Value indexes for scalar predicates:** PRIMARY KEY, UNIQUE, and `CREATE INDEX ... USING btree` columns now back lazily built, incrementally maintained per-column B-tree indexes. Equality, range, `IN`, and `IS [NOT] NULL` predicates resolve to posting lists in `O(log n + k)` and compose with the posting-list Boolean algebra; a semantics guard falls back to evaluated scans whenever the index cannot reproduce them (temporal keys, NaN targets). UPDATE / DELETE `WHERE` clauses preselect through the same machinery.
- **Bulk document reads:** the SQLite document store gains `get_many` and `get_fields_multi` (chunked `doc_id IN` probes or a single sequential scan chosen by request width, JSON fields extracted inside SQLite), used by row materialization, aggregation, ORDER BY key evaluation, and join loading.
- **usql encrypted databases:** `usql --db enc.db --key <key>` / `--key-file <file>` / `UQA_KEY` open SQLCipher databases and encrypted compressed containers; interactive sessions prompt (no echo) when a key is required, on-disk formats are auto-detected (`Engine::detect_database_file`, `Engine::open_auto`), a key on a plaintext database is rejected, and `--db new.db --key k` creates an encrypted database. `\open` and `\reset` reuse the session key.
- **PG17 differential harness:** `tests/parity/pg17/run_diff.py` executes a 300+ probe battery against a live PostgreSQL 17 container and the `usql` binary, normalizes outputs, and reports divergences by category; probes double as the compatibility worklist.
- **Persistent-backend benchmarks:** `sql_sqlite_e2e` Criterion bench measures count/point/indexed-filter/order-limit/group-by/join reads and insert/update writes through `Engine::open`, closing the gap where every engine bench previously ran in-memory only.
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

- **Persistent read path performance:** the SQLite document store uses cached prepared statements everywhere and no longer runs `CREATE TABLE IF NOT EXISTS` on reads; `SELECT count(*)` with no WHERE answers from a dirty-flagged document-count cache (which also feeds planner row estimates that previously issued `COUNT(*)` per statement); aggregation fetches only referenced columns (or nothing for `count(*)`); `ORDER BY ... LIMIT` uses top-K partial selection with field-only key evaluation and materializes only surviving rows; join qualifier-filter pushdown routes every pushed filter through the accelerated single-table path. Measured at 300k rows (release, SQLite backend): `count(*)` 3.5s -> 0.06ms, PK point select 346ms -> under 2ms, indexed equality filter 363ms -> 9ms, filtered join 1.24s -> 12ms, `ORDER BY ... LIMIT 10` 6.9s -> 0.59s unindexed, point UPDATE 1.47s -> 2-7ms.
- **Persistent restore path:** `Engine::open` now attaches to persisted GIN and IVF metadata without rebuilding indexes on database open, restores table doc-id watermarks via direct lookups, and lazy-loads column statistics on first use.
- **Storage construction boundary:** persistent engine restore now goes through `CatalogFacade` and `PersistentStorageBackend`, so new storage implementations can reuse the same table, graph, model, analyzer, statistics, and index restore path.
- **Model ownership:** engine and operator crates now keep only catalog persistence and SQL adapters for ML; model specs, training, and backend execution live in `uqa-ml`.
- **CLI storage wording:** `usql \open` and startup messaging describe persistent UQA storage rather than a single concrete backend.
- **Function registry reuse:** CLI completion and highlighting read UQA function names from `uqa-sql::registry::registered_names` instead of duplicating a hard-coded CLI list.
- **SQL point updates:** point UPDATE paths now use direct document replacement where possible while keeping FTS, vector, tensor, and KeyValue index state synchronized.
- **crossbeam-epoch advisory bump:** the lockfile moves `crossbeam-epoch` to 0.9.20 for RUSTSEC-2026-0204.
- **pyo3 0.29:** `uqa-python` moves to pyo3 0.29, clearing the remaining `cargo deny` advisories (RUSTSEC-2026-0176/0177); `cargo deny --workspace check` is fully green.

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
- **Decimal sort keys in the Volcano sort:** mixed float/decimal arithmetic now promotes to `double precision` (PostgreSQL numeric promotion) instead of returning `Value::Decimal`, and the executor's sort comparator gained Decimal comparison arms mirroring the expression evaluator. Previously an ORDER BY expression containing a decimal literal (for example `_score + 0.05 * boost(...)`) produced Decimal sort keys that the comparator treated as equal, so the tiebreak silently decided the row order.
- **Decimal sort keys in the top-k shortcut:** the LIMIT-below-row-count fast path has its own sort comparator, which also lacked Decimal arms; it now compares Decimal keys like the Volcano sort instead of falling through to the row-order tiebreak.
- **Multi-field match lowering parity:** the operator-tree pipeline's `multi_field_match` leaf now delegates to the same row-function implementation as the legacy dispatch, so bare, mixed-predicate, and fallback shapes share one no-match pad (the calibrated zero-evidence prior), one per-field search analyzer choice, and one statistics source. Previously the lowered path executed `MultiFieldSearchOperator` directly, which padded unmatched fields with 0.5 and analyzed scoring terms with the index default analyzer, re-introducing the small-corpus field-weight inversion for any query that combined `multi_field_match` with an ordinary predicate. The standalone operator itself now pads with the calibrated no-match prior and analyzes scoring terms per field as well.

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
