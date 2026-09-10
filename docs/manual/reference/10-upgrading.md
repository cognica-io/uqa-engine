# Upgrading to UQA Engine 0.2.3

Version 0.2.3 replaces resident persistent-graph replicas with direct storage access, shares immutable catalog and statistics snapshots across sessions, and maintains persistent column statistics in a background worker. It also prevents staged document-ID reuse during transaction snapshot refresh. The [release history](../../../HISTORY.md#023---2026-09-09) records the changes.

The 0.2 series includes SQL object and privilege lifecycle changes, durable expression and unique indexes, expanded sequences and PL/pgSQL, native cross-process notifications, and a Node.js HTTP client that runs without native addons. These changes were introduced in [0.2.0](../../../HISTORY.md#020---2026-09-05); the [compatibility guide](../sql/09-compatibility.md) defines the verified PostgreSQL 18 surface and the behavior still being implemented.

## Rust graph API and custom catalogs

Version 0.2.3 changes the public Rust graph API to support direct persistent storage reads. `Engine::graph_with` and `Engine::graph_with_mut` callbacks receive `uqa_graph::GraphStoreHandle`, not `MemoryGraphStore`. Import `GraphStore` for its methods, propagate storage errors, and use the owned `Result<Option<Vertex>>` and `Result<Option<Edge>>` point-read results without `.cloned()`. Mutation callbacks return `GraphStoreResult<T>` so an error rolls back the complete storage checkpoint. `PathIndex::lookup` likewise returns an owned, fallible result. Primary in-memory graph storage remains available through `MemoryGraphStore`.

Custom `GraphStore` implementations must support owned, fallible reads, edge memberships, and atomic mutation checkpoints; durable stores must implement bounded identity pages through indexed storage access. Custom `CatalogFacade` implementations must provide selective graph point reads, bounded identity pages, counts, memberships, and durable path-index data methods. Do not implement these by loading complete graph partitions into a resident map. Persistent sessions must bind graph handles to their own physical transaction. See [graph access](07-graphs.md) and [storage contracts](04-storage-and-security.md) for the current signatures and lifecycle requirements.

SQLite catalogs advance to version 46 for durable cache revisions, graph access metadata, path-index data, and invalidation. Initial open also performs the bounded, atomic legacy graph access/label metadata migration where needed. Validate an isolated copy before upgrading all processes; do not reopen a migrated database with an older binary.

## Package versions

Update the UQA packages used by one application together. Rust's `0.1` dependency requirement does not select `0.2.3`; change the requirement explicitly and regenerate the application's lockfile.

| Environment | Versioned installation |
| --- | --- |
| Embedded Rust | `cargo add uqa@0.2.3` |
| Rust HTTP client | `cargo add uqa-client@0.2.3` |
| Python and `usql` | `python -m pip install --upgrade uqa==0.2.3` |
| Embedded Node.js | `npm install @cognica-io/uqa@0.2.3` |
| Node.js HTTP only | `npm install --omit=optional @cognica-io/uqa@0.2.3` |
| Browser WASM | `npm install @cognica-io/uqa-wasm@0.2.3` |

The Rust workspace requires Rust 1.90 or newer. Python requires Python 3.8 or newer, and the Node.js package requires Node.js 16 or newer. The Node.js root package selects an exact-version native optional package for embedded execution; deploy the root and native packages from the same release. Deploy the Browser WASM JavaScript module and `uqa.wasm` from the same package together, including when updating a browser cache.

The [GitHub release](https://github.com/cognica-io/uqa-engine/releases/tag/v0.2.3) contains the Python and npm archives, standalone Node.js addons, and the status of publication to crates.io, PyPI, and npm. Rust applications using Git dependencies should select `tag = "v0.2.3"` consistently for every UQA dependency.

## Automatic statistics and session caches

Persistent query planning uses saved statistics while one database-level background worker collects and publishes replacements. Initial collection, committed-change thresholds, and a maximum dirty age of 60 seconds schedule maintenance automatically. Applications can inspect `Engine::automatic_statistics_status()` and continue to use explicit `ANALYZE` for a full collection. Pending changes survive reopen and follow transaction and savepoint rollback; existing estimates remain available until replacement.

Automatic collection samples at most 4,096 hierarchy rows. Both automatic and explicit collection omit text and binary values above 1,024 bytes from stored value samples, with an 8,192-byte encoded budget for other values; row and NULL counts still include those observations. Older oversized statistics are bounded during reopen and scheduled for one background replacement without another write. See [automatic column statistics](04-storage-and-security.md#automatic-column-statistics) for the complete estimate and payload contract.

SQLite tracks transactional cache revisions so data-only or statistics-only commits reuse unchanged definitions and physical handles. `Engine::new_session()` shares immutable committed catalog and statistics allocations while retaining independent transactions and storage handles. Custom providers may implement `CatalogFacade::cache_revisions()` with the same snapshot and rollback guarantees or return `None` for conservative refresh; an empty revision map must not stand in for unsupported tracking. See [cross-session cache refresh](04-storage-and-security.md#cross-session-cache-refresh).

The development branch releases unwritten rollback-journal readers before waiting for writer ownership or snapshot publication. This prevents automatic statistics publication from retaining a SQLite read lock while an application COMMIT waits for readers to finish. The logical transaction, savepoints, and detached repeatable-read snapshot remain intact.

## Cypher expression validation

Deeply nested Cypher and long operator or indexing chains now fail with a parse error instead of exhausting the process stack. The parser limits recursive expression parsing and constructed expression trees to 64 levels; flat lists and independent projection items remain supported. Rust code that exhaustively matches `uqa_graph::cypher::ParseError` must handle `ExpressionTooDeep { limit, position }`. See the [Cypher contract](../sql/07-graph.md#cypher-table-function).

## Prepared statement analysis in development

The development branch analyzes `PREPARE` before execution and infers omitted parameter types in PostgreSQL occurrence order. Statements with missing references or incompatible types can now fail when prepared. Replanning a statement whose result columns, types, or modifiers changed reports `0A000`; deallocate and prepare the updated query to adopt its new result contract. The session metadata view retains the original client SQL string. Register native SQL callbacks before preparing statements that refer to them; later callback registration invalidates cached plans and preserves the original result contract. Two-argument `round` calls require a numeric first argument, so cast floating-point expressions explicitly before supplying a precision. See [prepared statements](../sql/08-transactions-and-routines.md#prepared-statements).

The development optimizer returns `OptimizerResult<T>` with either `OptimizerError::Expression(SQLError)` or `OptimizerError::JoinGraph(JoinGraphError)`. Update exhaustive error matches and preserve the SQL error instead of converting every planning failure into an internal error. Immutable constant failures can now occur before execution; unused rule inputs are removed first, and EXECUTE binds arguments before planning the prepared body. Stored routines and views are optimized only when execution needs their definitions.

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

The development branch adds `bound_columns: Option<Vec<String>>` to `FromClause::Table` and `SourcePlan::Table`. Initialize it to `None` when constructing an ordinary query AST or plan. Engine-owned SQL-standard routine definitions capture their source columns and maintain them across column deletion and renaming, so later additions cannot shift stored positional aliases. Initial open migrates legacy source metadata and explicit string-to-regclass constants in stored schema expressions transactionally; subsequent catalog reloads validate the persisted definitions. See [stored column lifecycle](../sql/02-ddl.md#alter-table) for the SQL behavior.

The release includes all 354 PostgreSQL 18.4 core and isolation tests through the official PostgreSQL drivers, with a pinned source inventory and recorded execution provenance. The PostgreSQL reference run passes this corpus in CI. The UQA run of the complete corpus remains unaudited; release compatibility claims continue to follow the checked differential fixtures and feature manifest. See [verification](../internals/09-verification.md) for commands and evidence boundaries.

## Python CLI update

The release includes the 0.2.1 correction to `usql` interactive startup and `usql script.sql` execution. Upgrade Python installations of 0.2.0 to restore these modes. The entry point uses Python's command-line arguments, so the console launcher and interpreter options are not parsed as SQL input. Python engine APIs and `usql -c` keep their existing behavior.

The following storage guidance applies when upgrading from the 0.1 series.

## Custom Rust storage implementations

The storage traits changed in the 0.2 minor release. Applications implementing `uqa_storage::DocumentStore` must implement `put_stored` and `get_stored` using `StoredDocument`. A record keeps its public field map separate from `DocumentMetadata`, including tuple `xmin`. Preserve metadata through scans, rewrites, snapshots, and persistence; storing it as a user field can collide with application data. The default `put` implementation replaces public fields while preserving existing metadata, while a new tuple version uses `put_stored` with explicit metadata.

The B-tree methods on `uqa_storage::PersistentStorageBackend` now use `ValueIndexKey::Column` and `ValueIndexKey::Index`. Preserve both namespaces even when the enclosed names are equal. Named expression indexes store composite `Value::Row` keys; SQL expression binding and evaluation belong to the engine. Update custom backend signatures and physical key encoding before compiling against 0.2.3. See [Storage internals](../internals/03-storage.md) and the trait definitions in [`document_store.rs`](../../../crates/uqa-storage/src/document_store.rs) and [`backend.rs`](../../../crates/uqa-storage/src/backend.rs).

## Persistent database migration

Opening an older supported database performs the required provider and catalog migrations. The 0.2 minor release adds typed tuple metadata, richer object and column identities, ownership and ACL records, bound routine and rule dependencies, and expression-index metadata. Initial open owns migration writes; later catalog refresh validates the persisted representation. The shipped SQLite and key-value providers handle their storage migrations through the normal engine open path.

1. Stop writers, close every engine using the database, and create a recoverable backup through the [storage backup procedure](04-storage-and-security.md#backups-and-copies).
2. Open a copy with the exact 0.2.3 application and its selected provider, encryption key, and compression configuration.
3. Execute representative reads, writes, role and privilege checks, stored routines and views, and retrieval queries. Verify indexes, transaction rollback, and close-and-reopen behavior with the application's data.
4. Update every process sharing the database before reopening the original file. Register process-local runtime callbacks again when the application starts.
5. If the application must return to an older binary, restore the pre-upgrade backup. Do not rely on an older binary reading a file migrated by 0.2.3.

Keep migration failures visible and resolve them before admitting writes. Retain encryption keys and any external rollback anchor according to the [storage and security contract](04-storage-and-security.md).

## Node.js HTTP clients

`HttpEngine`, SQL parameter helpers, and streaming execute in JavaScript without a native addon. Existing imports from `@cognica-io/uqa` continue to expose these APIs; `@cognica-io/uqa/http` is the explicit HTTP entry point for both CommonJS and ESM. An HTTP-only deployment can omit optional dependencies. Embedded `Engine` use still requires the platform's native package.

Use an explicit URL and token or `HttpEngine.fromEnv()` when deployment configuration already supplies credentials. The asynchronous `local()` and `cloud()` constructors require the installed `uqa` CLI and resolve a project once. Keep using the documented parameter wrappers for vectors and tensors, JavaScript `bigint` for exact signed 64-bit integers, and `Buffer` or `Uint8Array` for binary values. The [HTTP Engine reference](09-http-engine.md) describes errors, response limits, streaming, and cancellation.

## SQLite provider ownership in development

The development branch moves all concrete SQLite persistence into `uqa-storage-sqlite`. This is a Rust source migration after 0.2.3; it does not change the SQLite database format, schema version, SQL behavior, encryption options, or compressed-container layout. High-level `Engine` constructors keep their signatures.

| Previous Rust API | Development Rust API |
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

The operator-tree `QueryOptimizer` accepts immutable `IndexScanCandidate` values through `with_index_candidates`. Its `index_manager` field and `with_index_manager` constructor are removed; callers discover applicable indexes and their scan costs from their catalog snapshot before invoking the optimizer. The planner no longer imports `uqa-storage` directly. Its existing retrieval IR still has transitive storage dependencies through `uqa-operators`; removing that coupling requires separating logical descriptors from bound execution objects.
