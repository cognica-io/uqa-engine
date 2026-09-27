# Browser WASM examples

These modules mirror the Rust, Python, and Node.js scenarios with the browser WASM binding. Build the WASM package with `bash scripts/build-wasm.sh`, serve the repository root with an HTTP server, and open `examples/browser/`.

The [real-browser Nori verification](../../benchmarks/nori/BROWSER.md) separately runs enabled and disabled packages in fresh Chrome sessions, checks persistent analyzer/index revisions after whole-page IndexedDB reloads, compares complete diagnostics with the native binding, and records host memory observations. Its commands also show how to retain a separate feature-disabled WASM artifact.

| Example | Coverage |
| --- | --- |
| [`unified-search.mjs`](unified-search.mjs) | Raw and Bayesian text retrieval, vector KNN, exact and robust fusion, cross-relation typed operator joins, a scalar callback, and Cypher over shared identities |
| [`vector-knn.mjs`](vector-knn.mjs) | Exact, HNSW, IVF and DiskANN access, canonical scores, filtering, mutation, rollback and persistent reopen |
| [`graph-cypher.mjs`](graph-cypher.mjs) | Named graph construction, mutation, traversal, and relational composition |
| [`storage-transactions.mjs`](storage-transactions.mjs) | IDBFS-backed reopen, rollback, savepoints, and independent sessions |
| [`extensibility.mjs`](extensibility.mjs) | Scalar, table, and aggregate JavaScript callbacks |

Each module also runs under Node.js against the generated Emscripten bundle. The JavaScript binding workflow additionally runs all five modules in real Chrome with `python3 scripts/verify-examples-browser.py --output output/playwright/examples-enabled/report.json`, then reloads the page and verifies the persisted DiskANN rows, scores and index definition. Use `--bundle target/morphology-binding-disabled/wasm/index.mjs` with a separately built feature-disabled artifact to verify that configuration. The Node.js and browser WASM examples share the SQL scenario modules in [`../javascript`](../javascript), while their entry points retain platform-specific engine construction, persistence, and close behavior.

For local or Cloud SQL without loading the embedded database, import the fetch-based `HttpEngine` from the same package; see the [HTTP Engine reference](../../docs/manual/reference/09-http-engine.md). The browser binding test executes SQL, atomic batch, and streaming requests through this class and verifies the typed wire representation.
