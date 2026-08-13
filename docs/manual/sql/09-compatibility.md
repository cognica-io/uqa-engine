# PostgreSQL Compatibility and Limits

UQA-RS deliberately uses PostgreSQL-oriented syntax and behavior while remaining an embedded engine with its own storage, planner, catalog, and extension model. Treat it as a compatible query surface for implemented features, not as a drop-in PostgreSQL server.

## Compatibility baseline

- SQL parsing uses PostgreSQL grammar through `libpg_query`.
- Session metadata reports `server_version` as `18.0-uqa`.
- The repository checks all 22 deterministic TPC-H-derived scale-factor `0.001` query results against PostgreSQL 18.4 fixtures.
- The optional wire crate implements PostgreSQL protocol 3.0 through 3.2 codec primitives, minor-version negotiation, reserved startup-option reporting, and protocol-specific cancellation-key validation.
- PostgreSQL-shaped `information_schema` and `pg_catalog` virtual relations support common inspection paths.
- Apache AGE-shaped `cypher(...) AS (...)` integrates graph results into SQL.

The fixture coverage is evidence for those queries and types, not a claim of complete PostgreSQL 18 compatibility.

## Embedded runtime differences

UQA-RS does not implement PostgreSQL processes, network protocol semantics as its core API, MVCC storage pages, roles, grants, extensions, background workers, replication, WAL administration, or server configuration. The optional PostgreSQL wire and FDW crates are adapters around the engine, not a replacement PostgreSQL server.

Integer aliases share a signed 64-bit carrier. Floating aliases share a 64-bit carrier. Text-like declarations can share one text carrier. These choices can differ from PostgreSQL overflow, storage, collation, and display behavior.

## Explicitly unsupported relation features

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

These forms fail during compilation without creating a partial object.

## Explicitly unsupported query clauses

- `NATURAL JOIN`
- `JOIN ... USING`
- Aliases on parenthesized join expressions
- Multi-function `ROWS FROM`
- Table functions `WITH ORDINALITY`
- `GROUP BY DISTINCT`
- Named `WINDOW` clauses
- `FETCH ... WITH TIES`
- `SELECT` row-locking clauses such as `FOR UPDATE`
- Recursive CTE `SEARCH` and `CYCLE`
- CTE `NOT MATERIALIZED`
- Explicit `ESCAPE` for `LIKE`, `ILIKE`, and `SIMILAR TO`
- `MERGE WHEN NOT MATCHED BY SOURCE`

Use an equivalent implemented shape only when it preserves the application's semantics. For example, replace `JOIN ... USING (id)` with an explicit `JOIN ... ON left.id = right.id` and project the desired key once.

## DDL limits

- Virtual and stored generated columns are not implemented.
- `WITHOUT OVERLAPS` keys and `PERIOD` foreign keys are not implemented because range and multirange column types are not yet available.
- Expression indexes are not implemented.
- SQL index access methods are B-tree, GIN, IVF, and HNSW.
- `DROP ... CASCADE` is rejected for implemented catalog objects.
- `ALTER TABLE DROP COLUMN CASCADE` is rejected.
- `CREATE SCHEMA AUTHORIZATION` and embedded schema elements are not implemented.
- Sequence minimum, maximum, cycle, cache, ownership, and identity-owned sequence options are not implemented.
- `VACUUM` is not implemented.
- `ANALYZE` accepts all tables or one table, without options or a column list.
- `EXPLAIN` options are limited to `ANALYZE`, `VERBOSE`, and `FORMAT TEXT` or `FORMAT JSON`.

## Type limits

- `INTERVAL` can be an expression value but is not a table column declaration.
- `VARCHAR(n)` should not be used as the sole enforcement of a length invariant; add a check.
- Declared integer widths do not imply PostgreSQL's distinct `int2`, `int4`, and `int8` overflow ranges.
- Large declared `NUMERIC` precision is bounded by the engine decimal carrier in actual values.
- Collation and locale behavior is not a complete PostgreSQL collation implementation.
- `VECTOR(n)` and `TENSOR(n)` are UQA-RS retrieval types rather than PostgreSQL core types.

## Catalog and administration limits

The virtual catalogs expose engine metadata needed by supported clients and tests. OIDs, ownership, ACLs, server processes, WAL, statistics views, and extension catalogs are not complete PostgreSQL implementations.

Known mutable settings are `search_path`, `client_encoding`, `datestyle`, `timezone`, and `work_mem`. Unknown or unsupported settings return an error rather than becoming ignored server configuration.

`DISCARD TEMP` is rejected because temporary relations are unavailable.

## Routine limits

SQL and PL/pgSQL routines cover a broad tested subset, including overloads, defaults, set returns, procedures, control flow, dynamic SQL, recursion limits, diagnostics, exception handling, and bound cursors used entirely within one routine activation. Dynamic cursor queries, `MOVE`, non-`NEXT` fetch directions, `refcursor` parameters and returns, and cursors that survive routine exit are not implemented because session portal state is not available. The routine surface does not claim the full PL/pgSQL language, PostgreSQL extension languages, security-definer ecosystem, or server privilege model.

Volatility affects planning, but UQA-RS does not reproduce every PostgreSQL catalog and privilege consequence of routine declarations.

## Graph compatibility

The `cypher` table function follows an Apache AGE-shaped SQL interface and uses `agtype` output. UQA-RS adds concrete SQL output types for direct joins. The Cypher parser is an implemented subset rather than complete AGE or Neo4j Cypher.

Regular path query syntax and functions such as `graph_traverse` and `graph_pagerank` are UQA-RS extensions.

## Retrieval compatibility

GIN, IVF, and HNSW names describe UQA-RS physical indexes and do not promise byte-format or parameter parity with PostgreSQL extensions. `_score`, retrieval predicates, Bayesian evidence functions, and model operators are UQA-RS query extensions.

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
