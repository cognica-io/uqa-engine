# Performance baseline

This document records UQA-RS benchmark baselines measured on developer hardware. The commands and deterministic inputs are reproducible, but the absolute numbers are machine-specific and have not been independently reproduced. Unless a section explicitly compares another engine, the measurements are internal regression baselines rather than evidence of competitive OLAP performance. Measurements use the `bench` profile (release + debug symbols, thin LTO, `codegen-units = 1`) unless a section explicitly records a package-scoped `release` runner.

## PostgreSQL 17 TPC-H-derived compatibility pass (2026-08-09)

The checked-in [`benchmarks/tpch`](../../benchmarks/tpch/README.md) workload contains all 22 default TPC-H-derived queries and deterministic `dbgen` data at scale factor `0.001`. It is an exact SQL compatibility and local latency fixture, not a compliant or audited TPC-H result. `cargo test -p uqa-engine --test sql_tpch` verifies columns, ordered rows, NULLs, text bytes, and type-aware canonical numeric values against PostgreSQL 17.10 before timing is considered.

The current UQA snapshot was built with `cargo build --release -p uqa-engine --example tpch_runner --locked` on an Apple M1 Ultra arm64 host with Rust 1.90.0. UQA values are medians of 201 executions after an untimed validation execution; PostgreSQL values are medians of five executions in the dedicated PostgreSQL 17.10 arm64 container on the same host. The runs were not interleaved, so their ratios are local directional evidence rather than a base/head regression estimate.

| Metric | Initial UQA release snapshot | Current UQA | PostgreSQL 17 |
| --- | ---: | ---: | ---: |
| Exact query results | 22 / 22 | 22 / 22 | Reference |
| Queries at or below PostgreSQL median | 4 / 22 | 14 / 22 | 22 / 22 |
| Sum of medians excluding Q20 | 45.917 ms | 14.184 ms | 16.626 ms |
| Q09 median | 4.059 ms | 1.774 ms | 1.432 ms |
| Q18 median | 4.708 ms | 1.227 ms | 1.340 ms |

The 69.1% reduction in the Q20-excluded UQA median sum records the optimization trajectory, not a statistically paired speedup claim. Q20 is excluded because PostgreSQL's 396.945 ms plan is pathological at this tiny scale; summing per-query medians is a compact status indicator rather than a throughput metric. The full 22-row table, query runner options, fixture provenance, and live differential command are in the [TPC-H benchmark README](../../benchmarks/tpch/README.md).

Sampling and exact regression tests drove the following execution changes:

1. `RowSchema` now maps logical identities and hidden aliases to physical slots, while `PhysicalRow` composes shared `Arc`-backed value fragments. Projection and join shape changes no longer rebuild string-keyed maps or clone every value.
2. Memory-table scans share stored positional values, compile supported predicates and aggregate inputs once, push independently supported `AND` conjuncts into projection, and leave only unsupported subquery residuals for scalar evaluation.
3. Aggregation hashes borrowed canonical keys, copies only new groups into an arena, reuses accumulator templates, keeps integer state exact, and promotes decimal SUM state lazily.
4. Immutable correlated `EXISTS` shapes decorrelate into collision-safe canonical hash key sets. Direct outer columns probe borrowed values, and safe key collection bypasses projected-row materialization.
5. Eligible single-consumer derived-table projections remain pull pipelines instead of serializing through `SharedSpill`; blocking and repeatable shapes retain the spill boundary.
6. Unique-key inner equijoins with direct column keys hash borrowed source slots, retain only hash-to-row positions, and resolve collisions against the original rows. Budget exhaustion rebuilds the exact encoded spill-capable index.
7. Planner literal folding and once-per-query expression compilation remove repeated constant arithmetic, pattern construction, aggregate output evaluation, and HAVING evaluation from row loops.

The remaining local gaps are Q02, Q07, Q09, Q13, Q16, Q19, Q21, and Q22. Q22 sampling attributes its largest remaining share to building and probing the correlated `NOT EXISTS` orders key set and to residual scalar evaluation; documenting that gap prevents the aggregate result from obscuring unfinished work.

## External-engine analytical comparison (2026-08-03)

`analytical_comparison` runs the same generated rows and SQL through UQA, SQLite, and DuckDB in one process and validates identical results before timing. Its versioned [`manifest.json`](../../benchmarks/analytical/manifest.json) records the row count, seed, generator, schema, queries, Criterion configuration, external ratio checks, and base/head regression ceilings. UQA, SQLite, and DuckDB all execute with warmed statement or plan caches; the comparison backends use `prepare_cached`, matching UQA's cached SQL boundary.

The measurement contract explicitly fixes Criterion's linear sampling mode and slope point estimator. Linear sampling changes the iteration count between samples, so the regression slope of elapsed time against iterations is the per-iteration estimate used for every comparison; the median of sample averages is a different statistic and must not be substituted. The runner writes toolchain, CPU model, platform, commit and dirty state, manifest, workload-identity, benchmark-source, and executable hashes, raw slope samples, medians, paired ratios, and check results to a versioned JSON artifact, and CI uploads that artifact on every run.

