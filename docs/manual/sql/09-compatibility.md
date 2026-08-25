# PostgreSQL Compatibility and Limits

UQA Engine deliberately uses PostgreSQL-oriented syntax and behavior while remaining an embedded engine with its own storage, planner, catalog, and extension model. PostgreSQL 18 is the behavioral oracle: every externally observable difference is a compatibility bug, including differences in features not yet implemented.

## Compatibility baseline

- SQL parsing uses PostgreSQL grammar through `libpg_query`.
- Session metadata reports `server_version` as `18.0-uqa`.
- The repository checks all 22 deterministic TPC-H-derived scale-factor `0.001` query results against PostgreSQL 18.4 fixtures.
- The optional wire crate implements PostgreSQL protocol 3.0 through 3.2 codec primitives, minor-version negotiation, reserved startup-option reporting, protocol-specific and middleware-layered cancellation-key validation, context-aware cleartext/MD5/GSS/SSPI/SASL authentication sequencing, shared extended-query and function-call text/binary format resolution, notifications, and COPY format validation. PostgreSQL 18.4 `psql`/libpq tests verify 3.0, 3.2, and `latest` startup, 3.2-to-3.0 downgrade, authentication ordering, SSL rejection and retry, legacy and 256-byte cancellation keys, and extended Parse/Bind/Describe/Execute/Sync flow; the pinned psycopg 3.3.4, pgx 5.10.0, and node-postgres 8.23.0 Docker matrix additionally exercises prepared reuse, binary result selection, binary parameters where the driver supports them, COPY in and out, failed-transaction recovery, and one-connection pool reuse against PostgreSQL 18.4 and the codec fixture server.
- PostgreSQL-shaped `information_schema` and `pg_catalog` virtual relations carry declared PostgreSQL 18 row types even when empty; implemented `pg_type` rows expose exact scalar, array, information-schema domain, pseudo-type, and `information_schema_catalog_name` metadata.
- Apache AGE-shaped `cypher(...) AS (...)` integrates graph results into SQL, and the AGE catalog surface (`LOAD 'age'`, `ag_catalog.ag_graph`, `ag_catalog.ag_label`, the `agtype` / `graphid` types, and the graph and label management functions) lets AGE drivers bootstrap against the embedded engine.
- Recursive CTE `SEARCH` and `CYCLE` preserve PostgreSQL 18 depth/breadth sequence types, path-sensitive cycle rows, generated-column visibility, recursive wildcard behavior, `UNION` distinctness, iteration-wide limiting, validation order, and stored-plan reopen behavior; `MATERIALIZED` and `NOT MATERIALIZED` select PostgreSQL's eligible folding policy while recursive and volatile definitions remain materialized.

The fixture coverage is evidence for those queries and types, not a claim of complete PostgreSQL 18 compatibility.

<!-- pg18-milestone-snapshot:start -->

Current milestone snapshot: complete — `M1` (Discovered semantic fixes), `M2` (Protocol 3.2); in progress — `M0` (PG18 baseline), `M3` (PG18 DDL and types), `M4` (Core regression parity), `M5` (Client parity); not started — `M6` (Complete compatibility). Each milestone status is derived from its owned evidence items and remains bounded by its exit gate.

<!-- pg18-milestone-snapshot:end -->

## Embedded runtime architecture

UQA Engine does not yet implement PostgreSQL processes, network protocol semantics as its core API, MVCC storage pages, roles, grants, extensions, background workers, replication, WAL administration, or server configuration. The optional PostgreSQL wire and FDW crates are adapters around the engine. Every externally visible difference caused by this architecture remains an open compatibility bug rather than an accepted alternative behavior.

Some declared SQL types share an internal runtime carrier, but scans and relational plans retain the declared `ColumnType`, integer writes and casts enforce `int2`/`int4`/`int8` ranges, source-sensitive OID/XID/bytea casts retain source width, and result schemas do not infer identity from values. Any remaining PostgreSQL 18 overflow, cast, storage, collation, binary-format, or display difference is an open compatibility bug.

Relational operators keep static row schemas until the final consumer. Spill format version 1 records the declared schema and logical `(alias, column)` identity directly; spilling is not a reason to materialize rows or flatten qualification early, and there is no legacy spill compatibility reader.

