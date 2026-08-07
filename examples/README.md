# UQA-RS examples

Each subdirectory is a standalone crate that depends on the workspace the way an external consumer would. Run any of them with `cargo run -p <package>`.

| Example | Package | What it shows |
| --- | --- | --- |
| [`unified-search`](unified-search) | `example-unified-search` | The flagship: BM25 and Bayesian BM25, vector KNN, both fusion contracts, a user-defined function, relational filtering, and citation-graph traversal against one dataset in one session |
| [`vector-knn`](vector-knn) | `example-vector-knn` | `VECTOR(n)` columns, the `knn_match` predicate, and the brute-force, HNSW, and IVF access paths |
| [`graph-cypher`](graph-cypher) | `example-graph-cypher` | Named graphs driven entirely from SQL through the AGE-compatible `cypher(...)` table function, including joining a traversal against a table |
| [`storage-transactions`](storage-transactions) | `example-storage-transactions` | redb-backed durability and reopen, plus transactions, savepoints, and session isolation |
| [`extensibility`](extensibility) | `example-extensibility` | Custom Rust scalar and aggregate functions, and PL/pgSQL routines |

Start with `unified-search` if you want the argument for a unified engine in one file; start with the others if you want one subsystem at a time.

These are separate crates rather than `cargo` examples so that each one declares only the dependencies it actually needs, and so the manifests show exactly what an application must depend on.

The engine also ships smaller single-file examples under [`crates/uqa-engine/examples/`](../crates/uqa-engine/examples), including the encrypted-storage variants and the `doomql` demo.
