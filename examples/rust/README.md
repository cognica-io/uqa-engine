# Rust examples

These standalone workspace crates exercise the same five scenarios as the Python, Node.js, and Browser WASM bindings. Run any scenario with `cargo run -p <package>` from the repository root.

| Example | Package | Coverage |
| --- | --- | --- |
| [`unified-search`](unified-search) | `example-unified-search` | Raw and Bayesian text retrieval, vector KNN, exact and robust fusion, cross-relation typed operator joins, a scalar callback, and Cypher over shared identities |
| [`vector-knn`](vector-knn) | `example-vector-knn` | Exact, HNSW, and IVF vector access plus relational filtering |
| [`graph-cypher`](graph-cypher) | `example-graph-cypher` | Named graph construction, mutation, traversal, and relational composition |
| [`storage-transactions`](storage-transactions) | `example-storage-transactions` | redb-backed reopen, rollback, savepoints, and independent sessions |
| [`extensibility`](extensibility) | `example-extensibility` | Scalar, table, and aggregate Rust callbacks |

Each crate declares only the dependencies needed by its scenario, so its manifest is also a dependency reference for an external Rust application.

The [shared persistent Nori contract](../../tests/parity/nori/BINDINGS.md) additionally executes the enabled and disabled analyzer scenarios through the existing Rust integration harness against SQLite and redb, alongside equivalent Python, Node.js, and actual-browser checks.

For local or Cloud SQL without embedding storage, use `uqa_client::HttpEngine`; see the [HTTP Engine reference](../../docs/manual/reference/09-http-engine.md). Its live protocol coverage is in the `uqa-client` integration harness.
