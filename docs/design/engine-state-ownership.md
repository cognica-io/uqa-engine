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

## Atomicity and locking

Transactional session values live behind one `SessionContext.state` lock. Snapshot and restore therefore cannot combine an old search path with a new prepared-plan cache, PRNG state, or sequence `currval` map.

Memory-engine transactions capture durable registries through `DurableCatalogState::snapshot` and restore them through one matching method. Those methods define the canonical registry lock order. The re-entrant statement gate excludes concurrent engine mutation while a multi-registry snapshot is assembled. Runtime extensions are outside SQL catalog rollback by design, except in-memory FDW row data, whose transaction snapshot is explicit.

Each epoch channel owns its published counter, local observation, dirty bit, and refresh mutex together. No standalone epoch or refresh lock lives on `Engine`. A derived session can share published counters only through `EpochCoordinator::share_published_from`, which resets every local observation before the first synchronization. A backend-provided committed change version additionally detects commits outside that in-process session family.

Public methods must not expose lock guards. Operations that perform storage I/O prepare candidate state first and publish it to an in-memory registry only after persistence succeeds. Any new cross-domain operation must enter through the statement boundary or document why it is safe without it.
