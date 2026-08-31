# Engine state ownership

`Engine` is a coordinator, not the owner of every mutable subsystem. Its state is divided into six explicit domains:

| Domain | Owns | Sharing rule |
| --- | --- | --- |
| `StorageContext` | session-bound table handles, catalog/backend facades, persistent provider | rebuilt for every logical session; provider shared as an immutable factory |
| `DurableCatalogState` | graph, model, scoring, view, schema, sequence, analyzer, FDW, index, and SQL-routine caches | private cache per session; synchronized by epochs |
| `SessionContext` | search path, variables, PRNG, `currval`, prepared plans, statement cache, transaction stack | never shared between sessions |
| `RuntimeExtensions` | Runtime callbacks and in-memory FDW rows | deliberately shared by derived sessions |
| `EpochCoordinator` | published/seen generations, dirty bits, and refresh serialization | published counters shared; observations remain local |
| `QueryRuntime` | statement boundary, cancellation, notices, routine-depth policy | private per session |

This makes session derivation explicit: `new_session` asks `PersistentStorageProvider` for catalog and backend handles bound to one new transaction context, rebuilds durable caches, shares only published epoch counters and runtime extensions, and creates fresh session/query-runtime state. SQLite sessions bind a `ManagedConnection`; redb sessions share the database but own separate read/write transaction state.

## Capability boundaries

Statement and catalog workflows receive only the state domains they need through narrow internal capability types and immutable statement data:

| Capability | Owned or borrowed inputs | Current responsibility |
| --- | --- | --- |
| `CatalogReadView` | Owned `Arc` snapshot of table definitions and durable registries | One immutable statement catalog for binding, projection, virtual scans, and relation metadata without mutation, locking, or cache publication |
| `RelationNameResolution` | Owned search path and temporary-schema name | Deterministic unqualified relation resolution against the same statement catalog snapshot |
| `SessionExecutionView` | `SessionContext` plus immutable session and transaction-snapshot identity | Search path, users, variables, transaction depth, and temporary-schema identity |
| `QueryRuntimeView` | `QueryRuntime`, `RuntimeExtensions`, and the session-state lock for runtime parameters | Cancellation, callback lookup, diagnostics, and execution memory policy |
| `MutationCoordinator` | Storage, durable catalog, session, epoch, and query-runtime owners | Schema registration, command mutation-overlay lifetime, and atomic catalog-registry cache publication |

These capabilities do not contain an `Engine` reference, implement an engine-recovering dereference, or expose unrelated state owners. `Engine` remains the composition facade that constructs them, while SQL leaf modules receive the narrow values directly. `UnifiedPlanExecutor` retains the single exhaustive plan match and captures the session, runtime, and mutation capabilities once at construction; `CteScope` captures one `CatalogReadView` and `RelationNameResolution` pair for binding and query execution.

The checked-in ownership policy enumerates the capability and support data types, rejects undeclared data types and service traits in the capability module, and rejects any capability data declaration, type alias, or function signature that retains, accepts, or returns `Engine`. Declared orchestration adapters remain explicit, while migrated catalog, query, mutation-carrier, and codec leaves fail the policy check on any `Engine` reference.

Static catalog metadata and row synthesis are engine-free leaves, while live catalog projection adapters consume `CatalogReadView`, `RelationNameResolution`, and session values. Schema binding has a deterministic catalog fixture without a full engine, physical construction receives explicit runtime capabilities, and reusable scalar traversal belongs to `uqa-execution`. `CREATE SCHEMA` delegates from the public facade or unified command arm to `MutationCoordinator`. INSERT, UPDATE, DELETE, and MERGE enter one mutation-command boundary, and their automatic-view, trigger-backed-view, rule, conflict, referential-action, and partition-routing branches use the same typed candidates, lock outcomes, row images, prepared actions, event queue, publication batch, and command overlay rather than retaining command-specific lifecycle implementations.

## Atomicity and locking

Transactional session values live behind one `SessionContext.state` lock. Snapshot and restore therefore cannot combine an old search path with a new prepared-plan cache, PRNG state, or sequence `currval` map.

Memory-engine transactions capture durable registries through `DurableCatalogState::snapshot` and restore them through one matching method. Those methods define the canonical registry lock order. The re-entrant statement gate excludes concurrent engine mutation while a multi-registry snapshot is assembled. Runtime extensions are outside SQL catalog rollback by design, except in-memory FDW row data, whose transaction snapshot is explicit.

Each epoch channel owns its published counter, local observation, dirty bit, and refresh mutex together. No standalone epoch or refresh lock lives on `Engine`. A derived session can share published counters only through `EpochCoordinator::share_published_from`, which resets every local observation before the first synchronization. A backend-provided committed change version additionally detects commits outside that in-process session family.

Public methods must not expose lock guards. Operations that perform storage I/O prepare candidate state first and publish it to an in-memory registry only after persistence succeeds. Any new cross-domain operation must enter through the statement boundary or document why it is safe without it.
