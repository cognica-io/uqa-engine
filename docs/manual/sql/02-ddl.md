# Data Definition Language

DDL changes the durable catalog and participates in engine transaction boundaries. Use explicit transactions when multiple catalog and data changes form one deployment invariant.

## Schemas

```sql
CREATE SCHEMA IF NOT EXISTS application;
SET search_path TO application, public;
CREATE TABLE tasks (id INTEGER PRIMARY KEY);

CREATE SCHEMA scratch;
DROP SCHEMA scratch;
```

Schema-qualified objects are supported. `CREATE SCHEMA AUTHORIZATION` and schema elements embedded inside `CREATE SCHEMA` are not implemented. Cross-database names are rejected. `DROP SCHEMA` requires an empty schema; `DROP SCHEMA ... CASCADE` is implemented only for graph namespaces, where it drops the named graph.

Every named graph reserves a namespace of its own name and `ag_catalog` is reserved for the Apache AGE catalog, so `CREATE SCHEMA` rejects those names as existing or reserved schemas and `DROP SCHEMA graph_name` fails until the graph is dropped; see [Graph SQL and Cypher](07-graph.md).

## Tables

```sql
CREATE TABLE IF NOT EXISTS orders (
    order_id BIGINT PRIMARY KEY,
    account_id BIGINT NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending',
    total NUMERIC(18, 2) NOT NULL CHECK (total >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (account_id, order_id)
);
```

Implemented table properties include columns, defaults, generated serial values, virtual and stored generated columns, nullability, key constraints, checks, foreign keys, and vector or tensor dimensions. Temporary, unlogged, inherited, partitioned, typed, or tablespace-bound relations are not implemented.

## Generated columns

PostgreSQL 18 generated-column syntax is supported with `VIRTUAL` as the default when neither kind is written:

```sql execute
CREATE TABLE generated_totals (
    quantity INTEGER,
    unit_price NUMERIC(10, 2),
    display_quantity INTEGER GENERATED ALWAYS AS (quantity + 1),
    line_total NUMERIC(12, 2) GENERATED ALWAYS AS (quantity * unit_price) STORED
);

INSERT INTO generated_totals VALUES (2, 4.50, DEFAULT, DEFAULT);
SELECT display_quantity, line_total FROM generated_totals;
```

A generation expression can reference non-generated columns in the same row and must be immutable. Subqueries, aggregate or window functions, parameters, `DEFAULT`, whole-row references, and references to another generated column are rejected before the table is created. The implemented expression surface is statically typed before catalog mutation. Stored SQL routine calls bind and persist the exact overload signature used for later evaluation and dependency checks. A generated column cannot also have a default or identity definition.

Virtual generated values are absent from physical row storage and are evaluated only when a logical projection or enforced constraint requires them. Stored generated values are recomputed exactly once at the prepared-write boundary of every insert, update, upsert, merge, referential action, and direct document replacement. Assigning a generated column directly is rejected; `DEFAULT` requests recomputation and is the only accepted explicit assignment.

Virtual generated columns cannot use user-defined routines or UQA Engine engine-defined types and cannot own primary-key, unique, foreign-key, or index constraints. Stored generated columns can participate in those constraints and indexes.

`ALTER TABLE ADD COLUMN` supports both generated kinds, and `ALTER COLUMN ... SET EXPRESSION AS (...)` replaces a generation expression. `DROP EXPRESSION` is available for a stored generated column and retains its last stored values; PostgreSQL 18 rejects that operation for a virtual generated column.

## Key and uniqueness constraints

Primary keys and unique constraints can cover one or more columns:

```sql
CREATE TABLE memberships (
    organization_id INTEGER NOT NULL,
    user_id INTEGER NOT NULL,
    email TEXT,
    PRIMARY KEY (organization_id, user_id),
    UNIQUE NULLS NOT DISTINCT (organization_id, email)
);
```

`NULLS NOT DISTINCT` makes NULL values compare as equal for uniqueness. A primary key also implies non-NULL key columns.

## Check constraints

```sql
CREATE TABLE measurements (
    id INTEGER PRIMARY KEY,
    value DOUBLE PRECISION NOT NULL,
    CHECK (value >= 0 AND value <= 1)
);
```

A check rejects rows for which its predicate is false. Follow SQL three-valued logic: a NULL result does not replace a separate `NOT NULL` requirement.

## Foreign keys