Pull-request CI treats the external-engine ratios as advisory evidence rather than a regression oracle. It builds the base and head benchmark binaries on one runner, executes four adjacent pairs in counterbalanced `head → base`, `base → head` order, and gates the median of the four paired head/base slope ratios. Q1 and Q6 permit at most a 1.10x slowdown; materialized and cursor scans permit 1.15x. The runner refuses the comparison unless the generator, row count, seed, memory budget, schema, and queries have identical workload identities at both revisions. This distinguishes a code regression from heterogeneous hosted-runner behavior while retaining the SQLite and DuckDB measurements for interpretation.

The refreshed full 20-sample run used 20,000 rows on macOS arm64 at clean commit `886d091e10ec884eee0f609446809c11632a8157`. The complete artifact is [`macos-arm64-2026-08-03.json`](../../benchmarks/analytical/reference/macos-arm64-2026-08-03.json).

| Workload | UQA | SQLite | DuckDB | UQA / SQLite | UQA / DuckDB |
| --- | ---: | ---: | ---: | ---: | ---: |
| Q1-style grouped aggregate | 2.540 ms | 7.522 ms | 0.492 ms | 0.338x | 5.16x |
| Q6-style filtered aggregate | 2.227 ms | 1.173 ms | 0.402 ms | 1.90x | 5.54x |
| Ordered result scan | 7.956 ms materialized / 8.098 ms cursor | 1.655 ms | 1.236 ms | 4.89x cursor | 6.55x cursor |

The earlier unoptimized artifact measured UQA at 234.015 ms for Q1, 96.089 ms for Q6, and 185.070/191.201 ms for the materialized/cursor scan. Those were real physical-execution costs, not an external-engine anomaly:

1. Aggregation serialized every input row into a `SpillBuffer` and externally sorted the complete input even for one global group or six small groups, crossing the 4 MB budget into disk I/O. The adaptive executor now retains mergeable aggregate states and spills compact partial states only when the state budget is exceeded.
2. The table source cloned complete document maps and repeatedly resolved string-keyed fields. It now fetches document IDs in batches, projects only required fields through interned layouts, and passes borrowed positional values into compiled aggregate inputs.
3. Ordered scans sorted primary-key-ordered input again. Ordering metadata now crosses scan and project boundaries, allowing the redundant sort to be elided, while the cursor consumes column vectors directly instead of converting them back into map-backed rows.

The separate indexed `tpch_style` path had another cost: its optimizer materialized each broad posting list to estimate cardinality and execution materialized it again. Cardinality now comes from value-bucket lengths, while membership-only intersections discard payloads and avoid graph-envelope decoding. That fix explains part of the indexed 100k-row result below; it does not explain the index-free external fixture in this section.

The remaining differences follow the execution models. DuckDB evaluates integer filters and aggregates over vectorized contiguous columns, while UQA dispatches row-at-a-time over dynamic `Value` instances and map-backed document storage. This fixture declares no secondary indexes, so Q1 and Q6 use UQA's borrowed projected-row scan. Q6 is a simple three-predicate scan and one aggregate, where dynamic-value and row-dispatch overhead cost more than SQLite's compact VM loop. Q1 favors UQA because compiled positional expressions and six retained aggregate groups avoid SQLite's grouping overhead. The separate 100k-row `tpch_style` Q6 declares three secondary indexes, so its broad posting-support branches and intersections are more sensitive to Rayon scheduling.

The cursor originally calculated exact encoded sizes, blocked until the complete result was sealed as a `SharedSpill`, deep-cloned every retained batch through the independent-reader API, and then pivoted map-backed rows into columns. `SQLCursor` uniquely owns that materialization, so it now uses a consuming reader that moves in-memory batches; repeatable shared CTE readers retain clone behavior. The current 1.018x cursor/materialized ratio is within run noise. The remaining delta is exact-size accounting, full-result blocking, and row-to-column conversion. The result occupies about 0.86 MiB in the spill encoding and the benchmark asserts that it remains in memory under the 4 MB budget, so disk I/O and codec decode are not causes here. The public materialized UQA path also constructs `BTreeMap<String, Value>` rows before extracting tuples, while SQLite and DuckDB read typed tuple slots directly; that API-boundary cost is part of the measured end-to-end result, especially for the 15,602-row scan.

| Observed difference | Dominant mechanism in this fixture |
| --- | --- |
| Q1: UQA is 2.96x faster than SQLite | Six retained groups plus compiled positional aggregate inputs avoid the general grouping/sort path; SQLite reports `USE TEMP B-TREE FOR GROUP BY`. |
| Q1: DuckDB is 5.16x faster than UQA | DuckDB keeps filter, group, and aggregate data in vectorized typed columns; UQA crosses dynamic row/value dispatch. |
| Q6: SQLite is 1.90x faster than UQA | Both plans are full scans, confirmed by SQLite `EXPLAIN QUERY PLAN`; SQLite's compact typed VM has less per-row dispatch. |
| Q6: DuckDB is 5.54x faster than UQA | The simple filter-and-aggregate shape exposes DuckDB's vectorized predicate and aggregation advantage. |
| Cursor: 1.018x materialized UQA | Moving uniquely owned batches removed the deep clone; exact-size accounting, blocking, and row-to-column conversion remain within run noise. |
| Scan: external engines are 4.89-6.55x faster than the cursor | SQLite satisfies `ORDER BY id` through its primary-key auto-index, while UQA propagates document-ID order; dynamic map-backed rows and transfer stages remain the gap. |

