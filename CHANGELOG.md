# Changelog

All notable changes to `uqa-rs` are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

The 0.1.0 release covers all eleven phases of
[`docs/plans/0001-uqa-python-to-rust-port.md`](docs/plans/0001-uqa-python-to-rust-port.md).
The remaining exit-criterion items are the 2x-vs-Python performance
ratio (Rust baseline measured in `docs/design/performance.md`; the
Python comparison harness is the next deliverable) and the release
tag itself.

### Added

- **uqa-core (Phase 0):** Boolean posting list algebra with property
  tests for the eleven Boolean axioms (Paper 1, Theorem 2.1.2),
  generalized posting list, `Value` / `Payload` / `PostingEntry`,
  index statistics. Pure stdlib, no external runtime dependencies.
- **uqa-analysis (Phase 1):** standard / whitespace / CJK analyzers,
  char filters, token filters, registry.
- **uqa-storage (Phase 1-3):** in-memory and SQLite-backed document
  store, inverted index, IVF vector index, B-tree, R*Tree, catalog
  with schema versioning and migrations, transactions, block-max
  metadata.
- **uqa-scoring (Phase 1-2, 8, 11):** BM25, Bayesian BM25, WAND, BMW,
  multi-field, parameter learner, calibration, IR metrics
  (NDCG@K, MAP@K, DCG@K, AP@K). Property tests for Theorem 3.2.2 /
  3.2.3 (monotonicity, supremum, IDF non-negativity).
- **uqa-fusion (Phase 2, 8):** confidence-scaled log-odds, scale-
  neutral mean log-odds, weighted log-odds, query-feature extractor,
  learned and attention fusion. Property tests for Theorem 4.2.x
  (n=1 identity, sign preservation, irrelevance / relevance
  preservation, symmetric disagreement).
- **uqa-operators (Phase 1-2, 8, 9):** Operator trait with Boolean,
  hybrid, primitive, multi-stage, sparse, progressive-fusion,
  hierarchical, deep-fusion (Embed / Signal / Dense / Flatten /
  GlobalPool / Softmax / BatchNorm / Dropout / Propagate / Conv /
  Pool / Attention layers).
- **uqa-graph (Phase 7):** `MemoryGraphStore`, openCypher front-end
  (lexer, AST, recursive-descent parser, read + mutating executors),
  RPQ NFA-to-DFA, centrality (PageRank, HITS, Betweenness),
  message passing, label / path / embedding indexes, versioned store
  with delta rollback, temporal traversal, incremental pattern
  matcher, cross-paradigm bridges. `Phi` homomorphism property tests.
- **uqa-joins (Phase 6):** relational, text-similarity (Jaccard),
  vector-similarity, hybrid, graph-driven, cross-paradigm joins.
- **uqa-sql (Phase 5-9):** libpg_query-backed parser, AST,
  CREATE/INSERT/SELECT/UPDATE/DELETE, JOIN, GROUP BY, window
  functions, recursive CTEs, function registry hooks for
  `text_match`, `knn_match`, `fuse_log_odds`, `multi_field_match`,
  `staged_retrieval`, `graph_*`, `deep_predict`. Robustness fuzz
  (proptest) covers ~1500 random inputs per CI run.
- **uqa-engine (Phase 1-9, 11):** schema-aware table store, catalog
  restore, `Engine::open` SQLite-backed durability, hash-join
  optimizer (~340x speedup vs nested-loop fallback on the inner-join
  bench), serializable `DeepModel` JSON persistence with reopen
  rehydration, named graph workspaces, `text_search` and
  `hybrid_search` examples. Concurrent-read smoke test.
- **uqa-fdw (Phase 10):** `ForeignServer`, `ForeignTable`, predicate
  push-down trait `FDWHandler`, `MemoryHandler` reference impl with
  SQL LIKE wildcard matcher.
- **uqa-api (Phase 10):** fluent `QueryBuilder` for the most common
  read patterns.
- **uqa-cli (Phase 10):** `usql` REPL with persistent history
  (`$UQA_HISTORY` override), meta commands, integration test
  driving the binary via piped stdin.
- **Parity (Phase 11):** SQL golden-file harness (in-memory + SQLite
  paths), BEIR-style relevance fixture (schema v2 with per-scorer
  floors for `bm25` and `bayesian_bm25`), `cargo deny` supply-chain
  gate, `cargo doc` rustdoc warning-free policy, libfuzzer scaffold
  under `fuzz/` for nightly cron, performance baseline doc.
- **CI:** GitHub Actions workflow with separate jobs for fmt, clippy,
  test, build-release, doc, deny, bench-build (cross-platform on
  ubuntu-24.04 and macos-14 where applicable).

### Notes

- The 2x-vs-Python performance gate from the master plan is split into
  the Rust baseline (`docs/design/performance.md`) and the Python
  comparison harness, which is the next deliverable.
- DPccp join enumeration, `deep_learn` training, HNSW vector index,
  and PyO3 Python bindings remain explicitly deferred per the master
  plan's non-goals.
