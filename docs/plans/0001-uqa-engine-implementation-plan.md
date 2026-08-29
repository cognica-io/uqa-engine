# Plan 0001: UQA Engine Implementation Plan

Status: Living implementation plan

Update rule: Reconcile this plan whenever workspace ownership, public surfaces, engineering policy, verification, or release gates change.

## 1. Goal and non-goals

### 1.1 Goal

Build UQA Engine as an embeddable Rust engine for the Unified Query Algebra:

1. **Contract-faithful.** Document support, payload merge, graph encoding, scoring, fusion, and aggregation each have their own property or golden tests.
2. **SQL and API compatible.** The public contract is the UQA SQL surface, `Engine`, `QueryBuilder`, `usql`, graph APIs, and crate-level Rust APIs.
3. **Embeddable.** The engine runs in-process with local storage backends and no required service dependency.
4. **Persistent by default where configured.** Catalog metadata, table data, secondary indexes, statistics, graphs, models, and analyzer configuration must survive reopen.
5. **Production-grade.** Startup, shutdown, migration, and recovery behavior must be deterministic and covered by tests.

### 1.2 Non-goals

- Distributed execution in the core engine.
- Runtime dependency on another language implementation.
- A separate SQL dialect that drifts from the UQA SQL contract.

## 2. Theoretical anchors

