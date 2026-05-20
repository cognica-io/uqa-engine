# Key/Value-only Storage Migration

This document captures the proposed move from the current relational SQLite catalog (19 typed tables) to a single Key/Value store backed by SQLite (`_key_value (key BLOB PRIMARY KEY, value BLOB)`), with the long-term goal of swappable backends (RocksDB / redb / LMDB / sled) for non-SQL deployment targets such as iOS, WASM, and embedded / edge devices.

This is a design doc only. No code lands until the plan in [Phasing](#phasing) is signed off.

## Why move

- **Backend portability.** SQLite is excellent on desktop / server but drags the full SQL surface onto every embed. iOS and WASM ship fine with SQLite today, but a future Rust-native Key/Value (`redb`) or RocksDB backend would need a much smaller blast radius than today's 19-table schema permits.
- **Catalog complexity.** Most catalog tables are already `(name PRIMARY KEY, json_blob)` shaped. The relational form buys little — row counts are tiny (tens to thousands) and queries reduce to point lookups. The two genuine relational consumers are `_graph_membership` joins and `_postings` prefix scans, both of which are expressible as Key/Value prefix iteration.
- **Migration cost.** Each new schema column on a hot table (`_postings`, `_documents`) carries a v_N ALTER and a backfill. A Key/Value layout pushes all such evolution into JSON value-side encoding, forward-compatible by default.
- **Compatibility tradeoff.** The current UQA catalog contract is relational. Splitting on this boundary lets the UQA-RS implementation diverge where it actually wins (mobile / edge embedding) while keeping everything above the storage trait identical.

## Non-goals

- Replacing SQLite as the _physical_ backend in v1. SQLite stays; only the _logical schema_ collapses to a single table.
- Removing relational catalog inspection. `usql` introspection commands stay relational over the Key/Value layer (decode-and-display).
- Performance regression. The Key/Value layout must equal or beat the current schema on the BEIR fixture and the existing benchmark harness; otherwise we abort.

## Storage trait

The first code-level preparation is already separated from RocksDB itself: `uqa-storage::PersistentStorageBackend` is the engine-facing factory for persistent document stores, inverted indexes, vector indexes, and transaction control, while `uqa-storage::CatalogFacade` is the engine-facing metadata boundary for tables, analyzers, models, graph registries, path indexes, and planner statistics. `SQLiteStorageBackend` and `Catalog` are the legacy relational implementations today, while `uqa-storage::KeyValueStorageBackend` and `uqa-storage::KeyValueCatalog` provide the backend-neutral Key/Value implementation. `uqa-engine` rebuilds persistent state through trait objects via `Engine::from_persistent_backends(...)` instead of constructing SQLite table/index stores or calling a concrete catalog facade directly. A future RocksDB backend still needs only the physical `KeyValueStore` implementation; it should not need to reopen the engine restore path.

```rust
pub trait KeyValueStore: Send + Sync {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>>;
    fn put(&self, key: &[u8], value: &[u8]) -> Result<()>;
    fn delete(&self, key: &[u8]) -> Result<()>;
    fn scan_prefix(&self, prefix: &[u8])
        -> Result<Box<dyn Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + '_>>;
    fn batch(&self) -> Box<dyn KeyValueBatch + '_>;
}

pub trait KeyValueBatch {
    fn put(&mut self, key: &[u8], value: &[u8]);
    fn delete(&mut self, key: &[u8]);
    fn delete_range(&mut self, start: &[u8], end: &[u8]);
    fn commit(self: Box<Self>) -> Result<()>;
}
```

`KeyValueStore` is the boundary every higher crate (`uqa-engine`, `uqa-graph`, the catalog) talks to. Concrete implementations:

| impl | crate | platform | notes |
| --- | --- | --- | --- |
| `SQLiteKeyValueStore` | `uqa-storage-sqlite` | desktop | one table `_key_value (key BLOB PK, value BLOB)` |
| `RedbKeyValueStore` | `uqa-storage-redb` (new) | iOS / WASM | pure Rust, single file |
| `RocksDBKeyValueStore` | `uqa-storage-rocks` (new) | server / large datasets | C++ FFI, build-time cost |
| `MemoryKeyValueStore` | `uqa-storage` | tests | `BTreeMap<Vec<u8>, Vec<u8>>` behind a lock |

A cargo feature or constructor selects the physical store at build time / open time (`SQLiteKeyValueStore` for desktop, `RedbKeyValueStore` for the iOS target).

## Key layout

Keys use a fixed one-byte prefix per logical table plus length-prefixed UTF-8 user segments, so encoded user strings cannot collide with the prefix boundary. Numeric ids are big-endian fixed-width so prefix iteration yields ascending order. JSON values stay as today's serde encoding.

```
metadata/<key>                                   → value
table/<name>                                     → TableSchema JSON
analyzer/<name>                                  → config JSON
field-analyzer/<table>/<field>/<phase>           → analyzer_name
foreign-server/<name>                            → ForeignServer JSON
foreign-table/<name>                             → ForeignTable JSON
catalog-index/<name>                             → IndexDef JSON
named-graph/<name>                               → "" (presence-only)
vertex/<u64-be>                                  → Vertex JSON (label + properties)
edge/<u64-be>                                    → Edge JSON
graph-member/<graph>/<entity-type>/<u64-be>      → "" (presence-only)
graph-edge-out/<graph>/<source-be>/<label>/<edge-be> → "" (secondary index)
graph-edge-in/<graph>/<target-be>/<label>/<edge-be>  → "" (secondary index)
path-index/<graph>/<name>                        → label_sequences JSON
document/<table>/<doc_id-be>                     → Document JSON
posting/<table>/<field>/<term>/<doc_id-be>       → positions blob
posting-doc-term/<table>/<doc_id-be>/<field>/<term> → "" (reverse index)
doc-length/<table>/<doc_id-be>/<field>           → length u64-be
field-stats/<table>/<field>                      → total_length u64-be
vector/<table>/<field>/<doc_id-be>               → vector blob
model/<name>                                     → DeepModel JSON
scoring/<name>                                   → params JSON
sequence/<name>                                  → next-id u64-be
```

### Why secondary indexes for graph edges

Today's `_graph_edges_out (source_id, label)` and `_graph_edges_in (target_id, label)` indexes are the only graph queries that need sub-PK lookup. The Key/Value equivalent is two sibling prefixes (`graph-edge-out`, `graph-edge-in`) that hold presence-only markers. Inserting an edge becomes one primary write + two secondary writes; deleting symmetric. Adjacency queries scan one prefix and dereference the resolved edge ids back through the primary `edge/` namespace.

For other graph queries (vertex by label, edges by label), the canonical UQA implementation's `_graph_vertices_label` and `_graph_edges_label` indexes are also prefix-friendly and follow the same pattern.

### Posting list scan ordering

`posting/<table>/<field>/<term>/<doc_id-be>` puts the doc id at the key tail, so a prefix scan of `posting/<table>/<field>/<term>/` yields postings sorted by doc id — exactly what the existing posting-list intersection / union expects. WAND / BMW iterators get the same order-by-doc-id contract for free.

## Cross-key consistency

Single-batch mutations (e.g. `add_graph_edge` writing primary + two secondary keys) must commit atomically. The trait's `batch()` returns a `KeyValueBatch` that maps to:

- SQLite: a `BEGIN ... COMMIT` block on the per-engine connection.
- RocksDB: `WriteBatchWithIndex`.
- redb: a `WriteTransaction`.
- `MemoryKeyValueStore`: a staging buffer then a single `parking_lot::Mutex` write.

Cross-batch operations (e.g. graph orphan purge after Cypher resync) wrap multiple `batch().commit()` calls in a higher-level transaction at the engine layer. The current SQLite engine already serialises these through `tx_stack`; the Key/Value port keeps that lock and adds transaction hooks on `KeyValueStore` for the RocksDB / redb backends that need an explicit handle.

## Migration of existing data

1. **In-place upgrade path.** `Engine::open` detects v7 (relational) catalogs by reading `_metadata.schema_version` ≤ `7`. If found, the v8 migration walks every existing table and re-writes its rows into the `_key_value` namespace using the layout above, then drops the source tables in the same transaction. The migration is idempotent (re-running on a v8 catalog is a no-op) and gated by `pragma user_version` so a concurrent reader never sees a half-migrated state.
2. **No backwards compat shim.** Once a catalog is on v8 there is no "fall back to relational" mode. This is in line with the project's no-workarounds rule: a single source of truth at any given `schema_version`.
3. **Dump / restore tool.** A `usql --export-keyvalue path.keyvalue` command serialises the Key/Value namespace as a sorted text dump so users on the v7 → v8 cliff can audit the migration result. Same tool reads a dump back via `usql --import-keyvalue` for fresh databases.

## Operational concerns

- **Backup.** A single-table SQLite Key/Value store is `cp catalog.db` friendly. Backends with WAL (RocksDB) need their native checkpoint hook surfaced through the trait.
- **Compaction.** SQLite's auto-compaction stays in place; for RocksDB the engine triggers `CompactRange(None, None)` after a bulk-drop. redb compacts on commit.
- **Inspection.** `usql` gains `\keyvalue list <prefix>` and `\keyvalue get <key>` meta commands so support can inspect the catalog without a SQL shell. The `parity.md` golden fixture format gains an optional `key_value_dump` block for catalog-shape regressions.

## Phasing

Each phase ships in its own PR, gated by the BEIR fixture + the operator pipeline + golden SQL test suite passing without regression.

- **Phase 1 — Trait + SQLite-backed default.** Land `KeyValueStore` / `KeyValueBatch` traits. Add `SQLiteKeyValueStore` and `MemoryKeyValueStore`. New crate test exercises every method against both backends, and an engine integration test verifies `Engine::from_persistent_backends(...)` over the Key/Value catalog/backend.
- **Phase 2 — Move the leaf registries.** `_models`, `_scoring_params`, `_analyzers` are pure Key/Value today. Add a v8 catalog migration that copies them into the `_key_value` table and drops the originals. Update the four catalog methods to read/write through `KeyValueStore`. Smallest possible blast radius for first production exposure.
- **Phase 3 — Move the catalog metadata.** `_tables`, `_table_field_analyzers`, `_foreign_servers`, `_foreign_tables`, `_catalog_indexes`, `_named_graphs`, `_path_indexes`. Same pattern: migration step + method swap. Engine `restore_from_catalog` rewires to Key/Value iteration.
- **Phase 4 — Move the graph data.** `_graph_vertices`, `_graph_edges`, `_graph_membership`. Add the secondary-index prefixes. The orphan-purge logic switches from `DELETE ... NOT IN (SELECT ...)` to a Rust-side prefix scan with per-batch deletes.
- **Phase 5 — Move the hot path.** `_documents`, `_postings`, `_doc_lengths`, `_field_stats`, `_vectors`. This is where the Key/Value layer's per-key encoding and prefix-iteration cost matter. Benchmarks gate the merge: BEIR `ndcg@5` floor unchanged, `bench_*` p95 within 10% of v7.
- **Phase 6 — Alternative backends.** `uqa-storage-redb` for iOS / WASM, `uqa-storage-rocks` for large-corpus server deployments. Each backend reuses the trait + tests; only the constructor is platform-specific. SQLite stays the default.

## Open questions

- **Posting list value encoding.** Today positions are a SQLite BLOB storing varint deltas. Keep verbatim, or move to length-prefixed Postcard / bincode for consistency with other JSON values? Affects tooling more than runtime; revisit at Phase 5.
- **Vector index in Key/Value.** HNSW node graphs and IVF posting lists are bigger than the rest of the catalog combined. Phase 5 needs a dedicated benchmark; if Key/Value iteration is too slow we keep `_vectors` relational under a feature flag and isolate the regression.
- **Multi-write call-site discipline.** Primary + secondary writes (e.g. `edge/` + `graph-edge-out/` + `graph-edge-in/`) must always go through a single `KeyValueBatch`, never two raw `put` calls. The trait alone cannot prevent this drift. We enforce it by funneling every multi-key mutation through helper methods on the catalog facade (`save_edge_with_indexes`, `delete_edge_with_indexes`) that own the batch — direct `KeyValueStore::put` is reserved for single-key updates.
- **compatibility reconciliation.** `parity.md` currently asserts 19-table schema parity. After Phase 5 the parity claim shifts to _behavioural_ parity only — same SQL surface, same query semantics, same scoring results. Update `parity.md` accordingly once Phase 5 lands.