The estimator correction was driven by ten retained Linux CI artifacts. Their Q6 UQA/SQLite ratios ranged from 1.596x to 3.390x when computed from sample medians and falsely failed the 3.0x ceiling five times; the slope ratios from the same measurements ranged from 1.608x to 2.751x and all passed. A later documentation-only commit then produced Q6 slope ratios of 3.169x and 3.140x on two hosted runners even though its executable inputs, runner image, and compiler matched a 1.827x passing run. That demonstrated a second flaw: an absolute cross-engine ceiling on heterogeneous hardware cannot identify a code regression. Tests now pin both corrections by supplying contradictory median/slope values and by proving that a repeatable paired head/base slowdown fails while an advisory external-ratio excursion does not.

`cargo bench --workspace --no-run` validates that every benchmark target builds, but it is not a measurement command. Workspace feature unification can produce a different LTO or code-layout binary from the package-scoped runner. Published comparisons must use `run-analytical-comparison.sh` end to end and must not mix measurements from different executable hashes.

These are same-process developer-machine measurements, not independent OLAP validation. The ratio ceilings are regression alarms rather than proof of parity. `work_mem = 1B` integration tests force backing spill and verify that the cursor yields at most 1,024 rows per batch.

Reproduce and emit a fresh provenance artifact with:

```sh
bash scripts/run-analytical-comparison.sh
```

Reproduce the CI regression protocol against a reachable base commit with:

```sh
python3 scripts/run-analytical-regression.py <base-commit>
```

## Hot-path optimization pass (2026-07-17)

The `feature/performance` branch removed five disproportionate hot-path costs found by sampling the criterion e2e suites with macOS `sample`:

1. `Value`'s `#[serde(untagged)]` derive built a formatted rejection error per variant per decoded value - about a quarter of the `SQLite` read profile was error construction. Replaced with a hand-written visitor (`uqa-core/src/types.rs`); a differential test pins parity against the derive.
2. `SQLite` field reads (`get_fields_bulk` / `get_fields_multi`) issued `json_type` + `json_extract` per field, so `SQLite` parsed each document body twice per requested field. They now fetch `body` once and extract fields in Rust.
3. The in-memory inverted index recomputed per-field document counts by walking every document (`O(corpus)` per query) and copied the vocabulary-wide doc-freq map per text query. Counts are now maintained incrementally and text search uses `field_stats_scalar`.
4. Text search deep-copied whole posting lists (one heap allocation per matching document) just to read term frequencies. `for_each_posting` walks entries in place.
5. INSERT maintained value indexes with a per-row old-value `SELECT` even for rows the uniqueness pre-check had proven new. Known-new writes skip the lookup; MERGE / INSERT SELECT / ON CONFLICT keep replacement semantics.

### Measured impact (before -> after, criterion medians)

In-memory engine, 10k docs (`cargo bench -p uqa-engine --bench sql_e2e`):

| Workload | Before | After | Change |
| --- | --- | --- | --- |
| `sql_text_match_10k` | 321.0 us | 120.8 us | -62.8% |
| `sql_text_match_multi_term_10k` | 977.7 us | 760.1 us | -23.9% |
| `sql_select_filter_10k` | 2.149 ms | 1.701 ms | -20.4% |

1M-doc scaling check (`cargo bench -p uqa-engine --bench sql_1m`):

| Workload | Before | After | Change |
| --- | --- | --- | --- |
| `sql_1m/text_match_top10` | 65.26 ms | 41.75 ms | -36.0% |

Persistent `SQLite` backend, 10k rows (`cargo bench -p uqa-engine --bench sql_sqlite_e2e`):

| Workload | Before | After | Change |
| --- | --- | --- | --- |
| `count_star_10k` | 756 ns | 739 ns | -2.5% |
| `pk_point_select_10k` | 38.05 us | 33.78 us | -11.5% |
| `indexed_eq_filter_10k` | 83.5 us | 46.3 us | -44.9% |
| `indexed_filter_order_limit_10k` | 437.7 us | 257.7 us | -40.4% |
| `order_limit_unindexed_10k` | 18.92 ms | 7.31 ms | -61.3% |
| `group_by_10k` | 26.89 ms | 11.09 ms | -58.4% |
| `filtered_join_10k_x_50` | 284.9 us | 143.4 us | -49.7% |
| `insert_batch_500` | 19.11 ms | 10.23 ms | -46.7% |
| `point_update_indexed` | ~350-540 us (unstable) | ~371 us (stable) | no regression |

Retrieval loop, 3 queries (`cargo bench -p uqa-engine --bench relevance`):

| Workload | Before | After | Change |
| --- | --- | --- | --- |
| BM25 | 51.2 us | 24.1 us | -52.9% |
| BayesianBM25 | 36.2 us | 24.1 us | -33.6% |

Unchanged paths (interleaved A/B verified, same-minute alternating binaries): `sql_inner_join_10k_x_1k` ~1.52 vs ~1.55 ms (+2%, p > 0.05, code-layout level), the then-current exact KNN 10k-by-32 engine case ~763 vs ~769 us (no change), and posting merge paths unchanged.

