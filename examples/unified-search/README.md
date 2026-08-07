# unified-search

The case for one engine, in one file.

```sh
cargo run -p example-unified-search
```

A paper-search pipeline over six documents and a citation graph. It runs, in order:

1. **Lexical retrieval two ways.** `text_match` scores with raw BM25; `bayesian_match` returns a calibrated posterior probability, which is what makes the scores comparable across queries and fusable below.
2. **Vector neighbours.** `knn_match` over a `VECTOR(3)` column.
3. **Fusion, with the contract named.** `fuse_bayesian_evidence` is exact Bayesian fusion, applying one prior to signed likelihood-ratio evidence. `fuse_log_odds` is robust positive-evidence pooling, a ranking heuristic with no calibration theorem. They are separate named functions so the difference is never blurred.
4. **A user-defined function in the ranking.** `recency_boost` is registered read-only and immutable, so the optimizer may fold and reorder it.
5. **Graph traversal joined to a table in one statement.** `cypher(...)` is a table function; declaring its output column `int` lets a traversal join an integer primary key with no cast.
6. **A two-hop closure as a subquery predicate.**
7. **All of it in one statement.**

The point of the last two steps is what does *not* happen: no identifier mapping between a search index and a graph store, no result set copied across a process boundary, no reconciling two notions of "document 3". Every stage addresses the same rows.
