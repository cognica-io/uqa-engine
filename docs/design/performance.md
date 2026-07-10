# Performance baseline

This document records UQA-RS benchmark baselines measured on the developer's Apple silicon macOS workstation. Every number is reproducible via the `cargo bench` invocations listed below.

## Rust baseline

Hardware: Apple silicon macOS workstation, default `cargo bench` release profile. Numbers are the median of three reported by Criterion with `--quick`. Replace `--quick` with the default sample count (100) when locking in a baseline you intend to compare against later.

| Workload | Bench | Median time | Throughput interpretation |
| --- | --- | --- | --- |
| Posting list union (100k entries) | `cargo bench -p uqa-core --bench posting_list` | ~987 us | ~100 M doc-ids merged/sec |
| Posting list intersect (100k entries) | same bench | ~365 us | ~270 M doc-ids/sec |
| BM25 score (100k inner-loop iterations) | `cargo bench -p uqa-scoring --bench bm25` | ~407 us | ~250 M scoring ops/sec |
| BM25 with stats refresh | same bench | ~2.77 ms | one `IndexStats` rebuild per call |
| Calibration Brier-loss (100k) | `cargo bench -p uqa-scoring --bench calibration` | ~97 us | ~1 G samples/sec |
| Calibration full-pass | same bench | ~673 us | per-query calibration update |
| Spatial radius search (5 km, 100k pts) | `cargo bench -p uqa-storage --bench spatial` | ~2.65 ms | R-tree path |
| SQL filter (10k rows) | `cargo bench -p uqa-engine --bench sql_e2e` | ~3.78 ms | end-to-end SELECT WHERE |
| SQL text match (10k docs) | same bench | ~17.26 ms | analyzer + posting list + score |
| SQL inner join (10k x 1k) | `cargo bench -p uqa-engine --bench join` | ~10.08 ms | hash-join optimizer hit |
| k-NN top-10 (10k docs, dim 32) | `cargo bench -p uqa-engine --bench knn` | ~2.0 ms | IVF vector path |
| SQL text match (1M docs, top 10) | `cargo bench -p uqa-engine --bench sql_1m` | ~1.48 s | scaling check |
| Relevance bench (3 queries, BM25) | `cargo bench -p uqa-engine --bench relevance` | ~84 us | retrieval loop only |
| Relevance bench (3 queries, BayesianBM25) | same bench | ~85 us | retrieval loop only |
| RPQ concat 3-hop (1k vertices) | `cargo bench -p uqa-graph --bench rpq` | ~2.17 us | NFA -> DFA -> traversal |

The hash-join path on `sql_inner_join_10k_x_1k` came down from ~3.46 s (nested-loop fallback) to ~10 ms (~340x speedup) once the engine detects the equijoin shape. That single rewrite is the biggest single performance win in this implementation; if you regress past 50 ms here the hash detector probably stopped firing.

## Persistent (SQLite-backed) baselines

`sql_sqlite_e2e` measures end-to-end SQL through `Engine::open` on a
temp file, 10k rows, release profile, warm value indexes. These are the
numbers that regressed unnoticed while every engine bench ran in-memory.

| Workload | Bench member | Result |
| --- | --- | --- |
| count(*) (cached doc count) | `sqlite_e2e/count_star_10k` | ~0.7 us |
| PK point select | `sqlite_e2e/pk_point_select_10k` | ~38 us |
| Indexed equality filter | `sqlite_e2e/indexed_eq_filter_10k` | ~82 us |
| Indexed filter + ORDER BY + LIMIT | `sqlite_e2e/indexed_filter_order_limit_10k` | ~400 us |
| ORDER BY + LIMIT, unindexed column | `sqlite_e2e/order_limit_unindexed_10k` | ~19 ms |
| GROUP BY (50 groups) | `sqlite_e2e/group_by_10k` | ~26 ms |
| Filtered join (10k x 50) | `sqlite_e2e/filtered_join_10k_x_50` | ~280 us |
| INSERT batch of 500 | `sqlite_e2e_write/insert_batch_500` | ~7 ms |
| Point UPDATE via PK index | `sqlite_e2e_write/point_update_indexed` | ~250 us |

At 300k rows the same paths went from `count(*)` 3.5 s / PK select
346 ms / filtered join 1.24 s / `ORDER BY LIMIT` 6.9 s / point UPDATE
1.47 s before the value-index + bulk-read work to 0.06 ms / under 2 ms /
12 ms / 590 ms / 2-7 ms respectively (usql `\timing`, release). Lazy
per-column index builds cost one bulk field scan (~0.4-0.5 s per column
at 300k rows) on first use after open and are maintained incrementally
afterwards.

## How to refresh

1. `cargo bench --workspace --no-run` — confirms every bench compiles.
2. `cargo bench -p <crate> --bench <name>` — produces Criterion JSON under `target/criterion/`.
3. Update the numbers in the table above.
4. Where a number moved by more than ~10%, write one sentence on what changed in the prose section so future readers know whether the shift was a real regression or a known optimization.

## Caveats

- Criterion times are wall-clock, single-threaded. Multi-threaded workloads are not represented in the table; the benchmark gate focuses on the hot single-threaded paths.
- `--quick` undercuts measurement stability; use the default sample count (100) for any number you intend to publish or compare against.
- `sql_1m` exercises a single text match across the full corpus without any pre-filtering. It exists as a scaling check, not as a representative latency target.