The implementation uses the [unified query algebra](https://doi.org/10.31219/osf.io/f56j2_v2), its [graph-data extension](https://doi.org/10.31219/osf.io/cgfae_v1), and the [Bayesian framework for hybrid search](https://doi.org/10.5281/zenodo.20768747) as design input, with the executable contracts stated at the type boundary:

- **Document-set Boolean algebra.** `DocSet` is the carrier for union, intersection, complement relative to an explicit universe, empty, De Morgan, and distributivity. The 11 Boolean laws are property-tested with ordinary `DocSet` equality.
- **Support projection, not a posting-list isomorphism.** `PostingList::support` forgets payloads. Constructing a posting list from `D: DocSet` assigns default payloads, so `support(PostingList::from(D)) == D`; reconstructing from `support(L)` equals a decorated `L` only when every payload is already default.
- **Value relations.** `Relation<K>` is a finite-support function `DocId -> K`. Pointwise combination is available when `K` supplies a semiring; `bool` recovers document-set support behavior and `LogSemiring` supplies log-space weight combination. `Payload` is deliberately not a semiring: posting collision merge adds scores, unions positions, and uses right-hand field precedence, so full-value idempotence and commutativity are not claimed.
- **Ranked views.** `PostingList` remains physically sorted by document id. `RankedView` owns the separate score ordering used for ranked iteration and top-K selection.
- **Operator rewrites.** Filter pushdown, vector threshold merging, facet additivity, join distribution, and aggregation decomposition are equivalence-preserving.
- **Graph payload encoding.** `Phi` is a lossless storage codec. `GraphPostingList` separately defines subgraph union/intersection/precedence policies and the engine preserves that graph carrier through set nodes; generic posting payload merge is not claimed as a graph-algebra homomorphism.
- **Bayesian BM25.** Query-level sigmoid transforms, priors, WAND/BMW bounds, calibration metrics, and parameter learning use deterministic numeric contracts. Score-ordered limits become physical WAND/BMW leaves; duplicate terms are retained, field statistics are scoped correctly, and Bayesian finalization occurs once after the raw sum. Persisted BMW bounds are scorer-versioned and atomically invalidated by posting writes. Unlabeled parameter estimation is named as a score transform; probability claims require held-out labels.
- **Fusion modes.** Exact signed `BayesianEvidenceFusion` preserves neutral evidence and applies the prior once. Confidence-scaled positive-evidence pooling is an explicitly separate robust ranking heuristic.
- **Vector calibration.** Query-pool Gaussian fitting is an unsupervised score transform. Reusable `VectorCalibrationModel` values store corpus/index/embedding-model/K/version provenance, reject mismatched runtime targets, and are gated with held-out reliability, ECE, Brier, log-loss, bootstrap confidence intervals, and threshold transfer.
- **Physical vector indexes.** Exact brute force is the default carrier; IVF and HNSW are separate opt-in physical indexes with independent parameters, persisted metadata, differential recall tests, and transaction-safe mutation paths.

Cardinality estimates are planner heuristics, not data-correctness contracts. Sampling accuracy requires an explicit estimator model and assumptions. For independent Bernoulli trials with mean `mu = E[X]`, the usual two-sided multiplicative Chernoff form for `0 < epsilon <= 1` gives a sufficient condition `epsilon >= sqrt(3 ln(2/delta) / mu)` for failure probability at most `delta`; a bare `1 / sqrt(E[X])` expression is not a universal guarantee. Cost and accuracy claims belong in reproducible benchmark reports with the corpus, sample count, estimator assumptions, error metric, and confidence procedure recorded.

## 3. Workspace layout

UQA Engine is a Cargo workspace with narrow crate boundaries:

| Crate | Responsibility |
| --- | --- |
| `uqa-core` | Document sets, semiring relations, posting storage, ranked views, predicates, cancellation |
| `uqa-analysis` | Tokenizers, analyzers, highlighting, token filters |
| `uqa-storage` | Storage contracts, catalog, document store, inverted and vector indexes, index metadata |
| `uqa-storage-redb` | Durable redb Key/Value storage provider |
| `uqa-storage-sqlite` | Durable SQLite and SQLCipher Key/Value storage provider |
| `uqa-scoring` | BM25, Bayesian BM25, calibration, external priors, WAND/BMW |
| `uqa-fusion` | Log-odds, adaptive, attention, learned, and boolean fusion |
| `uqa-operators` | Operator traits, primitive operators, hybrid operators, execution trees |
| `uqa-graph` | Graph store, graph operators, RPQ, Cypher, path indexes, temporal graph support |
| `uqa-joins` | Row-oriented joins and cross-paradigm join helpers |
| `uqa-planner` | Costing, cardinality, join enumeration, optimizer rewrites |
| `uqa-execution` | Batch execution, physical execution, spill helpers |
| `uqa-pg-query` | Reproducible PostgreSQL 18 parser and PL/pgSQL parser import |
| `uqa-sql` | SQL AST, expression evaluation, compiler helpers |
| `uqa-pg-wire` | PostgreSQL frontend/backend protocol compatibility |
| `uqa-fdw` | PostgreSQL foreign-data wrapper extension boundary |
| `uqa-engine` | User-facing engine, SQL execution, persistence wiring, migrations |
| `uqa` | Umbrella Rust package for the supported embedded surface |
| `uqa-client` | Local and HTTP engine connection abstraction |
| `uqa-api` | Fluent query builder and public API helpers |
| `uqa-ml` | Deep model specs, CPU inference, analytical training, and an experimental direct-crate MLX probe |
| `uqa-cli` | `usql` CLI and admin commands |
| `uqa-python` | Python extension and packaged `usql` executable |
| `uqa-node` | Native Node.js extension and platform package boundary |
| `uqa-wasm` | Browser WebAssembly binding |

## 4. Persistence contract

Persistent engines must not rebuild durable state from scratch on every open.

- Catalog migrations run idempotently on open.
- Inverted indexes, vector indexes, column statistics, analyzer assignments, graph indexes, and model metadata are persisted.
- Nested writes inside outer SQL transactions use savepoints.
- Statistics refresh automatically on data-changing statements and explicit `ANALYZE`.
- `\open <path>` in `usql` switches to a persistent UQA database path, not a SQLite-only abstraction.

## 5. SQL and API contract

The public SQL and API surface includes:

- Relational DDL/DML, scalar functions, aggregates, window functions, CTEs, subqueries, views, constraints, and prepared statements.
- Search-aware functions such as `text_match`, `knn_match`, `fuse_log_odds`, `adaptive_fuse`, `attention_fuse`, `multi_field_match`, `staged_retrieval`, `sparse_threshold`, `deep_predict`, and `deep_learn`.
- Graph functions and table functions including `cypher`, `traverse_match`, RPQ traversal, centrality, message passing, and graph embeddings.
- FDW/table-function integration for Arrow, DuckDB, Parquet, CSV, JSON, and PostgreSQL-compatible sources.
- QueryBuilder helpers for common read and retrieval flows. Validated helpers return errors before emitting invalid SQL, retrieval and fusion helpers render shared-IR `WHERE` predicates, and callers use `Engine::sql` for SQL shapes the builder does not model.

## 6. ML contract

`uqa-ml` is the boundary for model representation and inference:

- CPU inference is always available.
- Supported deep layers include dense, CNN1D, CNN2D, RNN, LSTM, graph propagation, convolution, pooling, batch norm, dropout, global pooling, softmax, flatten, and attention layers.
- `deep_learn` performs deterministic analytical training for supported training sets.
- Engine `deep_predict` currently runs through the CPU implementation; backend selection is not yet wired into `uqa-engine`.
- The current `uqa-ml/mlx` feature is an experimental direct-crate probe for one exact `Input -> Dense -> Softmax` feature shape. It silently calls CPU code for other paths, depends on externally installed libraries, and is not a supported engine or package capability; [`0004-mlx-runtime-support.md`](0004-mlx-runtime-support.md) defines its replacement and release gates.

## 7. CLI contract

`usql` is the interactive SQL shell for UQA Engine:

- Prompt history, completion, syntax highlighting, and multiline editing are part of the shipped UX.
- Completion uses engine metadata for schemas, tables, columns, commands, and command arguments.
- CLI help must describe UQA storage and engine behavior without naming a single backend as the whole database.

## 8. Engineering and verification gates

Each crate exposes exactly one integration-test executable. Additional integration domains live as submodules of that target so test-process startup, linking, and CI scheduling do not grow with every feature.

The 1,500-line Rust file check is a hard safety ceiling, not a design target. Split modules by ownership before they approach the ceiling when parsing, binding, planning, execution, persistence, or tests have separable responsibilities.

[`0005-rust-workspace-refactoring.md`](0005-rust-workspace-refactoring.md) governs the active ownership refactoring, transition ratchet, capability boundaries, and final 1,000-line limit. Until that plan reaches its final gate, passing the 1,500-line emergency ceiling is not evidence that a module is adequately decomposed.

When work starts on a confirmed gap governed by a living plan, record it there as incomplete before implementation and promote it only after its stated verification evidence passes; branch names and pull-request descriptions are not substitutes for repository planning state.

During iteration, format first and run the smallest focused checks that cover the changed ownership boundary:

```sh
cargo fmt --all --check
cargo test -p <affected-crate> <focused-module-or-test>
git diff --check
```

Fast repository-policy and formatting checks run on each pull-request head. Once code and review changes have converged, dispatch the change-aware full suite exactly once for the final remote head:

```sh
bash scripts/run-premerge-ci.sh
```

Any later push creates a new final head and invalidates that pre-merge result. Changes that touch ranking, search, persistence, ML, CLI interaction, SQL lowering, bindings, or release packaging need focused regression tests in the owning crate before dispatch.

## 9. Release criteria

A release is ready when:

- Workspace tests and CI gates pass.
- New behavior is documented in the manual and the relevant focused design or living plan document.
- `HISTORY.md` describes user-visible changes.
- Persistent database compatibility and migration behavior are covered by tests when storage schema changes.
