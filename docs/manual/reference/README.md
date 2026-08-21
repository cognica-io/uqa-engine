# Reference Manual

The reference manual describes public ways to operate and embed UQA Engine.

| Document | Subject |
| --- | --- |
| [Quick start](01-quick-start.md) | Installation, first SQL query, and first persistent database |
| [Rust engine API](02-rust-engine-api.md) | Engine construction, SQL execution, sessions, transactions, and extensions |
| [usql CLI](03-cli.md) | Command-line arguments, scripts, output, and backslash commands |
| [Storage and security](04-storage-and-security.md) | Memory, SQLite, redb, encryption, compression, and operational rules |
| [Search and ranking](05-search-and-ranking.md) | Text, vector, tensor, hybrid retrieval, and calibration |
| [Text analyzer pipelines](06-text-analyzers.md) | Built-ins, JSON stages, field binding, phases, synonyms, diagnostics, and lifecycle APIs |
| [Graphs](07-graphs.md) | Named graphs, Cypher, regular path queries, and SQL integration |
| [Bindings and extensions](08-bindings-and-extensions.md) | QueryBuilder, Python, Node.js, browser WASM, UDFs, and FDWs |
| [HTTP Engine](09-http-engine.md) | Direct authenticated local and Cloud SQL, atomic batches, streaming, and security |

For syntax organized by SQL feature, use the [Supported SQL manual](../sql/README.md). For implementation ownership and invariants, use the [Internals manual](../internals/README.md).