One deliberate semantic change rode along with the serde rewrite: serde's sequence form for internally-tagged enums no longer turns arrays like `[1, -1]` into `Temporal` / `Decimal` values - arrays that are not byte arrays now always decode as lists, which is the only round-trip-stable reading. `value_json_decoding_keeps_arrays_as_lists` documents it.

## TPC-H-style analytical pass (2026-07-19)

The `tpch_style` benchmark builds 100k synthetic `lineitem` rows and runs two repeatable analytical shapes: a Q1-style low-cardinality grouped aggregate with shared arithmetic subexpressions, and a Q6-style range-filtered revenue aggregate. Setup validates the Q1 qualifying-row count and exact Q6 revenue before timing. It is intentionally TPC-H-style rather than an audited TPC-H implementation; its purpose is to pin the engine paths that sampling identified, not to publish a TPC-H score.

The representation boundary remains unchanged: every scalar index scan produces a sorted, deduplicated `PostingList`, range predicates combine their document-id support, and only the final selected doc IDs cross into document projection. No row-set, bitmap, or format-specific filtering carrier was introduced. The dense bitmap in `BTreeIndex::scan` is a bounded internal sort/dedup strategy and is converted immediately into the same `PostingList`; sparse doc-ID spaces retain `sort_unstable`.

Sampling exposed five compounding costs:

1. Simple aggregates retained every input value and updated state their finalizers never read. Built-in accumulators now keep only the required streaming state, with exact `i128` integer sums, floating accumulation deferred until the first non-integer input, and lazy decimal promotion.
2. Single-table aggregation materialized whole document maps and cloned projected values. The document-store abstraction now offers an ordered visitor with a borrowed-value fast path for memory storage and an owned/batched default for persistent backends; dense sorted memory scans merge-walk documents and fields.
3. Every row rebuilt string-keyed evaluation state, lowercased aggregate names, and interpreted simple aggregate arguments through the general AST evaluator. Projected rows implement `RowLookup`, direct column inputs borrow slots, and supported binary aggregate expressions compile once while calling the canonical SQL binary-operation function for NULL, promotion, overflow, and division semantics. Built-ins without `FILTER`, `DISTINCT`, `ORDER BY`, or NULL-preserving collection behavior now feed those compiled inputs directly into their planned accumulator state; modified aggregates retain the general dispatcher.
4. Low-cardinality grouping repeatedly cloned keys and searched the result `BTreeMap`. Up to 32 groups now retain their accumulators in a bounded fingerprinted cache and flush once at the end; the 33rd distinct key permanently switches that query to the general map path.
5. Dense scalar range scans comparison-sorted large doc-ID vectors. When the doc-ID span is at most eight times the number of collected IDs, a cache-local bitmap performs sort/dedup using no more memory than the input vector; sparse ranges keep the prior comparison sort.

Final reference run (`cargo bench -p uqa-engine --bench tpch_style`, 20 Criterion samples):

| Workload | Central estimate |
| --- | --- |
| `tpch_style/q1_100k` | 29.154 ms |
| `tpch_style/q6_100k` | 5.854 ms |

The first exploratory `--quick` estimates were 250.21 ms for Q1 and 24.05 ms for Q6. Those figures document the optimization trajectory only: Q6 was first recorded after the earliest accumulator fix, and `--quick` is not statistically comparable to the final 20-sample run. The table above is preserved as the first analytical-pass baseline; the current baseline is recorded below.

## Unified retrieval and second analytical pass (2026-07-20)

This pass kept the same representation boundary across every workload: scalar, text, vector, graph, and hybrid candidate sets continue to cross execution stages as sorted `PostingList` values. The optimization adds a consuming intersection inside that abstraction, but it does not introduce a bitmap result type, row-set side channel, or data-type-specific carrier. Small consuming intersections use the existing allocating merge; at 4,096 entries or more the common API compacts the owned left buffer in place.

Time Profiler attributed 69.19% of Q1 samples to projected-expression evaluation and 17.66% exclusively to `memcmp`, mostly repeated document-field name comparisons. Q6 additionally spent material time allocating and destroying temporary posting-list intersections. The resulting fixes were:

1. Integer analytical expressions compile once to a postfix instruction stream evaluated on a fixed stack. Exact integer SUM/COUNT state is updated directly, while non-integer inputs, NULL, overflow, and division errors fall back to the canonical SQL evaluator.
2. `MemoryDocumentStore` interns document key layouts and resolves projection ordinals once per layout, eliminating per-row field-name comparisons without assuming every document has the same shape.
3. `PostingList::merge_intersection_owned` adaptively retains the allocating path for small inputs and reuses the left buffer for large owned inputs. Engine filters, Boolean operators, staged retrieval, and text/vector intersections use this same API.
4. Persistent text scoring can stream `(doc_id, term_frequency)` without decoding position blobs and fetch document lengths plus term frequencies in bulk. Hybrid search constructs statistics only for the analyzed query terms rather than copying vocabulary-wide field statistics.
5. The persistent postings primary key already covers `(table_name, field, term)`, so schema version 12 removes the redundant `_postings_term_idx`. GIN backfill projects only indexed text fields instead of cloning whole documents.
6. Vector top-k selection partitions candidates in linear expected time before sorting only the retained `k`. Graph label matching reads vertex IDs directly from the label index instead of cloning complete vertices.

