# Data Definition Language

DDL changes the durable catalog and participates in engine transaction boundaries. Use explicit transactions when multiple catalog and data changes form one deployment invariant.

## Database privileges

```sql
GRANT CONNECT, TEMPORARY ON DATABASE uqa TO app_reader;
GRANT CREATE ON DATABASE uqa TO app_writer WITH GRANT OPTION;
SELECT has_database_privilege('app_reader', 'uqa', 'CONNECT');
REVOKE GRANT OPTION FOR CREATE ON DATABASE uqa FROM app_writer CASCADE;
```

The embedded engine exposes one current database named `uqa`, owned by the bootstrap `uqa` role. Its default ACL grants `CONNECT` and `TEMPORARY` to `PUBLIC`, while the owner retains implicit `CONNECT`, `CREATE`, and `TEMPORARY` grant options even after self-revocation. Database ACLs support `ALL [PRIVILEGES]`, `PUBLIC`, `TEMP` as an alias for `TEMPORARY`, `WITH GRANT OPTION`, `GRANT OPTION FOR`, `GRANTED BY`, `RESTRICT`, and `CASCADE`; independent rooted grantor paths, role dependencies, transactions, savepoints, cross-engine refresh, and durable reopen are preserved. `CREATE SCHEMA` enforces the database `CREATE` privilege, and temporary tables, sequences, views, CTAS targets, `SELECT INTO` targets, indexes, and indexed key constraints enforce `TEMPORARY`; inherited grants, revocation after temporary-namespace allocation, transactions, savepoints, and PostgreSQL source-analysis and definition-error precedence are preserved. `pg_database.datdba` and `datacl` expose the owner and ACL. The six current-user or explicit-role name/OID `has_database_privilege` overloads accept comma-separated checks, preserve inherited ownership, strict NULLs, missing-name and missing-OID distinctions, exact error precedence, and PostgreSQL 18 `pg_proc` identities. Creating, altering, dropping, or transferring ownership of databases and enforcing `CONNECT` at the embedding connection boundary remain compatibility bugs.

## Schemas

```sql
CREATE SCHEMA IF NOT EXISTS application;
SET search_path TO application, public;
CREATE TABLE tasks (id INTEGER PRIMARY KEY);
GRANT USAGE ON SCHEMA application TO app_reader;
GRANT CREATE ON SCHEMA application TO app_writer;
SELECT has_schema_privilege('application', 'USAGE');

CREATE SCHEMA scratch;
DROP SCHEMA scratch;
```

Schema-qualified objects are supported, and the role active at `CREATE SCHEMA` owns the schema. Schema ACLs support `USAGE`, `CREATE`, `ALL [PRIVILEGES]`, `PUBLIC`, `WITH GRANT OPTION`, `GRANT OPTION FOR`, `GRANTED BY`, `RESTRICT`, and `CASCADE` through `GRANT` and `REVOKE` on explicit schema targets. Inherited ownership and grant-option paths are honored, dependent grants follow `RESTRICT` or `CASCADE`, schema owners and ACL grantors or grantees prevent `DROP ROLE`, and changes follow transaction, savepoint, cross-engine refresh, and durable-reopen lifecycle. `pg_namespace.nspowner` and `nspacl` expose the durable owner and ACL, while `information_schema.schemata` exposes schemas on which the current role has `USAGE` or `CREATE`. The six current-user or explicit-role name/OID `has_schema_privilege` overloads accept comma-separated `USAGE` and `CREATE` checks, including `WITH GRANT OPTION`, and preserve PostgreSQL 18 owner, inherited-role, system-schema, current temporary-schema, strict-NULL, missing-object, error-precedence, and `pg_proc` behavior. A numeric text argument remains a schema name, and `pg_temp` is not accepted as an alias by this inquiry function; use the allocated temporary namespace name or OID. Schema `CREATE` is enforced for durable tables, CTAS, `SELECT INTO`, views, materialized views, sequences, foreign tables, functions, procedures, standalone indexes, and indexed key constraints, including inherited grants, immediate revocation, transactions, savepoints, qualified versus search-path selection, inferred temporary views, and PostgreSQL source-analysis, definition-error, and collision precedence; unqualified creation and sibling-object creation through indexes or indexed constraints also require schema `USAGE`. Routine calls and ALTER, DROP, GRANT, and REVOKE routine-target lookup enforce schema `USAGE`. Ordinary relation queries, `INSERT`, `UPDATE`, `DELETE`, `TRUNCATE`, `ALTER TABLE`, `DROP TABLE`, trigger and rule definition or removal targets, constraint-trigger referenced relations, rule-action mutation targets, `INHERITS`, `PARTITION OF`, `ATTACH PARTITION`, ALTER-time inheritance and foreign-key targets, stored-view source binding, and hard or soft `regclass` input use the same rule: qualified inaccessible schemas report `42501` before object existence, authority, or column validation, while unqualified lookup skips inaccessible search-path entries. Inherited grants and immediate prepared-plan revocation are honored; views, materialized views, SQL-standard query bodies, declared cursors, and stored rule-action mutation targets retain exact definition-time relation identities instead of repeating namespace-name checks. System-catalog projection likewise follows canonical catalog relationships without applying the caller's namespace lookup to those stored identities. Remaining object paths, remaining relation-owner checks, and default privileges remain open compatibility bugs. `CREATE SCHEMA AUTHORIZATION` and schema elements embedded inside `CREATE SCHEMA` are not implemented. Cross-database names are rejected. `DROP SCHEMA` requires an empty schema; `DROP SCHEMA ... CASCADE` is implemented only for graph namespaces, where it drops the named graph.

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

Implemented table properties include columns, defaults, generated serial values, virtual and stored generated columns, nullability, key constraints, checks, foreign keys, ordinary inheritance, declarative partitioning, and vector or tensor dimensions. `TEMP` and `TEMPORARY` tables live in the session's `pg_temp` namespace, are omitted from durable storage, and support `ON COMMIT PRESERVE ROWS`, `ON COMMIT DELETE ROWS`, and `ON COMMIT DROP`; the drop action removes dependent temporary views and foreign-key links with PostgreSQL's internal cascade semantics. `DISCARD TEMP` removes the session's temporary tables, views, and sequences outside a transaction. `UNLOGGED` tables retain their catalog identity and rows across a clean reopen, although PostgreSQL crash-recovery truncation semantics remain unimplemented. Typed, storage-parameterized, access-method-selected, or tablespace-bound tables are not implemented.

