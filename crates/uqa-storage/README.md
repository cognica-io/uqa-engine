# uqa-storage

`uqa-storage` is the UQA Engine crate for provider-independent storage contracts, shared codecs, and in-memory data structures.

Concrete SQLite implementations belong to `uqa-storage-sqlite`; the common storage crate has no runtime dependency on a database provider.

Shared occurrence staging and versioned codecs preserve lossless term keys, token graph edges, source offsets, multiplicity, and independent normalization lengths. Memory indexes store these occurrences and original stream-end/revision metadata, expose exact-key lookups, and require an atomic source rebuild to change a populated field's index revision. Persistent provider graph integration and source migrations remain under development; the [format contract](https://github.com/cognica-io/uqa-engine/blob/main/docs/design/occurrence-posting-format.md) distinguishes these codecs from existing linear indexes.

Memory and Key/Value analysis retains immutable compiled index/search revisions. Revision rebuilds publish the candidate binding with replacement postings only after successful staging and storage publication; exact durable descriptor restoration remains under development.

Applications should depend on `uqa-engine` or `uqa-client`. See the [repository README](https://github.com/cognica-io/uqa-engine) and the [manual](https://github.com/cognica-io/uqa-engine/blob/main/docs/manual/README.md).
