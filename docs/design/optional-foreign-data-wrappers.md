# Optional foreign data wrappers

Issue [#776](https://github.com/cognica-io/uqa-engine/issues/776) removes the mandatory native handler dependencies. The maintainer's scope includes the entire `uqa-fdw` crate, with all handlers disabled by default. This changes build availability, not foreign-table catalog semantics or persistent formats.

## Feature selection and ownership

| Build | Included handlers | Unavailable scans |
| --- | --- | --- |
| Default Engine, facade, CLI and language bindings | None; no dependency on `uqa-fdw` | All foreign sources |
| `fdw` | Memory and, on native targets, Arrow IPC | DuckDB |
| `duckdb` | Enables `fdw` and native DuckDB | Native handlers remain unavailable on Emscripten |

`uqa-execution/fdw` selects the optional handler contracts and source adaptation. `uqa-engine/fdw` enables that feature and its own optional FDW adapter dependency. The facade, CLI and language bindings forward their explicit features without changing their defaults. Browser WASM exposes `fdw` for memory sources; it cannot execute native Arrow or DuckDB handlers. Selecting the `uqa-fdw` package itself builds its memory/Arrow implementation, while `uqa-fdw/duckdb` independently selects DuckDB.

SQL owns wrapper/server declarations and identities. Execution owns durable foreign definitions, restoration, security, source selection and predicate preparation. Engine retains statement snapshots, source rows and transaction adapters. The CLI describes native SQL definitions instead of depending on the handler's lossy column projection. No dependency direction is reversed and no handler algorithm moves into Engine or Core.

The Rust handler projection APIs `foreign_server`, `foreign_table` and `load_memory_foreign_table` require `fdw`. The always-available `foreign_server_definition` and `foreign_table_definition` methods expose the retained native SQL definitions. Listing, column lookup, DDL, permissions, transactional changes and persistence remain available without handlers.

An unavailable scan returns SQLSTATE `0A000`. Without `fdw`, its message is `FDW support is not available in this build`; with `fdw` but without `duckdb`, the existing DuckDB-specific unavailable-build error remains. Validation of a bound server and wrapper still precedes source dispatch. Missing handlers never produce a fabricated empty relation or discard a stored definition.

## Preservation argument

Let $C$ be a retained SQL catalog, $S$ the session and transaction state, and $D$ the ordinary table contents. Let $H_f$ be the set of source handlers compiled by feature configuration $f$. A foreign scan is a partial operation $\operatorname{scan}_h(C,S,q)$ producing the same ordered stream of typed rows and errors as handler $h$; its domain requires $h\in H_f$. The build change restricts this domain and leaves the operation itself unchanged wherever it is defined.

For catalog operations, the stored server/table definitions, identities, serialization, security checks, snapshot selection and transaction publication remain the existing SQL/Execution operations. Thus a catalog transition $T$ has $T_f(C,S,D)=T(C,S,D)$ for every configuration. The new metadata accessors clone those selected definitions; they neither reconstruct handler types nor change the retained catalog generation. Reopening and rollback therefore preserve the same definitions independently of $H_f$.

For an enabled scan, the moved predicate-preparation functions are unchanged and remain in Execution. Engine selects the same handler with the same captured server, projected columns, predicates and limit. Memory row retention changes its type spelling from `uqa_fdw::Row` to `uqa_sql::ResultRow`; both aliases are exactly `BTreeMap<String, uqa_core::Value>`, so this substitution is the identity on keys, values, iteration order and ownership. The resulting typed row stream, errors and retained state are unchanged.

Every relational, retrieval and ranked operator above the scan receives that same input and retains its original implementation. Induction over the operator tree gives equal observations for every existing composition whose scans use enabled handlers. This preserves multiplicity, ordering where specified, NULLs, typed values, scores and errors; it does not infer full-value equality from document support alone. Programs without foreign scans have identical operations in every configuration. When a required handler is absent, explicit failure preserves the partial-operation contract instead of introducing an empty carrier or an unsound algebraic identity. No new algebraic operator or approximation is introduced.

## Verification contract

- Check resolved native, development and Emscripten dependency graphs: default Engine and downstream builds exclude `uqa-fdw`, DuckDB and its native library; explicit `fdw` and `duckdb` select exactly their intended dependencies.
- Run foreign catalog creation, rollback, inspection, deletion and persistent reopening without handlers on memory, SQLite, SQLite Key/Value and redb. Exercise direct and joined scans to verify explicit unavailability.
- Run existing memory/Arrow behavior, PostgreSQL catalog/ACL/lifetime fixtures and runtime projection tests with `fdw`, and real DuckDB owner and Engine tests with `duckdb`, on Linux and macOS.
- Keep the analytical comparison's DuckDB dependency explicit through the benchmark's required features. Historical reference revisions without a feature declaration retain their original build arguments; build provenance records the actual selected features.
- Inspect actual Linux and macOS automatic performance results for the delivered source, including work, memory and output correctness. Do not infer a build-time or binary-size improvement ratio from dependency exclusion alone.

The issue and its implementation PR retain the current task status and source-scoped verification evidence.