```sql
CREATE TABLE parent (
    id INTEGER PRIMARY KEY,
    replacement_id INTEGER UNIQUE
);

CREATE TABLE child (
    id INTEGER PRIMARY KEY,
    parent_id INTEGER,
    FOREIGN KEY (parent_id) REFERENCES parent(id)
        MATCH SIMPLE
        ON UPDATE CASCADE
        ON DELETE SET NULL
);
```

Implemented match modes are `MATCH SIMPLE` and `MATCH FULL`. Referential actions are `NO ACTION`, `RESTRICT`, `CASCADE`, `SET NULL`, and `SET DEFAULT`. Column subsets are supported for `ON DELETE SET NULL` and `ON DELETE SET DEFAULT`. `MATCH PARTIAL` is not implemented.

Referenced columns must satisfy the implemented unique-key requirements. Mutations validate referential actions as part of the same transaction.

## Temporal keys and foreign keys

PostgreSQL 18 temporal keys place `WITHOUT OVERLAPS` on the final range or multirange key column, and temporal foreign keys place `PERIOD` before the final local and referenced columns:

```sql execute
CREATE TABLE account_periods (
    account_id INTEGER,
    valid_at DATERANGE,
    PRIMARY KEY (account_id, valid_at WITHOUT OVERLAPS)
);

CREATE TABLE account_events (
    event_id INTEGER PRIMARY KEY,
    account_id INTEGER,
    valid_at DATERANGE,
    FOREIGN KEY (account_id, PERIOD valid_at)
        REFERENCES account_periods (account_id, PERIOD valid_at)
);
```

A `PRIMARY KEY` or `UNIQUE` key with `WITHOUT OVERLAPS` rejects empty period values and rejects overlapping ranges for rows whose ordinary key prefix is equal; adjacent periods do not overlap. A `PERIOD` foreign key requires an exactly matching range or multirange type and a referenced `PRIMARY KEY` or `UNIQUE` constraint with `WITHOUT OVERLAPS` over the same columns. The referenced rows with one ordinary key prefix may cover the child period in aggregate, so adjacent parent ranges can jointly satisfy one child range.

Temporal constraints are enforced on insert, update, and delete. A parent update or delete is rejected when the remaining parent periods no longer cover an existing child, and `ALTER TABLE ADD CONSTRAINT` validates all existing rows before publishing any catalog change. The temporal flags persist across reopen and appear as `conperiod` in `pg_constraint`. The implemented temporal foreign-key action is `NO ACTION`; other referential actions are rejected before mutation. Physical GiST and exclusion-index planning for these constraints remains an open compatibility bug.

## ALTER TABLE

Implemented changes include:

- Add a column
- Add a primary-key, unique, check, or foreign-key constraint
- Drop a column without `CASCADE`
- Rename a column
- Rename a table
- Set or drop a column default
- Set or drop a stored generation expression
- Set or drop `NOT NULL`
- Change a column type

Examples:

```sql
ALTER TABLE orders ADD COLUMN note TEXT;
ALTER TABLE orders ALTER COLUMN note SET DEFAULT '';
ALTER TABLE orders ALTER COLUMN state SET NOT NULL;
ALTER TABLE orders RENAME COLUMN note TO customer_note;
ALTER TABLE orders ALTER COLUMN total TYPE NUMERIC(20, 2);
ALTER TABLE generated_totals ALTER COLUMN line_total SET EXPRESSION AS (quantity * unit_price * 2);
```

Type changes evaluate an optional `USING` expression once for each old row, validate all rewritten rows, constraints, and generated dependencies, and publish the new schema and data atomically. Built-in ranges can be rewritten to their paired multirange with `USING multirange(column)` while retaining `WITHOUT OVERLAPS`; changing one side of an existing `PERIOD` relationship to an incompatible range identity is rejected with PostgreSQL 18 datatype-mismatch SQLSTATE `42804`. `DROP COLUMN CASCADE` is rejected without changing the table.

## Relational B-tree indexes

The default access method is B-tree:

```sql
CREATE INDEX orders_state_idx ON orders (state);
CREATE UNIQUE INDEX orders_account_state_uq ON orders (account_id, state);
DROP INDEX orders_state_idx;
```

Expression indexes are not implemented. Index columns must be table columns. `DROP ... CASCADE` is rejected instead of discarding dependent objects implicitly.

## Full-text GIN indexes

```sql
CREATE INDEX articles_text_gin
ON articles USING gin (title, body)
WITH (analyzer = 'english');
```

A GIN index marks its text columns as searchable and maintains full-text postings. A named analyzer can be assigned through the analyzer option after it has been registered. The option applies to index and search analysis, backfills existing rows, and remains part of the durable index definition; see [Analyzer SQL](05-analyzers.md).

