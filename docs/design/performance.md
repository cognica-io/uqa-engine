# Performance baseline (Rust)

Phase 11 ships a 2x-vs-Python performance gate as a master-plan exit
criterion. We split that criterion in two:

* **Rust baseline (this doc).** Numbers measured on the developer's
  Apple silicon macOS workstation. Reproducible via the `cargo bench`
  invocations listed below.
* **Python comparison (open).** The Python reference lives at
  [`cognica-io/uqa`](https://github.com/cognica-io/uqa). The 2x gate is
  the ratio of the equivalent Python benchmark on the same hardware
  divided by the Rust number in the table below. We do not run the
  Python reference inside this workspace, so this file documents the
  Rust side and treats the Python ratio as a follow-up that lands as a
  separate `docs/design/python_parity.md` once the comparison harness
  is wired up.

## Rust baseline

Hardware: Apple silicon macOS workstation, default `cargo bench`
release profile. Numbers are the median of three reported by Criterion
with `--quick`. Replace `--quick` with the default sample count (100)
when locking in a baseline you intend to compare against later.

| Workload | Bench | Median time | Throughput interpretation |
|---|---|---|---|
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
| k-NN top-10 (10k docs, dim 32) | `cargo bench -p uqa-engine --bench knn` | ~2.0 ms | brute-force vector path |
| SQL text match (1M docs, top 10) | `cargo bench -p uqa-engine --bench sql_1m` | ~1.48 s | scaling check |
| Relevance bench (3 queries, BM25) | `cargo bench -p uqa-engine --bench relevance` | ~84 us | retrieval loop only |
| Relevance bench (3 queries, BayesianBM25) | same bench | ~85 us | retrieval loop only |
| RPQ concat 3-hop (1k vertices) | `cargo bench -p uqa-graph --bench rpq` | ~2.17 us | NFA -> DFA -> traversal |

The hash-join path on `sql_inner_join_10k_x_1k` came down from ~3.46 s
(nested-loop fallback) to ~10 ms (~340x speedup) once the engine
detects the equijoin shape. That single rewrite is the biggest single
performance win in the port; if you regress past 50 ms here the hash
detector probably stopped firing.

## How to refresh

1. `cargo bench --workspace --no-run` — confirms every bench compiles.
2. `cargo bench -p <crate> --bench <name>` — produces Criterion JSON
   under `target/criterion/`.
3. Update the numbers in the table above.
4. Where a number moved by more than ~10%, write one sentence on what
   changed in the prose section so future readers know whether the
   shift was a real regression or a known optimization.

## Locking in a comparison vs Python

The 2x gate is a *ratio*, so what matters is keeping the Python
benchmark conditions identical to the Rust ones. The minimum reusable
artifact is a small Python harness that:

1. Loads the same fixture inputs the Rust bench loads (e.g. the
   1M-doc corpus from `tests/parity/`).
2. Runs the same logical query path through the Python `uqa` package.
3. Reports a median wall-clock time using `pyperf` or `timeit`.

Once that harness exists, this doc grows a third column (`Python
median`, `ratio`) and Phase 11's exit criterion becomes a one-line
check (every ratio >= 2.0).

## Caveats

* Criterion times are wall-clock, single-threaded. Multi-threaded
  workloads are not represented in the table; Phase 11 focused on the
  hot single-threaded paths.
* `--quick` undercuts measurement stability; use the default sample
  count (100) for any number you intend to publish or compare against.
* `sql_1m` exercises a single text match across the full corpus
  without any pre-filtering. It exists as a scaling check, not as a
  representative latency target.
