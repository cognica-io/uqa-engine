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
| `hybrid_positive_evidence` | `pool_positive_evidence(bayesian_match(body, $1), calibrated_vector_match(embedding, $2, 100), alpha => 0.5)` |

All systems order by `_score DESC, id ASC` and return at most 10 rows. Quality runs all 300 test queries; Criterion times a fixed batch of 25 queries after the complete quality pass has warmed the reopened engine.

## Metrics and gates

The reporter computes NDCG@10 against the true ideal ranking over each complete qrels set, MAP@10 with the denominator capped at 10, Recall@10 against every relevant document, and MRR@10. It validates unique query and document IDs, deterministic score ordering, each score domain, exact workload identity, persistent SQLite, database reopen, `Engine::sql`, SQL construction statements, and Criterion identities before applying absolute quality floors.

Hybrid search must also exceed the better text or vector result by at least 0.02 NDCG@10, 0.02 MAP@10, and 0.01 Recall@10. These relative gates ensure a result labeled hybrid cannot pass by silently degrading to one input signal.

The runner writes `beir-observations.json`, Criterion artifacts below `target/criterion/beir_hybrid_query_batch/scifact`, and the validated combined `beir-report.json` below `target/benchmark-runs`.

## Local reference run

The 2026-08-11 clustered-posting rerun used the complete pinned SciFact test workload on a local macOS arm64 host with rustc 1.90.0 and reused hash-validated prepared artifacts. The generated report identified a dirty implementation worktree and is therefore same-machine directional evidence rather than a release artifact or independent reproduction. Relative to the documented pre-cluster absolute baseline, the final point estimates changed by -29.7% for text, -5.7% for vector, and -13.4% for hybrid; a repeat against the immediately preceding clustered artifacts found no statistically significant text or vector change and a further 2.9% hybrid improvement.

| Persistent SQL system | NDCG@10 | MAP@10 | Recall@10 | Previous | Clustered | Queries/s |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `text_bm25` | 0.6860 | 0.6375 | 0.8193 | 20.13 ms | 14.16 ms | 70.6 |
| `vector_hnsw` | 0.6451 | 0.5959 | 0.7833 | 2.87 ms | 2.71 ms | 369.3 |
| `hybrid_positive_evidence` | 0.7259 | 0.6829 | 0.8422 | 56.49 ms | 48.91 ms | 20.4 |

| Persistent SQL construction | Previous | Clustered | Change | Rows/s |
| --- | ---: | ---: | ---: | ---: |
| Table creation and parameterized load | 0.574 s | 0.617 s | +7.4% | 8,406 |
| GIN index | 4.012 s | 2.312 s | -42.4% | 2,242 |
| HNSW index | 12.042 s | 12.242 s | +1.7% | 423 |

The report retains the original forced-preparation observations of 67.508 seconds for corpus embedding and 0.591 seconds for query embedding on CPU; the clustered rerun skipped both because every prepared identity and hash matched. Quality metrics remained bit-for-bit unchanged and every absolute and hybrid-relative gate passed. Construction timings are one-shot observations rather than distribution comparisons; only the text query and GIN paths are directly changed by the clustered posting implementation.

## Interpretation

SciFact is a small scientific-claim retrieval dataset, and MiniLM is one fixed general-purpose embedding model. Results exercise a real end-to-end hybrid workload but do not establish quality on other BEIR datasets, languages, embedding models, filters, concurrent traffic, or cold page caches. Absolute latency is machine-specific and must not be presented as a cross-system comparison without an interleaved protocol.

Dataset details and the original BEIR evaluation methodology are available from the [BEIR project](https://github.com/beir-cellar/beir); model details and licensing are on the pinned [all-MiniLM-L6-v2 model page](https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2).
