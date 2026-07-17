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

## Reference numbers (post-optimization)

| Workload | Bench | Median time |
| --- | --- | --- |
| Posting list union (100k entries) | `cargo bench -p uqa-core --bench posting_list` | ~987 us |
| Posting list intersect (100k entries) | same bench | ~365 us |
| BM25 score (100k inner-loop iterations) | `cargo bench -p uqa-scoring --bench bm25` | ~407 us |
| SQL filter (10k rows) | `cargo bench -p uqa-engine --bench sql_e2e` | ~1.70 ms |
| SQL text match (10k docs) | same bench | ~121 us |
| SQL inner join (10k x 1k) | `cargo bench -p uqa-engine --bench join` | ~1.55 ms |
| k-NN top-10 (10k docs, dim 32) | `cargo bench -p uqa-engine --bench knn` | ~769 us |
| SQL text match (1M docs, top 10) | `cargo bench -p uqa-engine --bench sql_1m` | ~41.7 ms |
| Relevance bench (3 queries, BM25) | `cargo bench -p uqa-engine --bench relevance` | ~24 us |
| RPQ concat 3-hop (1k vertices) | `cargo bench -p uqa-graph --bench rpq` | ~2.2 us |

The hash-join path on `sql_inner_join_10k_x_1k` remains the load-bearing rewrite from the original pass (nested-loop fallback was ~3.46 s); if you regress past 50 ms here the hash detector probably stopped firing.

## How to refresh

1. `cargo bench --workspace --no-run` - confirms every bench compiles.
2. `cargo bench -p <crate> --bench <name> -- --save-baseline main` - produces criterion JSON under `target/criterion/` and names the baseline.
3. After changes: `cargo bench -p <crate> --bench <name> -- --baseline main` prints the delta per workload.
4. Update the numbers in the tables above. Where a number moved by more than ~10%, write one sentence on what changed so future readers know whether the shift was a real regression or a known optimization.

## Measurement caveats

- Criterion times are wall-clock, single-threaded. Multi-threaded workloads are not represented; the benchmark gate focuses on the hot single-threaded paths.
- Session-to-session drift on this machine reaches +/-10% for millisecond-scale e2e workloads (thermal and scheduling state). Deltas below that band are only trustworthy from interleaved A/B runs of both binaries within the same minute; the criterion `--baseline` comparison alone is sufficient only for larger movements.
- `--quick` undercuts measurement stability; use the default sample count (100) for any number you intend to publish.
- `sql_1m` exercises a single text match across the full corpus without pre-filtering. It is a scaling check, not a latency target.
- Write benches that mutate persistent state (`point_update_indexed`) accumulate WAL growth across iterations and wobble accordingly; compare them only via interleaved A/B.