## Table privileges

```sql
GRANT SELECT, INSERT ON TABLE orders TO app_writer;
GRANT SELECT ON ALL TABLES IN SCHEMA application TO app_reader;
GRANT UPDATE ON TABLE orders TO app_delegate WITH GRANT OPTION;
GRANT SELECT (order_id, state), UPDATE (state) ON TABLE orders TO app_reader;
SELECT has_table_privilege('app_reader', 'application.orders', 'SELECT');
SELECT has_column_privilege('app_reader', 'application.orders', 'state', 'UPDATE');
REVOKE GRANT OPTION FOR UPDATE ON TABLE orders FROM app_delegate CASCADE;
```

Every ordinary table starts with a NULL ACL and implicit owner access to `SELECT`, `INSERT`, `UPDATE`, `DELETE`, `TRUNCATE`, `REFERENCES`, `TRIGGER`, and `MAINTAIN`. Table-wide `GRANT` and `REVOKE` support `ALL [PRIVILEGES]`, `PUBLIC`, inherited roles, independent grantors, `WITH GRANT OPTION`, `GRANT OPTION FOR`, `GRANTED BY`, dependency-aware `RESTRICT` and `CASCADE`, owner self-revocation and transfer, role dependencies, read-only transactions, temporary tables, transactions, savepoints, cross-engine refresh, and durable reopen. Explicit `ON TABLE` targets route sequences through their `SELECT`, `UPDATE`, and `USAGE` ACL rules, while `ON ALL TABLES IN SCHEMA` excludes sequences. Parent-targeted inheritance and partition operations check the named parent's privilege rather than each physical child, while relations added by foreign-key `TRUNCATE ... CASCADE` require their own `TRUNCATE` privilege.

The engine enforces table-wide privileges for query sources, `INSERT`, `UPDATE`, `DELETE`, `MERGE`, `COPY FROM`, `COPY TO`, `TRUNCATE`, foreign-key references, ordinary-table trigger creation, and `ANALYZE` or `VACUUM (ANALYZE)`. Catalog-wide maintenance processes only tables on which the active role has `MAINTAIN` and emits PostgreSQL-compatible warnings for skipped tables. DML requires `SELECT` only when an expression reads target-table values, matching PostgreSQL's constant-assignment and `RETURNING` boundaries. Ordinary-table column ACLs support `SELECT`, `INSERT`, `UPDATE`, and `REFERENCES`, NULL and explicitly empty `attacl` state, table-to-column privilege implication, independent rooted grant-option paths, dependent `RESTRICT` and `CASCADE`, and exact column checks across direct, joined, correlated, CTE, DML-source, target-expression, `COPY`, `MERGE`, and foreign-key paths. Implicit-width inserts check only supplied positions, `DEFAULT VALUES` accepts any insertable column, system columns require table-wide `SELECT`, and rename, drop, owner transfer, role dependencies, transactions, savepoints, cross-engine refresh, and durable reopen preserve the column ACL state. `pg_class.relacl` and `pg_attribute.attacl` expose PostgreSQL ACL text, all six `has_table_privilege` and all twelve `has_column_privilege` current-user or explicit-role name/OID and column-name/attnum overloads preserve comma-any, grant-option, strict-NULL, missing-object, sequence-column, system-column, and error-precedence behavior, and `information_schema.tables`, `columns`, `column_privileges`, and `role_column_grants` apply PostgreSQL role visibility. Foreign tables use the same relation and column ACL model while their built-in wrappers remain read-only; default privileges and row-level security remain open compatibility bugs.

## Inheritance and partitioning

Ordinary `INHERITS` tables and declarative `LIST`, `RANGE`, and `HASH` partitioning retain independent physical rows while a parent scan includes descendants; `ONLY parent` scans only the parent's own storage. Partition inserts, updates, COPY streams, and hierarchy mutations route to the matching leaf, and bounds, local-versus-inherited column provenance, partition keys, default partitions, partitioned indexes, and inheritance edges are durable catalog state.

```sql
CREATE TABLE events (event_id INTEGER, occurred_on DATE) PARTITION BY RANGE (occurred_on);
CREATE TABLE events_2026 PARTITION OF events FOR VALUES FROM ('2026-01-01') TO ('2027-01-01');
CREATE TABLE events_other PARTITION OF events DEFAULT;
```

Direct hierarchy changes use PostgreSQL 18 `ALTER TABLE` forms:

```sql
ALTER TABLE audit_events INHERIT events;
ALTER TABLE audit_events NO INHERIT events;
ALTER TABLE events ATTACH PARTITION events_2027 FOR VALUES FROM ('2027-01-01') TO ('2028-01-01');
ALTER TABLE events DETACH PARTITION events_2027;
```

`INHERIT` and `NO INHERIT` validate compatible row types, inherited checks, persistence, duplicate edges, and cycles while retaining ordered `pg_inherits.inhseqno` values. Parent names in `CREATE TABLE ... INHERITS`, `PARTITION OF`, `ALTER TABLE ... INHERIT`, and `ATTACH PARTITION` resolve through the active role's effective schema `USAGE` boundary before their definitions are inspected; a missing qualified schema reports `3F000`, and the resulting hierarchy stores exact canonical parent identities. `ATTACH PARTITION` validates the exact partition row type, existing rows, sibling and default bounds, inherited checks, keys, foreign keys, identity generation, and every descendant before publishing the edge. `DETACH PARTITION` localizes the inherited schema state and restores a partition's prior serial generator when an attached parent identity had temporarily overridden it. Reopening a legacy catalog repairs and canonicalizes hierarchy parents before materializing and synchronizing partition-inherited foreign-key object identities; later detach uses their provenance. These changes are atomic, survive rename and reopen, and roll back with an explicit transaction.

`DETACH PARTITION ... CONCURRENTLY` is rejected inside a transaction block and while a default partition exists, and a successful detach retains the partition bound as an enforced typed CHECK constraint. The embedded engine completes a successful concurrent detach in the statement; PostgreSQL's externally interruptible pending-detach and later `FINALIZE` phase remains an open compatibility bug.