## Open PostgreSQL 18 relation-feature bugs

Temporary tables, views, sequences, CTAS, `SELECT INTO`, temporary-relation name resolution through `pg_temp`, automatic temporary views over temporary relations, all three temporary-table `ON COMMIT` actions, `DISCARD TEMP`, unlogged tables and sequences across clean reopen, ordinary materialized views and refresh, and validated view/materialized-view reloptions are implemented. Their catalog identity is exposed through `pg_namespace`, `pg_class`, `pg_attribute`, and the applicable `pg_views`, `pg_matviews`, or `pg_sequences` view.

- Unlogged-relation reset after crash recovery
- Table inheritance and partitioning
- Typed tables
- Table storage parameters and tablespaces
- Table `USING` access methods
- Cross-database relation or routine names
- Temporary and unlogged materialized views, concurrent refresh, materialized-view indexes, access methods, and tablespaces
- Complete privilege and optimizer effects for `security_invoker` and `security_barrier`, and updatable-view enforcement for `WITH CHECK OPTION`

Unsupported forms fail before catalog mutation. That failure is fail-safe behavior while implementation is incomplete, not a compatibility exemption.

## Open PostgreSQL 18 query-clause bugs

- Collations, domains, user-defined equality operators, and the complete common-type matrix for `JOIN ... USING` columns with different declared types

Each missing clause above must be implemented with PostgreSQL 18 semantics; source-query rewriting is not an accepted compatibility solution.

## Open PostgreSQL 18 DDL bugs

For implemented tables, views, sequences, and foreign tables, text-to-`regclass` casts resolve the visible relation to the OID exposed by `pg_class`; a missing cast target reports `42P01`, while `to_regclass(text)` returns `NULL`.

- Virtual and stored generated columns implement durable definitions, selective virtual evaluation, exactly-once stored evaluation, DML assignment rules, DDL-time static typing for the implemented expression surface, exact stored SQL routine overload binding and dependencies, supported constraints and indexes, catalogs, ALTER operations, failure atomicity, and reopen behavior. The complete PostgreSQL built-in function and operator overload matrix, privileges, inheritance and partition propagation, `pg_dump`/`pg_restore`, and complete upstream regression coverage remain open compatibility bugs.
- Named CHECK, foreign-key, and `NOT NULL` constraints implement `NOT VALID`, failure-atomic validation, supported `ALTER CONSTRAINT` state changes, actual `INITIALLY DEFERRED` foreign-key checks at outer commit, savepoint rollback, catalog and information-schema flags, dependency-aware drop, multi-action ALTER atomicity, and durable reopen. Per-transaction `SET CONSTRAINTS`, inheritance and partition propagation, the complete dependency graph, `pg_dump`/`pg_restore`, and the upstream constraint regression matrix remain open compatibility bugs.
- `WITHOUT OVERLAPS` keys and `PERIOD` foreign keys are not implemented because range and multirange column types are not yet available.
- Expression indexes are not implemented.
- SQL index access methods are B-tree, GIN, IVF, and HNSW.
- Dependency-sensitive `DROP ... CASCADE` remains incomplete for implemented catalog objects except graph namespaces and the bounded routine graph. `DROP FUNCTION signature CASCADE` removes the exact function, its generated columns, and direct or transitive stored views while retaining unrelated objects; `DROP PROCEDURE signature CASCADE` removes a procedure with no modeled dependents. Every additional routine-dependent object kind, dependent-procedure graph, multi-target CASCADE graph, deletion order, notice, and diagnostic is tracked by `routines.function-procedure-drop-cascade-extended`.
- `ALTER TABLE DROP COLUMN CASCADE` removes inbound foreign-key constraints and the target column's owned constraints; cascade over every other PostgreSQL dependency kind remains incomplete.
- `ALTER COLUMN TYPE USING` is preserved structurally and evaluated once per old row inside the atomic ALTER transaction; the complete PostgreSQL assignment-cast matrix, dependency and collation rewrites, domain checks, and upstream ALTER regression cases remain open.
- `CREATE SCHEMA AUTHORIZATION` and embedded schema elements are not implemented.
- Sequence minimum, maximum, cycle, cache, ownership, and identity-owned sequence options are not implemented.
- `VACUUM` is not implemented.
- `ANALYZE` accepts all tables or one table, without options or a column list.
- `EXPLAIN` options are limited to `ANALYZE`, `VERBOSE`, and `FORMAT TEXT` or `FORMAT JSON`.

