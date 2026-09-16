# Upgrading to UQA Engine 0.3.6

Version 0.3.6 fixes vector-threshold intersection optimization so every predicate retains its score contribution, matching documents and validation errors. Existing 0.3.5 database formats and analyzer configurations remain compatible. See the [release history](../../../HISTORY.md#036---2026-09-15).

Version 0.3.5 adds native Japanese Kuromoji analysis, completion and independent normalization, with shared Nori/Kuromoji mechanisms and both dictionaries in the CLI and official bindings. It also fixes analyzer parameter inference and updates TLS dependencies. Existing 0.3.0 generic/Nori descriptors and database formats remain compatible; Rust callers using analyzer struct literals or explicit simple-lowercase profiles must apply the source changes below. See the [release history](../../../HISTORY.md#035---2026-09-15).

The 0.3.0 release added native Korean Nori analysis, durable analyzer revisions and token graphs, graph-aware phrases and highlighting, PostgreSQL domains and data-modifying CTEs, and prepared-plan improvements. It also moved concrete SQLite APIs into `uqa-storage-sqlite` and changed low-level Rust SQL and retrieval interfaces. The [release history](../../../HISTORY.md#030---2026-09-14) records the changes.

The 0.2 series includes SQL object and privilege lifecycle changes, durable expression and unique indexes, expanded sequences and PL/pgSQL, native cross-process notifications, and a Node.js HTTP client that runs without native addons. These changes were introduced in [0.2.0](../../../HISTORY.md#020---2026-09-05); the [compatibility guide](../sql/09-compatibility.md) defines the verified PostgreSQL 18 surface and the behavior still being implemented.

## Unreleased native SQLite record adapter

The development `SQLiteRecordStore::for_native` API explicitly converts an initialized schema 48 native catalog to schema 49 with guarded current tables, table-owner bindings and a complete MVCC baseline. Conversion is atomic, checks the physical layouts and rejects a populated preexisting raw record history or duplicate sequence incarnations. Sequence commits reject concurrent aliases and competing live definition generations. Dynamic FTS skip/block-max accelerator tables are not yet mapped; conversion rejects files containing unmapped tables. Missing or changed native mapping guards cause reopen to fail. Plain, SQLCipher, compressed and encrypted compressed files use the same conversion. The released 0.3.6 catalog rejects schema 49, and its direct document writes are blocked by persistent guards. Returning to the released format requires restoring a closed-file backup.

Native mapping format 2 adds versioned graph label, adjacency and membership lookup entries. Opening a development format 1 file derives these entries from its complete source history in one atomic transaction, retaining original commit boundaries, database identity, allocations and receipts; failures preserve the previous format. This changes the native mapping marker, not catalog version 49 or the row codec. Earlier format 1 adapters reject the new marker. Prepared graph batches must now include their changed lookup entries together with source rows. Properties are not copied into these lookup entries, and property-only updates do not change their revisions. Complete graph API routing remains in progress.

This is a lower record-persistence API for the concurrent-storage implementation. Default `Engine::open`, native catalog/backend sessions and their SQL transaction routing do not yet use it; unbound legacy handles cannot operate on an explicitly converted file. The development `ManagedConnection::bind_native_records` entry point uses the same conversion and attaches its connection clones and native document/B-tree stores to common logical transactions; catalog metadata/schema/model/analyzer records, views, foreign servers/tables, ordinary table definitions, catalog indexes, column lifecycle, statistics and sequence catalog/value operations also join that session. `Catalog::open` can attach to an already bound native session, while graph/cache restoration and posting/vector APIs still require routing before Engine can open this format. Sequence value operations join the chosen session; Engine integration must still preserve autonomous allocation and transactional definition semantics. Document snapshots retain their read boundary across later writes and rollback, and reject mutation. Prepared native batches must include all rows affected by native cascades or graph invalidation. The adapter rejects omitted effects, merges provider-owned cache increments and commits current rows, history and receipts together. See the [storage contract](../internals/03-storage.md) and the remaining [implementation requirements](../../plans/0008-concurrent-storage-transactions.md).

## Unreleased SQLite Key/Value record format

The development SQLite Key/Value provider migrates the legacy `_key_value` table into versioned records on initial open. The copy, committed sequence and replacement guard views publish in one physical transaction; failed conversion preserves the complete previous format. Reopen validates the mapping and guard definitions instead of copying again. The actual crates.io 0.3.6 Key/Value writer and native catalog initializer reject upgraded files, including SQLCipher, compressed and encrypted compressed variants. This one-way upgrade requires restoring a closed-file backup to return to the released provider. Native relational SQLite databases are not converted by this Key/Value path.

`SQLiteKeyValueStore` keeps transaction state in a common logical session shared with its `ManagedConnection` clones. `new_session` creates independent private state. Binding during an active native transaction is rejected. The default retained-memory limit is 64 MiB per session; set it through `SQLiteKeyValueStorage::open_with_options`, `from_connection_with_options` or `SQLiteKeyValueStore::with_options`. Rebinding one connection with a different limit is rejected. Sibling sessions inherit the limit, which remains separate from SQL statement memory. Resource exhaustion is typed, and this implementation does not spill.

Low-level Rust code must perform ordinary reads and writes through the Key/Value store. `ManagedConnection::with` and `with_mut` reject a logical Key/Value session because physical SQLite closures cannot observe its private writes. Use `with_physical` only for explicit diagnostics or physical maintenance outside that session's transaction; it reads committed physical state, leaves internal write guards active and does not provide logical transaction semantics. `change_version` reports the durable record sequence instead of SQLite's connection-relative `data_version`.

Failed commits retain their sealed attempt, as with redb below. Retry `commit_transaction` to resolve the same prepared bytes and receipt; do not replay application operations after an uncertain native outcome. A failed rollback retains unresolved transaction state. Direct Key/Value concurrency does not yet enable concurrent Engine SQL: native relational session routing, shared-index merges, SQL isolation/publication, reclamation and platform acceptance remain in the [implementation plan](../../plans/0008-concurrent-storage-transactions.md).

## Unreleased Rust session affinity

Engine now checks that persistent catalog and data handles report the same transaction context before restoring or attaching a session. This includes `from_persistent_backends`, provider initial sessions and `new_session`. Native SQLite, SQLite Key/Value and redb report `StorageSessionAffinity`; combining handles from separate sessions fails with `StorageBackendError::Backend` whose source is `StorageSessionMismatch`, even when both refer to the same file. Use a provider-created pair or clone handles from one session. Connection clones preserve affinity; `new_session` creates another identity.

Custom `CatalogFacade`, `PersistentStorageBackend` and `KeyValueStore` wrappers must forward `transaction_affinity` from a reporting implementation. A reported identity cannot be paired with `None`. Existing custom pairs that both return `None` retain their caller-managed contract, but this does not establish support for the proposed concurrent transaction model. The affinity token is process-local and must not be persisted as a database or transaction ID.

## Unreleased Engine commit resolution

Engine callers using common logical providers must distinguish an ordinary failed transaction from an unresolved commit. `pending_commit()` exposes the latter and SQL reports `08007` while resolution is pending. Retry COMMIT to resolve the same prepared batch without replaying application operations; whole-transaction ROLLBACK can abort an uncommitted attempt, but if the receipt proves it already committed, Engine completes publication and returns `25000`. A later COMMIT that confirms a recorded abort restores rollback state and returns `25000`. Rust callback transactions and implicit SQL batch cleanup retain unresolved attempts instead of automatically treating them as rolled back. See the [Rust transaction contract](02-rust-engine-api.md#transactions-and-batches).

## Unreleased redb record format

The development redb provider migrates existing Key/Value files when `RedbStorage::open` opens them under redb's exclusive file-owner admission. It copies legacy records into version histories and atomically replaces the old writable table with a typed guard. Migration preserves binary keys, values and the committed change counter; interrupted publication reopens as a complete old or new format, and reopening the new format does not copy data again. The actual crates.io `uqa-storage-redb` 0.3.6 provider rejects the migrated file before writing; the redb dependency remains version 4.1.0. Keep a closed-file backup before this one-way upgrade; restoring that original file is required to return to the released provider. SQLite formats are unchanged by this redb conversion.

`RedbKeyValueStore` now reexports the common `VersionedKeyValueStore`. A failed commit retains a sealed attempt instead of discarding transaction state; `commit_transaction` retries the identical prepared records and resolves their receipt. Further mutations require resolving or rolling back that attempt. `rollback_transaction` reports an error containing the receipt if persistence says it already committed, and leaves the attempt available for commit resolution. Callers must not replay application operations merely because a native commit returned an error.

Private changes, savepoint history, batches and retained view metadata share a default 64 MiB session allowance. Use `RedbStorage::open_with_options(path, VersionedSessionOptions { retained_bytes })` to set it explicitly. This limit is separate from SQL statement memory; exhaustion returns a typed error and no spill files are written. Provider version/receipt reclamation and full concurrent Engine SQL support remain tracked in the [implementation plan](../../plans/0008-concurrent-storage-transactions.md).

## Vector-threshold optimizer compatibility

`uqa_planner::TreeOptimizerConfig::enable_merge_vector_thresholds` is deprecated and ignored for both `true` and `false`. Remove explicit assignments to avoid deprecation warnings; callers using `TreeOptimizerConfig::default()` need no source change. The optimizer keeps separate threshold operators for identical and nearby query vectors, preserving additive intersection scores, document support and invalid-threshold errors even inside nested operators. No data migration is required when upgrading from 0.3.5.

## Korean analysis and package features

Rust applications using Korean analysis enable `nori` on `uqa` or `uqa-engine`, for example `cargo add uqa@0.3.6 --features nori`. The feature includes the immutable dictionary through `uqa-nori-data`; the official Python, Node.js, and browser WASM packages enable it. No JVM or runtime dictionary download is required. Builds without this feature retain the non-Korean analyzers and reject Korean analysis requests explicitly.

The built-in `nori` analyzer and custom Korean pipelines retain exact dictionary and user-rule identities in durable descriptors. Deploy the same feature configuration and required resources in every process opening the database. Rich analysis retains UTF-16 terms, morphology, token graph edges, and corrected source spans; existing string projections remain available but cannot represent isolated UTF-16 units. See the [analyzer reference](06-text-analyzers.md) and [binding contracts](08-bindings-and-extensions.md) for analysis, normalization, and result APIs.

## Rust graph API and custom catalogs

Version 0.2.3 changes the public Rust graph API to support direct persistent storage reads. `Engine::graph_with` and `Engine::graph_with_mut` callbacks receive `uqa_graph::GraphStoreHandle`, not `MemoryGraphStore`. Import `GraphStore` for its methods, propagate storage errors, and use the owned `Result<Option<Vertex>>` and `Result<Option<Edge>>` point-read results without `.cloned()`. Mutation callbacks return `GraphStoreResult<T>` so an error rolls back the complete storage checkpoint. `PathIndex::lookup` likewise returns an owned, fallible result. Primary in-memory graph storage remains available through `MemoryGraphStore`.

Custom `GraphStore` implementations must support owned, fallible reads, edge memberships, and atomic mutation checkpoints; durable stores must implement bounded identity pages through indexed storage access. Custom `CatalogFacade` implementations must provide selective graph point reads, bounded identity pages, counts, memberships, and durable path-index data methods. Do not implement these by loading complete graph partitions into a resident map. Persistent sessions must bind graph handles to their own physical transaction. See [graph access](07-graphs.md) and [storage contracts](04-storage-and-security.md) for the current signatures and lifecycle requirements.

SQLite catalogs advance to version 46 for durable cache revisions, graph access metadata, path-index data, and invalidation. Initial open also performs the bounded, atomic legacy graph access/label metadata migration where needed. Validate an isolated copy before upgrading all processes; do not reopen a migrated database with an older binary.

## Package versions

Update the UQA packages used by one application together. Rust's `0.1` and `0.2` dependency requirements do not select `0.3.6`; change the requirement explicitly and regenerate the application's lockfile.

| Environment | Versioned installation |
| --- | --- |
| Embedded Rust | `cargo add uqa@0.3.6` |
| Rust HTTP client | `cargo add uqa-client@0.3.6` |
| Python and `usql` | `python -m pip install --upgrade uqa==0.3.6` |
| Embedded Node.js | `npm install @cognica-io/uqa@0.3.6` |
| Node.js HTTP only | `npm install --omit=optional @cognica-io/uqa@0.3.6` |
| Browser WASM | `npm install @cognica-io/uqa-wasm@0.3.6` |

The Rust workspace requires Rust 1.90 or newer. Python requires Python 3.8 or newer, and the Node.js package requires Node.js 16 or newer. The Node.js root package selects an exact-version native optional package for embedded execution; deploy the root and native packages from the same release. Deploy the Browser WASM JavaScript module and `uqa.wasm` from the same package together, including when updating a browser cache.

The [GitHub release](https://github.com/cognica-io/uqa-engine/releases/tag/v0.3.6) contains the Python and npm archives, standalone Node.js addons, and the status of publication to crates.io, PyPI, and npm. Rust applications using Git dependencies should select `tag = "v0.3.6"` consistently for every UQA dependency.

## Automatic statistics and session caches

Persistent query planning uses saved statistics while one database-level background worker collects and publishes replacements. Initial collection, committed-change thresholds, and a maximum dirty age of 60 seconds schedule maintenance automatically. Applications can inspect `Engine::automatic_statistics_status()` and continue to use explicit `ANALYZE` for a full collection. Pending changes survive reopen and follow transaction and savepoint rollback; existing estimates remain available until replacement.

Automatic collection samples at most 4,096 hierarchy rows. Both automatic and explicit collection omit text and binary values above 1,024 bytes from stored value samples, with an 8,192-byte encoded budget for other values; row and NULL counts still include those observations. Older oversized statistics are bounded during reopen and scheduled for one background replacement without another write. See [automatic column statistics](04-storage-and-security.md#automatic-column-statistics) for the complete estimate and payload contract.

SQLite tracks transactional cache revisions so data-only or statistics-only commits reuse unchanged definitions and physical handles. `Engine::new_session()` shares immutable committed catalog and statistics allocations while retaining independent transactions and storage handles. Custom providers may implement `CatalogFacade::cache_revisions()` with the same snapshot and rollback guarantees or return `None` for conservative refresh; an empty revision map must not stand in for unsupported tracking. See [cross-session cache refresh](04-storage-and-security.md#cross-session-cache-refresh).

Version 0.3.0 releases unwritten rollback-journal readers before waiting for writer ownership or snapshot publication. This prevents automatic statistics publication from retaining a SQLite read lock while an application COMMIT waits for readers to finish. The logical transaction, savepoints, and detached repeatable-read snapshot remain intact.

## Cypher expression validation

Deeply nested Cypher and long operator or indexing chains now fail with a parse error instead of exhausting the process stack. The parser limits recursive expression parsing and constructed expression trees to 64 levels; flat lists and independent projection items remain supported. Rust code that exhaustively matches `uqa_graph::cypher::ParseError` must handle `ExpressionTooDeep { limit, position }`. See the [Cypher contract](../sql/07-graph.md#cypher-table-function).

## Prepared statement analysis

Version 0.3.0 analyzes `PREPARE` before execution and infers omitted parameter types in PostgreSQL occurrence order. Statements with missing references or incompatible types can now fail when prepared. Replanning a statement whose result columns, types, or modifiers changed reports `0A000`; deallocate and prepare the updated query to adopt its new result contract. The session metadata view retains the original client SQL string. Register native SQL callbacks before preparing statements that refer to them; later callback registration invalidates cached plans and preserves the original result contract. Two-argument `round` calls require a numeric first argument, so cast floating-point expressions explicitly before supplying a precision. See [prepared statements](../sql/08-transactions-and-routines.md#prepared-statements).

The optimizer returns `OptimizerResult<T>` with either `OptimizerError::Expression(SQLError)` or `OptimizerError::JoinGraph(JoinGraphError)`. Update exhaustive error matches and preserve the SQL error instead of converting every planning failure into an internal error. Immutable constant failures can now occur before execution; unused rule inputs are removed first, and EXECUTE binds arguments before planning the prepared body. Stored routines and views are optimized only when execution needs their definitions.

`SQLError::Diagnostic` carries separate SQLSTATE, primary message, detail, and hint fields. Exhaustive error matches must handle this variant; the PostgreSQL server sends detail and hint in their protocol fields. `TableConstraintSet::columns_declared` records whether an empty relation has a declared SQL schema. Engine-managed SQL declarations set it to `Some(true)`; custom catalogs must preserve this field, while missing legacy metadata infers declaration from existing columns. Native document tables and table functions can retain deferred descriptors without making declared SQL tables accept missing columns.

## SQL AST and CHECK catalog updates

The 0.2 series adds `ColumnDef.check_is_local`, `ColumnDef.check_object_id`, `TableCheck.is_local`, and `TableCheck.object_id`. Rust applications constructing these structs directly initialize local-origin fields to `true` and unassigned CHECK identities to `None`; SQL compilation and engine-owned inheritance fill them automatically. Applications with custom catalogs must also implement the graph storage methods described above.

Initial open assigns and persists missing CHECK identities through the existing transactional catalog-migration boundary. CHECK OIDs then remain stable across constraint and relation renames and reopen. Old serialized definitions lack declaration history and retain their historical local-origin projection; new declarations and hierarchy changes record their actual origin. See [CHECK inheritance and lifecycle](../sql/02-ddl.md#inheritance-and-partitioning) for the SQL changes.

## SQL constraint and ownership updates

Recursive column, CHECK, and NOT NULL additions require ownership of each descendant whose definition changes, including a child whose existing definition is merged. Table privileges alone do not grant this authority; inherited membership in the child's owning role does. Recursion stops after merging an existing child definition and continues through every inheritance edge that still requires a change. Unauthorized operations restore the parent and all previously visited children.

`ALTER TABLE ONLY parent ALTER COLUMN column SET NOT NULL` creates a `NO INHERIT` constraint when an ordinary inheritance parent has children. A later recursive `SET NOT NULL` reports `0A000` instead of silently changing that constraint's inheritance status. An ONLY change on a partition parent with existing partitions reports `42P16`; an ordinary leaf or empty partition parent keeps an inheritable constraint. Existing constraint names remain stable when validation is completed, and constraint state remains consistent through rollback and reopen. See the [compatibility guide](../sql/09-compatibility.md) for the verified boundary.

`pg_constraint.conislocal` now distinguishes an inherited NOT NULL constraint from a local NOT NULL declaration, independently from `coninhcount` and from whether the column was redeclared locally. Recursive changes retain existing child constraint names and give newly inherited constraints their parent's name. Removing the last supplying parent or detaching a partition makes its retained constraint local; attaching a partition makes constraints supplied by its parent inherited. Explicit SET on a previously inherited NOT VALID constraint first makes it local; a subsequent SET validates it.

NOT NULL origin is recorded on new declarations and hierarchy mutations. Older serialized columns lack the original declaration history and keep their previous local catalog projection; opening a database does not infer that missing intent. CHECK identities are assigned by the initial-open catalog migration described above. The shipped providers migrate existing database files through initial open; custom storage implementations must satisfy the graph and catalog contracts above before upgrading.

The public Rust SQL AST adds `ColumnDef.not_null_is_local`. Applications using exhaustive `ColumnDef` struct literals must initialize it to `true` for locally declared columns; inherited NOT NULL definitions use `false`. SQL parsing and engine-managed inheritance initialize the field automatically, and deserialization defaults missing fields to the historical local projection.

## Compatibility verification

Version 0.3.0 adds `bound_columns: Option<Vec<String>>` to `FromClause::Table` and `SourcePlan::Table`. Initialize it to `None` when constructing an ordinary query AST or plan. Engine-owned SQL-standard routine definitions capture their source columns and maintain them across column deletion and renaming, so later additions cannot shift stored positional aliases. Initial open migrates legacy source metadata and explicit string-to-regclass constants in stored schema expressions transactionally; subsequent catalog reloads validate the persisted definitions. See [stored column lifecycle](../sql/02-ddl.md#alter-table) for the SQL behavior.

The release includes all 354 PostgreSQL 18.4 core and isolation tests through the official PostgreSQL drivers, with a pinned source inventory and recorded execution provenance. The PostgreSQL reference run passes this corpus in CI. The UQA run of the complete corpus remains unaudited; release compatibility claims continue to follow the checked differential fixtures and feature manifest. See [verification](../internals/09-verification.md) for commands and evidence boundaries.

## Python CLI update

The release includes the 0.2.1 correction to `usql` interactive startup and `usql script.sql` execution. Upgrade Python installations of 0.2.0 to restore these modes. The entry point uses Python's command-line arguments, so the console launcher and interpreter options are not parsed as SQL input. Python engine APIs and `usql -c` keep their existing behavior.

The following storage guidance applies when upgrading from the 0.1 series.

## Custom Rust storage implementations

The storage traits changed in the 0.2 minor release. Applications implementing `uqa_storage::DocumentStore` must implement `put_stored` and `get_stored` using `StoredDocument`. A record keeps its public field map separate from `DocumentMetadata`, including tuple `xmin`. Preserve metadata through scans, rewrites, snapshots, and persistence; storing it as a user field can collide with application data. The default `put` implementation replaces public fields while preserving existing metadata, while a new tuple version uses `put_stored` with explicit metadata.

The B-tree methods on `uqa_storage::PersistentStorageBackend` now use `ValueIndexKey::Column` and `ValueIndexKey::Index`. Preserve both namespaces even when the enclosed names are equal. Named expression indexes store composite `Value::Row` keys; SQL expression binding and evaluation belong to the engine. Update custom backend signatures and physical key encoding before compiling against 0.3.0. See [Storage internals](../internals/03-storage.md) and the trait definitions in [`document_store.rs`](../../../crates/uqa-storage/src/document_store.rs) and [`backend.rs`](../../../crates/uqa-storage/src/backend.rs).

## Persistent database migration

Opening an older supported database performs the required provider and catalog migrations. The 0.2 minor release adds typed tuple metadata, richer object and column identities, ownership and ACL records, bound routine and rule dependencies, and expression-index metadata. Initial open owns migration writes; later catalog refresh validates the persisted representation. The shipped SQLite and key-value providers handle their storage migrations through the normal engine open path.

1. Stop writers, close every engine using the database, and create a recoverable backup through the [storage backup procedure](04-storage-and-security.md#backups-and-copies).
2. Open a copy with the exact 0.3.6 application and its selected provider, encryption key, and compression configuration.
3. Execute representative reads, writes, role and privilege checks, stored routines and views, and retrieval queries. Verify indexes, transaction rollback, and close-and-reopen behavior with the application's data.
4. Update every process sharing the database before reopening the original file. Register process-local runtime callbacks again when the application starts.
5. If the application must return to an older binary, restore the pre-upgrade backup. Do not rely on an older binary reading a file migrated by 0.3.6.

Keep migration failures visible and resolve them before admitting writes. Retain encryption keys and any external rollback anchor according to the [storage and security contract](04-storage-and-security.md).

## Node.js HTTP clients

`HttpEngine`, SQL parameter helpers, and streaming execute in JavaScript without a native addon. Existing imports from `@cognica-io/uqa` continue to expose these APIs; `@cognica-io/uqa/http` is the explicit HTTP entry point for both CommonJS and ESM. An HTTP-only deployment can omit optional dependencies. Embedded `Engine` use still requires the platform's native package.

Use an explicit URL and token or `HttpEngine.fromEnv()` when deployment configuration already supplies credentials. The asynchronous `local()` and `cloud()` constructors require the installed `uqa` CLI and resolve a project once. Keep using the documented parameter wrappers for vectors and tensors, JavaScript `bigint` for exact signed 64-bit integers, and `Buffer` or `Uint8Array` for binary values. The [HTTP Engine reference](09-http-engine.md) describes errors, response limits, streaming, and cancellation.

## SQLite provider ownership

Version 0.3.0 moves all concrete SQLite persistence into `uqa-storage-sqlite`. This provider extraction is a Rust source migration from 0.2.3 and does not itself change the database format, SQL behavior, encryption options, or compressed-container layout. The analyzer and positional-index changes in this release require the separate migration described below. High-level `Engine` constructors keep their signatures.

| Previous Rust API | Rust API in 0.3.0 |
| --- | --- |
| `uqa_storage::sqlite::{Catalog, ManagedConnection, ...}` | `uqa_storage_sqlite::{Catalog, ManagedConnection, ...}` |
| `uqa_storage::{SQLiteStorageBackend, SQLiteStorageProvider, SQLiteTransaction, SQLiteCompressionOptions, SQLiteError, ...}` | The same concrete types under `uqa_storage_sqlite` |
| `uqa_graph::SQLiteGraphStore` | `uqa_storage_sqlite::SQLiteGraphStore` |
| `uqa_storage::IndexManager::new(connection)` | `uqa_storage::IndexManager::new()` |
| SQLite persistence methods on `BlockMaxIndex` | Import `uqa_storage_sqlite::SQLiteBlockMaxPersistence` to call `save_to_sqlite` and `load_from_sqlite` |

Add `uqa-storage-sqlite` as a direct dependency when importing its types. The common storage and graph crates do not re-export concrete SQLite implementations. `StorageBackendError::SQLite` is replaced by `StorageBackendError::Backend { backend: "SQLite", source }`; use `source.downcast_ref::<uqa_storage_sqlite::SQLiteError>()` for typed diagnostics. `TransactionError::Storage` now carries the provider-independent `StorageBackendError`. `Index::build` and `Index::drop_index` return `StorageBackendResult`, and SQLite value-key encoding belongs to the provider rather than implementing rusqlite traits on the shared `ValueIndexKey`.

## Internal Rust SQL ownership

SQL statement and scalar models, static row schemas, and type resolution now live in `uqa_sql::plan`, `uqa_sql::ir`, `uqa_sql::schema`, and `uqa_sql::type_resolution`. Existing planner and execution exports refer to the same definitions. Low-level callers of `RowSchema::view` or `RowSchema::relayout_physical_row` must import `uqa_execution::RowSchemaExecution`; physical rows and materialization remain execution-owned. Applications using `Engine` require no SQL or query-result migration for this ownership change.

Construct the plan-native `uqa_planner::OptimizerConfig` with `OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar)` when using the physical scalar runtime. The planner no longer supplies a default execution backend. Import `PlanExecutor`, `OperatorTreeDriver`, `OperatorOutput`, and `ExecutionStats` from `uqa_execution::operator_tree`, and parallel execution helpers from `uqa_execution::parallel`. Engine-level query APIs keep their existing signatures.

The operator-tree `QueryOptimizer` accepts immutable `IndexScanCandidate` values through `with_index_candidates`. Its `index_manager` field and `with_index_manager` constructor are removed; callers discover applicable indexes and their scan costs from their catalog snapshot before invoking the optimizer. The planner has no runtime dependency on `uqa-storage`; optimizer regression tests use it as a development dependency. Its existing retrieval IR still has transitive storage dependencies through `uqa-operators`; removing that coupling requires separating logical descriptors from bound execution objects.

## Durable analyzer descriptors

Version 0.3.0 persists exact named analyzer descriptors and independent index/search bindings. SQLite schema version 47 adds `descriptor_json` to `_analyzers` and `binding_json` to `_table_field_analyzers`. Key/Value catalogs store equivalent descriptor records alongside their compatibility labels. Initial open resolves legacy definitions and any synonym files, validates ownership, and rebuilds affected full-text indexes from original documents in the owning catalog transaction. Subsequent opens restore verified descriptors without reading original synonym files. See [analyzer persistence](../sql/05-analyzers.md#transactions-and-persistence).

Custom `CatalogFacade` implementations must atomically implement `save_analyzer_revision` and `replace_table_field_analyzer_binding`, expose the corresponding `load_analyzer_descriptors` and `load_table_field_analyzer_bindings` reads, and carry the complete records through rename, deletion, and transaction operations. The default write methods reject unsupported persistence. Custom inverted indexes must implement atomic `set_field_analyzer_revisions` for exact restoration. A configuration-only implementation cannot safely substitute for either contract.

Key/Value indexes now store complete token occurrences and original-source metadata under canonical binary term keys. Opening an older positional index requires its original documents and resolvable analyzer descriptors; Engine rebuilds it in the initial catalog transaction, including tokenless fields. Failure preserves the prior positional data. Direct `KeyValueInvertedIndex` users must supply original documents to `try_rebuild_documents` when `source_rebuild_required` returns true. The [occurrence format](../../design/occurrence-posting-format.md#keyvalue-publication-and-migration) describes the persistent keys and migration boundary. SQLite schema 48 stores complete occurrence graphs and source metadata separately from legacy positional tables. Initial open rebuilds legacy full-text indexes from original documents in the same catalog transaction; missing sources or unresolved descriptors fail without committing a partial migration. Standalone SQLite catalog preparation preserves the source-rebuild obligation. See [native SQLite ownership](../../design/occurrence-posting-format.md#native-sqlite-ownership) for the format and publication contract.

## Japanese analysis and distribution features

Version 0.3.5 adds opt-in `kuromoji` to `uqa` and `uqa-engine`, independently of `nori`; both Rust packages retain empty defaults. CLI, Python, Node.js and WASM distribution builds now default to both languages. `--no-default-features` excludes both dictionaries; add `--features nori` or `--features kuromoji` to select one. The WASM build script accepts those same feature arguments.

Deploy a build with the required language features and exact resources to every process opening a database with retained analyzers. The source and binary packages include the pinned Lucene, JDK, MeCab and IPADIC notices and both resource/model identities. The archive verifier checks complete embedded dictionary bytes in wheel, Node and WASM runtimes and complete resources in the source archive.

## Explicit normalization configuration

Version 0.3.5 adds `normalization: Option<NormalizationConfig>` to `uqa_analysis::Analyzer`. Existing Rust struct literals must add `normalization: None` or use `Analyzer::new` or `Analyzer::default`; the existing constructors still omit normalization. Use `.with_normalization(plan)` to select it explicitly. Omitted JSON configurations and existing generic/Nori descriptor identities remain unchanged, so this source change does not rewrite stored revisions or require a storage migration. `CompiledAnalyzer::normalize` and `normalize_budgeted` are now available in all analysis feature configurations; pipelines with no plan return `NormalizationUnavailable`. See [explicit normalization plans](06-text-analyzers.md#explicit-normalization-plans).

## Simple-lowercase profile configuration

`SimpleLowercaseConfig` now belongs to `uqa_analysis` and is still re-exported from `uqa_analysis::nori`. Its `unicode_profile` field changes from `String` to `UnicodeProfileSource`. Convert existing owned strings with `.into()`; existing string-literal `.into()` expressions and the default constructor continue to work. Legacy JSON strings preserve their Nori interpretation, resolved form and descriptor identity. The new explicit `UnicodeProfile` object selects a provider and dictionary independently of the tokenizer. See [profile selection](06-text-analyzers.md#simple-lowercase-profile-selection).

## Lossless Rust query terms

`WANDQuery::terms`, `CursorWANDQuery::terms`, and `ScoreOperator::query_terms` now contain `TokenTermKey` values, and `BlockMaxIndex::entries` yields those canonical keys. Existing `new` constructors still accept string term arrays; use `new_keys` for terms obtained from rich analysis. Explicit struct construction and direct term-array mutation should convert strings with `TokenTermKey::from_text` or `Into`, and preserve non-scalar terms with `TokenTermKey::from_term`. Analyzer string projections still reject unpaired UTF-16; query execution consumes lossless keys directly. See [search internals](../internals/05-search-and-ranking.md) for scoring and cursor semantics.

Native lexical execution is available through `uqa_scoring::score_text_query` and `score_text_terms`; `rebuild_text_block_max` uses the same scorer identity. `TextSearchAlgorithm` and `TextSearchProfile` now live in `uqa-scoring` and remain re-exported from `uqa-engine` at their existing paths.
