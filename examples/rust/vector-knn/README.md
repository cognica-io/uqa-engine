# vector-knn

```sh
cargo run -p example-vector-knn
```

A `VECTOR(4)` column, bound vector parameters, and the four physical access paths available through `knn_match`:

- **Brute force** with no index: an exact scan and the score reference.
- **HNSW** (`USING hnsw`): a graph index trading a little recall for sublinear probing.
- **IVF** (`USING ivf WITH (lists, probes, train_threshold)`): partitions vectors into cells and probes the nearest ones.
- **DiskANN** (`USING diskann WITH (max_degree, search_list_size, beam_width)`): navigates a paged graph and scores candidates from canonical vectors.

The example asserts the literal top-three identities and exact equality between DiskANN and the unindexed scores on this small fixture. It orders results by `_score DESC, id`, composes KNN with a relational filter, checks a private vector replacement and rollback, then commits a replacement whose literal cosine score is one. The filter applies to the selected KNN pool, so its candidate count is widened to include all six rows.

The same scenario runs in memory and in a fresh temporary SQLite database. Closing and reopening that database preserves all six committed rows, their scores and the DiskANN index definition. The temporary directory is removed after its engines close. This is functional verification; the tiny corpus establishes no recall or performance claim for larger datasets.