## Open PostgreSQL 18 type bugs

Supported declarations retain distinct `SMALLINT`, `INTEGER`, `BIGINT`, `OID`, `XID`, `REAL`, `DOUBLE PRECISION`, `TEXT`, `NAME`, `UUID`, `VARCHAR(n)`, `CHAR(n)`, `INTERVAL`, array, and information-schema domain identities. Character lengths, numeric precision/scale, integer ranges, source-sensitive OID/XID/bytea casts, `ALTER TYPE USING`, foreign-table schemas, and supported migration types are enforced by the implemented paths; unknown migration types fail instead of becoming text.

- The complete implicit, assignment, and explicit cast-context matrix is not implemented for every source and target pair.
- User-defined domains, enums, composite declarations, ranges, and multiranges are not implemented completely; the absent enum, range, and multirange value carriers also keep actual `anyenum`, `anyrange`, `anymultirange`, `anycompatiblerange`, and `anycompatiblemultirange` routine substitutions open.
- Large declared `NUMERIC` precision is bounded by the engine decimal carrier in actual values.
- Collation and locale behavior is not a complete PostgreSQL collation implementation.
- Type I/O routine OIDs are present in implemented `pg_type` rows, but the complete corresponding `pg_proc`, typmod, binary I/O, and extension-type catalog surface remains open.
- `VECTOR(n)` and `TENSOR(n)` are UQA Engine retrieval types rather than PostgreSQL core types.

## Open PostgreSQL 18 catalog and administration bugs

The virtual catalogs expose engine metadata needed by supported clients and tests. Implemented PostgreSQL 18 identities include catalog row schemas, information-schema domain OIDs, core/system type layout and I/O routine OIDs, `regnamespace`, `record`, `_record`, `void`, and the `information_schema_catalog_name` view/composite/array with its `pg_class` and `pg_attribute` rows. User SQL and PL/pgSQL routines expose input-only `proargtypes` identity, `proallargtypes`, modes, names, defaults, variadic element OIDs, return identity, set-returning state, and implemented polymorphic pseudo-type OIDs in `pg_proc`. Legacy `int2vector` and `oidvector` values use PostgreSQL's space-separated text representation in casts, tabular and expanded CLI output, and COPY text output. The Apache AGE extension catalog is implemented: `ag_catalog.ag_graph`, `ag_catalog.ag_label`, the `agtype`, `graphid`, `label_id`, and `label_kind` types, one namespace per graph, and the label relations and sequences of every graph appear in `pg_namespace`, `pg_class`, `pg_attribute`, `pg_sequences`, `pg_type`, and the information schema. The complete OID graph, all remaining built-in and extension `pg_proc` rows, ownership, ACLs, server processes, WAL, statistics views, and other extension catalogs remain open.

Known mutable settings are `search_path`, `client_encoding`, `datestyle`, `timezone`, and `work_mem`. Unknown or unsupported settings return an error rather than becoming ignored server configuration.

`DISCARD TEMP` removes the current session's temporary tables, views, sequences, and sequence state, and is rejected inside a transaction as PostgreSQL requires.

`LOAD` accepts the Apache AGE library names (`age`, `age.so`, `$libdir/age`, `$libdir/age.so`) as no-ops because the AGE surface is embedded; every other library fails as a missing `$libdir` file because the engine loads no shared objects.

## Open PostgreSQL 18 routine bugs

SQL and PL/pgSQL routines cover a broad tested subset, including overloads, defaults, set returns, procedures, control flow, dynamic SQL, recursion limits, diagnostics, exception handling, qualified named types, table-backed `%TYPE`, strict assignment/return casts, and bound cursors used entirely within one routine activation. Dynamic cursor queries, `MOVE`, non-`NEXT` fetch directions, `refcursor` parameters and returns, and cursors that survive routine exit are not implemented because session portal state is not available. The routine surface does not claim the full PL/pgSQL language, PostgreSQL extension languages, security-definer ecosystem, or server privilege model.

