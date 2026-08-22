# PostgreSQL Compatibility and Limits

UQA Engine deliberately uses PostgreSQL-oriented syntax and behavior while remaining an embedded engine with its own storage, planner, catalog, and extension model. PostgreSQL 18 is the behavioral oracle: every externally observable difference is a compatibility bug, including differences in features not yet implemented.

## Compatibility baseline

- SQL parsing uses PostgreSQL grammar through `libpg_query`.
- Session metadata reports `server_version` as `18.0-uqa`.
- The repository checks all 22 deterministic TPC-H-derived scale-factor `0.001` query results against PostgreSQL 18.4 fixtures.
- The optional wire crate implements PostgreSQL protocol 3.0 through 3.2 codec primitives, minor-version negotiation, reserved startup-option reporting, protocol-specific cancellation-key validation, FunctionCall, GSS/SSPI message shapes, notifications, and COPY format validation. PostgreSQL 18.4 `psql`/libpq tests verify 3.0, 3.2, and `latest` startup, 3.2-to-3.0 downgrade, authentication ordering, SSL rejection and retry, legacy and 256-byte cancellation keys, and extended Parse/Bind/Describe/Execute/Sync flow.
- PostgreSQL-shaped `information_schema` and `pg_catalog` virtual relations carry declared PostgreSQL 18 row types even when empty; implemented `pg_type` rows expose exact scalar, array, information-schema domain, pseudo-type, and `information_schema_catalog_name` metadata.
- Apache AGE-shaped `cypher(...) AS (...)` integrates graph results into SQL, and the AGE catalog surface (`LOAD 'age'`, `ag_catalog.ag_graph`, `ag_catalog.ag_label`, the `agtype` / `graphid` types, and the graph and label management functions) lets AGE drivers bootstrap against the embedded engine.

The fixture coverage is evidence for those queries and types, not a claim of complete PostgreSQL 18 compatibility.

## Embedded runtime architecture

UQA Engine does not yet implement PostgreSQL processes, network protocol semantics as its core API, MVCC storage pages, roles, grants, extensions, background workers, replication, WAL administration, or server configuration. The optional PostgreSQL wire and FDW crates are adapters around the engine. Every externally visible difference caused by this architecture remains an open compatibility bug rather than an accepted alternative behavior.

Some declared SQL types share an internal runtime carrier, but scans and relational plans retain the declared `ColumnType`, integer writes and casts enforce `int2`/`int4`/`int8` ranges, source-sensitive OID/XID/bytea casts retain source width, and result schemas do not infer identity from values. Any remaining PostgreSQL 18 overflow, cast, storage, collation, binary-format, or display difference is an open compatibility bug.

Relational operators keep static row schemas until the final consumer. Spill format version 1 records the declared schema and logical `(alias, column)` identity directly; spilling is not a reason to materialize rows or flatten qualification early, and there is no legacy spill compatibility reader.

## Open PostgreSQL 18 relation-feature bugs

- Temporary and unlogged tables, views, and sequences
- Table inheritance and partitioning
- Typed tables
- Table storage parameters and tablespaces
- Table `USING` access methods
- `ON COMMIT` table behavior
- Cross-database relation or routine names
- Materialized views
- View column alias lists, view options, and `WITH CHECK OPTION`
- `SELECT INTO`
- CTAS column-name lists and `WITH NO DATA`

These forms currently fail during compilation without creating a partial object. That failure is fail-safe behavior while implementation is incomplete, not a compatibility exemption.

## Open PostgreSQL 18 query-clause bugs

- Collations, domains, user-defined equality operators, and the complete common-type matrix for `JOIN ... USING` columns with different declared types
- Aliases on parenthesized join expressions
- Multi-function `ROWS FROM`
- Table functions `WITH ORDINALITY`
- `GROUP BY DISTINCT`
- Named `WINDOW` clauses
- Recursive CTE `SEARCH` and `CYCLE`
- CTE `NOT MATERIALIZED`
- Explicit `ESCAPE` for `LIKE`, `ILIKE`, and `SIMILAR TO`
- `MERGE WHEN NOT MATCHED BY SOURCE`

Each missing clause above must be implemented with PostgreSQL 18 semantics; source-query rewriting is not an accepted compatibility solution.

## Open PostgreSQL 18 DDL bugs

- Virtual and stored generated columns implement durable definitions, selective virtual evaluation, exactly-once stored evaluation, DML assignment rules, DDL-time static typing for the implemented expression surface, exact stored SQL routine overload binding and dependencies, supported constraints and indexes, catalogs, ALTER operations, failure atomicity, and reopen behavior. The complete PostgreSQL built-in function and operator overload matrix, privileges, inheritance and partition propagation, `pg_dump`/`pg_restore`, and complete upstream regression coverage remain open compatibility bugs.
- `WITHOUT OVERLAPS` keys and `PERIOD` foreign keys are not implemented because range and multirange column types are not yet available.
- Expression indexes are not implemented.
- SQL index access methods are B-tree, GIN, IVF, and HNSW.
- `DROP ... CASCADE` is rejected for implemented catalog objects except graph namespaces, where `DROP SCHEMA graph_name CASCADE` drops the graph.
- `ALTER TABLE DROP COLUMN CASCADE` is rejected.
- `ALTER COLUMN TYPE USING` is preserved structurally and evaluated once per old row inside the atomic ALTER transaction; the complete PostgreSQL assignment-cast matrix, dependency and collation rewrites, domain checks, and upstream ALTER regression cases remain open.
- `CREATE SCHEMA AUTHORIZATION` and embedded schema elements are not implemented.
- Sequence minimum, maximum, cycle, cache, ownership, and identity-owned sequence options are not implemented.
- `VACUUM` is not implemented.
- `ANALYZE` accepts all tables or one table, without options or a column list.
- `EXPLAIN` options are limited to `ANALYZE`, `VERBOSE`, and `FORMAT TEXT` or `FORMAT JSON`.