All measurements below ran with `CARGO_BUILD_JOBS=10` and `RAYON_NUM_THREADS=10` on the same workstation.

### Analytical results

Configured 20-sample run (`cargo bench -p uqa-engine --bench tpch_style`):

| Workload | First pass | Current | Change |
| --- | --- | --- | --- |
| `tpch_style/q1_100k` | 29.154 ms | 17.306 ms | -40.6% |
| `tpch_style/q6_100k` | 5.854 ms | 5.169 ms | -11.7% |

The consuming-input posting-list benchmark uses equivalent owned inputs on both sides, so input destruction is part of both timed paths. At 1k entries the adaptive API remains statistically neutral (4.370 us allocating versus 4.482 us adaptive); at 100k it reduces the configured 100-sample central estimate from 918.07 us to 841.98 us (-8.3%).

### Unified retrieval results

`retrieval_workloads` builds equivalent fixtures for every index case, validates non-empty/capped results before timing, and then measures warm search on already-indexed data.

| Index build | Corpus | Central estimate |
| --- | ---: | ---: |
| Persistent GIN | 2k documents | 68.749 ms |
| Persistent IVF | 2k documents | 12.650 ms |
| Persistent GIN + IVF | 2k documents | 77.823 ms |
| Graph `PathIndex`, depth 1-3 | 1k vertices | 1.015 ms |

| Warm search | Corpus | Central estimate |
| --- | ---: | ---: |
| Bayesian text SQL, top 100 | 4k documents | 4.864 ms |
| IVF vector SQL, top 100 | 4k documents | 1.131 ms |
| Graph label `VertexMatch` | 1k vertices | 86.836 us |
| Direct text/vector hybrid, top 100 | 4k documents | 8.571 ms |

Targeted before/after probes isolated the largest retrieval changes: k-NN top-10 over 10k 32-dimensional vectors moved from about 802 us to 274 us (-65.8%), graph label matching moved from 206.64 us to 85.83 us (-58.5%), and the same 4k direct hybrid fixture moved from 12.010 ms to 8.802 ms (-26.7%) after query-scoped statistics. The configured unified run above is the publication baseline; these `--quick` pairs record direction and mechanism only.

The release-profile persistent probe independently recorded a 4k-document GIN build at 173.554 ms, IVF build at 25.082 ms, warm Bayesian SQL at 4.860 ms, direct Bayesian API search at 4.340 ms, vector SQL at 2.462 ms, and direct hybrid API search at 9.159 ms. Hybrid relevance remained above every existing floor: small-corpus NDCG@10/MAP@10 were 0.9143/0.3899 (floors 0.90/0.34), and large-corpus values were 0.7784/0.1062 (floors 0.74/0.05).

## Unified query-plan matrix (2026-07-31)

`query_matrix` is the end-to-end coverage benchmark for the unified executor. Its 31 cases cover every relational root (`QueryBlock`, set operation, and standalone `VALUES`), every `SourcePlan` variant (table, values, function, subquery, join, and `LATERAL`), regular and recursive CTEs, grouping sets, scalar and correlated subqueries, row/operator/hybrid retrieval, and the complete DML family (insert values/select/conflict, update/returning/from, delete/using, and merge). The relational and retrieval fixtures each contain 2k rows. Every case is validated before timing and uses 30 samples, a 500 ms warmup, and a 2 s target measurement window.

Mutation cases use fixed-state custom iterations. Preparation and restoration run outside the timer, while the complete target autocommit statement remains inside it. An INSERT therefore does not grow the input seen by later samples, and update/delete/merge cases always begin from the same row values.

Reproduce the published baseline with:

```sh
cargo bench -p uqa-engine --bench query_matrix --locked -- --save-baseline query_matrix_20260731
```

### Read plans

| Case | Central estimate |
| --- | ---: |
| Constant query block | 1.132 us |
| Row query block | 540.82 us |
| Aggregate query block | 4.077 ms |
| Window query block | 43.263 ms |
| `UNION` | 4.102 ms |
| `UNION ALL` | 1.499 ms |
| `INTERSECT` | 4.865 ms |
| `EXCEPT` | 4.845 ms |
| Standalone `VALUES` | 1.709 us |
| `VALUES` source | 3.911 us |
| Table-function source | 199.41 us |
| Subquery source | 513.43 us |
| Join source | 13.624 ms |
| `LATERAL` source | 914.60 us |
| Non-recursive CTE | 763.07 us |
| Recursive CTE (500 rows) | 2.772 ms |
| Grouping sets | 15.363 ms |
| Uncorrelated scalar subquery | 4.479 ms |
| Correlated `EXISTS` | 47.595 ms |

### Retrieval and mutation plans

| Case | Central estimate |
| --- | ---: |
| Text operator tree | 494.18 us |
| Bayesian operator tree | 2.002 ms |
| Vector operator tree | 1.781 ms |
| Hybrid residual filter | 1.164 ms |
| Hybrid fusion | 6.063 ms |
| Insert values | 717.77 us |
| Insert select | 1.155 ms |
| Insert on conflict | 737.37 us |
| Update returning | 1.156 ms |
| Update from | 7.595 ms |
| Delete using | 3.412 ms |
| Merge matched | 4.766 ms |

