# Storage Internals

The engine separates logical table and index behavior from provider mechanics. Persistent providers bind catalog and data handles to one session transaction context, while in-memory implementations satisfy the same high-level contracts without file durability.

## Storage boundary

```mermaid
flowchart TD
    A[uqa-engine] --> B[CatalogFacade]
    A --> C[PersistentStorageBackend]
    D[PersistentStorageProvider] --> B
    D --> C
    C --> E[Document store]
    C --> F[Inverted index]
    C --> G[Vector and tensor indexes]
    C --> H[B-tree and other value indexes]
    B --> I[Schema, graphs, models, routines, statistics]
    J[SQLite provider] --> D
    K[redb provider] --> D
```

`PersistentStorageProvider` creates catalog and backend handles together for a new session. `Engine::from_persistent_provider` retains that factory and can create sibling sessions. `Engine::from_persistent_backends` accepts already-bound handles and therefore cannot manufacture another transaction context.

## Logical storage contracts

`uqa-storage` defines backend-neutral traits for document rows, inverted postings, vector and tensor values, B-tree values, block-max metadata, spatial data, catalog records, and ordered Key/Value operations.

`KeyValueStore` supports point reads, ordered prefix scans, bounded key-only paging, atomic batches, and range deletion. Binary keys encode segments unambiguously and use big-endian numeric identities so lexical order preserves document order.

## Provider matrix

| Provider | Main implementation | Session transaction identity | Security notes |
| --- | --- | --- | --- |
| Memory | In-engine memory stores | Engine session state | No durability |
| SQLite | Catalog and storage modules in `uqa-storage` plus `uqa-storage-sqlite` Key/Value implementation | Managed connection | Plain, SQLCipher, or compressed VFS open paths |
| redb | `uqa-storage-redb` | Independent read or write transaction over shared database | No encryption at rest |

SQLite is the default persistent engine. redb uses the same SQL and logical storage surface through the provider contract.

## Durable catalog

The persistent catalog records:

- Schemas, tables, columns, constraints, views, and sequences
- Documents and table identities
- Full-text fields, postings, analyzer configuration, and analyzer assignment
- Vector and tensor field metadata, physical IVF and HNSW identities, and values
- Relational catalog indexes and statistics
- Named graphs, vertices, edges, memberships, deltas, and path indexes
- Scoring parameters and calibration models
- Foreign servers and tables
- Serializable ML model definitions
- SQL and PL/pgSQL routines

Runtime Rust or Python callback code is not a durable catalog object.

Analyzer JSON, field-phase assignments, GIN ownership, reopen behavior, and external synonym resources are detailed in [Analyzer pipeline internals](04-analyzer-pipeline.md).

## Full-text posting layout

Persistent postings are clustered rather than stored as one value per term and document. One cluster key represents `(table, field, term, doc_id / 65536)`. Document identities are delta encoded, and score data is separated from positional data.

```mermaid
flowchart LR
    A[Term posting stream] --> B[Cluster directory]
    B --> C[Score blocks up to 128 postings]
    B --> D[Separate positions]
    C --> E[BM25, WAND, and BMW]
    D --> F[Phrase and positional consumers]
```

A score cursor loads the directory and reuses one decode buffer for its current block. Score-only ranking carries document identity, term frequency, and document length without reading positions or making per-document length lookups.

SQLite stores clustered values in `_posting_clusters` and `_posting_documents`. redb and the SQLite Key/Value implementation use the same codec under separate score, position, and document-term namespaces.

## Vector storage

A vector field begins with exact brute-force access. `CREATE INDEX USING ivf` or `USING hnsw` installs a distinct physical index identity and durable metadata. Reopen attaches the stored structure; it does not rebuild merely because the process restarted.

Mutation maintains the selected physical index according to its contract. Index creation, replacement, and failure must publish catalog identity only after the physical candidate is durable and validated.

## Graph storage

Named graphs have explicit durable identity. Vertex, edge, property, membership, temporal delta, and path-index state is restored with the catalog. Graph and relational mutations enter the same engine transaction coordinator when they occur in one statement or explicit transaction.

## Statistics

Writes and schema changes invalidate affected statistics. Planning or introspection recomputes them lazily, while `ANALYZE` is the eager refresh path. Statistics are cost evidence and never query-correctness authority.

## Atomic publication

For a cross-store mutation, the engine prepares candidate row, catalog, index, graph, model, and cache state inside the transaction. Persistence succeeds before in-memory published registries advance. Rollback restores transaction-owned registries and discards provider changes.

```mermaid
sequenceDiagram
    participant Statement
    participant Engine
    participant Provider
    participant Cache
    Statement->>Engine: Candidate mutation
    Engine->>Provider: Persist candidate state
    alt persistence succeeds
        Provider-->>Engine: Durable
        Engine->>Cache: Publish and advance epochs
        Engine-->>Statement: Success
    else persistence fails
        Provider-->>Engine: Error
        Engine->>Cache: Keep prior published state
        Engine-->>Statement: Propagate error
    end
```

## Migrations

Provider open runs required schema and posting-format migrations before table and index handles are restored. The clustered-posting migration is bounded, atomic, idempotent, and validates output before recording its format marker. Failure retains the legacy representation and leaves no partial new representation.

An application upgrade should test open, restore, query, mutation, close, and reopen against a copy of production-shaped data. Storage compatibility is a release boundary even when the public SQL remains unchanged.

## Encryption and compression

SQLCipher is the preferred encrypted provider for security-sensitive deployments. The compressed VFS format uses authenticated encryption and commit metadata, but detecting replacement by an older valid whole-file snapshot requires an exact-state anchor stored in an independent trusted domain.

The [compressed VFS security contract](../../design/compressed-vfs-security.md) is mandatory reading before deploying compressed encryption. The [Key/Value backend design](../../design/kv-storage-backends.md) gives the full provider and redb contract.

## Source entry points

| Area | Path |
| --- | --- |
| Storage traits | [`crates/uqa-storage/src/lib.rs`](../../../crates/uqa-storage/src/lib.rs) |
| SQLite catalog | [`crates/uqa-storage/src/sqlite`](../../../crates/uqa-storage/src/sqlite) |
| SQLite Key/Value store | [`crates/uqa-storage-sqlite/src/lib.rs`](../../../crates/uqa-storage-sqlite/src/lib.rs) |
| redb provider | [`crates/uqa-storage-redb/src/lib.rs`](../../../crates/uqa-storage-redb/src/lib.rs) |
| Engine open and restore | [`crates/uqa-engine/src/engine_open`](../../../crates/uqa-engine/src/engine_open) |
