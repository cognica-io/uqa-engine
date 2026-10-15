# Optional foreign data wrappers

Issue [#776](https://github.com/cognica-io/uqa-engine/issues/776) removes mandatory native handler dependencies. Memory sources and shared foreign-table contracts remain available in every build. Independent `arrow` and `duckdb` features select the native handlers, both disabled by default. This changes build availability, not foreign-table catalog semantics or persistent formats.

## Feature selection and ownership

| Build | Included handlers | Unavailable scans |
| --- | --- | --- |
| Default Engine, facade, CLI and language bindings | Memory | Arrow IPC and DuckDB |
| `arrow` | Memory and native Arrow IPC | DuckDB |
| `duckdb` | Memory and native DuckDB | Arrow IPC |
| `arrow,duckdb` | Memory and both native handlers | Native handlers remain unavailable on Emscripten |

`uqa-fdw` owns the lightweight contracts and memory implementation plus optional native handlers. Its `arrow` feature selects optional Arrow dependencies; `duckdb` independently selects the bundled DuckDB dependency. Engine enables the corresponding owner feature and source adapter; Execution always provides catalog projection and predicate preparation. Facade, CLI and native language bindings forward these features without changing their defaults. Browser WASM always has memory sources and cannot execute either native handler.

DuckDB's own Rust interface uses Arrow arrays internally; that transitive dependency does not activate UQA's Arrow IPC handler. The fluent `uqa-api` crate separately retains its existing Arrow/Parquet result-conversion dependencies. Dependency checks distinguish compiled libraries from enabled FDW handlers.

SQL owns wrapper/server declarations and identities. Execution owns durable foreign definitions, restoration, security, source selection and predicate preparation. Engine retains statement snapshots, source rows and transaction adapters. The CLI describes native SQL definitions instead of depending on the handler's lossy column projection. No dependency direction is reversed and no handler algorithm moves into Engine or Core.

The Rust handler projection APIs `foreign_server`, `foreign_table` and `load_memory_foreign_table` are always available, as are the native SQL metadata accessors `foreign_server_definition` and `foreign_table_definition`. Listing, column lookup, DDL, permissions, transactional changes and persistence remain available for omitted native handlers.

The SQL scan capability retains one feature-independent interface. Engine always supplies its memory-capable adapter, which calls Execution's predicate preparation before invoking the retained source. Cargo unification can enable `uqa-fdw/arrow` through another dependency without selecting `uqa-engine/arrow`; it does not change the shared context's shape or implicitly enable Engine's Arrow source dispatch.

An unavailable native scan returns SQLSTATE `0A000` with `FDW type` and the omitted handler name. Validation of a bound server and wrapper still precedes source dispatch. Missing handlers never produce a fabricated empty relation or discard a stored definition.

## Preservation argument

Let $C$ be a retained SQL catalog, $S$ the session and transaction state, and $D$ the ordinary table contents. Let $H_f$ be the set of source handlers compiled by feature configuration $f$. A foreign scan is a partial operation $\operatorname{scan}_h(C,S,q)$ producing the same ordered stream of typed rows and errors as handler $h$; its domain requires $h\in H_f$. Every configuration includes the memory handler; native feature selection restricts this domain and leaves the operation itself unchanged wherever it is defined.

For catalog operations, the stored server/table definitions, identities, serialization, security checks, snapshot selection and transaction publication remain the existing SQL/Execution operations. Thus a catalog transition $T$ has $T_f(C,S,D)=T(C,S,D)$ for every configuration. The metadata accessors clone those selected definitions; they neither reconstruct handler types nor change the retained catalog generation. Reopening and rollback therefore preserve the same definitions independently of $H_f$.

For an enabled scan, the predicate-preparation functions are unchanged and remain in Execution. Engine selects the same handler with the same captured server, projected columns, predicates and limit. Memory row retention uses `uqa_sql::ResultRow`, while the handler interface uses `uqa_fdw::Row`; both aliases are exactly `BTreeMap<String, uqa_core::Value>`, so the boundary is the identity on keys, values, iteration order and ownership. The resulting typed row stream, errors and retained state are unchanged.

Every relational, retrieval and ranked operator above the scan receives that same input and retains its original implementation. Induction over the operator tree gives equal observations for every existing composition whose scans use enabled handlers. This preserves multiplicity, ordering where specified, NULLs, typed values, scores and errors; it does not infer full-value equality from document support alone. Programs without foreign scans have identical operations in every configuration. When a required handler is absent, explicit failure preserves the partial-operation contract instead of introducing an empty carrier or an unsound algebraic identity. No new algebraic operator or approximation is introduced.

## Verification contract

- Check resolved native, development and Emscripten dependency graphs: default Engine and downstream builds retain `uqa-fdw` but omit native handler libraries, except the fluent API's pre-existing Arrow/Parquet output dependencies. Verify `arrow`, `duckdb` and their combination activate exactly the intended handler features.
- Check Engine compilation when only its FDW dependency enables `arrow`; feature unification must not alter Engine's dispatch availability.
- Run foreign catalog creation, rollback, inspection, deletion and persistent reopening on memory, SQLite, SQLite Key/Value and redb. Exercise default memory scans plus direct and joined scans of omitted native handlers to verify explicit unavailability.
- Run existing memory behavior, PostgreSQL catalog/ACL/lifetime fixtures and runtime projection tests without optional features; run real Arrow and DuckDB owner/Engine tests with each feature independently on Linux and macOS. Check their combined configuration too.
- Keep the analytical comparison's DuckDB dependency explicit through the benchmark's required features. Historical reference revisions without a feature declaration retain their original build arguments; build provenance records the actual selected features.
- Inspect actual Linux and macOS automatic performance results for the delivered source, including work, memory and output correctness. Do not infer a build-time or binary-size improvement ratio from dependency exclusion alone.

The issue and its implementation PR retain the current task status and source-scoped verification evidence.
