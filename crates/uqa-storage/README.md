# uqa-storage

`uqa-storage` is the UQA Engine crate for provider-independent storage contracts, shared codecs, and in-memory data structures.

Concrete SQLite implementations belong to `uqa-storage-sqlite`; the common storage crate has no runtime dependency on a database provider.

Shared occurrence staging and versioned codecs preserve lossless term keys, token graph edges, source offsets, multiplicity, and independent normalization lengths. Provider graph integration and source rebuilds remain under development; the [format contract](https://github.com/cognica-io/uqa-engine/blob/main/docs/design/occurrence-posting-format.md) distinguishes these codecs from existing linear indexes.

Applications should depend on `uqa-engine` or `uqa-client`. See the [repository README](https://github.com/cognica-io/uqa-engine) and the [manual](https://github.com/cognica-io/uqa-engine/blob/main/docs/manual/README.md).
