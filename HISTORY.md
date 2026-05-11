# History

All notable changes to `uqa-rs` are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **SQLCipher storage:** the bundled SQLite backend now builds against SQLCipher with vendored OpenSSL. `Engine::open` keeps the plaintext initialization path, while `Engine::open_encrypted(path, key)` applies the SQLCipher key before catalog access and reuses the same restore pipeline. The `sqlcipher_encrypted_catalog` example shows create, reopen, and wrong-key failure behavior.
- **Compressed SQLite containers:** `uqa-storage` now registers a schema-neutral `uqa_compressed` SQLite VFS that stores byte-addressed SQLite files as zstd- or LZ4-compressed chunks. `ManagedConnection::open_compressed` / `Engine::open_compressed` use the compressed container path, and `open_compressed_encrypted` compresses chunks before encrypting them with per-container keys. The `compressed_encrypted_catalog` example shows create, reopen, wrong-key, and plaintext-open failure behavior.

## [0.1.0] - 2026-05-09

The 0.1.0 release covers all eleven phases of [`docs/plans/0001-uqa-python-to-rust-port.md`](docs/plans/0001-uqa-python-to-rust-port.md). The remaining post-release exit-criterion item is the 2x-vs-Python performance ratio; the Rust baseline is measured in `docs/design/performance.md`, and the Python comparison harness is the next deliverable.

### Added

- **uqa-core (Phase 0):** Boolean posting list algebra with property tests for the eleven Boolean axioms (Paper 1, Theorem 2.1.2), generalized posting list, `Value` / `Payload` / `PostingEntry`, index statistics. Pure stdlib, no external runtime dependencies.
- **uqa-analysis (Phase 1):** standard / whitespace / CJK analyzers, char filters, token filters, registry.
- **uqa-storage (Phase 1-3):** in-memory and SQLite-backed document store, inverted index, IVF vector index, B-tree, R*Tree, catalog with schema versioning and migrations, transactions, block-max metadata.
- **uqa-scoring (Phase 1-2, 8, 11):** BM25, Bayesian BM25, WAND, BMW, multi-field, parameter learner, calibration, IR metrics (NDCG@K, MAP@K, DCG@K, AP@K). Property tests for Theorem 3.2.2 / 3.2.3 (monotonicity, supremum, IDF non-negativity).
- **uqa-fusion (Phase 2, 8):** confidence-scaled log-odds, scale-neutral mean log-odds, weighted log-odds, query-feature extractor, learned and attention fusion. Property tests for Theorem 4.2.x (n=1 identity, sign preservation, irrelevance / relevance preservation, symmetric disagreement).
- **uqa-operators (Phase 1-2, 8, 9):** Operator trait with Boolean, hybrid, primitive, multi-stage, sparse, progressive-fusion, hierarchical, deep-fusion (Embed / Signal / Dense / Flatten / GlobalPool / Softmax / BatchNorm / Dropout / Propagate / Conv / Pool / Attention layers).
- **uqa-graph (Phase 7):** `MemoryGraphStore`, openCypher front-end (lexer, AST, recursive-descent parser, read + mutating executors), RPQ NFA-to-DFA, centrality (PageRank, HITS, Betweenness), message passing, label / path / embedding indexes, versioned store with delta rollback, temporal traversal, incremental pattern matcher, cross-paradigm bridges. `Phi` homomorphism property tests.
- **uqa-joins (Phase 6):** relational, text-similarity (Jaccard), vector-similarity, hybrid, graph-driven, cross-paradigm joins.
- **uqa-sql (Phase 5-9):** libpg_query-backed parser, AST, CREATE/INSERT/SELECT/UPDATE/DELETE, JOIN, GROUP BY, window functions, recursive CTEs, function registry hooks for `text_match`, `knn_match`, `fuse_log_odds`, `multi_field_match`, `staged_retrieval`, `graph_*`, `deep_predict`. Robustness fuzz (proptest) covers ~1500 random inputs per CI run.
- **uqa-engine (Phase 1-9, 11):** schema-aware table store, catalog restore, `Engine::open` SQLite-backed durability, hash-join optimizer (~340x speedup vs nested-loop fallback on the inner-join bench), serializable `DeepModel` JSON persistence with reopen rehydration, named graph workspaces, `text_search` and `hybrid_search` examples. Concurrent-read smoke test.
- **uqa-fdw (Phase 10):** `ForeignServer`, `ForeignTable`, predicate push-down trait `FDWHandler`, `MemoryHandler` reference impl with SQL LIKE wildcard matcher.
- **uqa-api (Phase 10):** fluent `QueryBuilder` for the most common read patterns.
- **uqa-cli (Phase 10):** `usql` REPL with persistent history (`$UQA_HISTORY` override), meta commands, integration test driving the binary via piped stdin.
- **Parity (Phase 11):** SQL golden-file harness (in-memory + SQLite paths), BEIR-style relevance fixture (schema v2 with per-scorer floors for `bm25` and `bayesian_bm25`), `cargo deny` supply-chain gate, `cargo doc` rustdoc warning-free policy, libfuzzer scaffold under `fuzz/` for nightly cron, performance baseline doc.
- **CI:** GitHub Actions workflow with separate jobs for fmt, clippy, test, build-release, doc, deny, bench-build (cross-platform on ubuntu-24.04 and macos-14 where applicable).

