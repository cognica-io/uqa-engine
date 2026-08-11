# History

All notable changes to `uqa-rs` are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Added a deterministic all-22-query TPC-H-derived scale-factor `0.001` fixture, exact PostgreSQL 17.10 result gate, package-scoped release timing runner, and live differential script.
- Added a machine-checked integration-harness coverage contract so test sources cannot silently become unregistered or duplicate Cargo targets.
- Added a backend-neutral clustered posting codec, score-only lazy cursors, and automatic atomic migration of existing SQLite and Key/Value/redb full-text indexes.

### Changed

- Consolidated integration sources into domain harnesses so workspace builds and tests share linker work while retaining direct module filtering.
- Replaced map-backed relational rows with positional `RowSchema` mappings and shared-fragment `PhysicalRow` composition across scans, projections, joins, aggregates, subqueries, spill boundaries, and result collection.
- Streamed eligible single-consumer derived-table projections into their parent operators while retaining materialization for blocking, repeatable, or volatile shapes.

### Performance

- Added compiled projected predicates and aggregate inputs, borrowed canonical group keys, group arenas, reusable accumulator templates, lazy decimal SUM promotion, and once-per-query aggregate output and HAVING compilation.
- Decorrelated supported immutable `EXISTS` predicates into collision-safe borrowed-key hash probes and collected direct inner keys without projected-row materialization.
- Added borrowed-slot hashing for unique-key inner equijoins with exact collision verification and encoded spill fallback when `work_mem` is exceeded.
- Replaced one physical posting value per `(term, doc_id)` with 65,536-document term clusters, split score columns from positions, and connected exhaustive scoring plus WAND/BMW directly to 128-entry lazy score blocks.
- Reduced the local TPC-H-derived Q20-excluded sum of per-query release medians from 45.917 ms to 14.184 ms while retaining exact PostgreSQL results; this development snapshot is documented as local directional evidence rather than an audited TPC-H score.

## [0.1.0] - 2026-08-07

Initial preproduction release of UQA-RS.

### Added