`pg_class.relpartbound` has `pg_node_tree` identity, `pg_partitioned_table` exposes each partition key, and `pg_get_expr` and `pg_get_partkeydef` deparse the stored definitions. An index declared on a partitioned parent has PostgreSQL's `relkind = 'I'` identity, and its derived child-index hierarchy appears in `pg_class`, `pg_index`, `pg_indexes`, and `pg_inherits`.

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

User-routine calls in column defaults and column- or table-level CHECK constraints bind the exact overload when the schema change is published. Renaming that routine rewrites the stored expression without changing its object identity, so recreating the old name cannot retarget the expression. `DROP FUNCTION ... RESTRICT` reports `2BP01` while one of these expressions depends on the routine; `CASCADE` removes only the dependent default or CHECK constraint and retains its column and table. Replacing or dropping a default or CHECK constraint atomically replaces or releases its dependency, and committed bindings survive catalog reopen.

## Foreign keys

```sql
CREATE TABLE parent (
    id INTEGER PRIMARY KEY,
    replacement_id INTEGER UNIQUE
);

CREATE TABLE child (
    id INTEGER PRIMARY KEY,
    parent_id INTEGER,
    score INTEGER,
    FOREIGN KEY (parent_id) REFERENCES parent(id)
        MATCH SIMPLE
        ON UPDATE CASCADE
        ON DELETE SET NULL
);
```

Implemented match modes are `MATCH SIMPLE` and `MATCH FULL`. Referential actions are `NO ACTION`, `RESTRICT`, `CASCADE`, `SET NULL`, and `SET DEFAULT`. Column subsets are supported for `ON DELETE SET NULL` and `ON DELETE SET DEFAULT`. `MATCH PARTIAL` is not implemented.

When the referenced column list is omitted, as in `REFERENCES parent`, the referenced table's primary-key columns are inferred in declaration order. Column and table foreign-key declarations in `CREATE TABLE`, plus `ALTER TABLE ... ADD ... FOREIGN KEY`, resolve the referenced table through the active role's effective schema `USAGE` boundary once and store its canonical identity; a missing qualified schema reports `3F000`. A later non-null child-key write keeps that exact identity but checks the executing role's current `USAGE` on the referenced schema before reading the parent, matching PostgreSQL 18. Explicit or inferred referenced columns must form a primary-key or unique key, the referencing and referenced column counts must match, and each aligned type pair must support equality comparison. Mutations validate referential actions as part of the same transaction.

## Constraint lifecycle

PostgreSQL 18 named `CHECK`, foreign-key, and `NOT NULL` constraints support creation with `NOT VALID`, later validation, catalog inspection, alteration where PostgreSQL permits it, and removal:

```sql
ALTER TABLE child
    ADD CONSTRAINT score_positive CHECK (score > 0) NOT VALID,
    ADD CONSTRAINT child_parent_fk FOREIGN KEY (parent_id) REFERENCES parent(id) NOT VALID;

ALTER TABLE child VALIDATE CONSTRAINT score_positive;
ALTER TABLE child VALIDATE CONSTRAINT child_parent_fk;
ALTER TABLE child ALTER CONSTRAINT child_parent_fk NOT ENFORCED;
ALTER TABLE child ALTER CONSTRAINT child_parent_fk ENFORCED;
ALTER TABLE child ALTER CONSTRAINT child_parent_fk DEFERRABLE INITIALLY DEFERRED;
ALTER TABLE child DROP CONSTRAINT score_positive;
```

`NOT VALID` skips the existing-row scan while an enforced constraint still checks every new or changed row. `VALIDATE CONSTRAINT` scans existing rows and publishes `convalidated = true` only after the complete scan succeeds. Changing a foreign key from `NOT ENFORCED` to `ENFORCED` performs the same failure-atomic scan. PostgreSQL does not permit changing CHECK or named `NOT NULL` enforceability, and UQA Engine returns the corresponding error instead of approximating that operation.

A named `NOT NULL` constraint can be declared inline, in table-constraint form, or through `ALTER TABLE ... ADD CONSTRAINT name NOT NULL column [NOT VALID] [NO INHERIT]`. Its `pg_constraint` row uses `contype = 'n'`, its validation and inheritance flags survive reopen, and dropping it clears `pg_attribute.attnotnull` unless the column remains part of a primary key.

An `INITIALLY DEFERRED` foreign key is checked exactly once before temporary-table `ON COMMIT` actions at the outer transaction commit. The final transaction state may therefore insert the child before its parent or temporarily delete a referenced parent, and savepoint rollback removes pending checks introduced after that savepoint. `SET CONSTRAINTS { ALL | name [, ...] } { DEFERRED | IMMEDIATE }` changes implemented deferrable foreign-key modes for the current transaction; names may be schema-qualified, an unqualified name resolves every match in the first matching effective `search_path` schema including the configured position of `pg_temp`, an allocated temporary namespace remains resolvable after its last object is dropped unless its first allocation is rolled back, and `ALL` ignores non-deferrable constraints when selecting a new mode and remains the default for deferrable constraints created later in the transaction.

Changing a mode to `IMMEDIATE` checks pending row events retroactively in event order across nested transaction frames and leaves the prior mode in place if validation fails; `ALL IMMEDIATE` also fires events queued while a constraint was deferrable even if its catalog state later becomes non-deferrable. Each event remains bound to the durable identity of the exact originating constraint and follows its target row across a primary-key identity rewrite, so a same-name replacement in the current or another session cannot inherit its mode or capture its event. Partition-inherited events additionally retain their exact physical relation when unrelated constraint DDL reconciles transaction state.

Dropping a foreign key from a partitioned root removes every inherited clone, a direct drop on a clone reports `42P16`, and a root drop reports `55006` when a physical clone has a pending event. A dependency cascade caused by dropping the referenced key or table removes a child foreign key and its child-side pending events while retaining the child rows, matching PostgreSQL 18; an event fired by the DDL target itself still blocks the target rewrite.

Child-side events are created for inserts and updates that change at least one local foreign-key value, not child deletes or updates of unrelated columns, while a deferred parent-side `NO ACTION` key DELETE or UPDATE records its firing event even when no child row currently matches. Mode changes follow savepoint and nested-transaction rollback, dropping and recreating a constraint or disabling and re-enabling its foreign-key triggers restores the new triggers' initial mode unless an `ALL` mode applies, and table rename retains both the active mode and pending checks across sessions. Each pending trigger event also retains the relation that fired it.

