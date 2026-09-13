# uqa-storage

`uqa-storage` is the UQA Engine crate for provider-independent storage contracts, shared codecs, and in-memory data structures.

Concrete SQLite implementations belong to `uqa-storage-sqlite`; the common storage crate has no runtime dependency on a database provider.

Shared occurrence staging and versioned codecs preserve lossless term keys, token graph edges, source offsets, multiplicity, and independent normalization lengths. Memory indexes store these occurrences and original stream-end/revision metadata, expose exact-key lookups, and require an atomic source rebuild to change a populated field's index revision. SQLite and redb retain the same graph representation and migrate incompatible source-backed indexes on open; the [format contract](https://github.com/cognica-io/uqa-engine/blob/main/docs/design/occurrence-posting-format.md) describes the durable occurrence metadata.

Memory and Key/Value analysis retains immutable compiled index/search revisions. Revision rebuilds publish the candidate binding with replacement postings only after successful staging and storage publication; durable descriptor restoration resolves exact bundle and user-rule identities.

Applications should depend on `uqa-engine` or `uqa-client`. See the [repository README](https://github.com/cognica-io/uqa-engine) and the [manual](https://github.com/cognica-io/uqa-engine/blob/main/docs/manual/README.md).