### Root-cause fixes

The matrix exposed three execution-boundary costs rather than isolated operator regressions:

1. An uncorrelated scalar subquery was executed once per outer row. A conservative physical-plan correlation analysis now distinguishes true outer references, and statement-scoped scalar/`EXISTS` caches initialize independent subqueries once while preserving per-row execution for correlated plans. The pre-fix Criterion pilot estimated about 10.6 s per invocation for the 2k-row case; the configured post-fix estimate is 4.479 ms (about 2,375x faster). The pre-fix full sample run was aborted because Criterion projected more than five minutes.
2. Exact single-statement calls reparsed, lowered, and optimized SQL on every execution. The cache now retains the parsed statement and logical plan, plus the optimized plan for in-memory read-only execution. Persistent sessions still lower and optimize after pinning each storage snapshot, and explicit transactions optimize against their current state. Against the saved pre-cache baseline, `SELECT 1` moved from 4.987 us to 1.132 us (-77.3%) and standalone `VALUES` moved from 14.850 us to 1.709 us (-88.5%). Scan-dominated cases remained statistically unchanged.
3. Every repeatable CTE/intermediate result was forced to a temporary file, including a one-row recursive working set. Shared materializations now retain encoded batches while they fit `work_mem` and retain the existing disk format after that budget is crossed. The 500-step recursive CTE moved from 163.03 ms to 2.772 ms (-98.3%, 58.8x), while the non-recursive CTE improved by about 46%. Forced-spill tests at `work_mem = 1B` continue to cover accumulated rows, recursive working sets, and duplicate state.

## Algebra carrier separation (2026-07-31)

The document-support refactor split three contracts that the former payload-bearing `PostingList` API combined:

- `DocSet` is the Boolean-algebra carrier.
- `Relation<K>` is a finite-support `DocId -> K` function whose value combination is defined by `K`'s semiring.
- `PostingList` remains document-id-ordered payload storage, while `RankedView` owns score order and top-k selection.

`carrier_layers` measures the new semantic carriers directly, without payload cloning or posting-storage policy. The configured 100-sample run is reproducible with `cargo bench -p uqa-core --bench carrier_layers`.

| Carrier operation (100k entries, 30% overlap) | Central estimate |
| --- | ---: |
| `DocSet::union` | 190.21 us |
| `DocSet::intersect` | 121.55 us |
| `DocSet::difference` | 154.77 us |
| `Relation<bool>::plus` | 147.54 us |
| `Relation<bool>::times` | 104.54 us |
| `Relation<LogSemiring>::plus` | 484.01 us |
| `Relation<LogSemiring>::times` | 124.76 us |

The existing `posting_list` benchmark IDs were retained so the payload-storage paths could be compared against an archive of `main` HEAD on the same machine. The full-run deltas for 100k union, intersection, consuming intersection, and difference ranged from -3.9% to +3.4%; the directions were mixed and all remained inside the workstation's +/-10% drift band.

The carrier split exposed a separate top-k optimization. A borrowed `RankedView::top_k` still builds the complete rank order because its output is rank ordered. `RankedView::select_top_k`, however, materializes a document-id-ordered `PostingList`; when the view has not already been ranked, it now partitions the candidates around k in linear time and sorts only the selected entries by document id. Empty and full-width selections bypass ranking entirely.

| Materialized top-k from 100k scored postings | HEAD | Carrier split + selection | Change |
| --- | ---: | ---: | ---: |
| k = 0 | 5.723 ms | 4.983 ns | >99.99% lower |
| k = 10 | 10.357 ms | 278.17 us | -97.3% |
| k = 100 | 10.396 ms | 287.76 us | -97.2% |
| k = 1,000 | 10.720 ms | 302.97 us | -97.2% |
| k = 100,000 | 535.60 us | 548.27 us | +2.4% |

The small full-width difference is not claimed as a regression. The k = 10 through 1,000 deltas are far outside the measured drift band and come from the O(N log N) to O(N) algorithm change. An earlier sequential run of the unchanged full-sort algorithm appeared to show a roughly 50% regression, but rerunning the HEAD binary after the CPU entered the same sustained-load state reproduced the slower absolute time; that apparent delta was thermal/frequency drift. The k = 0 and k = N cases are now permanent benchmark inputs so boundary fast paths cannot silently regress into a full score sort.

## Search hot-path pass (2026-08-09)

This pass measured the public persisted text path and the in-memory physical vector-index path in optimized `bench` binaries on the same local arm64 workstation. The machine exposes 20 logical CPUs; the second pre-baseline `top` sample reported 86.10% idle CPU with load averages 4.40, 3.97, and 3.60, while the second pre-final sample reported 85.43% idle after release linking. Each before/after Criterion run used 30 samples, a two-second warmup, a five-second target measurement, `--noplot`, the same deterministic fixture, and one `CRITERION_HOME`; absolute latency remains machine-specific.

`text_top_k` uses a persisted 5,000-document SQLite corpus, query `plan rust crate`, and `k = 10`. The implementation now counts the sorted posting union by multiway merge instead of materializing a `BTreeSet`, reuses pivot-bound and term-score buffers, and reads one document length per scored `(document, field)` rather than once per matching term. Candidate counts, fully scored counts, skip rates, and result exactness did not change.

