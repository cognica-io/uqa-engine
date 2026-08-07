# vector-knn

```sh
cargo run -p example-vector-knn
```

A `VECTOR(4)` column, the `knn_match(field, ARRAY[...], k)` predicate, and the three physical access paths the planner can use:

- **Brute force** with no index: an exact scan, and the correctness reference for the other two.
- **HNSW** (`USING hnsw`): a graph index trading a little recall for sublinear probing.
- **IVF** (`USING ivf WITH (lists, probes, train_threshold)`): partitions vectors into cells and probes the nearest ones.

All three return the same rows here, which is the expected result: HNSW and IVF are approximate, and on a corpus this small they should agree exactly with brute force.

The example ends by composing KNN with a relational predicate. Note that the filter applies to the KNN candidate pool, not to the whole table, so the pool is widened to keep the intended number of results.