## IVF vector indexes

```sql
CREATE INDEX items_embedding_ivf
ON items USING ivf (embedding)
WITH (lists = 128, probes = 16, train_threshold = 2000);
```

IVF accepts positive integer `lists`, `probes`, and `train_threshold` settings and their documented aliases. It can index one `VECTOR(n)` field and uses approximate partition probing.

## HNSW vector indexes

```sql
CREATE INDEX items_embedding_hnsw
ON items USING hnsw (embedding)
WITH (
    m = 16,
    ef_construction = 200,
    ef_search = 64,
    rebuild_threshold = 1000,
    seed = 42
);
```

HNSW option values are unsigned integers. Underscore and documented hyphenated aliases are accepted. One vector column cannot have both IVF and HNSW physical ownership simultaneously.

SQL `CREATE INDEX` accepts B-tree, GIN, IVF, and HNSW. Other access methods, including R-tree, are not exposed by SQL DDL.

## Views

```sql
CREATE VIEW open_orders (order_id, account_id, total) AS
SELECT order_id, account_id, total
FROM orders
WHERE state = 'pending';

CREATE OR REPLACE VIEW open_orders AS
SELECT order_id, account_id, total, created_at
FROM orders
WHERE state = 'pending';

DROP VIEW open_orders;
```

An optional view column-name list renames query outputs positionally and may name only a leading subset. It cannot contain more names than the query returns, the final names must be unique, and quoted names retain their exact spelling. `CREATE OR REPLACE VIEW` must preserve the name and declared type of every existing column in order, but it may append columns at the end. Creation analyzes the query without executing it, then validates the column list and target relation; the durable definition retains the fixed public names across nested views, transactions, and reopen. Materialized views, view storage options, and `WITH CHECK OPTION` are not implemented.

## CREATE TABLE AS

```sql
CREATE TABLE pending_orders (order_id, account_id, total) AS
SELECT order_id, account_id, total
FROM orders
WHERE state = 'pending';
```

CTAS creates and populates a table from a query, preserves the query's declared output types for implemented SQL types, and creates nullable columns without copying source constraints. An optional column-name list replaces output names positionally and may be shorter than the query output, in which case remaining names come from the query; quoted case is preserved. More names than output columns raise `42601`, while duplicate names and PostgreSQL system-column names raise `42701`, before the query is executed. `WITH NO DATA` creates and durably persists the same typed schema, including vector and tensor field metadata, without evaluating row-producing expressions or volatile functions; relation, column, function, type-input, and column-name-list analysis still occurs in PostgreSQL order. Top-level `SELECT ... INTO [TABLE] name` creates the same ordinary durable table and executes the query; PL/pgSQL `SELECT ... INTO` remains variable assignment. Temporary persistence, storage options, access methods, `ON COMMIT`, and tablespaces are not implemented.

## Sequences

```sql
CREATE SEQUENCE ticket_ids START WITH 1000 INCREMENT BY 1;
SELECT nextval('ticket_ids');
SELECT currval('ticket_ids');
SELECT setval('ticket_ids', 2000);
ALTER SEQUENCE ticket_ids RESTART WITH 3000;
```

`CREATE SEQUENCE` supports start and increment. `ALTER SEQUENCE` supports restart, increment, and start. SQL `DROP SEQUENCE` is not implemented; the Rust engine API provides `Engine::drop_sequence`. Minimum, maximum, cache, cycle, ownership, identity ownership, and temporary sequence clauses are not implemented.

## Foreign servers and tables

```sql
CREATE SERVER analytics
FOREIGN DATA WRAPPER duckdb_fdw
OPTIONS (database 'analytics.duckdb');

CREATE FOREIGN TABLE external_events (
    event_id BIGINT,
    payload JSONB
)
SERVER analytics
OPTIONS (table 'events');
```

Registered built-in server types are `memory_fdw`, `duckdb_fdw`, and `arrow_fdw` on targets that include their native handlers. Server and table options are validated by the selected handler. Browser WASM does not include native DuckDB or Arrow handlers.

## TRUNCATE and DROP

```sql
TRUNCATE TABLE staging_a, staging_b;
DROP TABLE IF EXISTS staging_a;
```

`TRUNCATE` removes all rows from its listed tables under a transaction boundary. SQL drop supports implemented table, index, view, schema, function, and procedure targets. Dependency-sensitive `CASCADE` forms fail rather than partially changing the catalog.
