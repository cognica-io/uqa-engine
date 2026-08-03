# History

All notable changes to `uqa-rs` are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-08-03

Initial preproduction release of UQA-RS.

### Fixed

- **Legacy HNSW alias reopen:** SQLite catalog migration v20 recognizes historical `hnsw` rows whose durable metadata is IVF, records their actual physical index kind, and prevents persistent-HNSW restore from rejecting valid pre-HNSW databases.
- **HNSW duplicate-vector connectivity:** Layer-zero pruning preserves a bounded deterministic backbone, so large groups of identical or near-identical vectors cannot isolate the entry point or produce a graph that fails persistence validation.

### Added

- **Unified query runtime:** PostgreSQL-oriented SQL, full-text retrieval, vector search, graph queries, ranking, fusion, and machine-learning operators execute through one embeddable Rust engine.
- **Explicit query carriers:** `DocSet` owns document-support Boolean algebra, `Relation<K>` owns finite-support semiring combination, `PostingList` owns decorated posting storage, `RankedView` owns score order and top-K, and generalized postings preserve join-tuple identity.
- **Typed score domains:** raw BM25 scores, evidence logits, prior logits, and posterior probabilities use distinct public types so invalid mathematical combinations are visible at API boundaries.
- **Unified planning:** statements compile to `UnifiedPlan`, pass through the plan-native optimizer, and execute through `UnifiedPlanExecutor`; specialized retrieval paths remain explicit children of the shared plan.
- **Relational SQL:** schemas, `search_path`, DDL, DML, constraints, referential actions, MERGE, CTAS, recursive CTEs, set operations, subqueries, LATERAL joins, grouping sets, window frames, sequences, views, prepared statements, JSON/JSONB, arrays, temporal values, numeric values, `BYTEA`, and virtual PostgreSQL catalog views.
- **Physical execution:** pull-based row batches, columnar result batches, bounded materialization, external sorting, spillable aggregation, disk-backed set operations, bounded hash joins, and streaming `sql_cursor` and `sql_columnar` APIs.
- **Join planning:** statistics-aware cardinality and cost estimation, DPccp inner-join enumeration, hash and nested-loop strategies, and explicit preservation of outer and lateral boundaries.
- **Text retrieval:** analyzers and filters, persistent GIN-style inverted indexes, BM25, query-level Bayesian BM25, multi-field search, highlighting, facets, staged retrieval, and score-aware SQL predicates.
- **Exact text top-K:** WAND and Block-Max WAND physical plans preserve duplicate query terms, field-scoped statistics, monotone Bayesian finalization, persisted bound fingerprints, and exhaustive top-K equivalence.
- **Vector and tensor retrieval:** vector and tensor SQL types, KNN predicates, distinct persistent IVF and HNSW physical indexes, calibrated vector matching, candidate-K provenance, and one-result-per-row tensor scoring.
- **Fusion contracts:** exact Bayesian evidence fusion applies one prior to signed likelihood-ratio evidence, while robust positive-evidence pooling is separately named and documented as a ranking heuristic.
- **Calibration:** persisted scoring parameters, query-length scaling, model provenance, unsupervised score transforms, labeled reliability metrics, ECE, Brier score, log loss, bootstrap confidence intervals, threshold transfer, and candidate-K stability checks.
- **Graph runtime:** memory and SQLite graph stores, named graphs, Cypher reads and mutations, Apache AGE-compatible `agtype` values, graph pattern matching, RPQ automata, centrality, message passing, embeddings, path indexes, temporal traversal, and versioned deltas.
- **Graph carrier contracts:** graph payload support is validated, overlap behavior uses explicit policies, and the versioned Phi codec preserves graph context without claiming an isomorphism between arbitrary graphs and document sets.
- **Cross-paradigm joins:** relational, text-similarity, vector-similarity, hybrid, graph-driven, and generalized tuple-preserving join operators.
- **Persistent catalogs:** schemas, documents, constraints, postings, scalar and vector indexes, tensors, analyzers, graphs, scoring parameters, models, routines, views, sequences, foreign definitions, and statistics restore through shared catalog and backend boundaries.
- **Storage abstraction:** in-memory stores, relational SQLite stores, backend-neutral key/value contracts, a physical SQLite key/value backend, atomic batches, ordered prefix scans, range deletion, and reusable persistent-engine construction.
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
- **Workspace policy:** crate dependency budgets, public-repository hygiene, Rust source-header checks, file-size gates, formatting, Clippy, workspace tests, release builds, documentation checks, dependency audits, and benchmark compilation are enforced by repository scripts and CI.
- **Licensing policy:** AGPL-3.0-only remains the open-source base, with optional FOSS and noncommercial application exceptions, separate commercial licensing, and a contributor-rights policy that preserves the public core.
