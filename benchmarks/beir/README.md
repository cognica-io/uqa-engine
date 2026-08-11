# BEIR hybrid-search benchmark

This benchmark downloads the pinned BEIR SciFact archive, generates real 384-dimensional `sentence-transformers/all-MiniLM-L6-v2` embeddings for all 5,183 documents and 300 test queries, loads them into a persistent SQLite database through parameterized SQL, creates GIN and HNSW indexes through SQL DDL, reopens the database, and evaluates text, vector, and hybrid retrieval entirely through `Engine::sql`.

The benchmark is a module of the existing `uqa-engine` `retrieval_workloads` benchmark executable. It does not add a standalone Cargo target, and the former in-memory `beir_hybrid_eval` example has been removed so there is one maintained BEIR execution path.

## Pinned inputs

[`manifest.json`](manifest.json) is the benchmark contract. It pins the SciFact HTTPS URL and archive SHA-256, the `test` qrels split, corpus and query counts, sentence-transformers package version, model repository and immutable model commit, embedding dimensions and normalization, CPU inference, SQL statements, index parameters, quality floors, comparative hybrid gates, and Criterion measurement settings.

The preparation script rejects an archive whose SHA-256 differs from the manifest, rejects unsafe ZIP paths and symlinks, loads the model with `trust_remote_code=False` at the pinned revision, verifies finite unit-length embeddings of the declared dimension, and hashes both prepared JSONL artifacts. Prepared artifacts live below `target/benchmark-runs` and are not committed.

Install the pinned embedding dependency into the Python environment used by the runner:

```sh
python3 -m pip install -r benchmarks/beir/requirements.txt
```

## Run

Run the complete cached pipeline from the repository root:

```sh
bash scripts/run-beir-benchmark.sh
```

Use `--force` to redownload or revalidate the archive as needed, load the pinned model from the benchmark-owned cache, and regenerate every embedding before SQL evaluation:

```sh
bash scripts/run-beir-benchmark.sh --force
```

The archive, extracted source data, and model cache are stored below `target/benchmark-runs/beir-cache`. Prepared corpus and query JSONL files are reused only while their hashes and the complete dataset and embedding identities match; changing SQL, index parameters, Criterion settings, or quality floors does not waste time by recomputing unchanged embeddings.

## Measured boundary

The Rust module opens a temporary SQLite file with `Engine::open`, creates `beir_documents` through SQL, and inserts `(id, source_id, body, embedding)` in parameterized 64-row batches inside one transaction. It executes SQL `CREATE INDEX ... USING gin` and SQL `CREATE INDEX ... USING hnsw`, closes the engine, reopens the same file, validates its row count, and then runs all quality and timed queries with `Engine::sql`.

| System | SQL retrieval predicate |
| --- | --- |
| `text_bm25` | `text_match(body, $1)` |
| `vector_hnsw` | `knn_match(embedding, $2, 10)` |
| `hybrid_log_odds` | `text_match(body, $1) AND calibrated_vector_match(embedding, $2, 100)`; the optimizer lowers this to exact single-prior Bayesian evidence fusion |

All systems order by `_score DESC, id ASC` and return at most 10 rows. Quality runs all 300 test queries; Criterion times a fixed batch of 25 queries after the complete quality pass has warmed the reopened engine.

## Metrics and gates

The reporter computes NDCG@10 against the true ideal ranking over each complete qrels set, MAP@10 with the denominator capped at 10, Recall@10 against every relevant document, and MRR@10. It validates unique query and document IDs, deterministic score ordering, each score domain, exact workload identity, persistent SQLite, database reopen, `Engine::sql`, SQL construction statements, and Criterion identities before applying absolute quality floors.

Log-odds hybrid search must also exceed the better text or vector result by at least 0.02 NDCG@10, 0.02 MAP@10, and 0.01 Recall@10. These relative gates ensure a result labeled hybrid cannot pass by silently degrading to one input signal.

The runner writes `beir-observations.json`, Criterion artifacts below `target/criterion/beir_hybrid_query_batch/scifact`, and the validated combined `beir-report.json` below `target/benchmark-runs`.

## Result provenance

Only a generated report whose embedded manifest hash matches the checked `manifest.json` describes this benchmark. Measurements from the former positive-evidence predicate are a different system and must not be relabeled as `hybrid_log_odds`; changing the fusion contract invalidates those quality and latency rows even when the corpus, indexes, and machine are unchanged.

## Exact-fusion reference run

The 2026-08-12 reference run used the complete pinned SciFact test workload on a local macOS 26.5.1 arm64 host with rustc 1.90.0. It regenerated every embedding with sentence-transformers 5.2.2 and model revision `1110a243fdf4706b3f48f1d95db1a4f5529b4d41`, then executed the persistent SQLite construction, reopen, quality, and Criterion phases from a dirty implementation worktree. The values below are same-machine evidence for this exact-fusion change, not a release artifact or a cross-machine performance claim.

| Persistent SQL system | NDCG@10 | MAP@10 | Recall@10 | Mean latency/query | Queries/s |
| --- | ---: | ---: | ---: | ---: | ---: |
| `text_bm25` | 0.6860 | 0.6375 | 0.8193 | 0.79 ms | 1,264.4 |
| `vector_hnsw` | 0.6451 | 0.5959 | 0.7833 | 2.46 ms | 407.1 |
| `hybrid_log_odds` | 0.7226 | 0.6820 | 0.8322 | 3.29 ms | 303.6 |

| Persistent SQL construction | Elapsed | Rows/s |
| --- | ---: | ---: |
| Table creation and parameterized load | 0.582 s | 8,903 |
| GIN index | 2.298 s | 2,256 |
| HNSW index | 12.284 s | 422 |

Fresh CPU embedding took 69.369 seconds for the corpus and 0.579 seconds for the queries after a 3.356-second verified archive download. The exact hybrid system cleared every absolute gate and exceeded the better single signal by 0.0366 NDCG@10, 0.0445 MAP@10, and 0.0129 Recall@10. These deltas are empirical properties of this pinned workload; the exact Bayesian fusion theorem itself does not promise that an additional signal improves ranking quality.

## Interpretation

SciFact is a small scientific-claim retrieval dataset, and MiniLM is one fixed general-purpose embedding model. Results exercise a real end-to-end hybrid workload but do not establish quality on other BEIR datasets, languages, embedding models, filters, concurrent traffic, or cold page caches. Absolute latency is machine-specific and must not be presented as a cross-system comparison without an interleaved protocol.

Dataset details and the original BEIR evaluation methodology are available from the [BEIR project](https://github.com/beir-cellar/beir); model details and licensing are on the pinned [all-MiniLM-L6-v2 model page](https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2).