The bounded original PostgreSQL 18 additions inventory is verified. This closes only `functions.identified-pg18-additions`; the complete PostgreSQL 18 function, operator, type, cast, extension, and catalog matrix remains an open compatibility bug under `functions.full-pg18-matrix`. The implemented-carrier polymorphic, variadic, and pseudo-type routine slice is independently verified under `routines.polymorphic-variadic-pseudotype-overloads`; missing enum carriers, extension languages and security, and extended dependency graphs remain separate items rather than broadening that claim.

The implemented fixed-signature built-ins documented in the function manual share candidate selection with visible SQL routines for qualification, search-path shadowing, exact and implicit matches, preferred and unknown categories, domains, named and default slots, and stable stored bindings where generated expressions are allowed. Catalog evidence includes the PostgreSQL 18 `pg_proc` identities for unit `random()` (1598), `to_hex(integer|bigint)` (2089 and 2090), `gen_random_uuid()` (3432), `casefold(text)` (6412), `uuidv4()` (6428), and `uuidv7()` / `uuidv7(interval)` (6429 and 6430). The complete PostgreSQL built-in, operator, cast, and `pg_proc` matrix remains open.

Ordinary scalar SQL and PL/pgSQL, table-returning, and `SETOF` functions, plus procedures and `CALL`, use PostgreSQL-shaped candidate selection for the implemented routine type surface. Function calls preserve declared types from direct casts and scalar-subquery results, while procedures preserve direct casts and concrete PL/pgSQL datum declarations, including typed NULL variables; named and default slots are matched before effective-signature search-path shadowing; typed identities persist across catalog reopen; and exact information-schema domain overloads outrank their base types when the call expression retains or restores the domain through nested casts. Compiler bindings distinguish unqualified `COALESCE`, `GREATEST`, `LEAST`, and `NULLIF` syntax from quoted or schema-qualified ordinary calls, and regular function names continue to obey search-path selection in generated expressions. Select-list projection preserves the selected scalar or set-returning overload through execution, including search-path selection between user routines and built-ins. Table-function source and `ROWS FROM` member bindings remain exact through UPDATE, DELETE, MERGE, correlated LATERAL execution, and stored-view reopen. A stored view records exact user-function dependencies across scalar, table-function, function-group, CTE, subquery, join, and set-operation shapes; `DROP FUNCTION` RESTRICT rejects an exact referenced signature with `2BP01`, unrelated overloads remain droppable, multi-target drops are atomic, and replacing a view replaces its dependency set. `CALL` rejects subquery arguments and reserves required OUT/TABLE placeholders when matching a function of the wrong kind, preserving undefined procedure (`42883`) when the placeholder is omitted and wrong object type (`42809`) when it is supplied.

For scalar and array carriers represented by the engine, user routines resolve the simple `anyelement`/`anyarray`/`anynonarray` family and the common-type `anycompatible`/`anycompatiblearray`/`anycompatiblenonarray` family. Concrete return and parameter substitutions survive SQL and PL/pgSQL scalar calls, `TABLE`, `SETOF`, `RETURN NEXT`, `CALL`, nested overloads, generated columns, stored views, and reopen. Concrete and polymorphic variadic routines implement positional expansion, explicit array pass-through, defaults, named explicit notation, fixed-versus-expanded ranking, and declared-array identity, with PostgreSQL SQLSTATEs for indeterminate, undefined, ambiguous, and invalid-declaration cases.

`ALTER FUNCTION` and kind-neutral `ALTER ROUTINE` change volatility and null-input attributes for exact or uniquely visible function identities while preserving the compiled body and durable catalog state. Explicit empty and omitted signatures remain distinct, `%TYPE`, search-path, ambiguity, missing-object, and wrong-kind resolution follow the PostgreSQL 18 evidence, and applying these function-only attributes to a procedure reports `42P13`. Other ALTER actions and the complete ownership, ACL, security, configuration, support-function, leakproof, parallel-safety, and extension-language consequences remain open under `routines.extension-language-privilege-security`.

