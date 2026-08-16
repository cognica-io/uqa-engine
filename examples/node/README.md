# Node.js examples

These programs mirror the Rust, Python, and browser WASM scenarios with the Node-API binding. Build `crates/uqa-node`, then run any file with `node examples/node/<name>.mjs`.

| Example | Coverage |
| --- | --- |
| [`unified-search.mjs`](unified-search.mjs) | Raw and Bayesian text retrieval, vector KNN, exact and robust fusion, typed operator joins, a scalar callback, and Cypher over shared identities |
| [`vector-knn.mjs`](vector-knn.mjs) | Exact, HNSW, and IVF vector access plus relational filtering |
| [`graph-cypher.mjs`](graph-cypher.mjs) | Named graph construction, mutation, traversal, and relational composition |
| [`storage-transactions.mjs`](storage-transactions.mjs) | Persistent reopen, rollback, savepoints, and independent sessions |
| [`extensibility.mjs`](extensibility.mjs) | Scalar, table, and aggregate JavaScript callbacks |

The Node.js and browser WASM examples share the SQL scenario modules in [`../javascript`](../javascript), while their entry points retain platform-specific engine construction, persistence, and close behavior.

For local or Cloud SQL without embedding storage, construct `HttpEngine` or call `HttpEngine.fromEnv()` from the same `uqa` package; see the [HTTP Engine reference](../../docs/manual/reference/09-http-engine.md). The Node.js binding test executes SQL, atomic batch, and streaming requests through this class.
