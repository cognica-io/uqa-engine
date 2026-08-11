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

Schema-qualified objects are supported. `CREATE SCHEMA AUTHORIZATION` and schema elements embedded inside `CREATE SCHEMA` are not implemented. Cross-database names are rejected.

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

Implemented table properties include columns, defaults, generated serial values, nullability, key constraints, checks, foreign keys, and vector or tensor dimensions. Temporary, unlogged, inherited, partitioned, typed, or tablespace-bound relations are not implemented.

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

## ALTER TABLE

Implemented changes include:

- Add a column
- Add a primary-key, unique, check, or foreign-key constraint
- Drop a column without `CASCADE`
- Rename a column
- Rename a table
- Set or drop a column default
- Set or drop `NOT NULL`
- Change a column type

Examples:

```sql
ALTER TABLE orders ADD COLUMN note TEXT;
ALTER TABLE orders ALTER COLUMN note SET DEFAULT '';
ALTER TABLE orders ALTER COLUMN state SET NOT NULL;
ALTER TABLE orders RENAME COLUMN note TO customer_note;
ALTER TABLE orders ALTER COLUMN total TYPE NUMERIC(20, 2);
```

Type changes validate existing values before publishing the new schema. `DROP COLUMN CASCADE` is rejected without changing the table.

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
CREATE VIEW open_orders AS
SELECT order_id, account_id, total
FROM orders
WHERE state = 'pending';

CREATE OR REPLACE VIEW open_orders AS
SELECT order_id, account_id, total, created_at
FROM orders
WHERE state = 'pending';

DROP VIEW open_orders;
```

View queries are durable and rebound against catalog and function state. Materialized views, view column alias lists, view storage options, and `WITH CHECK OPTION` are not implemented.

## CREATE TABLE AS

```sql
CREATE TABLE pending_orders AS
SELECT order_id, account_id, total
FROM orders
WHERE state = 'pending';
```

CTAS creates and populates a table from a query. CTAS column-name lists, `WITH NO DATA`, temporary persistence, storage options, access methods, `ON COMMIT`, and tablespaces are not implemented. `SELECT INTO` is not an alias for CTAS in UQA-RS.

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
