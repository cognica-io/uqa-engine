# unified-search

The case for one engine, in one file.

```sh
cargo run -p example-unified-search
```

A paper-search pipeline over six live documents, a four-document archive, and a citation graph. It runs, in order:

1. **Lexical retrieval two ways.** `text_match` scores with raw BM25; `bayesian_match` returns a calibrated posterior probability, which is what makes the scores comparable across queries and fusable below.
2. **Vector neighbours.** `knn_match` over a `VECTOR(3)` column.
3. **Fusion, with the contract named.** A same-relation text-and-vector conjunction automatically uses exact signed single-prior log-odds fusion. `fuse_bayesian_evidence` and `fuse_log_odds` explicitly select that exact contract, while `pool_positive_evidence` explicitly selects the robust ranking heuristic.
4. **Typed operator joins across relations.** `vector_similarity_join` and `hybrid_join` compare `papers` with `archived_papers`, preserve both identities, and use independently named vector fields, while `cross_paradigm_join` bridges a live graph vertex property to an archive field and then participates as a costed DPccp source in an ordinary SQL join.
5. **A user-defined function in the ranking.** `recency_boost` is registered read-only and immutable, so the optimizer may fold and reorder it.
6. **Graph traversal joined to a table in one statement.** `cypher(...)` is a table function; declaring its output column `int` lets a traversal join an integer primary key with no cast.
7. **A two-hop closure as a subquery predicate.**
8. **All of it in one statement.**

The point of the last two steps is what does *not* happen: no identifier mapping between a search index and a graph store, no result set copied across a process boundary, no reconciling two notions of "document 3". The operator-join stage separately demonstrates that two real SQL relations retain their own row identities.
