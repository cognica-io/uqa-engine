# UQA-RS examples

The example suite is organized by language binding and platform. Every public binding provides the same five scenarios so that users can compare equivalent SQL, data, assertions, and lifecycle behavior without translating from Rust first.

| Scenario | Rust | Python | Node.js | Browser WASM |
| --- | --- | --- | --- | --- |
| Unified search | [`rust/unified-search`](rust/unified-search) | [`python/unified_search.py`](python/unified_search.py) | [`node/unified-search.mjs`](node/unified-search.mjs) | [`browser/unified-search.mjs`](browser/unified-search.mjs) |
| Vector KNN | [`rust/vector-knn`](rust/vector-knn) | [`python/vector_knn.py`](python/vector_knn.py) | [`node/vector-knn.mjs`](node/vector-knn.mjs) | [`browser/vector-knn.mjs`](browser/vector-knn.mjs) |
| Graph and Cypher | [`rust/graph-cypher`](rust/graph-cypher) | [`python/graph_cypher.py`](python/graph_cypher.py) | [`node/graph-cypher.mjs`](node/graph-cypher.mjs) | [`browser/graph-cypher.mjs`](browser/graph-cypher.mjs) |
| Storage and transactions | [`rust/storage-transactions`](rust/storage-transactions) | [`python/storage_transactions.py`](python/storage_transactions.py) | [`node/storage-transactions.mjs`](node/storage-transactions.mjs) | [`browser/storage-transactions.mjs`](browser/storage-transactions.mjs) |
| Extensibility | [`rust/extensibility`](rust/extensibility) | [`python/extensibility.py`](python/extensibility.py) | [`node/extensibility.mjs`](node/extensibility.mjs) | [`browser/extensibility.mjs`](browser/extensibility.mjs) |

Start with unified search for the complete relational, full-text, vector, graph, fusion, and host-language UDF story. Use the focused scenarios when learning or verifying one subsystem.

## Run by binding

- Rust: `cargo run -p example-unified-search`, replacing the package name with any entry in [`rust/README.md`](rust/README.md).
- Python: install a built `uqa` wheel, then run `python3 examples/python/unified_search.py`.
- Node.js: build `crates/uqa-node`, then run `node examples/node/unified-search.mjs`.
- Browser WASM: run `bash scripts/build-wasm.sh`, serve the repository root over HTTP, then open `examples/browser/`.

The Node.js and Browser WASM entry points share the binding-neutral JavaScript scenarios in [`javascript/`](javascript). Platform-specific files still own engine construction, persistence, callback registration, and resource cleanup.

All four environments also expose the local and Cloud HTTP SQL path: Rust uses `uqa_client::HttpEngine`, while Python, Node.js, and Browser WASM export `HttpEngine` from their existing packages. The binding test suites execute materialized SQL, atomic batch, and streaming requests against controlled HTTP servers; the [HTTP Engine reference](../docs/manual/reference/09-http-engine.md) contains user-facing examples.

The engine also ships smaller single-file Rust examples under [`crates/uqa-engine/examples/`](../crates/uqa-engine/examples), including encrypted-storage variants and the `doomql` demo.