| Text path | Candidates | Fully scored | Skip rate | Before | Current | Change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Block-Max WAND | 4,674 | 208 | 95.55% | 4.7080 ms | 3.8584 ms | -18.0% |
| WAND | 4,674 | 272 | 94.18% | 4.9115 ms | 3.9801 ms | -19.0% |

The vector cases use 10,000 deterministic 32-dimensional vectors and report `k = 10` and `k = 50` from the single `uqa-storage` `storage` benchmark executable. IVF and HNSW now cache each raw vector norm, compute the query norm once, and retain raw cosine score bits while removing per-candidate norm reductions and square roots; tensor-score collapse compacts one vector in place. HNSW also borrows adjacency slices instead of cloning them and uses a hash set for visited nodes.

| Vector path | k | Before | Current | Change |
| --- | ---: | ---: | ---: | ---: |
| Trained IVF | 10 | 333.46 us | 188.80 us | -43.4% |
| Trained IVF | 50 | 335.43 us | 198.79 us | -40.7% |
| HNSW | 10 | 212.63 us | 148.31 us | -30.2% |
| HNSW | 50 | 214.41 us | 145.17 us | -32.3% |

A 2026-08-10 completion audit rebuilt the current source and wrote fresh Criterion artifacts while unrelated sustained jobs drove idle CPU as low as 47.5%. Under that contention, Block-Max WAND measured 4.0084 ms (-14.9% from the original baseline), WAND 4.0781 ms (-17.0%), trained IVF 205.39 us and 204.98 us for k = 10 and k = 50 (-38.4% and -38.9%), and HNSW 159.72 us and 159.92 us (-24.9% and -25.4%); the unchanged brute-force controls simultaneously slowed 4.5% and 2.9%. This audit confirms that the final source retains the optimization and exact candidate/scoring counts, but the lower-load table above remains the representative latency comparison.

Reproduce the measured release benchmarks with the following commands. A saved baseline directory may be supplied through `CRITERION_HOME` to obtain Criterion's distribution comparison in the same run.

```sh
cargo bench -p uqa-engine --bench text_top_k --locked -- --warm-up-time 2 --measurement-time 5 --sample-size 30 --noplot
cargo bench -p uqa-storage --bench storage --locked -- vector_index_knn --warm-up-time 2 --measurement-time 5 --sample-size 30 --noplot
```

### Vector quality and throughput suite

The checked vector runner is a module of the existing `uqa-engine` `retrieval_workloads` benchmark executable. It opens a real SQLite file, creates and loads one vector table through parameterized SQL, closes and reopens the engine for the exact phase, creates IVF through timed SQL DDL and reopens for the IVF phase, drops IVF through SQL, creates HNSW through timed SQL DDL, and reopens for the HNSW phase. Every quality and timed query calls `Engine::sql`, so statement-cache handling, persistent snapshot synchronization, SQL lowering and optimization, physical-index selection, execution, scoring, ordering, and result-row materialization remain inside the query boundary; no direct `VectorIndex` or `Engine::knn_search` call supplies benchmark results.

```sh
bash scripts/run-vector-search-benchmark.sh
```

The default `standard` profile contains 100,000 persistent 128-dimensional rows, 1,000 held-out quality queries, and 25 held-out queries per timed batch; `smoke` uses 10,000 rows by 32 dimensions and 100 quality queries, while `large` uses 1,000,000 rows by 128 dimensions and 1,000 quality queries. Exact SQL supplies top-10 ground truth, and the reporter combines Criterion performance with recall@10, top-1 accuracy, MRR@10, exact-set rate, result completeness, top-1 cosine loss, and shared-result score error for IVF and HNSW. The manifest selects Criterion's mean because an expensive fixed batch may use flat sampling; the report divides that batch estimate by the profile's declared timed-query count and reports the one-shot SQL load and index DDL elapsed times separately. Raw ranks and validated reports use profile-suffixed paths below `target/benchmark-runs`; workload identities, SQL and storage boundaries, parameters, quality floors, metric definitions, and synthetic-fixture limitations are versioned in the [vector-search benchmark](../../benchmarks/vector-search/README.md).

The 2026-08-10 `standard` run below used rustc 1.90.0 on the local 20-CPU arm64 workstation. The first pre-run sample reported load averages 5.60, 6.34, and 6.09 with 34% idle CPU, while the final pre-run sample reported 7.07, 6.58, and 5.83 with 70% idle CPU, so these absolute values establish a functional local baseline rather than an interleaved low-noise regression claim. An initial HNSW run with `ef_search = 128` correctly failed the first 100-query quality gate at recall@10 0.382; the final 1,000-query run with the declared search width of 4,096 reached recall@10 0.9993 and 2.02x the exact SQL query throughput.

