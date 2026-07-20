# Performance baseline

This document records UQA-RS benchmark baselines measured on the developer's Apple silicon macOS workstation (Apple M1 Ultra, 20 cores, 128 GB). Every number is reproducible via the `cargo bench` invocations listed below. All measurements use the `bench` profile (release + debug symbols, thin LTO, `codegen-units = 1`).

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

Unchanged paths (interleaved A/B verified, same-minute alternating binaries): `sql_inner_join_10k_x_1k` ~1.52 vs ~1.55 ms (+2%, p > 0.05, code-layout level), `knn_top10_10k_dim32` ~763 vs ~769 us (no change), posting-list algebra unchanged.

One deliberate semantic change rode along with the serde rewrite: serde's sequence form for internally-tagged enums no longer turns arrays like `[1, -1]` into `Temporal` / `Decimal` values - arrays that are not byte arrays now always decode as lists, which is the only round-trip-stable reading. `value_json_decoding_keeps_arrays_as_lists` documents it.

## TPC-H-style analytical pass (2026-07-19)

The `tpch_style` benchmark builds 100k synthetic `lineitem` rows and runs two repeatable analytical shapes: a Q1-style low-cardinality grouped aggregate with shared arithmetic subexpressions, and a Q6-style range-filtered revenue aggregate. Setup validates the Q1 qualifying-row count and exact Q6 revenue before timing. It is intentionally TPC-H-style rather than an audited TPC-H implementation; its purpose is to pin the engine paths that sampling identified, not to publish a TPC-H score.

The representation boundary remains unchanged: every scalar index scan produces a sorted, deduplicated `PostingList`, range predicates combine through the normal posting-list Boolean algebra, and only the final selected doc IDs cross into document projection. No row-set, bitmap, or format-specific filtering carrier was introduced. The dense bitmap in `BTreeIndex::scan` is a bounded internal sort/dedup strategy and is converted immediately into the same `PostingList`; sparse doc-ID spaces retain `sort_unstable`.

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
3. `PostingList::intersect_owned` adaptively retains the allocating path for small inputs and reuses the left buffer for large owned inputs. Engine filters, Boolean operators, staged retrieval, and text/vector intersections use this same API.
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
| k-NN top-10 (10k docs, dim 32) | `cargo bench -p uqa-engine --bench knn` | ~274 us |
| SQL text match (1M docs, top 10) | `cargo bench -p uqa-engine --bench sql_1m` | ~41.7 ms |
| TPC-H-style Q1 aggregate (100k rows) | `cargo bench -p uqa-engine --bench tpch_style` | ~17.3 ms |
| TPC-H-style Q6 indexed aggregate (100k rows) | same bench | ~5.17 ms |
| Persistent Bayesian text search (4k docs, top 100) | `cargo bench -p uqa-engine --bench retrieval_workloads` | ~4.86 ms |
| Persistent IVF search (4k docs, top 100) | same bench | ~1.13 ms |
| Persistent direct hybrid search (4k docs, top 100) | same bench | ~8.57 ms |
| Graph label match (1k vertices) | same bench | ~86.8 us |
| Relevance bench (3 queries, BM25) | `cargo bench -p uqa-engine --bench relevance` | ~24 us |
| RPQ concat 3-hop (1k vertices) | `cargo bench -p uqa-graph --bench rpq` | ~2.2 us |

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