PostgreSQL-blocked relation rewrites, including supported column and constraint changes, `DROP TABLE`, and `TRUNCATE`, report `55006` when that relation has pending events, while table, column, trigger, and rule renames remain allowed and retain the event identity; `DROP TABLE ... RESTRICT` traverses view, schema-expression, and foreign-key dependencies and reports an existing dependency with `2BP01` before examining pending events, whereas `CASCADE` reaches the pending-event check. A parent-side `NO ACTION` event does not by itself block child-only deferrability changes, and changing that child constraint to `NOT DEFERRABLE` does not discard the already queued event; dropping the foreign key also removes its referenced-parent trigger, so that operation checks the parent relation and reports `55006` while its event is pending.

A missing name reports `42704`, a missing explicitly named schema reports `3F000`, setting a named non-deferrable constraint to `DEFERRED` reports `42809` while named `IMMEDIATE` ignores it, and a different database qualifier reports `0A000`. A top-level use outside a transaction block emits a warning, still resolves the supplied names and reports any resolution or deferrability error, and has no lasting effect when resolution succeeds.

SQL routines, dynamic PL/pgSQL, and reentrant host callbacks share the surrounding transaction's mode state. In one externally submitted multi-statement simple-query string, statements share an implicit transaction until `COMMIT` or `ROLLBACK` closes it, including when the string began inside a pre-existing transaction and a later segment starts after that block closes; `BEGIN` promotes the implicit transaction without committing preceding work, and savepoint commands require a block made explicit by an earlier `BEGIN`, exactly as in PostgreSQL 18.

```sql
BEGIN;
SET CONSTRAINTS child_parent_fk DEFERRED;
INSERT INTO child (id, parent_id) VALUES (10, 500);
INSERT INTO parent (id) VALUES (500);
SET CONSTRAINTS child_parent_fk IMMEDIATE;
COMMIT;
```

Comma-separated `ALTER TABLE` actions execute in one transaction, so a later validation or duplicate-name failure rolls back every earlier action. Dropping a CHECK, foreign key, or named `NOT NULL` constraint removes only that owned constraint. Dropping a referenced primary-key or unique constraint uses PostgreSQL dependency behavior: `RESTRICT` reports dependent foreign keys and `CASCADE` removes those foreign keys without dropping their tables, including self-referencing foreign keys.

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
- Validate, alter, or drop a named constraint on the implemented lifecycle surface
- Drop a column and its owned constraints, with `CASCADE` removal of inbound foreign keys
- Rename a column
- Rename a table
- Set or drop a column default
- Set or drop a stored generation expression
- Set or drop `NOT NULL`
- Change a column type
- Transfer an ordinary table to another role with `OWNER TO`
- Add or remove an ordinary inheritance edge
- Attach or detach a partition, including the validated `CONCURRENTLY` boundary

Examples:

```sql
ALTER TABLE orders ADD COLUMN note TEXT;
ALTER TABLE orders ALTER COLUMN note SET DEFAULT '';
ALTER TABLE orders ALTER COLUMN state SET NOT NULL;
ALTER TABLE orders RENAME COLUMN note TO customer_note;
ALTER TABLE orders ALTER COLUMN total TYPE NUMERIC(20, 2);
ALTER TABLE generated_totals ALTER COLUMN line_total SET EXPRESSION AS (quantity * unit_price * 2);
ALTER TABLE orders OWNER TO app_owner;
```

Type changes evaluate an optional `USING` expression once for each old row, validate all rewritten rows, constraints, and generated dependencies, and publish the new schema and data atomically. Built-in ranges can be rewritten to their paired multirange with `USING multirange(column)` while retaining `WITHOUT OVERLAPS`; changing one side of an existing `PERIOD` relationship to an incompatible range identity is rejected with PostgreSQL 18 datatype-mismatch SQLSTATE `42804`. `DROP COLUMN CASCADE` removes inbound foreign keys before dropping the column; other dependency kinds that are not yet modeled for cascade still reject the operation atomically.

The role active at `CREATE TABLE` owns the ordinary table. ALTER requires the current role to be a superuser or to inherit the table owner, while DROP also permits the owning role of the containing schema. `ALTER TABLE name OWNER TO role` requires an existing target role, a SET-enabled path to it, and target-role `CREATE` on the containing schema unless the caller is a superuser. Owner transfer preserves relation and storage identities, rewrites owned serial and identity sequence ownership, updates table and index `pg_class.relowner` plus `pg_tables.tableowner`, blocks removal of dependent roles, and follows transaction, savepoint, temporary-table, cross-engine refresh, and durable-reopen lifecycle. Table ACLs and owner checks on the remaining relation-administration paths outside this standalone-index boundary are still open compatibility bugs.

## Relational B-tree indexes

The default access method is B-tree:

```sql
CREATE INDEX orders_state_idx ON orders (state);
CREATE UNIQUE INDEX orders_account_state_uq ON orders (account_id, state);
DROP INDEX orders_state_idx;
```

An index belongs to its table's schema and has no independent owner: creation requires inherited ownership of the table, and `pg_class.relowner` always follows that table's owner. `CREATE INDEX` first applies schema `USAGE` during table lookup, then checks table ownership, schema `CREATE`, and the index name and definition in PostgreSQL order. `DROP INDEX` permits inherited table-owner authority or ownership of the containing schema, validates every named index before deleting any, and does not let `IF EXISTS` bypass authorization for an existing index. The durable index identity stores its schema and local name as separate components. Distinct schemas may therefore contain indexes with the same local name, while an index cannot share one schema-local relation name with a table, view, materialized view, sequence, or foreign table. Unnamed indexes allocate PostgreSQL-style schema-local names such as `orders_state_idx` and then `orders_state_idx1`. `DROP INDEX` resolves one exact identity through the effective `search_path`, skips inaccessible unqualified schemas, and applies qualified schema `USAGE`, missing-schema, missing-index, wrong-relation-kind, and owner-error precedence before mutation. Index `regclass` values and the corresponding `pg_class`, `pg_index`, and `pg_indexes` rows use the same identity and remain stable across owner transfer, transaction rollback, catalog refresh, and durable reopen. Indexes on temporary tables stay session-local and are never written to the durable catalog.

Expression indexes are not implemented. Index columns must be table columns. `DROP INDEX CASCADE` syntax is accepted, but dependency-sensitive index cascade behavior remains incomplete.

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