| Persistent SQL query path, 100k × 128, k = 10 | Recall@10 | Top-1 | Exact-set rate | Mean/query | Queries/s |
| --- | ---: | ---: | ---: | ---: | ---: |
| Exact `sqlite-bruteforce` | 1.0000 | 1.000 | 1.000 | 61.79 ms | 16.2 |
| IVF (`nlist=256`, `nprobe=32`) | 0.4596 | 0.496 | 0.000 | 38.01 ms | 26.3 |
| HNSW (`m=16`, `ef_search=4096`) | 0.9993 | 1.000 | 0.993 | 30.59 ms | 32.7 |

| Persistent SQL construction stage, 100k × 128 | Elapsed | Rows/s |
| --- | ---: | ---: |
| SQL table creation and parameterized batched load | 6.69 s | 14,953 |
| SQL `CREATE INDEX ... USING ivf` | 57.84 s | 1,729 |
| SQL `CREATE INDEX ... USING hnsw` | 284.26 s | 352 |

Release verification ran all 257 `uqa-storage` library tests and 129 storage integration tests, all 217 `uqa-scoring` unit and integration tests, and all 157 enabled tests in the consolidated `uqa-engine` `engine_search` harness; its two explicit profiling probes remained ignored because Criterion supplied the measurements. The vector suite includes graph invariants, recall floors, tensor collapse, persistence, SQLite and Key/Value restore, and transactional mutation; text exactness tests compare WAND and BMW with exhaustive scoring. These local measurements establish a same-machine regression baseline, not cross-machine or competitive performance claims.

## Reference numbers (post-optimization)

| Workload | Bench | Median time |
| --- | --- | --- |
| Posting list union (100k entries) | `cargo bench -p uqa-core --bench posting_list` | ~987 us |
| Posting list intersect (100k entries) | same bench | ~365 us |
| Posting list consuming intersect (100k owned inputs) | same bench | ~842 us |
| BM25 score (100k inner-loop iterations) | `cargo bench -p uqa-scoring --bench bm25` | ~407 us |
| SQL filter (10k rows) | `cargo bench -p uqa-engine --bench sql_e2e` | ~1.70 ms |
| SQL text match (10k docs) | same bench | ~121 us |
| SQL inner join (10k x 1k) | `cargo bench -p uqa-engine --bench join` | ~1.55 ms |
| Trained IVF top-10 (10k docs, dim 32) | `cargo bench -p uqa-storage --bench storage` | ~189 us |
| HNSW top-10 (10k docs, dim 32) | same bench | ~148 us |
| SQL text match (1M docs, top 10) | `cargo bench -p uqa-engine --bench sql_1m` | ~41.7 ms |
| TPC-H-style Q1 aggregate (100k rows) | `cargo bench -p uqa-engine --bench tpch_style` | ~20.1 ms |
| TPC-H-style Q6 indexed aggregate (100k rows) | same bench | ~6.54 ms |
| Persistent Bayesian text search (4k docs, top 100) | `cargo bench -p uqa-engine --bench retrieval_workloads` | ~4.86 ms |
| Persistent IVF search (4k docs, top 100) | same bench | ~1.13 ms |
| Persistent direct hybrid search (4k docs, top 100) | same bench | ~8.57 ms |
| Graph label match (1k vertices) | same bench | ~86.8 us |
| Relevance bench (3 queries, BM25) | `cargo bench -p uqa-engine --bench relevance` | ~24 us |
| RPQ concat 3-hop (1k vertices) | `cargo bench -p uqa-graph --bench rpq` | ~2.2 us |
| Recursive CTE (500 rows) | `cargo bench -p uqa-engine --bench query_matrix` | ~2.77 ms |
| Uncorrelated scalar subquery (2k outer rows) | same bench | ~4.48 ms |

The hash-join path on `sql_inner_join_10k_x_1k` remains the load-bearing rewrite from the original pass (nested-loop fallback was ~3.46 s); if you regress past 50 ms here the hash detector probably stopped firing.

## How to refresh

1. `cargo bench --workspace --no-run` - confirms every bench compiles.
2. `cargo bench -p <crate> --bench <name> -- --save-baseline main` - produces criterion JSON under `target/criterion/` and names the baseline.
3. After changes: `cargo bench -p <crate> --bench <name> -- --baseline main` prints the delta per workload.
4. Update the numbers in the tables above. Where a number moved by more than ~10%, write one sentence on what changed so future readers know whether the shift was a real regression or a known optimization.

## Measurement caveats

- Criterion reports wall-clock time. Most reference workloads focus on single-threaded hot paths, while `tpch_style/q6_100k` can execute independent predicate branches through the Rayon-backed branch executor; scheduler contention therefore affects that result more strongly.
- Session-to-session drift on this machine reaches +/-10% for millisecond-scale e2e workloads (thermal and scheduling state). Deltas below that band are only trustworthy from interleaved A/B runs of both binaries within the same minute; the criterion `--baseline` comparison alone is sufficient only for larger movements.
- Unrelated build and test processes must be stopped before recording reference numbers; branch-parallel indexed filters are especially sensitive to scheduler contention.
- `--quick` undercuts measurement stability; use the benchmark's configured full sample count (20 for `tpch_style`, 100 by default elsewhere) for any number you intend to publish.
- `sql_1m` exercises a single text match across the full corpus without pre-filtering. It is a scaling check, not a latency target.
- Write benches that mutate persistent state (`point_update_indexed`) accumulate WAL growth across iterations and wobble accordingly; compare them only via interleaved A/B.