### Round 2 (2026-05-06)

Post-phase parity sweep against the Python reference, focused on SQL surface area, engine API parity, and CI hardening. The QueryOptimizer (10-pass algebraic / graph-aware / fusion-reordering optimiser) — previously ported 1:1 in `uqa_planner::query_optimizer` but never wired into the engine — now runs on every single-table `SELECT ... WHERE ...`. SQL WHERE clauses lower into an `OperatorTree`, the optimiser fires (`simplify_algebra`, `push_filters_down`, `push_filter_into_traverse`, `push_filter_below_graph_join`, `push_graph_pattern_filters`, `fuse_join_pattern`, `merge_vector_thresholds`, `reorder_intersect`, `reorder_fusion_signals`, `apply_index_scan`), and `PlanExecutor` runs the rewritten tree against an engine-backed `OperatorTreeDriver`. Shapes the operator IR can't represent (arithmetic across columns, sub-queries, window calls, ...) fall back to the legacy direct dispatch path.

#### Added

- **SQL: sequences + CTAS + PREPARE/EXECUTE/DEALLOCATE + standalone VALUES** (`#40`, `#44`, `#45`, `#46`).
- **SQL: CHECK / FOREIGN KEY / DEFAULT constraint validators** (`#41`, `#42`, `#43`) with referential-integrity enforcement on `INSERT` / `UPDATE` / `DELETE`.
- **SQL: DROP CASCADE / RESTRICT semantics** (`#47`).
- **SQL: UPDATE FROM other_table / DELETE USING other_table** (`#48`, `#49`) — joined updates/deletes resolve the auxiliary row source through the same row evaluator as SELECT joins.
- **SQL: scalar / IN(SELECT) / EXISTS subqueries** (`#50`) wired through an `EngineHook::run_subquery` indirection so `uqa-sql` stays free of an `uqa-engine` dependency.
- **SQL: LATERAL join executor** (`#51`) — re-runs the right side per left row so the right body can reference outer columns.
- **SQL: GROUPING SETS / ROLLUP / CUBE expansion** (`#52`) with a relaxed aggregator that emits NULL for non-active grouping columns.
- **SQL: Window FRAME (ROWS/RANGE BETWEEN)** (`#53`) for SUM / COUNT / AVG / MIN / MAX over framed partitions; honours `FRAMEOPTION_NONDEFAULT` so PG's implicit default frame is preserved as `None` while explicit frames are applied per row.
- **SQL: table functions in FROM** (`#54`) — `generate_series`, `unnest`, `regexp_split_to_table`, `json_each`, `json_array_elements`, `create_analyzer`, `drop_analyzer`, `list_analyzers`, `set_table_analyzer`, plus the new `rpq(expr, start, graph)` Regular Path Query relation.
- **SQL: information_schema + pg_catalog virtual views** (`#55`): `tables`, `columns`, `pg_tables`, `pg_views`, `pg_indexes`, `pg_type` synthesised from the engine's registry.
- **SQL: CREATE / DROP / SET ANALYZER DDL** (`#56`).
- **SQL: CREATE FOREIGN SERVER / CREATE FOREIGN TABLE DDL** (`#57`).
- **SQL: MERGE statement** (`#58`) with WHEN MATCHED / WHEN NOT MATCHED branches; pairing semantics fixed to skip target rows that don't pair with any source.
- **SQL: ORDER BY ... NULLS FIRST / NULLS LAST** with PostgreSQL defaults (ASC → NULLS LAST, DESC → NULLS FIRST). pg_query `SortbyNulls` enum (1=Default / 2=First / 3=Last) mapped correctly.
- **SQL: SET search_path TO ...** parsing and execution; engine-side `search_path / set_search_path / list_schemas / tables_in_schema` accessors. CREATE TABLE retains `schema.table` qualification.
- **SQL: EXPLAIN SELECT ...** returns a single-column `plan` table mirroring Python `_explain_plan` output shape.
- **Storage: `DocumentStore` extended trait** with `get_fields_bulk`, `has_value`, `eval_path`, `iter_all`, plus `eval_path_in_document` free function. `PathSegment` / `PathExpr` moved to `uqa-core` so the engine surface can use them without depending on `uqa-operators`.
- **Storage: `GraphStore::vertices` / `edges`** snapshot accessors.
- **Storage: `_scoring_params` catalog table (migration v4)** with `save / load / load_all / drop_scoring_params` round-trip.
- **Engine: Cypher write engine wiring** — `Engine::run_cypher` runs CREATE / MERGE / SET / DELETE / UNWIND through `CypherWriter` against a named in-memory graph.
- **Engine: graph helpers** — `Engine::has_graph / list_graphs / add_graph_vertex / add_graph_edge / apply_graph_delta`.
- **Engine: path-index lifecycle** — `build_path_index / drop_path_index / get_path_index / list_path_indexes`. Applying a graph delta auto-invalidates the affected indexes.
- **Engine: scoring-params API** — `Engine::save_scoring_params / load_scoring_params / load_all_scoring_params / drop_scoring_params` with SQLite catalog write-through.
- **Engine: analyzer aliases** — Python-name aliases `create_analyzer / drop_analyzer / set_table_analyzer / get_table_analyzer` over the existing named-analyzer registry.
- **Engine: transaction conveniences** — `begin / commit / rollback / savepoint / release_savepoint / rollback_to_savepoint`, `transaction_depth`, and `close()` (rolls back open frames and clears foreign-server/table registries). `Engine::cancel_token` Python-name alias.
- **Engine: `delete_model`** alias for `drop_model`.
- **Engine: sequence support and `EngineHook`** trait (`Engine` implements `nextval / currval / setval / run_subquery` so SQL expressions can reach sequences and run scalar/`EXISTS`/IN subqueries through the hook).
- **API: `QueryBuilder` fluent additions** — `fuse_attention`, `fuse_learned`, `calibrated_vector_match`, `bayesian_match`, `rpq`, `traverse`, `temporal_traverse`, `highlight`, `facets`, `deep_learn`, plus `explain()` that lifts the assembled SELECT through the engine's `EXPLAIN` driver. `uqa_api::query(engine, table)` free-function helper.
- **CLI: meta commands** — `\stats <table>`, `\dg` / `\graphs`, `\dfs`, `\dft`, `\da` / `\analyzers` over the matching engine introspection APIs.
- **Tests:** ~40 new regression tests across the new SQL features, engine APIs, and CLI behaviours.

