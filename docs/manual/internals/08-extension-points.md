# Extension Points

UQA Engine extends through runtime SQL functions, foreign data handlers, model specifications, fluent query construction, protocol codecs, and language bindings. Extensions attach at explicit boundaries instead of modifying the SQL parser for every backend.

## Runtime SQL functions

The engine registries accept scalar, table, and aggregate implementations:

| Trait | Execution role |
| --- | --- |
| `SQLScalarFunction` | Receives `&[Value]` and returns one `Value` or `SQLError` |
| `SQLTableFunction` | Returns a table result or pull-based `SQLTableFunctionStream` |
| `SQLRowSource<T>` | Produces rows and explicitly closes a scan with a fallible result |
| `SQLAggregateFunction` | Existing infallible Rust aggregate factory, adapted to `SQLAggregateFactory` |
| `SQLAggregateFactory` | Creates an independent `SQLAggregateState` or returns `SQLError` |
| `SQLAggregateState` | Observes argument rows, reads a value and explicitly closes host resources |

The `register_*_function_with_options` methods attach volatility and mutation properties. The default is `VOLATILE` and may mutate, which prevents unsafe optimizer assumptions.

Aggregate registration accepts `SQLAggregateFactory::try_create_state`. Existing `SQLAggregateFunction` implementations and infallible factory closures receive a blanket adapter, so they retain their source interface. Execution propagates creation errors before observing or finishing a missing state, including global aggregates over empty inputs, sorted and DISTINCT replay, and window frame restarts. Partially constructed accumulator sets release their successful states when a later creation fails. Window state starts when a frame is first evaluated; planning and empty window inputs do not invoke the factory. The owning transaction receives the original SQLSTATE, message, detail and hint.

Custom native `AggregateFunctionRegistry` adapters return `Arc<dyn SQLAggregateFactory>`; shared runtime maps store that same factory interface. This owner-level change does not add a second aggregate registration name or require existing infallible callback implementations to change their state factory.

`SQLAggregateState::finish` reads a value without consuming the state; a window can read the same state for multiple frames. `close` releases host resources after their final use and may return `SQLError`. Execution consumes ownership before calling `close`, so a failing cleanup is not retried. The default implementation succeeds and preserves existing Rust states that use infallible `Drop`. Explicit cleanup covers partial group construction, grouped and global aggregation, ordered and DISTINCT replay, window frame replacement, partition completion, input or expression failure and cancellation. `AggregateExecutor::close` carries cleanup through the physical operator when input ends early. Every initialized state is closed before statement completion; if both execution and cleanup fail, the original diagnostic remains primary and typed cleanup failures are attached to it. Dropping an abandoned state is a best-effort fallback and cannot report a cleanup error.

`SQLFunctionContext` carries host-owned invocation data from the calling session. Scalar and table implementations may override `call_with_context`; streaming table implementations override `call_stream_with_context`; fallible aggregate factories may override `try_create_state_with_context`. Defaults forward to the existing Rust methods. Install a context once with `Engine::install_sql_function_context` before accepting queries. Public sibling sessions start without host data. Internal readers share the installation cell, including readers created before installation, and portal workers use the calling session's cell. A bound aggregate factory keeps that context through state creation; a context should contain weak session references where the host needs session access. Custom `QueryRuntimeView` constructors supply its borrowed `function_context`; their owner can retain an empty context when no host data is needed.

```mermaid
flowchart LR
    A[Registration or retirement] --> B[Validate name and options]
    B --> C[Acquire registry membership and idle session gates]
    C -->|Active execution| D[Return Busy without changing registration]
    C --> E[Invalidate shared session plans]
    E --> F[Publish entry or retire its exact identity]
    F --> G[Advance runtime registry revision]
    G --> H[Release locks before callback destruction]
```

`register_scalar_function_retained`, `register_table_function_retained` and `register_aggregate_function_retained` accept a name, `SQLFunctionOptions` and an implementation, and return `SQLFunctionRegistration`. The handle owns the shared registry independently of the creating session. Existing registration methods retain their signatures and leave the entry registered without an explicit retirement handle. Dropping a handle releases its lease without unregistering the entry; `unregister` explicitly retires it. Retirement returns `SQLFunctionRegistrationError::Busy` while execution or escaped function resources retain the registration, preserving the handle and current entry. A handle for a replaced entry cannot remove its replacement. Successful retirement releases both the function and the handle's shared registry lease; retaining an already-retired handle cannot retain unrelated callbacks after Engine close. Repeated successful retirement is a no-op.