An optional view column-name list renames query outputs positionally and may name only a leading subset. It cannot contain more names than the query returns, the final names must be unique, and quoted names retain their exact spelling. `CREATE OR REPLACE VIEW` must preserve the name and declared type of every existing column in order, but it may append columns at the end. Creation analyzes the query without executing it, expands projection stars against the creation-time source row types, then validates the column list and target relation; the durable definition retains the fixed public names and row width across nested views, later base-column additions, transactions, and reopen. `TEMP` and `TEMPORARY` views are session-local, and a view over a temporary table or view becomes temporary as in PostgreSQL. The active creating role owns a view; inherited owner authority protects replacement and ALTER, while direct DROP also permits the owner of the containing schema. `ALTER VIEW ... OWNER TO` requires an existing SET-accessible target role with `CREATE` on the containing schema unless the caller is a superuser. Ownership blocks dependent role removal, is shown by `pg_class.relowner` and `pg_views.viewowner`, and follows transaction, savepoint, temporary, cross-engine refresh, durable-reopen, and stable-OID lifecycle. View options `security_barrier`, `security_invoker`, and `check_option`, including `WITH [LOCAL | CASCADED] CHECK OPTION`, are validated, retained in `pg_class.reloptions`, and may be changed with owner-authorized `ALTER VIEW ... SET/RESET`.

Regular views support PostgreSQL table-shaped relation and column `GRANT` and `REVOKE`, including all eight relation privileges, column `SELECT`, `INSERT`, `UPDATE`, and `REFERENCES`, `PUBLIC`, independent rooted grant-option paths, dependent `RESTRICT` and `CASCADE`, implicit owner rights, role dependencies, owner-transfer grantor rewriting, `ALL TABLES IN SCHEMA`, and preservation through replacement, transactions, savepoints, temporary lifetime, cross-engine refresh, and reopen. `pg_class.relacl` and `pg_attribute.attacl` expose the durable ACLs, the name and OID forms of `has_table_privilege` and `has_column_privilege` accept views, and `information_schema.tables`, `views`, `columns`, `column_privileges`, and `role_column_grants` apply PostgreSQL visibility rules while excluding materialized views. A caller must hold the requested privilege on every regular-view boundary; the underlying query or automatically rewritten DML uses each view owner's privileges unless that view has `security_invoker=true`, in which case it retains the invoking privilege subject. SQL-visible `current_user` remains the caller while a regular view executes.

A single-source projection view is automatically updatable when its underlying table or view is automatically updatable and its target list does not contain a set-returning expression. `INSERT` values and queries, `ON CONFLICT`, `UPDATE`, `UPDATE FROM`, `DELETE`, `DELETE USING`, and `MERGE` write the base relation through its ordinary defaults, constraints, row and statement triggers, partition routing, and `RETURNING` path; an implicit INSERT may supply only the leading view columns and lets omitted writable columns take their base defaults. The public view row type is the DML name boundary, so unprojected base columns report `42703` even when the view shape itself is non-updatable; unqualified names owned only by a `FROM` or `USING` source remain source columns instead of colliding with hidden base columns, and ordinary source relations named `old` or `new` remain source relations outside explicit `RETURNING` row-image aliases. Computed and system-column projections, including `tableoid`, remain readable in predicates and `RETURNING` but are not writable; partitioned DML reports the physical leaf relation for current, `OLD`, `NEW`, and rule row images, including a partition-moving update. A view with no writable columns rejects INSERT and UPDATE with `55000` while remaining automatically deletable. Normal PostgreSQL ambiguity checks apply between the target, `excluded`, and `FROM` or `USING` sources. Correlated scalar subqueries retain that complete containing DML namespace, including explicit `OLD` and `NEW` aliases in `RETURNING`. A bare `RETURNING *` from `UPDATE FROM` or `DELETE USING` emits view-target columns before source columns, while `MERGE` emits source columns before public view-target columns. Rewriting follows nested view aliases and predicates, permits a row to leave a view without a check option, and enforces nested `LOCAL` and `CASCADED` check options from the innermost view outward against the final row after `BEFORE` row triggers with statement atomicity, including `UPDATE FROM` and `MERGE`; a check option on a directly non-updatable view is rejected with `0A000`. `ALSO` and `INSTEAD` rules on every automatically rewritten view layer run in PostgreSQL rewrite order and may provide `RETURNING` for `INSERT`, `UPDATE`, and `DELETE`; `MERGE` rejects a user rewrite rule on any targeted view layer with `0A000`. An unconditional `INSTEAD` rule suppresses the original view mutation and its row and statement triggers, evaluates only input or assignment expressions required by matching rule conditions and actions, accepts supplied computed view columns when an action consumes them, and reports the affected-row count of the final executed action; a conditional `INSTEAD` rule without an unconditional `INSTEAD` rule does not make the view updatable. DML through an outer view may terminate at a nonautomatically updatable underlying view when that view has an applicable unconditional rule; outer view rule layers remain ordered around that boundary as in PostgreSQL. For `INSERT`, `UPDATE`, and `DELETE`, a defined `INSTEAD OF` row trigger selects the trigger path independently of whether `session_replication_role` suppresses that trigger; a suppressed trigger performs no automatic base write. `information_schema.views`, `information_schema.tables`, and `information_schema.columns` expose automatic, trigger, and per-column updatability. Remaining PostgreSQL updatability shapes involving CTE-backed definitions or other unverified query forms and complete optimizer effects for `security_barrier` remain open compatibility bugs.

For `MERGE`, every named mutation action must select one complete automatic or `INSTEAD OF` trigger path; PostgreSQL rejects a statement that mixes those paths, and a nonautomatically updatable view needs an action-specific trigger for each mutation kind. Automatic outer-view rewriting may stop at a trigger-updatable inner view. The final row returned by the trigger is then mapped back through the outer public row types and validated by their `LOCAL` or `CASCADED` check options. `DO NOTHING` requires no trigger, and a defined but replication-suppressed trigger path performs no fallback base write.

