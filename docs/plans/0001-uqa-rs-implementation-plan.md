# Plan 0001: UQA-RS Implementation Plan

Status: Living implementation plan Target repo: `uqa-rs`

## 1. Goal and non-goals

### 1.1 Goal

Build UQA-RS as a native Rust implementation of the Unified Query Algebra:

1. **Algebraically faithful.** Core posting-list, graph, scoring, fusion, and aggregation laws are enforced by property and golden tests.
2. **SQL and API compatible.** The public contract is the UQA SQL surface, `Engine`, `QueryBuilder`, `usql`, graph APIs, and crate-level Rust APIs.
3. **Embeddable.** The engine runs in-process with local storage backends and no required service dependency.
4. **Persistent by default where configured.** Catalog metadata, table data, secondary indexes, statistics, graphs, models, and analyzer configuration must survive reopen.
5. **Production-grade.** Startup, shutdown, migration, and recovery behavior must be deterministic and covered by tests.

### 1.2 Non-goals

- Distributed execution in the core engine.
- Runtime dependency on another language implementation.
- A separate SQL dialect that drifts from the UQA SQL contract.
- HNSW as the first vector-index backend. IVF and calibrated vector scoring remain the implemented baseline.

## 2. Theoretical anchors

The implementation must preserve the UQA paper contracts:

- **Posting-list Boolean algebra.** Union, intersection, complement, empty, universal, De Morgan, and distributivity laws hold for `PostingList` and generalized posting lists.
- **Document/posting-list isomorphism.** `PL(doc(L)) == L` and `doc(PL(D)) == D` for supported document sets.
- **Operator rewrites.** Filter pushdown, vector threshold merging, facet additivity, join distribution, and aggregation decomposition are equivalence-preserving.
- **Graph homomorphism.** `Phi` preserves graph posting-list Boolean structure; graph pattern and RPQ operators remain compatible with algebraic rewrites.
- **Bayesian BM25.** Probability transforms, priors, WAND/BMW bounds, calibration metrics, and parameter learning use deterministic numeric contracts.
- **Log-odds fusion.** Confidence-scaled log-odds, identity laws, sign preservation, disagreement collapse, and clamped probability boundaries are tested.
- **Vector calibration.** Likelihood-ratio calibration and hybrid additive fusion keep scoring signals inside the probabilistic model.

## 3. Workspace layout

UQA-RS is a Cargo workspace with narrow crate boundaries:

| Crate | Responsibility |
| --- | --- |
| `uqa-core` | Shared value types, posting lists, predicates, cancellation |
| `uqa-analysis` | Tokenizers, analyzers, highlighting, token filters |
| `uqa-storage` | Catalog, document store, inverted index, SQLite persistence, index metadata |
| `uqa-scoring` | BM25, Bayesian BM25, calibration, external priors, WAND/BMW |
| `uqa-fusion` | Log-odds, adaptive, attention, learned, and boolean fusion |
| `uqa-operators` | Operator traits, primitive operators, hybrid operators, execution trees |
| `uqa-graph` | Graph store, graph operators, RPQ, Cypher, path indexes, temporal graph support |
| `uqa-joins` | Row-oriented joins and cross-paradigm join helpers |
| `uqa-planner` | Costing, cardinality, join enumeration, optimizer rewrites |
| `uqa-execution` | Batch execution, physical execution, spill helpers |
| `uqa-sql` | SQL AST, expression evaluation, compiler helpers |
| `uqa-engine` | User-facing engine, SQL execution, persistence wiring, migrations |
| `uqa-ml` | Deep model specs, CPU inference, analytical training, optional MLX backend |
| `uqa-cli` | `usql` CLI and admin commands |
| `uqa-api` | Fluent query builder and public API helpers |
| `uqa-pg-wire` | PostgreSQL wire-protocol compatibility |

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
- QueryBuilder methods that render the same UQA surface without requiring handwritten SQL.

## 6. ML contract

`uqa-ml` is the boundary for model representation and inference:

- CPU inference is always available.
- Supported deep layers include dense, CNN1D, CNN2D, RNN, LSTM, graph propagation, convolution, pooling, batch norm, dropout, global pooling, softmax, flatten, and attention layers.
- `deep_learn` performs deterministic analytical training for supported training sets.
- `deep_predict` runs batch inference through the selected backend.
- The `mlx` feature enables Apple MLX acceleration through `mlx-c` where available and falls back cleanly when unavailable.

## 7. CLI contract

`usql` is the interactive SQL shell for UQA-RS:

- Prompt history, completion, syntax highlighting, and multiline editing are part of the shipped UX.
- Completion uses engine metadata for schemas, tables, columns, commands, and command arguments.
- CLI help must describe UQA storage and engine behavior without naming a single backend as the whole database.

## 8. Verification gates

Before landing functional changes, run the smallest focused checks that cover the touched surface, then the shared gates when the change is broad:

```sh
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo doc --workspace --no-deps
cargo deny --workspace check
git diff --check
```

Changes that touch ranking, search, persistence, ML, CLI interaction, or SQL lowering need focused regression tests in the owning crate.

## 9. Release criteria

A release is ready when:

- Workspace tests and CI gates pass.
- New behavior is documented in `README.md` or focused design docs.
- `HISTORY.md` describes user-visible changes.
- Persistent database compatibility and migration behavior are covered by tests when storage schema changes.
