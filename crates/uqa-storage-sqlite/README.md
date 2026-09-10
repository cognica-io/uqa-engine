# uqa-storage-sqlite

`uqa-storage-sqlite` is the UQA Engine crate for SQLite connections, catalogs and migrations, document and retrieval indexes, transactions, graph persistence, key/value storage, encryption, and compressed VFS.

Import concrete types such as `ManagedConnection`, `SQLiteStorageProvider`, `SQLiteCompressionOptions`, `SQLiteError`, and `SQLiteGraphStore` from `uqa_storage_sqlite`. This provider implements the backend-neutral contracts in `uqa-storage` and `uqa-graph`. See the [development Rust migration notes](https://github.com/cognica-io/uqa-engine/blob/main/docs/manual/reference/10-upgrading.md#sqlite-provider-ownership-in-development) for the previous import paths and error-handling changes.

Applications should depend on `uqa-engine` or `uqa-client`. See the [repository README](https://github.com/cognica-io/uqa-engine) and the [manual](https://github.com/cognica-io/uqa-engine/blob/main/docs/manual/README.md).
