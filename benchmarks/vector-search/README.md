# SQL vector-search benchmark

This benchmark measures persistent SQLite vector search through the public SQL boundary. It is a module of the existing `uqa-engine` `retrieval_workloads` benchmark executable, so exact, IVF, and HNSW keep separate Criterion IDs without adding another Cargo benchmark binary to build and link.

## Measured boundary

The fixture opens a real temporary SQLite file with `Engine::open`, creates the table with SQL, and inserts every `(id, embedding)` row through parameterized SQL inside a transaction. It then closes and reopens the database before each query phase. The exact phase runs with the persistent `sqlite-bruteforce` vector index, the IVF phase follows a timed SQL `CREATE INDEX ... USING ivf` and reopen, and the HNSW phase follows SQL `DROP INDEX`, a timed SQL `CREATE INDEX ... USING hnsw`, and another reopen. All quality and Criterion queries call `Engine::sql` with `SELECT id, _score ... WHERE knn_match(...) ORDER BY _score DESC, id ASC LIMIT k`, so statement-cache handling, persistent snapshot synchronization, SQL lowering and optimization, physical-index selection, execution, scoring, ordering, and row materialization are inside the timed query boundary.

One database and one vector column are reused sequentially instead of materializing three corpus copies. Query vectors and bound `SQLParam` objects are generated before timing. The query measurements are warm-engine measurements after reopen and Criterion warmup, not cold filesystem-cache or concurrent-service measurements. SQL load and index DDL elapsed times are single construction observations rather than repeated Criterion distributions.

## Profiles

| Profile | Persistent rows | Dimensions | Quality queries | Timed queries per batch | Purpose |
| --- | ---: | ---: | ---: | ---: | --- |
| `smoke` | 10,000 | 32 | 100 | 25 | Fast implementation and CI validation |
| `standard` | 100,000 | 128 | 1,000 | 25 | Default local regression measurement |
| `large` | 1,000,000 | 128 | 1,000 | 10 | Explicit scale run with substantial setup cost |

Every profile uses `k = 10`, fixed disjoint corpus and query seeds, and deterministic signed `f32` vectors. Exact SQL supplies the top-k ground truth. IVF and HNSW parameters, Criterion sample counts, warmup and measurement durations, quality floors, construction SQL identities, and complete workload identities are versioned in [`manifest.json`](manifest.json).

The deterministic synthetic distribution makes regressions reproducible but does not model a production embedding model, filtered ANN queries, concurrent traffic, a cold page cache, or a standard ANN leaderboard dataset. Absolute results must not be used as cross-system performance claims.

## Metrics and gates

| Metric | Meaning |
| --- | --- |
| `recall_at_k` | Mean fraction of exact SQL top-k row IDs returned by the candidate SQL query |
| `top_1_accuracy` | Fraction of queries whose highest-ranked row equals exact SQL search |
| `mrr_at_k` | Mean reciprocal rank of the exact nearest row in the candidate top-k |
| `exact_set_rate` | Fraction of queries whose complete top-k row set equals exact SQL search |
| `result_count_rate` | Returned row count divided by `quality_query_count × k` |
| `mean_top_1_similarity_loss` | Mean exact-best SQL score minus candidate-best SQL score |
| `max_shared_score_abs_error` | Maximum `_score` error for a row returned by both SQL searches |

The reporter rejects missing rows, duplicate IDs, non-finite or out-of-range cosine scores, non-deterministic rank order, workload drift, storage-boundary drift, SQL-boundary drift, index-parameter drift, construction-statement drift, and quality below the checked floors. It reports SQL load and index-build rows per second, Criterion mean latency per query, and derived single-process queries per second.

## Run

Run the default 100k-row standard profile from the repository root:

```sh
bash scripts/run-vector-search-benchmark.sh
```

Select smoke or the explicit one-million-row profile with the first argument:

```sh
bash scripts/run-vector-search-benchmark.sh smoke
bash scripts/run-vector-search-benchmark.sh large
```

The runner writes ranked SQL observations to `target/benchmark-runs/vector-search-observations-<profile>.json`, Criterion artifacts below `target/criterion/sql_vector_search_query_batch/<profile>`, and the validated combined report to `target/benchmark-runs/vector-search-<profile>.json`. The report divides the Criterion mean for one fixed query batch by that profile's declared batch count; it does not label the aggregate as p95 latency.

## Changing the benchmark

Any change to the corpus size, dimensions, generator, seeds, quality-query count, timed batch count, `k`, SQL text, storage backend, reopen policy, index parameters, construction stages, quality metrics, or Criterion estimator must update both the Rust fixture and `manifest.json`. The reporter compares the emitted profile, storage identity, SQL execution identity, workload, algorithm parameters, and construction statements with the manifest so a changed boundary cannot silently reuse an old label.