`CREATE MATERIALIZED VIEW` stores a query snapshot, supports `WITH [NO] DATA`, persists its rows and static schema across reopen, and remains stale until `REFRESH MATERIALIZED VIEW [WITH [NO] DATA]`; direct `INSERT`, `UPDATE`, and `DELETE` first enforce the requested materialized-view privilege and then report PostgreSQL's relation-kind SQLSTATE `42809`. The active creating role owns the materialized view; inherited owner authority protects ALTER, direct DROP also permits the owner of the containing schema, and `ALTER MATERIALIZED VIEW ... OWNER TO` applies the same target-role rules and durable lifecycle as regular views. Materialized views support the same durable relation and column ACL machinery as regular views; `SELECT` controls snapshot reads independently of source privileges, and delegated `MAINTAIN` permits refresh without `SELECT` on either the snapshot or its sources. Refresh evaluates the stored query with the owner's privileges and `current_user`, then restores the caller's session identity. `pg_class` exposes relation kind `m`, owner, ACL, population state, persistence, and reloptions, while `pg_attribute`, `pg_matviews`, `has_table_privilege`, and `has_column_privilege` expose the corresponding implemented metadata. The supported materialized-view option is `fillfactor`, including owner-authorized `ALTER MATERIALIZED VIEW ... SET/RESET`; temporary and unlogged materialized views, concurrent refresh, materialized-view indexes, access methods, tablespaces, and dependencies on temporary relations are not implemented.

## CREATE TABLE AS

```sql
CREATE TABLE pending_orders (order_id, account_id, total) AS
SELECT order_id, account_id, total
FROM orders
WHERE state = 'pending';
```

CTAS creates and populates a table from a query, preserves the query's declared output types for implemented SQL types, and creates nullable columns without copying source constraints. An optional column-name list replaces output names positionally and may be shorter than the query output, in which case remaining names come from the query; quoted case is preserved. More names than output columns raise `42601`, while duplicate names and PostgreSQL system-column names raise `42701`, before the query is executed. `WITH NO DATA` creates and durably persists the same typed schema, including vector and tensor field metadata, without evaluating row-producing expressions or volatile functions; relation, column, function, type-input, and column-name-list analysis still occurs in PostgreSQL order. CTAS supports ordinary, temporary, and unlogged targets, and temporary targets support all three `ON COMMIT` actions. Top-level `SELECT ... INTO [TEMPORARY | TEMP | UNLOGGED] [TABLE] name` creates the same corresponding table and executes the query; PL/pgSQL `SELECT ... INTO` remains variable assignment. Storage options, access methods, and tablespaces are not implemented.

## Sequences

```sql
CREATE TABLE tickets (ticket_id integer);
CREATE SEQUENCE ticket_ids AS integer START WITH 1000 INCREMENT BY 1 MINVALUE 1000 MAXVALUE 999999 CACHE 64 CYCLE OWNED BY tickets.ticket_id;
SELECT nextval('ticket_ids');
SELECT currval('ticket_ids');
SELECT lastval();
SELECT setval('ticket_ids', 2000);
SELECT setval('ticket_ids', 2500, false);
SELECT pg_get_serial_sequence('tickets', 'ticket_id');
GRANT USAGE, SELECT ON SEQUENCE ticket_ids TO app_reader;
GRANT UPDATE ON TABLE ticket_ids TO app_writer;
SELECT has_sequence_privilege('app_reader', 'ticket_ids', 'USAGE');
ALTER SEQUENCE ticket_ids MAXVALUE 2000000 CACHE 128 NO CYCLE RESTART WITH 3000;
ALTER SEQUENCE ticket_ids OWNED BY NONE;
ALTER SEQUENCE ticket_ids SET UNLOGGED;
ALTER TABLE ticket_ids SET LOGGED;
ALTER SEQUENCE ticket_ids RENAME TO archived_ticket_ids;
ALTER SEQUENCE archived_ticket_ids SET SCHEMA archive;
```

`CREATE SEQUENCE` and `ALTER SEQUENCE` support `AS smallint`, `AS integer`, and `AS bigint`; positive or negative nonzero increments; `START [ WITH ]`; `RESTART [ WITH ]`; `MINVALUE`, `MAXVALUE`, `NO MINVALUE`, and `NO MAXVALUE`; positive `CACHE` sizes; `CYCLE` or `NO CYCLE`; and `OWNED BY table.column` or `OWNED BY NONE` for ordinary, temporary, and unlogged sequences. `ALTER SEQUENCE name SET LOGGED|UNLOGGED` changes a nontemporary sequence's persistence, and the historical `ALTER TABLE name SET LOGGED|UNLOGGED` spelling has the same behavior for a sequence target. `ALTER SEQUENCE name RENAME TO new_name` and `ALTER SEQUENCE name SET SCHEMA schema_name`, plus their historical `ALTER TABLE` spellings for sequence targets, change the catalog name without changing the sequence object identity or definition. Type and direction determine PostgreSQL's default start and bounds, explicit bounds are validated against the declared type, a noncycling sequence reports `2200H` without advancing past a bound, and a cycling sequence wraps directly to the opposite bound. A cache reservation stops at the configured bound instead of wrapping within the same block; the next reservation wraps when cycling is enabled. Temporary sequences live in `pg_temp`, participate in `DISCARD TEMP`, do not survive a reopen, allow rename, reject logged-state changes with `42P16`, and reject schema moves with `0A000`; unlogged sequence state and persistence survive a clean reopen, while crash-recovery reset semantics remain open. The two-argument `setval` marks the installed value as called, while the three-argument form accepts `false` to make the next `nextval` return the installed value exactly.

```sql execute
CREATE SCHEMA sequence_archive;
CREATE SEQUENCE sequence_lifecycle_ids CACHE 3;
ALTER SEQUENCE sequence_lifecycle_ids OWNER TO CURRENT_USER;
SELECT nextval('sequence_lifecycle_ids');
ALTER SEQUENCE sequence_lifecycle_ids RENAME TO renamed_sequence_lifecycle_ids;
ALTER SEQUENCE renamed_sequence_lifecycle_ids SET SCHEMA sequence_archive;
SELECT nextval('sequence_archive.renamed_sequence_lifecycle_ids');
```

Rename and schema-move operations preserve the sequence's `pg_class.oid`, numeric and literal `regclass` bindings, reserved cache block, `currval`, and `lastval` in the current session and in sessions that observe the new name later. They rewrite implemented column-default, serial and identity, and stored-view dependencies to the new qualified name, remain durable across reopen, and follow transaction and savepoint rollback without reclaiming values already returned by `nextval`. A serial or identity sequence may be renamed and `pg_get_serial_sequence` follows it, but moving an owned sequence to another schema reports `0A000`. Moving a sequence to its current schema is a no-op. A missing sequence reports `42P01`, another relation kind reports `42809`, a target-name collision reports `42P07`, a missing target schema reports `3F000`, and a read-only transaction reports `25006` before target lookup; `IF EXISTS` converts only a missing source into a notice.