## Open PostgreSQL 18 type bugs

Supported declarations retain distinct `SMALLINT`, `INTEGER`, `BIGINT`, `OID`, `XID`, `REAL`, `DOUBLE PRECISION`, `TEXT`, `NAME`, `UUID`, `VARCHAR(n)`, `CHAR(n)`, `INTERVAL`, array, and information-schema domain identities. Character lengths, numeric precision/scale, integer ranges, source-sensitive OID/XID/bytea casts, `ALTER TYPE USING`, foreign-table schemas, and supported migration types are enforced by the implemented paths; unknown migration types fail instead of becoming text.

- The complete implicit, assignment, and explicit cast-context matrix is not implemented for every source and target pair.
- User-defined domains, enums, composite declarations, ranges, and multiranges are not implemented completely.
- Large declared `NUMERIC` precision is bounded by the engine decimal carrier in actual values.
- Collation and locale behavior is not a complete PostgreSQL collation implementation.
- Type I/O routine OIDs are present in implemented `pg_type` rows, but the complete corresponding `pg_proc`, typmod, binary I/O, and extension-type catalog surface remains open.
- `VECTOR(n)` and `TENSOR(n)` are UQA Engine retrieval types rather than PostgreSQL core types.

## Open PostgreSQL 18 catalog and administration bugs

The virtual catalogs expose engine metadata needed by supported clients and tests. Implemented PostgreSQL 18 identities include catalog row schemas, information-schema domain OIDs, core/system type layout and I/O routine OIDs, `regnamespace`, `record`, `_record`, `void`, and the `information_schema_catalog_name` view/composite/array with its `pg_class` and `pg_attribute` rows. The Apache AGE extension catalog is implemented: `ag_catalog.ag_graph`, `ag_catalog.ag_label`, the `agtype`, `graphid`, `label_id`, and `label_kind` types, one namespace per graph, and the label relations and sequences of every graph appear in `pg_namespace`, `pg_class`, `pg_attribute`, `pg_sequences`, `pg_type`, and the information schema. The complete OID graph, every `pg_proc` row, ownership, ACLs, server processes, WAL, statistics views, and other extension catalogs remain open.

Known mutable settings are `search_path`, `client_encoding`, `datestyle`, `timezone`, and `work_mem`. Unknown or unsupported settings return an error rather than becoming ignored server configuration.

`DISCARD TEMP` is rejected because temporary relations are unavailable.

`LOAD` accepts the Apache AGE library names (`age`, `age.so`, `$libdir/age`, `$libdir/age.so`) as no-ops because the AGE surface is embedded; every other library fails as a missing `$libdir` file because the engine loads no shared objects.

## Open PostgreSQL 18 routine bugs

SQL and PL/pgSQL routines cover a broad tested subset, including overloads, defaults, set returns, procedures, control flow, dynamic SQL, recursion limits, diagnostics, exception handling, qualified named types, table-backed `%TYPE`, strict assignment/return casts, and bound cursors used entirely within one routine activation. Dynamic cursor queries, `MOVE`, non-`NEXT` fetch directions, `refcursor` parameters and returns, and cursors that survive routine exit are not implemented because session portal state is not available. The routine surface does not claim the full PL/pgSQL language, PostgreSQL extension languages, security-definer ecosystem, or server privilege model.

Volatility affects planning, but UQA Engine does not reproduce every PostgreSQL catalog and privilege consequence of routine declarations.

## Graph compatibility

The `cypher` table function follows an Apache AGE-shaped SQL interface and uses `agtype` output. UQA Engine adds concrete SQL output types for direct joins. The Cypher parser is an implemented subset rather than complete AGE or Neo4j Cypher.

Graph and label management follows AGE's `graph_commands.c` and `label_commands.c`: `create_graph`, `drop_graph`, `graph_exists`, `create_vlabel`, `create_elabel`, `drop_label`, and `alter_graph` validate names with AGE's Unicode identifier rules and raise AGE's messages and SQLSTATEs, and `ag_catalog.ag_graph` / `ag_catalog.ag_label` report the same rows an AGE database holds. Label relations such as `graph._ag_label_vertex` are catalog metadata only; entities are read through `cypher(...)`, and `LOAD 'age'` is a no-op rather than a shared-library load. One deliberate difference: AGE's `drop_label` runs `DROP TABLE ... RESTRICT`, which removes an empty default label relation and leaves the graph unusable, while the engine always rejects dropping `_ag_label_vertex` and `_ag_label_edge` with `cannot drop table graph.label because other objects depend on it` (`2BP01`) because every graph depends on its default labels.

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
