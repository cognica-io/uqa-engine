# Browser WASM examples

These modules mirror the Rust, Python, and Node.js scenarios with the browser WASM binding. Build the WASM package with `bash scripts/build-wasm.sh`, serve the repository root with an HTTP server, and open `examples/browser/`.

| Example | Coverage |
| --- | --- |
| [`unified-search.mjs`](unified-search.mjs) | Raw and Bayesian text retrieval, vector KNN, exact and robust fusion, cross-relation typed operator joins, a scalar callback, and Cypher over shared identities |
| [`vector-knn.mjs`](vector-knn.mjs) | Exact, HNSW, and IVF vector access plus relational filtering |
| [`graph-cypher.mjs`](graph-cypher.mjs) | Named graph construction, mutation, traversal, and relational composition |
| [`storage-transactions.mjs`](storage-transactions.mjs) | IDBFS-backed reopen, rollback, savepoints, and independent sessions |
| [`extensibility.mjs`](extensibility.mjs) | Scalar, table, and aggregate JavaScript callbacks |

Each module also runs under Node.js against the generated Emscripten bundle, which is how CI verifies the same SQL and assertions without a browser UI. The Node.js and browser WASM examples share the SQL scenario modules in [`../javascript`](../javascript), while their entry points retain platform-specific engine construction, persistence, and close behavior.

For local or Cloud SQL without loading the embedded database, import the fetch-based `HttpEngine` from the same package; see the [HTTP Engine reference](../../docs/manual/reference/09-http-engine.md). The browser binding test executes SQL, atomic batch, and streaming requests through this class and verifies the typed wire representation.