The role active at `CREATE SEQUENCE` owns the sequence. Sequence definition, persistence, name, namespace, and drop operations require the current user to be a superuser or to inherit the owning role. `ALTER SEQUENCE name OWNER TO role` and its historical `ALTER TABLE` spelling require that authority, an existing target role, and a SET-enabled path to the target role; an independently owned serial or identity sequence rejects role transfer with `0A000`. A transfer changes `pg_class.relowner` and `pg_sequences.sequenceowner` without changing the stable relation OID, definition generation, value or session cache, and it follows transaction, savepoint, temporary-object, rename, and durable-reopen lifecycle. SQL role owners prevent `DROP ROLE` until the sequence is reassigned or removed.

Sequence ACLs support `USAGE`, `SELECT`, and `UPDATE`, `ALL [PRIVILEGES]`, `PUBLIC`, `WITH GRANT OPTION`, `GRANT OPTION FOR`, `GRANTED BY`, `RESTRICT`, and `CASCADE` through `GRANT` and `REVOKE`. Explicit `ON SEQUENCE` targets, the historical `ON TABLE sequence_name` spelling, and `ON ALL SEQUENCES IN SCHEMA` are supported. Owners retain implicit grant options even after revoking their own ordinary privileges; nonowners may delegate through direct or inherited rooted grant-option paths; alternate paths survive a cascading revoke; and ACL grantors or grantees prevent `DROP ROLE`. Owner transfer rewrites owner-issued paths, and explicit ACLs appear in `pg_class.relacl` with PostgreSQL's `r`, `w`, and `U` codes. ACL changes follow statement, transaction, savepoint, temporary-object, rename, cross-engine refresh, and durable-reopen lifecycle.

`nextval` requires `USAGE` or `UPDATE`, `currval` and `lastval` require `USAGE` or `SELECT`, and `setval` requires `UPDATE`. The six current-user or explicit-role name/OID `has_sequence_privilege` overloads accept comma-separated privilege checks, including `WITH GRANT OPTION`; a missing sequence name reports `42P01`, a missing sequence OID returns `NULL`, and another relation kind reports `42809`. A durable `CREATE SEQUENCE` requires `CREATE` on its target schema but not `USAGE`; definition errors precede that check, while the check precedes collision and `IF NOT EXISTS` handling. Name-based value functions, direct scans, ALTER, DROP, sequence grants, and privilege inquiry require schema `USAGE` before sequence privileges or relation lookup; qualified inaccessible names report `42501`, while unqualified lookup skips inaccessible search-path schemas. `OWNER TO` additionally requires the new owner to have `CREATE` on the current schema, and `SET SCHEMA` requires the acting owner to have `CREATE` on the target schema without requiring target `USAGE`. Read-only rejection and target, role, schema, relation-kind, and invalid-privilege resolution follow the tested PostgreSQL 18 precedence.

A sequence is directly selectable as a one-row relation with the PostgreSQL columns `last_value bigint`, `log_cnt bigint`, and `is_called boolean`; direct scans require `SELECT`. `pg_class` exposes the sequence as relkind `S` with three attributes and no row type, while `pg_attribute` exposes those three non-null, positive-numbered physical columns in PostgreSQL order. The physical value and log counter follow bounded cache reservations, survive durable reopen, and reset on `setval`, `RESTART`, or an allocation-affecting definition change. `pg_sequence_parameters(oid)` returns the configured start, bounds, increment, cycle flag, cache size, and data-type OID to a role with any sequence privilege; `pg_get_sequence_data(regclass)` returns the physical value and called state only with `SELECT` and otherwise returns a null record; `pg_sequence_last_value(regclass)` and `pg_sequences.last_value` return the physical value only after the sequence is called and only with `SELECT` or `USAGE`. These functions expose their PostgreSQL 18 OIDs, argument modes and names, strictness, volatility, and parallel-safety through `pg_proc`.

`OWNED BY` requires the sequence and its ordinary, inherited, or partitioned owner table to be in the same schema. It records an automatic dependency without creating or changing the column default, survives table and column renames through stable object identities, appears through `pg_get_serial_sequence(text, text)`, and causes an owner-column or owner-table drop to remove the sequence. If another default or view depends on that sequence, an owner drop with `RESTRICT` reports `2BP01`, while `CASCADE` removes the dependent default and complete view closure. Multiple sequences may own one column. `TRUNCATE ... CONTINUE IDENTITY` preserves their values, while `TRUNCATE ... RESTART IDENTITY` restarts them. `OWNED BY NONE` detaches the dependency; assigning a new owner moves it. These changes follow statement, transaction, savepoint, and durable-reopen semantics.

`SERIAL` columns use the same automatic dependency, so their sequence may be reassigned or detached and may be dropped directly subject to ordinary default-expression dependencies. Identity columns use an internal dependency: their generated sequence cannot be reassigned with `ALTER SEQUENCE ... OWNED BY` or dropped directly even with `CASCADE`, and dropping the identity column removes it. Both forms choose their implicit sequence name with PostgreSQL's schema-local `table_column_seq` rule: table and column components are balanced and clipped at UTF-8 boundaries to fit the 63-byte identifier limit, and a collision with any existing relation retries with `seq1`, `seq2`, and later numeric labels.

`smallserial` and `serial2` backing sequences use `smallint`, `serial` and `serial4` use `integer`, and `bigserial` and `serial8` use `bigint`; identity backing sequences use their declared integer column type. `information_schema.sequences` reports the declared type and numeric precision plus exact start, minimum, maximum, increment, and cycle state, excludes internally owned identity sequences, and exposes a row only when the current user has schema `USAGE` and either inherits the owner or holds `SELECT`, `UPDATE`, or `USAGE` on the sequence. Both it and `pg_sequences` hide temporary sequences owned by other sessions. These types, dependencies, metadata, and visibility survive durable reopen.