#### Fixed

- **uqa-scoring: BMW pruning bound** — `BlockMaxWANDScorer` was using the cursor's current block as the bound for every term, which prematurely pruned candidates whose contribution lived in a later block. The bound now folds `block_max` over `[cur_block .. total_blocks]` so no doc with score above the threshold gets skipped. Property test `bmw_top_k_equals_exhaustive` passes consistently.
- **uqa-sql: NOT LIKE / NOT ILIKE** silently behaving as their positive forms (compiler now negates).
- **uqa-sql: window frame default** — PG always encodes a default frame; the compiler now honours `FRAMEOPTION_NONDEFAULT` so `OVER ()` and `OVER (ORDER BY x)` produce `frame = None` and the engine applies the correct semantics (whole partition vs running).
- **uqa-engine: information_schema fast-path** — single-table search-aware path now skips schema-qualified or non-real names so the virtual-view dispatcher gets the row.
- **uqa-engine: Sort NULL placement** — `ORDER BY` no longer slams NULL to the smallest value; the comparator branches on the resolved `nulls_first` flag.
- **uqa-engine: GROUPING SETS aggregator** — strict aggregator errored on non-grouped columns; relaxed variant emits NULL.

#### Tooling

- `cargo clippy --workspace --all-targets -- -D warnings` clean on macOS / Linux.
- `cargo doc --workspace --no-deps` clean (no broken intra-doc links, no private link leaks in public items).
- `cargo test --workspace --all-targets --locked` — 105 test groups passing.
- `cargo build --workspace --release --locked` clean.

### Round 3 (2026-05-09)

#### Changed

- **Vector indexing:** vector fields and `CREATE INDEX ... USING ivf` now use IVF as the primary backend instead of a brute-force SQLite vector scan.
- **SQL compatibility:** `USING hnsw` is accepted as an alias for IVF and is stored in the catalog as `ivf`.
- **Release notes:** renamed `CHANGELOG.md` to `HISTORY.md`.

#### Fixed

- **SQLite IVF durability:** persisted IVF centroids and assignments are restored on `Engine::open`, so indexes are not rebuilt from all raw vectors on every database open.
- **Vector semantics:** IVF keeps each original vector alongside its normalized copy and scores candidates against the original vector values, preserving existing cosine-threshold behavior.
- **DDL lifecycle:** vector-index metadata is cleared or rebuilt when vector columns are dropped or renamed.

### Notes

- The 2x-vs-Python performance gate from the master plan is split into the Rust baseline (`docs/design/performance.md`) and the Python comparison harness, which is the next deliverable.
- DPccp join enumeration, `deep_learn` training, and PyO3 Python bindings remain explicitly deferred per the master plan's non-goals.
