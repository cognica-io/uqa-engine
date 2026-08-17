# Python examples

These programs mirror the Rust, Node.js, and browser WASM scenarios with the Python binding. Install a built `uqa` wheel, then run any file with `python3 examples/python/<name>.py`.

| Example | Coverage |
| --- | --- |
| [`unified_search.py`](unified_search.py) | Raw and Bayesian text retrieval, vector KNN, exact and robust fusion, typed operator joins, a scalar callback, and Cypher over shared identities |
| [`vector_knn.py`](vector_knn.py) | Exact, HNSW, and IVF vector access plus relational filtering |
| [`graph_cypher.py`](graph_cypher.py) | Named graph construction, mutation, traversal, and relational composition |
| [`storage_transactions.py`](storage_transactions.py) | Persistent reopen, rollback, savepoints, and independent sessions |
| [`extensibility.py`](extensibility.py) | Scalar, table, and aggregate Python callbacks |

For local or Cloud SQL without embedding storage, call `uqa.HttpEngine.local(project)`, `uqa.HttpEngine.cloud(project, organization=...)`, `uqa.HttpEngine.from_env()`, or the explicit constructor; see the [HTTP Engine reference](../../docs/manual/reference/09-http-engine.md). The Python binding test covers CLI project lookup, SQL, atomic batch, and streaming requests through this class.
