# uqa-graph

`uqa-graph` is the UQA Engine crate for named graphs, Cypher, regular path queries, and graph algorithms.

Memory graph stores and the backend-neutral persistent graph contract live here. The standalone `SQLiteGraphStore` adapter lives in `uqa-storage-sqlite`; graph algorithms do not depend on a SQLite driver or provider.

Applications should depend on `uqa-engine` or `uqa-client`. See the [repository README](https://github.com/cognica-io/uqa-engine) and the [manual](https://github.com/cognica-io/uqa-engine/blob/main/docs/manual/README.md).
