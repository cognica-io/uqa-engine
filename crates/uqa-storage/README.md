# uqa-storage

`uqa-storage` is the UQA Engine crate for provider-independent storage contracts, shared codecs, and in-memory data structures.

Concrete SQLite implementations belong to `uqa-storage-sqlite`; the common storage crate has no runtime dependency on a database provider.

Applications should depend on `uqa-engine` or `uqa-client`. See the [repository README](https://github.com/cognica-io/uqa-engine) and the [manual](https://github.com/cognica-io/uqa-engine/blob/main/docs/manual/README.md).
