# Rust examples

These standalone workspace crates exercise the same five scenarios as the Python, Node.js, and Browser WASM bindings. Run any scenario with `cargo run -p <package>` from the repository root.

| Example | Package | Coverage |
| --- | --- | --- |
| [`unified-search`](unified-search) | `example-unified-search` | Raw and Bayesian text retrieval, vector KNN, exact and robust fusion, typed operator joins, a scalar callback, and Cypher over shared identities |
| [`vector-knn`](vector-knn) | `example-vector-knn` | Exact, HNSW, and IVF vector access plus relational filtering |
| [`graph-cypher`](graph-cypher) | `example-graph-cypher` | Named graph construction, mutation, traversal, and relational composition |
| [`storage-transactions`](storage-transactions) | `example-storage-transactions` | redb-backed reopen, rollback, savepoints, and independent sessions |
| [`extensibility`](extensibility) | `example-extensibility` | Scalar, table, and aggregate Rust callbacks |

Each crate declares only the dependencies needed by its scenario, so its manifest is also a dependency reference for an external Rust application.