The bounded routine CASCADE implementation removes generated columns and direct or transitive stored views bound to one exact function, preserves unrelated overloads and objects, supports a no-dependent procedure, remains atomic on RESTRICT and wrong-kind failures, and survives reopen. The remaining dependency-object and ordering matrix is kept partial under `routines.function-procedure-drop-cascade-extended`.

Multi-function `ROWS FROM` follows PostgreSQL 18 row shape for the implemented table-function surface: members resolve independently, columns concatenate in declaration order, rows zip to the longest member with NULL padding, outer aliases rename the combined output, one group-wide `WITH ORDINALITY` column follows all member columns, and correlated groups are implicitly lateral. An unqualified multi-argument `unnest` in a FROM range-function position is PostgreSQL syntax that expands to unary `pg_catalog.unnest` members before ordinary user-overload selection; single-argument unqualified calls and schema-qualified calls remain ordinary functions, while `pg_catalog.unnest` has no ordinary multi-argument signature.

Volatility affects planning and can be changed through the implemented ALTER lifecycle, but UQA Engine does not reproduce every PostgreSQL catalog, security, and privilege consequence of routine declarations.

## Graph compatibility

The `cypher` table function follows an Apache AGE-shaped SQL interface and uses `agtype` output. UQA Engine adds concrete SQL output types for direct joins. The Cypher parser is an implemented subset rather than complete AGE or Neo4j Cypher.

Graph and label management follows AGE's `graph_commands.c` and `label_commands.c`: `create_graph`, `drop_graph`, `graph_exists`, `create_vlabel`, `create_elabel`, `drop_label`, and `alter_graph` validate names with AGE's Unicode identifier rules and raise AGE's messages and SQLSTATEs, and `ag_catalog.ag_graph` / `ag_catalog.ag_label` report the same durable rows an AGE database holds. `drop_label` uses dependency-based `DROP TABLE ... RESTRICT`: same-kind inherited labels and stored views block the drop with `2BP01`, while an otherwise independent default or user label is removed even when nonempty; vertex-label removal preserves incident rows in other edge-label relations and can therefore leave dangling endpoints. Direct `DROP TABLE graph.label` is protected by AGE's object-access rule and fails with `2BP01` instead of bypassing `drop_label`. View bindings follow graph renames durably, and a cascading graph drop removes their transitive dependent-view closure. A removed default stays absent across rename and reopen, the graph remains catalog-visible, the surviving kind remains usable, and recreating the whole graph restores both defaults. Apache AGE 1.8.0 can crash a PostgreSQL backend on some operations against a graph missing a default relation; UQA Engine preserves the same observable catalog and dangling-row lifecycle but returns deterministic `42P01` errors for accesses that require the missing kind. Label relations such as `graph._ag_label_vertex` are selectable and participate in stored-view dependencies; default relations include the rows of their surviving same-kind child labels, `cypher(...)` remains the graph-query interface, and `LOAD 'age'` is a no-op rather than a shared-library load.

Regular path query syntax and functions such as `graph_traverse` and `graph_pagerank` are UQA Engine extensions.

## Retrieval compatibility

GIN, IVF, and HNSW names describe UQA Engine physical indexes and do not promise byte-format or parameter parity with PostgreSQL extensions. `_score`, retrieval predicates, Bayesian evidence functions, and model operators are UQA Engine query extensions.

Approximate IVF and HNSW results can differ from exact KNN by design. Text top-K remains exact when the planner selects WAND or Block-Max WAND because unsafe skipping falls back to scoring.

## Porting checklist

1. Inventory statements, types, functions, extensions, catalog reads, and transaction assumptions.
2. Compare them with this manual and reject unsupported shapes early.
3. Load a representative fixture through the target storage backend.
4. Compare column names, row order, NULLs, text bytes, numeric values, and errors.
5. Evaluate retrieval relevance, calibration, and approximate vector recall separately from SQL equality.
6. Test concurrent sessions, savepoints, failures, cancellation, close, reopen, and storage migration.
7. Run the repository compatibility and domain tests relevant to the port.

Never infer compatibility from successful parsing alone. The contract is successful compilation and execution with the required result and failure semantics.