Sequence definition changes are transactional. A parameter-changing `ALTER SEQUENCE`, including a same-value `CACHE` change, invalidates outstanding blocks in every session. An actual logged-state change does the same, while `SET LOGGED` on an already logged sequence or `SET UNLOGGED` on an already unlogged sequence preserves every session's block. An allocation made after an uncommitted `ALTER SEQUENCE` or `RESTART` follows that definition's transaction or savepoint ownership, while an earlier reservation against the retained definition remains nontransactional. Rolling back an unused logged-state change leaves that earlier block usable; once the changed definition allocates a value, rollback discards its block and resumes from the retained definition without reviving an abandoned block. The affected session `currval` and `lastval` still retain the most recently returned value across rollback, matching PostgreSQL 18.

`DROP SEQUENCE [ IF EXISTS ] name [, ...] [ CASCADE | RESTRICT ]` resolves relation names through the current `search_path`, validates every target before mutation, ignores duplicate targets, and uses `RESTRICT` by default. A missing target reports `42P01` unless `IF EXISTS` requests a notice and continuation, a target of another relation kind reports `42809` even with `IF EXISTS`, a dependency rejected by `RESTRICT` reports `2BP01`, and a read-only transaction reports `25006` before target lookup.

`CASCADE` removes referencing column defaults, column- and table-level `CHECK` constraints, and the complete closure of dependent views while retaining the underlying tables; the same expression-granular behavior applies to foreign-table defaults and `CHECK` constraints. Dropping a serial sequence with `CASCADE` removes its column default and serial ownership metadata; if the serial default was replaced first, an ordinary drop succeeds and preserves the replacement expression. Sequence rename rewrites these stored schema expressions to the new exact relation identity, and recreating the old name cannot retarget them. Sequence drops are transactional: transaction and savepoint rollback restore the catalog object and its session-local `currval` and `lastval` identity, while a committed drop remains absent after reopen and does not transfer session values to a same-named replacement.

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

The engine stores one canonical SQL schema for each foreign table and projects only names and physical types when calling an FDW handler. `NOT NULL`, column defaults, column- and table-level `CHECK` constraints, stored generated columns, `SERIAL`, and identity columns therefore remain visible through `information_schema.columns`, `pg_attribute`, `pg_attrdef`, and `pg_constraint` and survive transactions, catalog refresh, and durable reopen. Routine and sequence references in those expressions bind at creation, follow exact-object rename, and participate in `DROP ... RESTRICT` and expression-granular `CASCADE`; dropping a routine-dependent generated column follows PostgreSQL and retains the foreign table. Foreign-table `SERIAL` and identity declarations create automatic and internal owned-sequence dependencies respectively, work with `pg_get_serial_sequence`, move sequence ownership with the foreign-table owner, and drop their sequences with the owning foreign table while preserving PostgreSQL's external-dependency `RESTRICT` and `CASCADE` behavior. Legacy column-array catalog rows, including rows whose generated sequences were not materialized, are upgraded only during the initial open, while ordinary reload validates the versioned schema without repairing it. Primary-key, unique, and foreign-key constraints are rejected with `0A000`, matching PostgreSQL's foreign-table constraint boundary.

The role active at `CREATE FOREIGN TABLE` owns the foreign table and its implicit `SERIAL` and identity sequences. `ALTER FOREIGN TABLE name OWNER TO role` and the historical `ALTER TABLE name OWNER TO role` spelling require inherited owner authority, a SET-enabled path to the target role, and `CREATE` for that role on the containing schema; superusers bypass the latter two privilege restrictions, and the target does not need `USAGE` on the foreign server. Owner transfer moves every owned sequence in the same transaction. The owner or containing-schema owner may use `DROP FOREIGN TABLE`, `RESTRICT` rejects dependent views and external dependents of owned sequences, and `CASCADE` removes their complete closure. Ownership appears in `pg_class.relowner`, blocks `DROP ROLE`, and follows transaction, savepoint, cross-engine refresh, explicit catalog migration, corruption rejection, and durable-reopen lifecycle.

Foreign tables use the same nullable relation ACL and per-column ACL model as other table-shaped relations. `GRANT` and `REVOKE ... ON TABLE` support all eight relation privileges and column `SELECT`, `INSERT`, `UPDATE`, and `REFERENCES`, including `PUBLIC`, independent rooted grant-option paths, dependent `RESTRICT` and `CASCADE`, implicit owner rights, `ALL TABLES IN SCHEMA`, owner-transfer grantor rewriting, and role dependencies. SQL scans enforce table or exact-column `SELECT` across direct, joined, stored definer-view, and `security_invoker` paths; the built-in foreign wrappers expose a read-only scan interface, so `information_schema.tables.is_insertable_into` and foreign columns' `is_updatable` are `NO`. `pg_class.relacl`, `pg_attribute.attacl`, all name/OID `has_table_privilege` and `has_column_privilege` forms, `information_schema.tables`, `columns`, `column_privileges`, and `role_column_grants` expose the same durable state through transactions, savepoints, cross-engine refresh, migration, corruption validation, and reopen.

Foreign tables accept ordinary `BEFORE` and `AFTER` row and statement trigger definitions for `INSERT`, `UPDATE`, `DELETE`, and `TRUNCATE`, including `UPDATE OF` and `WHEN`; constraint triggers, transition relations, and `INSTEAD OF` timing are rejected with PostgreSQL's foreign-table diagnostic. Creation enforces the foreign table's `TRIGGER` privilege and the function's `EXECUTE` privilege. The foreign table owner controls trigger rename and `ALTER FOREIGN TABLE` or historical `ALTER TABLE` enable modes, while `DROP TRIGGER` derives authority from the same live owner. `pg_trigger`, `pg_class.relhastriggers`, function dependencies, owner transfer, rollback, cross-engine refresh, durable reopen, and automatic trigger removal with `DROP FOREIGN TABLE` use the durable trigger catalog. The built-in foreign wrappers remain read-only, so writable foreign-table DML and trigger execution remain compatibility work.

## TRUNCATE and DROP

```sql
TRUNCATE TABLE staging_a, staging_b;
DROP TABLE IF EXISTS staging_a;
```

`TRUNCATE` removes all rows from its listed tables under a transaction boundary. SQL drop supports implemented table, foreign-table, index, view, materialized-view, sequence, schema, function, and procedure targets. `DROP TABLE ... CASCADE` and `DROP FOREIGN TABLE ... CASCADE` remove the complete transitive closure of dependent views without requiring separate ownership of those dependent objects, and relation-kind mismatches return PostgreSQL-compatible errors. Unsupported dependency-sensitive `CASCADE` forms fail rather than partially changing the catalog.