Registry mutation serializes membership and takes every live session's existing statement gate without waiting for active execution. This also rejects mutation from inside a callback on the same thread. A new sibling joins that membership before publication. Changes invalidate statement, prepared and routine plans across those sessions; prepared definitions remain registered and reanalyze when next used. Statement cache entries carry the nontransactional function-registry revision, so restoring a transaction or savepoint snapshot cannot restore a valid pre-change plan. Registration validates before publication and does not execute prepared definitions or eagerly rebind unrelated SQL. Registry errors do not alter transaction state.

Escaped table scans and aggregate states hold their original registration identity until resource release. Explicit row/state close retains its fallible completion path, and cleanup failure consumes that resource once. Retained aggregate factories also prevent premature retirement. Plan invalidation keeps replaced function objects alive until all registry and session locks are released; host destructors and callbacks therefore execute outside those locks. Engine owns these session and resource adapters; Execution owns callback dispatch, row production and aggregate execution.

## Function safety

An immutable function depends only on arguments. A stable function may observe statement context but remains stable during that statement. A volatile function may change per call. Any callback that may mutate engine-visible state must be volatile.

Callbacks are shared by derived sessions and are not stored in the durable catalog. Register them before accepting concurrent work; subsequent registry changes require every shared session to be idle and can return Busy. A Rust callback that refers back to its owning engine or a sibling session must capture a `Weak<Engine>` and upgrade it only for the invocation; capturing a strong `Arc<Engine>` forms a cycle through the shared callback registry and retains that session's database resources. Keep the session's strong owner in the host. Do not hold engine registry locks while invoking callback code.

Host-language callbacks need a reverse call boundary in addition to the forward method dispatch used by a generated binding. Node.js must move callback execution from an engine worker back to the owning JavaScript thread, while browser WASM must re-enter JavaScript through Emscripten and retain callback and aggregate-state identities for the full registry lifetime. Without those lifecycle-safe bridges, registration would violate runtime thread ownership or leave dangling callback state, so forward binding generation alone cannot expose UDF callbacks correctly.

Node.js owns a strong function reference for same-thread execution and a weak thread-safe function bridge for asynchronous SQL workers. A worker sends converted arguments to the owning JavaScript thread, waits for the synchronous result, and converts thrown errors into SQL errors. The package wrapper rejects `Engine` method re-entry while a callback is active.

Browser WASM assigns numeric callback and aggregate-state identifiers in JavaScript. Emscripten imports perform synchronous reverse dispatch across a JSON value bridge, and aggregate state is released after finish, query failure, or engine-group teardown. Derived sessions retain the shared callback group until its last engine closes, and the wrapper rejects `Engine` method re-entry during reverse dispatch.

Streaming table functions can yield a late error after rows. `SQLTableFunctionStream::new` adapts an ordinary iterator whose release is infallible. A scan with fallible cleanup implements `SQLRowSource<Vec<Value>>` and passes it to `SQLTableFunctionStream::from_source(columns, source)`. Its `next_row` returns a row, EOF or the original SQL error; `close` releases the source even when release reports an error. The owned `SQLRowStream` calls close once at EOF, a production error or explicit early termination. It consumes the source before invoking close, so a failing close is never retried. A cleanup error is an error item, never EOF, and subsequent pulls remain exhausted.

Physical table scans, set projections and lateral inputs retain this stream owner through cancellation, row-width checks and ordinality. Use `map_rows` for row conversion; erasing the stream into an ordinary iterator discards its explicit cleanup interface. Custom physical adapters use `PhysicalRowIteratorScan::from_stream`, and the `PhysicalProjectRows` and `LateralRows` aliases carry `SQLRowStream` rather than boxed iterators. Explicitly close an unfinished stream while its consumer can report failure. Dropping an abandoned Rust stream provides best-effort release but cannot report its cleanup error.

When execution and cleanup both fail, the primary SQLSTATE, message, hint, position and typed cause remain authoritative. `SQLError::with_cleanup` retains the secondary typed diagnostic, makes it available through `cleanup()`, and includes its fields after the original DETAIL. A cleanup failure alone retains its own diagnostic. The statement's ordinary failure and rollback boundary receives that result before reporting successful completion.

## SQL and PL/pgSQL routines

Durable SQL routines are compiled by `uqa-sql` and owned by the engine catalog. Their source, signature, volatility, language, defaults, return shape, and compiled plan identity survive reopen. They are distinct from runtime host-language callbacks.

Routine depth is bounded in `QueryRuntime`. Dynamic SQL re-enters normal parse, plan, transaction, and error boundaries.

## Foreign data wrappers

`uqa-fdw` defines foreign servers, foreign tables, projection, predicates, limit pushdown, and handler contracts. Concrete handlers are:

- In-memory relations
- DuckDB databases, expressions, and file sources
- Arrow IPC files and streams

The SQL compiler owns generic `CREATE SERVER` and `CREATE FOREIGN TABLE` syntax but does not depend on DuckDB or Arrow. `uqa-engine` resolves a server type to a handler at the extension boundary.

Engine and Execution depend on the lightweight `uqa-fdw` contracts and memory implementation. Native Arrow and DuckDB dependencies are selected independently by the default-disabled `arrow` and `duckdb` features; an omitted handler reports an explicit unavailable-scan error. CLI catalog display consumes retained SQL definitions and has no direct FDW dependency. See the [feature boundary and preservation proof](../../design/optional-foreign-data-wrappers.md).

A handler must validate options, report unsupported pushdown, preserve row and type errors, and avoid claiming a predicate was pushed when it remains required as an engine residual.

## QueryBuilder

`uqa-api::QueryBuilder` is a fluent client layer over engine SQL and retrieval primitives. It does not define a second planner. Builder output enters the same `Engine::sql`, columnar, Arrow, or Parquet execution boundaries.

New builder methods should compose existing engine syntax or typed operations and include exact SQL-generation tests. Raw expression hooks remain trust boundaries.

## Models

`uqa-ml` owns serializable model specifications, CPU inference, and analytical training. Its current `mlx` feature is an experimental direct-crate probe, not an engine-selected or packaged runtime; the replacement model, runtime, isolation, and distribution contracts are tracked in the [MLX runtime support plan](../../plans/0004-mlx-runtime-support.md). Model catalog records are durable, while backend process objects and runtime resources must remain process-local and reconstructible.

The current bare `DeepModel` JSON is a legacy unversioned format. A replacement or additional model kind must version input schema, layer specification, parameter encoding, normalization, precision, output semantics, and backend requirements, and loading must validate all of them before publication.

## PostgreSQL wire codec

`uqa-pg-wire` decodes frontend and encodes backend PostgreSQL v3 messages. Its shared format-code resolver expands extended-query and function-call zero, one, or one-per-value text/binary formats; its authentication exchange decodes the context-dependent password, MD5, GSS, SSPI, and SASL response shapes while enforcing message order; and its cancellation-key API preserves opaque downstream secrets through bounded middleware prefixes. It deliberately does not own sockets, TLS, credential verification or storage, scheduling, SQL planning, transaction policy, pooling, or process recovery. An embedding server maps protocol messages to independent engine sessions and closes malformed peers.

## Language bindings

Python, Node.js, and Emscripten WASM wrap the engine rather than reimplement SQL semantics. Binding conversion code owns language-native values, errors, async scheduling, and lifecycle.

| Binding | Special boundary |
| --- | --- |
| Python | pyo3 value conversion, interpreter lock release, and Python callbacks |
| Node.js | Node-API promises, synchronous variants, buffers, typed arrays, cancellation, and thread-safe JavaScript callback dispatch |
| WASM | Emscripten request and reverse-callback dispatch, browser async lifecycle, callback-state ownership, and IndexedDB synchronization |

When adding an engine method, decide explicitly whether each binding can support its storage, threading, native-library, callback, and security requirements. Absence from a binding should be deliberate and documented.

## Adding an extension

1. Put syntax-only behavior in `uqa-sql` and concrete execution behind an owning trait or engine boundary.
2. Define input and output carriers, error behavior, and type conversion.
3. Declare transaction class, volatility, mutation, and session sharing.
4. Define persistence and reopen behavior, or state that the extension is runtime-only.
5. Define cache dependencies and epoch invalidation.
6. Add unit tests for the implementation and engine integration tests for registration, execution, failure, rollback, and rebind.
7. Add binding and type-declaration coverage where the capability is exposed.
8. Update the workspace dependency policy if a runtime dependency edge changes.

## Source entry points

| Area | Path |
| --- | --- |
| Function traits | [`crates/uqa-engine/src/functions.rs`](../../../crates/uqa-engine/src/functions.rs) |
| Function registry | [`crates/uqa-engine/src/sql_registry.rs`](../../../crates/uqa-engine/src/sql_registry.rs) |
| FDW contracts | [`crates/uqa-fdw/src/lib.rs`](../../../crates/uqa-fdw/src/lib.rs) |
| QueryBuilder | [`crates/uqa-api/src/query_builder.rs`](../../../crates/uqa-api/src/query_builder.rs) |
| ML runtime | [`crates/uqa-ml/src/lib.rs`](../../../crates/uqa-ml/src/lib.rs) |
| Wire codec | [`crates/uqa-pg-wire/src/lib.rs`](../../../crates/uqa-pg-wire/src/lib.rs) |