- **Unified query runtime:** PostgreSQL-oriented SQL, full-text retrieval, vector search, graph queries, ranking, fusion, and machine-learning operators execute through one embeddable Rust engine.
- **Explicit query carriers:** `DocSet` owns document-support Boolean algebra, `Relation<K>` owns finite-support semiring combination, `PostingList` owns decorated posting storage, `RankedView` owns score order and top-K, and generalized postings preserve join-tuple identity.
- **Typed score domains:** raw BM25 scores, evidence logits, prior logits, and posterior probabilities use distinct public types so invalid mathematical combinations are visible at API boundaries.
- **Unified planning:** statements compile to `UnifiedPlan`, pass through the plan-native optimizer, and execute through `UnifiedPlanExecutor`; specialized retrieval paths remain explicit children of the shared plan.
- **Relational SQL:** schemas, `search_path`, DDL, DML, constraints, referential actions, MERGE, CTAS, recursive CTEs, set operations, subqueries, LATERAL joins, grouping sets, window frames, sequences, views, prepared statements, JSON/JSONB, arrays, temporal values, numeric values, `BYTEA`, and virtual PostgreSQL catalog views.
- **Subquery composition:** retrieval predicates compose with `IN`, `NOT IN`, `EXISTS`, and scalar subqueries in the same WHERE clause, and correlated subqueries resolve outer references written against either the outer table name or an alias.
- **Physical execution:** pull-based row batches, columnar result batches, bounded materialization, external sorting, spillable aggregation, disk-backed set operations, bounded hash joins, and streaming `sql_cursor` and `sql_columnar` APIs.
- **Join planning:** statistics-aware cardinality and cost estimation, DPccp inner-join enumeration, hash and nested-loop strategies, and explicit preservation of outer and lateral boundaries.
- **Text retrieval:** analyzers and filters, persistent GIN-style inverted indexes, BM25, query-level Bayesian BM25, multi-field search, highlighting, facets, staged retrieval, and score-aware SQL predicates.
- **Exact text top-K:** WAND and Block-Max WAND physical plans preserve duplicate query terms, field-scoped statistics, monotone Bayesian finalization, persisted bound fingerprints, and exhaustive top-K equivalence.
- **Vector and tensor retrieval:** vector and tensor SQL types, KNN predicates, distinct persistent IVF and HNSW physical indexes, calibrated vector matching, candidate-K provenance, and one-result-per-row tensor scoring.
- **HNSW duplicate-vector connectivity:** layer-zero pruning preserves a bounded deterministic backbone, so large groups of identical or near-identical vectors cannot isolate the entry point or produce a graph that fails persistence validation.
- **Legacy HNSW index compatibility:** SQLite catalog migration v20 recognizes historical `hnsw` rows whose durable metadata is IVF and records their actual physical index kind, so persistent-HNSW restore accepts valid pre-HNSW databases.
- **Fusion contracts:** exact Bayesian evidence fusion applies one prior to signed likelihood-ratio evidence, while robust positive-evidence pooling is separately named and documented as a ranking heuristic.
- **Calibration:** persisted scoring parameters, query-length scaling, model provenance, unsupervised score transforms, labeled reliability metrics, ECE, Brier score, log loss, bootstrap confidence intervals, threshold transfer, and candidate-K stability checks.
- **Graph runtime:** memory and SQLite graph stores, named graphs, Cypher reads and mutations, Apache AGE-compatible `agtype` values, graph pattern matching, RPQ automata, centrality, message passing, embeddings, path indexes, temporal traversal, and versioned deltas.
- **Graph carrier contracts:** graph payload support is validated, overlap behavior uses explicit policies, and the versioned Phi codec preserves graph context without claiming an isomorphism between arbitrary graphs and document sets.
- **Cross-paradigm joins:** relational, text-similarity, vector-similarity, hybrid, graph-driven, and generalized tuple-preserving join operators.
- **Persistent catalogs:** schemas, documents, constraints, postings, scalar and vector indexes, tensors, analyzers, graphs, scoring parameters, models, routines, views, sequences, foreign definitions, and statistics restore through shared catalog and backend boundaries.
- **Storage abstraction:** in-memory stores, relational SQLite stores, backend-neutral key/value contracts, a physical SQLite key/value backend, atomic batches, ordered prefix scans, range deletion, and reusable persistent-engine construction.
- **Backend-neutral sessions:** `PersistentStorageProvider` creates catalog and physical backend handles bound to one session, so `Engine::new_session` serves every backend rather than depending on a hidden SQLite connection.
- **Pure-Rust redb storage:** `uqa-storage-redb` implements ordered byte keys, atomic batches, MVCC read sessions, explicit write transactions, committed generation tracking, reopen, and SQL-compatible savepoints through a transaction-local undo journal.
- **Reusable storage conformance:** third-party `KeyValueStore` implementations can run shared ordering, cursor, batch, transaction, savepoint, read-only, and session-isolation checks.
- **Persistent index lifecycle:** B-tree, inverted, vector, tensor, graph, and block-max metadata remain synchronized across insert, update, delete, truncate, schema changes, transactions, rollback, and reopen.
- **Encrypted storage:** SQLCipher-backed catalogs, authenticated compressed containers using zstd or LZ4, automatic format detection, wrong-key rejection, and an external trusted-anchor contract for whole-file rollback detection.
- **Transaction and session isolation:** each logical session owns transaction affinity, variables, search path, prepared plans, cancellation, sequence state, statement cache, and statement serialization while published generations coordinate shared state.
- **SQL routines:** SQL-language and PL/pgSQL functions and procedures, `DO`, `CALL`, control flow, dynamic execution, diagnostics, exception handling, notices, set-returning routines, catalog persistence, and guarded recursion.
- **Runtime extensions:** embedders can register Rust scalar, table, and aggregate functions with explicit properties for transaction classification and optimization safety.
- **Foreign data wrappers:** foreign server and table contracts, predicate/projection/limit pushdown, and DuckDB, Arrow IPC, and in-memory handlers.
- **Machine learning:** serializable model specifications, analytical training, CPU inference for dense, convolutional, recurrent, graph, pooling, normalization, dropout, softmax, and attention layers, plus an optional Apple MLX backend.
- **Developer APIs:** the embedded `Engine`, fluent `QueryBuilder`, structured SQL parameters and results, graph and calibration helpers, and profiling APIs for text-search candidate, scoring, skip, and latency measurements.
- **Language bindings:** Python bindings through pyo3, asynchronous Node.js bindings with generated TypeScript declarations, and browser WASM bindings with SQLite persistence on IndexedDB.
- **Command-line shell:** `usql` supports in-memory and persistent databases, SQLCipher and compressed containers, one-shot and script execution, multiline editing, durable history, completion, highlighting, introspection, timing, output control, and Python-catalog migration.
- **PostgreSQL wire codec:** a network-independent PostgreSQL v3 protocol crate decodes frontend traffic and encodes authentication, row, command, notice, error, and ready-for-query messages.
- **Compatibility validation:** PostgreSQL 17 differential probes, Apache AGE container-captured fixtures, SQL golden files, storage reopen tests, transaction tests, graph codec properties, algebraic carrier laws, and randomized optimizer and top-K differential tests.
- **Performance evidence:** Criterion suites cover storage, SQL, planning, scoring, fusion, operators, graph, retrieval, calibration, and analytical execution, with provenance manifests and ratio-based regression gates for published baselines.
- **Rust baseline:** the workspace toolchain and declared minimum supported Rust version are Rust 1.90.
- **Workspace policy:** crate dependency budgets, public-repository hygiene, Rust source-header checks, file-size gates, formatting, Clippy, workspace tests, release builds, documentation checks, dependency audits, and benchmark compilation are enforced by repository scripts and CI.
- **Licensing policy:** AGPL-3.0-only remains the open-source base, with optional FOSS and noncommercial application exceptions, separate commercial licensing, and a contributor-rights policy that preserves the public core.
