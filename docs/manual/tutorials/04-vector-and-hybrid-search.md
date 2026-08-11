# Tutorial 4: Vector and Hybrid Search

This tutorial builds four-dimensional embeddings, compares the exact and approximate vector access paths, and combines vector evidence with full-text evidence.

## 1. Create a retrieval table

```sql
CREATE TABLE passages (
    id INTEGER PRIMARY KEY,
    title TEXT NOT NULL,
    topic TEXT NOT NULL,
    body TEXT NOT NULL,
    embedding VECTOR(4) NOT NULL
);

INSERT INTO passages (id, title, topic, body, embedding) VALUES
    (1, 'Async runtimes', 'systems', 'Rust futures and task scheduling', ARRAY[0.95, 0.10, 0.05, 0.00]),
    (2, 'Ownership', 'systems', 'Rust ownership and borrowing', ARRAY[0.90, 0.20, 0.00, 0.10]),
    (3, 'Query planning', 'database', 'Relational and retrieval query plans', ARRAY[0.80, 0.10, 0.25, 0.05]),
    (4, 'Sourdough', 'cooking', 'Starter feeding and fermentation', ARRAY[0.05, 0.95, 0.10, 0.00]),
    (5, 'Knife skills', 'cooking', 'Safe slicing and chopping', ARRAY[0.00, 0.90, 0.20, 0.05]),
    (6, 'Search ranking', 'database', 'BM25 and vector retrieval', ARRAY[0.70, 0.05, 0.55, 0.05]);
```

Every vector must contain exactly four finite values because the column is `VECTOR(4)`.

## 2. Establish an exact baseline

Before creating a vector index, `knn_match` uses brute-force cosine scoring:

```sql
SELECT id, title, topic, _score
FROM passages
WHERE knn_match(embedding, ARRAY[1.0, 0.0, 0.0, 0.0], 3)
ORDER BY _score DESC, id ASC;
```

Save exact results when evaluating an approximate index. Recall at $k$ is

$$
\operatorname{recall@k} = \frac{|A_k \cap E_k|}{k},
$$

where $A_k$ is the approximate top-K identity set and $E_k$ is the exact set.

## 3. Build HNSW

```sql
CREATE INDEX passages_embedding_hnsw
ON passages USING hnsw (embedding)
WITH (m = 8, ef_construction = 32, ef_search = 16, seed = 42);
```

Run the same query and compare identities with the exact baseline. This data set is too small to demonstrate a meaningful speedup, but it verifies syntax and physical selection.

Drop the index before testing another physical index on the same column:

```sql
DROP INDEX passages_embedding_hnsw;
```

## 4. Build IVF

```sql
CREATE INDEX passages_embedding_ivf
ON passages USING ivf (embedding)
WITH (lists = 2, probes = 2, train_threshold = 4);
```

`lists` partitions the vector space and `probes` controls how many partitions are searched. The low training threshold only makes this six-row exercise possible; select real parameters from production-shaped measurements.

## 5. Understand candidate pools

The KNN predicate creates its candidate support before residual relational filtering:

```sql
SELECT id, title, topic, _score
FROM passages
WHERE knn_match(embedding, ARRAY[1.0, 0.0, 0.0, 0.0], 6)
  AND topic = 'database'
ORDER BY _score DESC, id ASC
LIMIT 2;
```

The query asks KNN for six candidates so that the topic filter still has enough rows. A KNN pool of two could lose relevant database rows before the filter runs.

## 6. Add text retrieval

```sql
CREATE INDEX passages_body_gin
ON passages USING gin (body);
```

Verify the two signals separately before fusing them:

```sql
SELECT id, title, _score
FROM passages
WHERE bayesian_match(body, 'query retrieval')
ORDER BY _score DESC, id ASC;

SELECT id, title, _score
FROM passages
WHERE knn_match(embedding, ARRAY[0.75, 0.05, 0.65, 0.00], 6)
ORDER BY _score DESC, id ASC;
```

## 7. Fuse text and vector evidence automatically

```sql
SELECT id, title, _score
FROM passages
WHERE text_match(body, 'query retrieval')
  AND knn_match(embedding, ARRAY[0.75, 0.05, 0.65, 0.00], 6)
ORDER BY _score DESC, id ASC
LIMIT 3;
```

The planner recognizes the same-relation text and vector leaves as hybrid retrieval, converts them to prior-free evidence, and combines their union support with exact Bayesian log-odds fusion. The text and vector modalities are conditionally independent under this automatic contract, and the resolved corpus prior enters exactly once. A relational conjunct such as `topic = 'database'` would remain a strict filter after fusion.

Use an explicit function when the fusion contract or its options are part of the application specification:

```sql
SELECT id, title, _score
FROM passages
WHERE pool_positive_evidence(
    bayesian_match(body, 'query retrieval'),
    knn_match(embedding, ARRAY[0.75, 0.05, 0.65, 0.00], 6)
)
ORDER BY _score DESC, id ASC
LIMIT 3;
```

The explicit robust pool replaces the automatic exact contract for this query. It is a ranking heuristic with gating and confidence scaling, and any explicit fusion call always overrides automatic selection.

## 8. Call hybrid retrieval from Rust

```rust
use uqa_engine::{Engine, HybridSearchParams};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = Engine::new();
    // Load the table, GIN index, rows, and vector index before this call.
    let hits = engine.hybrid_search(&HybridSearchParams {
        table: "passages",
        text_field: "body",
        text_query: "query retrieval",
        vector_field: "embedding",
        query_vector: vec![0.75, 0.05, 0.65, 0.00],
        knn_pool: 100,
        top_k: 10,
    })?;
    println!("{hits:?}");
    Ok(())
}
```

The API applies the same exact single-prior log-odds contract as automatic SQL. It always validates the vector field, dimension, index state, and finite query values, even when the vector candidate pool is empty. Applications that deliberately want gated robust ranking call `robust_hybrid_search` with `RobustHybridSearchParams` instead.

## 9. Evaluate before tuning

Measure exact recall, latency distributions, index build time, storage size, and relevance metrics on held out queries. Tune HNSW `ef_search` or IVF `probes` only against those measurements, and re-evaluate after data distribution changes.
