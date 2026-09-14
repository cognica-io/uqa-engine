# Python examples

These programs mirror the Rust, Node.js, and browser WASM scenarios with the Python binding. Install a built `uqa` wheel, then run any file with `python3 examples/python/<name>.py`.

For Nori-specific artifact verification, run `python -m pytest -q tests/python` against the installed wheel. The [shared persistent contract](../../tests/parity/nori/BINDINGS.md) documents its complete enabled scenario and the explicit `UQA_TEST_NORI=disabled` mode for custom wheels built without Nori.

| Example | Coverage |
| --- | --- |
| [`unified_search.py`](unified_search.py) | Raw and Bayesian text retrieval, vector KNN, exact and robust fusion, cross-relation typed operator joins, a scalar callback, and Cypher over shared identities |
| [`vector_knn.py`](vector_knn.py) | Exact, HNSW, and IVF vector access plus relational filtering |
| [`graph_cypher.py`](graph_cypher.py) | Named graph construction, mutation, traversal, and relational composition |
| [`storage_transactions.py`](storage_transactions.py) | Persistent reopen, rollback, savepoints, and independent sessions |
| [`extensibility.py`](extensibility.py) | Scalar, table, and aggregate Python callbacks |

For local or Cloud SQL without embedding storage, call `uqa.HttpEngine.local(project)`, `uqa.HttpEngine.cloud(project, organization=...)`, `uqa.HttpEngine.from_env()`, or the explicit constructor; see the [HTTP Engine reference](../../docs/manual/reference/09-http-engine.md). The Python binding test covers CLI project lookup, SQL, atomic batch, and streaming requests through this class.
