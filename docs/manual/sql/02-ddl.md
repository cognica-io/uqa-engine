# Data Definition Language

DDL changes the durable catalog and participates in engine transaction boundaries. Use explicit transactions when multiple catalog and data changes form one deployment invariant.

Tables, indexes, sequences, views, materialized views and foreign tables share schema-local relation names. Creation and rename reserve their destination name until transaction completion or rollback of the savepoint that acquired the reservation. A competing operation whose initial lookup found that name absent waits for the reservation: a committed competing definition produces catalog uniqueness SQLSTATE `23505`, while rollback lets the waiting operation continue. This also applies to names allocated for constraint and partition indexes. An existing name found before reservation follows the statement's ordinary duplicate, `IF NOT EXISTS` or replacement rules. Catalog names refresh after a wait while REPEATABLE READ and SERIALIZABLE retain their ordinary data snapshot. Temporary names remain local to each session.

CHECK, NOT NULL, foreign-key, key and constraint-trigger names share a namespace within their owning table. Additions and constraint renames reserve a destination through transaction and savepoint completion, including key renames requested with `ALTER INDEX`. A competing commit produces `23505`; rollback lets the waiter continue. Pre-existing constraint names produce the statement's normal duplicate diagnostic (`42710` for ordinary additions and renames, `23505` when creating a constraint trigger with another constraint's name). Automatic table, key, domain and foreign-table constraint names skip visible constraints throughout their schema, including constraint triggers and domain constraints. Explicit names remain local to their owning table or domain, and the same explicit name can be used by another owner. Key constraints also avoid occupied index relation names. Generated constraint and index names reserve their label and numeric collision suffix within PostgreSQL's 63-byte identifier limit, balancing the table/domain and column components before clipping at UTF-8 boundaries. Unnamed indexes retain quoted identifier spelling, including case, spaces and punctuation. Recursive ALTER chooses a generated CHECK or NOT NULL name at the parent and propagates it; a child that directly declares NOT NULL keeps its own local name. A DDL command that waits retains another session's committed index renames instead of restoring names copied before the wait. New key constraints reserve their index relation name before their constraint name. `CREATE TABLE` and `ADD COLUMN` preserve every CHECK on a column, with each constraint's name, enforcement and inheritance attributes. `ADD COLUMN` retains explicit PRIMARY KEY and UNIQUE names and their NULL semantics; On an ordinary table, `IF NOT EXISTS` skips the whole declaration when the column already exists.

## Database privileges

```sql
GRANT CONNECT, TEMPORARY ON DATABASE uqa TO app_reader;
GRANT CREATE ON DATABASE uqa TO app_writer WITH GRANT OPTION;
SELECT has_database_privilege('app_reader', 'uqa', 'CONNECT');
REVOKE GRANT OPTION FOR CREATE ON DATABASE uqa FROM app_writer CASCADE;
```

The embedded engine exposes one current database named `uqa`, owned by the bootstrap `uqa` role. Its default ACL grants `CONNECT` and `TEMPORARY` to `PUBLIC`, while the owner retains implicit `CONNECT`, `CREATE`, and `TEMPORARY` grant options even after self-revocation. Database ACLs support `ALL [PRIVILEGES]`, `PUBLIC`, `TEMP` as an alias for `TEMPORARY`, `WITH GRANT OPTION`, `GRANT OPTION FOR`, `GRANTED BY`, `RESTRICT`, and `CASCADE`; independent rooted grantor paths, role dependencies, transactions, savepoints, cross-engine refresh, and durable reopen are preserved. `CREATE SCHEMA` enforces the database `CREATE` privilege, and temporary tables, sequences, views, CTAS targets, `SELECT INTO` targets, indexes, and indexed key constraints enforce `TEMPORARY`; inherited grants, revocation after temporary-namespace allocation, transactions, savepoints, and PostgreSQL source-analysis and definition-error precedence are preserved. `pg_database.datdba` and `datacl` expose the owner and ACL. The six current-user or explicit-role name/OID `has_database_privilege` overloads accept comma-separated checks, preserve inherited ownership, strict NULLs, missing-name and missing-OID distinctions, exact error precedence, and PostgreSQL 18 `pg_proc` identities. Creating, altering, dropping, or transferring ownership of databases and enforcing `CONNECT` at the embedding connection boundary remain compatibility bugs.

Creating a table, view, materialized view, foreign table, sequence, schema, function or procedure retains its owner role through the transaction. A concurrent DROP ROLE waits for uncommitted creation, then reports `2BP01` if a committed object still depends on the role. Creation waiting for a deleted or replaced owner reports `42704` and does not adopt a new role with the same name. Temporary table, view and sequence ownership and ACL references also prevent role deletion from another session after commit; dropping the object, removing the reference or ending its owning session releases that dependency. Rollback and savepoint undo preserve the corresponding earlier state. `IF NOT EXISTS` targets already present during preflight and `OR REPLACE` that preserves ownership add no dependency on the invoking role.

## Schemas

```sql execute
CREATE ROLE schema_owner_example;
CREATE SCHEMA reports_example AUTHORIZATION schema_owner_example;
SELECT schema_owner FROM information_schema.schemata WHERE schema_name = 'reports_example';
```

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

`CREATE SCHEMA [IF NOT EXISTS] [name] [AUTHORIZATION role]` creates a schema owned by the named role, `CURRENT_ROLE`, `CURRENT_USER`, or `SESSION_USER`; without `AUTHORIZATION`, the current role owns it. An omitted schema name uses the resolved owner name. Quoted role names remain literal even when they spell a session-role keyword. The invoking role needs database `CREATE` and permission to `SET ROLE` to the owner; the target owner does not itself need database `CREATE`. Role existence is checked before database privileges, SET ROLE authority, reserved `pg_` names and duplicate schemas. `IF NOT EXISTS` still checks authorization and emits a notice without changing the existing owner. `ALTER SCHEMA OWNER` likewise checks database `CREATE` on the invoking owner. These changes preserve the current session role, participate in transactions and savepoints, and persist across reopen. Schema ACLs support `USAGE`, `CREATE`, `ALL [PRIVILEGES]`, `PUBLIC`, `WITH GRANT OPTION`, `GRANT OPTION FOR`, `GRANTED BY`, `RESTRICT`, and `CASCADE` through `GRANT` and `REVOKE` on explicit schema targets. Inherited ownership and grant-option paths are honored, dependent grants follow `RESTRICT` or `CASCADE`, schema owners and ACL grantors or grantees prevent `DROP ROLE`, and changes follow transaction, savepoint, cross-engine refresh, and durable-reopen lifecycle. `pg_namespace.nspowner` and `nspacl` expose the durable owner and ACL, while `information_schema.schemata` exposes schemas on which the current role has `USAGE` or `CREATE`. The six current-user or explicit-role name/OID `has_schema_privilege` overloads accept comma-separated `USAGE` and `CREATE` checks, including `WITH GRANT OPTION`, and preserve PostgreSQL 18 owner, inherited-role, system-schema, current temporary-schema, strict-NULL, missing-object, error-precedence, and `pg_proc` behavior. A numeric text argument remains a schema name, and `pg_temp` is not accepted as an alias by this inquiry function; use the allocated temporary namespace name or OID. Schema `CREATE` is enforced for durable tables, CTAS, `SELECT INTO`, views, materialized views, sequences, foreign tables, functions, procedures, standalone indexes, and indexed key constraints, including inherited grants, immediate revocation, transactions, savepoints, qualified versus search-path selection, inferred temporary views, and PostgreSQL source-analysis, definition-error, and collision precedence; unqualified creation and sibling-object creation through indexes or indexed constraints also require schema `USAGE`. Routine calls and ALTER, DROP, GRANT, and REVOKE routine-target lookup enforce schema `USAGE`. Ordinary relation queries, `INSERT`, `UPDATE`, `DELETE`, `TRUNCATE`, `ALTER TABLE`, `DROP TABLE`, trigger and rule definition or removal targets, constraint-trigger referenced relations, rule-action mutation targets, `INHERITS`, `PARTITION OF`, `ATTACH PARTITION`, ALTER-time inheritance and foreign-key targets, stored-view source binding, and hard or soft `regclass` input use the same rule: qualified inaccessible schemas report `42501` before object existence, authority, or column validation, while unqualified lookup skips inaccessible search-path entries. Inherited grants and immediate prepared-plan revocation are honored; views, materialized views, SQL-standard query bodies, declared cursors, and stored rule-action mutation targets retain exact definition-time relation identities instead of repeating namespace-name checks. System-catalog projection likewise follows canonical catalog relationships without applying the caller's namespace lookup to those stored identities. Remaining object paths, remaining relation-owner checks, and default privileges remain open compatibility bugs. Schema elements embedded inside `CREATE SCHEMA` remain unimplemented. Cross-database names are rejected. `DROP SCHEMA` requires ownership of the schema and, by default, an empty schema. See the deletion contract below.

Durable schemas have stored namespace OIDs and separate incarnation and tuple-replacement identities. ACL and owner changes preserve the namespace OID; deletion followed by recreation allocates a new identity even when the name is reused. `pg_namespace`, referencing catalog rows, `regnamespace`, routine-name output and OID-based privilege inquiry use the same captured namespace identity. GRANT/REVOKE retain the schema lifetime and serialize tuple replacement, including unchanged grants and empty revokes. A committed competing replacement produces `XX000` (`tuple concurrently updated`); full or savepoint rollback lets the waiter proceed. Owner transfer retains its originally selected tuple and reports `tuple concurrently deleted` if deletion or recreation wins. GRANT lookup instead retries after a lifetime wait and can bind a replacement namespace, or reports `3F000` when it disappeared. Competing same-name schema creation waits for the name reservation; a committed creator produces `23505`, including with `IF NOT EXISTS`, while an already visible schema follows the ordinary existing-schema rule.

Creating a persistent table, view, materialized view, sequence or foreign table retains the selected schema lifetime until transaction completion or rollback of the acquiring savepoint. `DROP SCHEMA` waits, then checks newly committed dependents for `RESTRICT` or `CASCADE`. Creation rechecks the requested name, search path and `CREATE` privilege after waiting for schema deletion; qualified missing targets report `3F000`, and a recreated schema requires its own current authority. Ordinary `CREATE TABLE IF NOT EXISTS` retains the schema when it skips, while skipped `CREATE TABLE AS` and `CREATE MATERIALIZED VIEW` do not. Routine and domain declarations retain their separate catalog publication behavior.

Every named graph reserves a namespace of its own name and `ag_catalog` is reserved for the Apache AGE catalog, so `CREATE SCHEMA` rejects those names as existing or reserved schemas and `DROP SCHEMA graph_name` fails until the graph is dropped; see [Graph SQL and Cypher](07-graph.md).

Existing `information_schema` and `ag_catalog` namespaces follow duplicate-schema checks: ordinary CREATE returns `42P06`, and `IF NOT EXISTS` skips them with a notice after authorization. Names beginning with `pg_` still report `42939` before duplicate handling.

`ALTER SCHEMA name OWNER TO { new_owner | CURRENT_ROLE | CURRENT_USER | SESSION_USER }` transfers schema ownership and rewrites owner-rooted ACL entries while retaining other grants. Unless the requested owner is already current, the caller must own the schema and be able to SET ROLE to the new owner; the invoking owner must have database CREATE privilege. A superuser bypasses these privilege checks. A missing role reports `42704`, a missing schema reports `3F000`, and insufficient authority reports `42501`. It returns `ALTER SCHEMA` without rows and participates in transaction rollback, refresh, and reopen. An unchanged owner skips ownership, SET and database CREATE checks. Existing system-schema ownership overrides also survive refresh, rollback and reopen.

`ALTER SCHEMA name RENAME TO new_name` renames a schema as `RenameSchema` does: the namespace keeps its OID, owner, privileges and dependents, so a stored `regnamespace` or `regclass` constant, `pg_get_viewdef`, `pg_get_expr` and the `reg*` output print the new name, and every table, view, materialized view, sequence, foreign table, index, function, procedure, enum, domain and composite type the schema holds is reachable under it while the old name no longer resolves (a `search_path` entry naming it is skipped). The schema must exist (`3F000`), the new name must not be taken by a schema, a graph namespace or the schema itself (`42P06`), the current user must own the schema (`42501` `must be owner of schema name`) and hold `CREATE` on the database, and the new name may not start with `pg_` (`42939` with the DETAIL `The prefix "pg_" is reserved for system schemas.`). `public` can be renamed; the virtual `pg_catalog`, `information_schema` and `ag_catalog` namespaces, the session's temporary schema and a graph's namespace cannot (`0A000`), where PostgreSQL lets a superuser rename its system schemas.

Table, view, materialized-view, foreign-table, sequence, schema and routine owner changes coordinate with concurrent `DROP ROLE` of the new owner. Deletion waits for an uncommitted ownership change: commit leaves an ownership dependency and reports `2BP01`, while rollback or savepoint undo allows deletion if no other dependency remains. An ownership change waiting for role deletion retains the originally selected role identity; committed deletion or same-name recreation reports `42704`, while rollback allows the change to continue.

`DROP SCHEMA [IF EXISTS] name [, ...] [CASCADE | RESTRICT]` accepts namespace identifiers and returns the `DROP SCHEMA` command tag without rows. The schema owner, an inheriting role, or a superuser may remove the schema, including objects owned by other roles. `RESTRICT` is the default; any object the schema contains prevents removal with `2BP01`, and the error detail names each one as depending on the schema. A missing schema reports `3F000`; `IF EXISTS` skips it with a notice. A nonowner reports `42501` before dependency checks. Duplicate targets are removed once, and all target names and ownership checks precede deletion.

`CASCADE` removes the contained objects and everything in any schema that depends on them, as [dependency-aware deletion](#dependency-aware-deletion) describes, and reports them in one notice. Dependent views and SQL-standard routines are removed; referencing tables survive after removal of dependent foreign keys, defaults, CHECK constraints, or typed and generated columns. Domains based on a removed type are removed, as are domains whose defaults require a removed type or function. Remaining columns retain their rows. The whole statement participates in transaction and savepoint rollback. The ordinary `public` schema may be dropped and remains absent across reopen; creating it again is explicit. Named graph namespaces continue to remove their graphs with `CASCADE`. Deleting the virtual catalog schemas remains tracked implementation work in [Compatibility](09-compatibility.md).

```sql execute
CREATE SCHEMA disposable_schema;
CREATE TABLE disposable_schema.parent (id integer PRIMARY KEY);
CREATE TABLE schema_drop_reference (id integer REFERENCES disposable_schema.parent(id));
DROP SCHEMA disposable_schema CASCADE;
INSERT INTO schema_drop_reference VALUES (42);
DROP TABLE schema_drop_reference;
```

## Domain declarations and deletion

```sql
CREATE DOMAIN schema_name.domain_name AS base_type
    DEFAULT default_expression
    CONSTRAINT not_null_name NOT NULL
    CONSTRAINT check_name CHECK (VALUE > 0);
```

The schema, default, constraint names, and constraints are optional. A domain retains its own type identity over a scalar, array, or another domain. `VALUE` denotes the value being checked; CHECK expressions must return Boolean, cannot reference other columns or contain subqueries, and accept TRUE or NULL. Multiple CHECK constraints run in alphabetical order of their names, after inherited domain checks. A column default overrides the domain default.

Domain ownership retains the owner role's OID and incarnation. `DROP ROLE` reports `2BP01` while the role owns a domain; dropping the domain releases the dependency, and transaction or savepoint undo restores it. Reusing an old role name does not transfer domain authority.

Domain creation participates in the surrounding transaction. Definitions, defaults, constraint bindings, and type identities survive SQLite reopen and remain available to new sessions. A duplicate type name reports `42710`; invalid CHECK result types report `42804`, failed checks report `23514`, and a prohibited NULL conversion reports `23502`.

Domains share their type-name namespace with tables, regular and materialized views, and foreign tables. Creation and relation rename reserve a destination until transaction completion or savepoint undo. A concurrent creator waits; a committed competitor produces `23505`, and rollback makes the destination available. Sequences and indexes may use the same name as a domain and can publish independently. These creation checks use current catalog state while ordinary data reads retain the selected isolation snapshot.

Native SQLite, SQLite Key/Value and redb allow transactions changing distinct domains to commit independently. Catalog refresh preserves private domain replacements and deletions alongside independently committed definitions. Transaction or savepoint rollback undoes only its own changes, and REPEATABLE READ and SERIALIZABLE retain their ordinary data snapshots during current-catalog inspection.

Constraints run when a value is converted into a domain. Assigning an already typed domain value preserves its identity without checking it again, including a typed NULL produced by an empty scalar subquery. Explicit casts follow the base type's explicit conversion rules; assignments enforce its declaration limits. For example, casting to a `varchar(5)` domain truncates an overlength string, while assigning an overlength string to its column reports `22001`.

```sql execute
CREATE DOMAIN positive_amount AS integer DEFAULT 1
    NOT NULL CHECK (VALUE > 0);
CREATE TABLE domain_orders (id integer, amount positive_amount);
INSERT INTO domain_orders (id) VALUES (1);
INSERT INTO domain_orders VALUES (2, 5);
SELECT id, amount FROM domain_orders ORDER BY id;
```

`ALTER DOMAIN name ADD [CONSTRAINT constraint_name] CHECK (expression) [NOT VALID]`, `ADD [CONSTRAINT constraint_name] NOT NULL`, `DROP CONSTRAINT [IF EXISTS] constraint_name [CASCADE | RESTRICT]`, and `VALIDATE CONSTRAINT constraint_name` change domain constraints and return `ALTER DOMAIN` with no result rows. The domain owner may change its constraints; its type and existing constraint identities remain stable. A duplicate constraint name reports `42710`; a missing constraint reports `42704`, or a notice with `DROP CONSTRAINT IF EXISTS`.

Adding a CHECK or NOT NULL constraint validates existing columns of the domain and its derived domains, including materialized views. Validation retains relation locks and reads the latest committed rows plus the transaction's own changes, even under REPEATABLE READ or SERIALIZABLE; ordinary queries keep their isolation snapshot. Failed validation reports `23514` for CHECK or `23502` for NOT NULL and rolls back the catalog change. A stored array or composite containing the domain prevents validation with `0A000`, including an empty table with such a column, as PostgreSQL 18 does.

`NOT VALID` skips the existing-row scan and leaves `pg_constraint.convalidated` false while conversions into the domain immediately enforce the new CHECK. `VALIDATE CONSTRAINT` scans existing values and changes that flag only after success; it repeats the scan for an already validated CHECK and rejects a NOT NULL target with `42809`. Validation evaluates only the selected CHECK, preserving its normal function calls without rerunning older CHECKs. Each relation retains a snapshot before scanning, so rows inserted by a CHECK function into that relation are not scanned again during the same validation. Added CHECKs join the alphabetical evaluation order; adding NOT NULL to an already NOT NULL domain leaves the existing constraint unchanged. All changes participate in transaction and savepoint rollback and survive durable reopen.

```sql execute
ALTER DOMAIN positive_amount ADD CONSTRAINT amount_ceiling CHECK (VALUE <= 100) NOT VALID;
ALTER DOMAIN positive_amount VALIDATE CONSTRAINT amount_ceiling;
ALTER DOMAIN positive_amount DROP CONSTRAINT amount_ceiling;
```

`DROP DOMAIN [IF EXISTS] name [, ...] [CASCADE | RESTRICT]` removes domains after resolving every target and checking domain-owner or containing-schema-owner authority. Qualified names require schema `USAGE`; unqualified names skip inaccessible search-path schemas. `IF EXISTS` reports missing types or schemas as notices, while a non-domain type still reports `42809`. Missing domains report `42704`, missing schemas report `3F000`, and insufficient authority reports `42501`.

RESTRICT is the default and reports `2BP01` when another object depends on a target. Explicitly naming both a base and its derived domain permits their joint deletion when no outside dependency remains. CASCADE removes derived domains, typed columns, generated columns, dependent views and SQL-standard routines, and indexes whose expressions or predicates require the domain. Defaults and CHECK constraints that require it are removed while their independent columns and domains survive. Column-dependent indexes are removed through the column lifecycle together with their owning PRIMARY KEY or UNIQUE constraints and referencing foreign keys. Direct removal of a constraint-owned index still reports `2BP01`. A table retains its unrelated columns and rows. SQL-standard query and INSERT, UPDATE, DELETE, and MERGE bodies retain column dependencies; routines reading only unrelated columns and string-literal SQL bodies survive column deletion.

The command returns `DROP DOMAIN` with no result rows. All target and dependency changes participate in transaction and savepoint rollback, catalog refresh, and SQLite reopen. Domain deletion in a read-only transaction reports `25006`.

```sql execute
CREATE DOMAIN cleanup_amount AS integer CHECK (VALUE > 0);
CREATE TABLE cleanup_orders (id integer, amount cleanup_amount);
INSERT INTO cleanup_orders VALUES (1, 5);
CREATE FUNCTION cleanup_amount_reader() RETURNS integer
    LANGUAGE SQL BEGIN ATOMIC SELECT amount::integer FROM cleanup_orders; END;
DROP DOMAIN cleanup_amount CASCADE;
SELECT * FROM cleanup_orders;
SELECT to_regprocedure('cleanup_amount_reader()') IS NULL AS routine_removed;
```

## Enum types

```sql
CREATE TYPE schema_name.type_name AS ENUM ('first_label', 'second_label');
ALTER TYPE schema_name.type_name ADD VALUE IF NOT EXISTS 'new_label' BEFORE 'second_label';
ALTER TYPE schema_name.type_name RENAME VALUE 'first_label' TO 'renamed_label';
DROP TYPE IF EXISTS schema_name.type_name CASCADE;
```

`ADD VALUE` also accepts `AFTER 'existing_label'` or no position, which appends the label, and `DROP TYPE` accepts several names and `RESTRICT`.

`CREATE TYPE ... AS ENUM` declares the labels in order; an empty list is allowed. A label longer than 63 bytes reports `42602`, and a repeated label reports PostgreSQL's `23505` unique-index violation on `pg_enum_typid_label_index`. The type receives a `pg_type` row with `typtype = 'e'` and category `E`, a generated array type named like PostgreSQL's array names, and one `pg_enum` row per label. Type names share the type namespace with domains, generated array names and relation row types: a name held by another type reports `42710`, while a name held only by an enum's generated array type moves that array to a new name first, as PostgreSQL does. The array type OID is assigned before the enum type OID.

`ADD VALUE` appends the label or places it before or after an existing label; an existing label reports `42710`, or a `NOTICE` with `IF NOT EXISTS`, and a missing neighbor reports `22023`. `RENAME VALUE` reports `22023` for a missing label and `42710` for an existing new label, and both commands report `42602` for an overlong label. They require ownership of the type (`42501`) and an enum type (`42809`, including its array type). Concurrent label changes to one type are serialized for the rest of the owning transaction. Definitions, labels, positions, owners and OIDs participate in transaction and savepoint rollback and survive reopen on native SQLite, SQLite Key/Value and redb; transactions changing different enum types commit independently, while a concurrent change to the same type reports `40001`.

```sql execute
CREATE TYPE review_state AS ENUM ('draft', 'approved');
ALTER TYPE review_state ADD VALUE 'reviewed' BEFORE 'approved';
ALTER TYPE review_state RENAME VALUE 'draft' TO 'submitted';
SELECT enumlabel, enumsortorder
FROM pg_enum
WHERE enumtypid = 'review_state'::regtype
ORDER BY enumsortorder;
```

`DROP TYPE` removes enum types, composite types and domains after resolving every target, checking type ownership or containing-schema ownership, and applying the dependency rules of [domain deletion](#domain-declarations-and-deletion): RESTRICT reports `2BP01` while columns, domains, arrays, views or routines depend on the type, and CASCADE removes those dependents. The error and the cascade notice come from the [catalog dependencies](#catalog-dependencies) as PostgreSQL's `findDependentObjects` finds them: the RESTRICT detail names each dependent and the object it depends on, newest dependents last, and a column default or generation expression and a view's rule are reported as the column or view they belong to. Built-in types report `2BP01` as required by the database system, a generated array type cannot be dropped by itself (`2BP01`, naming its element type), and the row type of a table, view, materialized view or foreign table always reports `2BP01` naming its relation.

## Composite types

```sql
CREATE TYPE schema_name.type_name AS (first_attribute integer, second_attribute text COLLATE "C");
DROP TYPE IF EXISTS schema_name.type_name CASCADE;
```

`CREATE TYPE ... AS (...)` declares a standalone composite type with its attributes in order; an empty attribute list is allowed. The checks follow PostgreSQL's `DefineCompositeType`: the creation schema and its `CREATE` privilege, then the type name, where a name held by another type reports `42710` and a name held only by a generated array type moves that array aside, then the attributes, where more than 1600 report `54011`, a repeated name `42701`, an unknown type `42704` (`serial` names no type here), a type without `USAGE` `42501`, an unknown collation `42704`, a collation on a type that has none `42804`, `SETOF` `42P16` and a pseudo-type such as `record` or `void` `42P16`, and then the name of the composite relation, which no table, view, sequence, index or other composite relation may hold (`42P07`). `COLLATE` accepts PostgreSQL's built-in collations: `default`, `C`, `POSIX`, `ucs_basic`, `unicode`, `pg_c_utf8` and `pg_unicode_fast`. A type in `pg_catalog` is refused with `42501` after its relation OID is assigned, as `heap_create` refuses it.

The type receives a composite relation in `pg_class` (`relkind = 'c'`), one `pg_attribute` row per attribute and a `pg_type` row with `typtype = 'c'`, category `C` and PostgreSQL's `record_in` and `record_out` routines, together with a generated array type. The relation OID is assigned first, then the array type's and then the type's, as `heap_create_with_catalog` assigns them. `pg_depend` records that the relation belongs to the type, that the array type belongs to the type and that each attribute depends on its type, and `pg_describe_object` names the relation `composite type name` and an attribute `column attribute of composite type name`. Values of the type are named records: `'(1,"a b")'::type_name` reads text as `record_in` does, including its `22P02` diagnostics for a malformed literal, text output quotes fields as `record_out` does, `ROW(...)::type_name` and assignment coerce an anonymous row attribute by attribute (`42846` when the column counts differ), and `(value).attribute` selects a field (`42703` for a missing attribute, `42809` on a value that is not composite). Composite values nest in other composite types and in arrays, compare field by field with NULL fields sorting last, and are null for `IS NULL` only when every field is null and not null for `IS NOT NULL` only when no field is. `UPDATE ... SET column.attribute = value` and `INSERT INTO table (column.attribute)` assign a field, through arrays and nested composites as well; assigning a field of a NULL value creates a row of NULL fields.

Relation commands refuse a composite relation as PostgreSQL does: reading or changing its rows, creating an index, inheriting from it or defining a rule or trigger on it reports `42809` `cannot open relation`, `DROP TABLE` and the other relation drops report `42809` with a hint to use `DROP TYPE`, `ALTER TABLE` reports `42809` with a hint to use `ALTER TYPE`, and `TRUNCATE` and `GRANT` on it report `42809`. `DROP TYPE` removes the type with its relation and array type; with `CASCADE`, an attribute of another composite type whose type is removed is dropped from that type and from its stored values, and keeps its number in `pg_attribute` as a dropped attribute. Definitions and stored values participate in transaction and savepoint rollback and survive reopen on native SQLite, SQLite Key/Value and redb.

```sql execute
CREATE TYPE shipping_address AS (street text, city text, zip varchar(10));
CREATE TABLE shipments (id integer, destination shipping_address);
INSERT INTO shipments VALUES (1, ROW('1 Main St', 'Springfield', '12345')), (2, '("2 Oak Ave",Shelbyville,)');
UPDATE shipments SET destination.zip = '54321' WHERE id = 2;
SELECT id, destination, (destination).city FROM shipments ORDER BY id;
SELECT attname, format_type(atttypid, atttypmod) AS attribute_type
FROM pg_attribute
WHERE attrelid = 'shipping_address'::regclass
ORDER BY attnum;
```

## Type lifecycle and privileges

```sql
ALTER TYPE schema_name.type_name RENAME TO new_name;
ALTER TYPE schema_name.type_name SET SCHEMA new_schema;
ALTER TYPE schema_name.type_name OWNER TO role_name;
ALTER DOMAIN schema_name.domain_name RENAME TO new_name;
GRANT USAGE ON TYPE schema_name.type_name TO role_name WITH GRANT OPTION;
REVOKE GRANT OPTION FOR USAGE ON DOMAIN schema_name.domain_name FROM role_name CASCADE;
```

`ALTER TYPE` changes enum types, composite types and domains; `ALTER DOMAIN` accepts only domains (`42809`). The commands apply PostgreSQL's check order: a missing type reports `42704`, a missing schema `3F000`, a generated array type `42809` with a hint naming its element type, a non-owner `42501`, a taken name `42710`, and `OWNER TO` requires membership in the new owner and `CREATE` on the schema. A type's generated array type is renamed and moved with it, and an array type already holding the new name is moved out of the way first. A composite type's relation is renamed, moved and given the new owner with it, so a rename or move also reports `42P07` when a relation holds the name.

Columns, domains, views, defaults, CHECK constraints, generated columns, index expressions and predicates, routine signatures and SQL-standard routine bodies refer to user-defined types by identity, so they follow a rename or schema move without changing. Stored enum constants refer to their labels by identity and follow `RENAME VALUE`. Output such as `pg_get_viewdef`, `pg_get_expr`, `pg_get_constraintdef`, `pg_get_function_arguments`, `format_type` and diagnostics spells a type by its current name, qualified by its schema when the search path does not reach it.

`GRANT` and `REVOKE` of `USAGE` (or `ALL`) on a type or domain record an access control list in `pg_type.typacl`, with grant options, `CASCADE` for dependent grants (`2BP01` without it) and PostgreSQL's warnings when nothing is granted or revoked. The default privileges grant `USAGE` to `PUBLIC`; after the first `GRANT` or `REVOKE` they are recorded explicitly. Array types have no privileges of their own (`0LP01`). `has_type_privilege` answers in all six forms, and `aclexplode` expands an access control list.

Declaring a type requires `USAGE` on it for a table, temporary or foreign table column (inherited and partition columns included), `ADD COLUMN`, `ALTER COLUMN TYPE`, a domain's base type, a routine's argument and result types, and the columns of a view, materialized view, `CREATE TABLE AS` or `SELECT INTO`; a denial reports `42501` `permission denied for type`. An array type defers to its element type, a domain is governed by its own privileges rather than its base type's, and casts need no privilege. Each check takes PostgreSQL's place among the command's other checks: for example, `CREATE TABLE` reports a taken relation name only after its columns' types, privileges, names and pseudo-types have been checked.

Routine bodies written as strings are compiled by each session that runs them, as PostgreSQL's per-session function cache does. The session that creates a PL/pgSQL routine keeps the compilation that validated it, a SQL-language body compiles when a session first runs it, and a compiled body keeps the enum types it resolved; another session, or a reopened database, resolves the names again, so a body naming a type that was renamed afterwards reports `42704` there. Loading the catalog never compiles such bodies. A SQL-standard body (`RETURN` or `BEGIN ATOMIC`) is bound when the routine is created.

```sql execute
CREATE TYPE ticket_state AS ENUM ('open', 'closed');
CREATE TABLE tickets (id integer, state ticket_state);
CREATE VIEW open_tickets AS SELECT id FROM tickets WHERE state = 'open';
ALTER TYPE ticket_state RENAME TO ticket_status;
SELECT format_type(atttypid, atttypmod) AS column_type
FROM pg_attribute
WHERE attrelid = 'tickets'::regclass AND attname = 'state';
SELECT pg_get_viewdef('open_tickets'::regclass) AS definition;
```

## Relation row type catalogs

Tables, inherited and partitioned tables, views, materialized views and foreign tables expose their automatically created row type through `pg_class.reltype = pg_type.oid`. Its `typtype` is `c`, `typrelid` identifies the relation and `typarray` identifies the generated array type. The array's `typelem` points back to the row type. Row types use PostgreSQL's record input/output metadata; arrays use its array metadata. Sequences and indexes do not own row types.

`'schema.relation'::regtype` and `to_regtype('schema.relation')` resolve that row type. Appending `[]` to the relation name selects its array type; additional dimensions select the same array type. A generated array's catalog name, such as `_relation`, can also be resolved directly, but appending `[]` to that already-array name reports `42704` because it has no further array type. `to_regtype` returns NULL for that missing type. `regtype` text output and `format_type` use the current relation name, qualify it when another type shadows it in the effective search path, and include the session's temporary namespace.

The generated array name is stored with its relation. A new explicit type or row type can displace an existing generated array name without changing its OID. Renaming the relation chooses its new array name using PostgreSQL's collision rules; moving the relation to another schema preserves its current array name and rejects an occupied destination with `42710`. Owner changes apply to both catalog rows. Transaction and savepoint rollback restore names and definitions, and durable reopen preserves their identities. Stored `regtype` constants continue to denote the same type after a rename and prevent dropping the owning relation while a dependent stored expression remains.

`ALTER TABLE`, `ALTER VIEW`, `ALTER MATERIALIZED VIEW` and `ALTER FOREIGN TABLE name SET SCHEMA destination` move the relation and its row and array types together. A table's indexes and owned `SERIAL` and identity sequences move with it; their catalog identities and dependent view references remain intact. The caller must own the relation and have `CREATE` on the destination schema. A conflicting relation, index or sequence name reports `42P07`; a missing schema reports `3F000`. Moving into or out of a temporary schema reports `0A000`.

```sql execute
CREATE TABLE row_type_example(id integer);
SELECT t.typname, t.typtype, a.typname AS array_name,
       t.typrelid = c.oid AS same_relation,
       a.typelem = t.oid AS same_element
FROM pg_class c
JOIN pg_type t ON t.oid = c.reltype
JOIN pg_type a ON a.oid = t.typarray
WHERE c.oid = 'row_type_example'::regclass;
SELECT 'row_type_example'::regtype::text,
       'row_type_example[][]'::regtype::text;
DROP TABLE row_type_example;
```

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

Implemented table properties include columns, defaults, generated serial values, virtual and stored generated columns, nullability, key constraints, checks, foreign keys, ordinary inheritance, declarative partitioning, and vector or tensor dimensions. After resolving the target namespace and its privileges, `CREATE TABLE IF NOT EXISTS` checks the shared relation name before analyzing the proposed columns, types, constraints, inheritance, typed-table source, storage parameters, access method, or tablespace; an existing table, view, materialized view, sequence, index, or foreign table therefore produces a notice without definition errors or generated-sequence side effects. `TEMP` and `TEMPORARY` tables live in the session's `pg_temp` namespace, are omitted from durable storage, and support `ON COMMIT PRESERVE ROWS`, `ON COMMIT DELETE ROWS`, and `ON COMMIT DROP`; the drop action removes dependent temporary views and foreign-key links with PostgreSQL's internal cascade semantics. The session's first temporary table, view, sequence, CTAS or `SELECT INTO` target creates the namespace `pg_temp_N` and then its TOAST namespace `pg_toast_temp_N`, each with the database counter's next OID, before the relation takes its own OIDs, as `InitTempTableNamespace` does; this happens before the statement checks its columns, so a statement that fails afterwards still uses the two OIDs. `pg_namespace` then lists both, owned by the bootstrap superuser with a NULL ACL, `pg_my_temp_schema()` returns the temporary namespace's OID, and `pg_is_other_temp_schema` reports neither as another session's; before the creation `pg_my_temp_schema()` returns 0. A rollback or savepoint rollback past the creation removes both namespaces, and the next temporary object creates them again with new OIDs, while `DISCARD TEMP` and `DISCARD ALL` keep them. Other sessions do not yet see a session's temporary namespaces, and the namespaces end with their session instead of remaining in `pg_namespace`; both remain compatibility bugs. A new relation's namespace and persistence follow `RangeVarGetCreationNamespace` and `RangeVarAdjustRelationPersistence`: a table, view, sequence, CTAS or `SELECT INTO` target qualified with `pg_temp`, or created without a schema while `pg_temp` is the first usable entry of `search_path`, goes to the session's temporary namespace and is temporary, creating the namespace first; `TEMP` with another schema reports `42P16` `cannot create temporary relation in non-temporary schema`, and `UNLOGGED` in the temporary namespace reports `42P16` `only temporary relations may be created in temporary schemas`, both before `IF NOT EXISTS` finds an existing relation, except that CTAS and `SELECT INTO` look for the existing relation first, as `CreateTableAsRelExists` does. A view whose query uses a temporary relation becomes temporary with the notice `view "name" will be a temporary view`. No relation may be created in `pg_catalog` or in the session's TOAST namespace: the statement reports `42501` `permission denied to create "schema.name"` with the detail `System catalog modifications are currently disallowed.` after the relation has taken its OID. Temporary materialized views and temporary foreign tables, which PostgreSQL creates in `pg_temp`, remain compatibility bugs. `DISCARD TEMP` removes the session's temporary tables, views, and sequences outside a transaction. `UNLOGGED` tables retain their catalog identity and rows across a clean reopen, although PostgreSQL crash-recovery truncation semantics remain unimplemented. Typed, storage-parameterized, access-method-selected, or tablespace-bound tables are not implemented when the target name is free.

`CREATE TABLE` and `ALTER TABLE ... ADD COLUMN` check each column definition as `transformColumnDefinition` does, once the relation is found and in written order: an array of a SERIAL type reports `0A000` `array of serial is not implemented` before the column's type is looked up; then `DEFERRABLE`, `NOT DEFERRABLE`, `INITIALLY DEFERRED` and `INITIALLY IMMEDIATE` must follow a `PRIMARY KEY`, `UNIQUE` or `REFERENCES` clause and `ENFORCED` or `NOT ENFORCED` a `CHECK` or `REFERENCES` clause, each at most once (`42601` `misplaced DEFERRABLE clause`, `multiple DEFERRABLE/NOT DEFERRABLE clauses not allowed`, `constraint declared INITIALLY DEFERRED must be DEFERRABLE`); then the clauses are read in order, and a repeated `DEFAULT`, identity or generation expression, a `DEFAULT` with an identity or generation expression, `NULL` against `NOT NULL`, `PRIMARY KEY`, a SERIAL type or an identity, `NOT NULL NO INHERIT` with a `PRIMARY KEY`, a SERIAL type or an identity, and two `NOT NULL` clauses with different names or `NO INHERIT` report `42601` with PostgreSQL's message naming the column and table, as `multiple default values specified for column "a" of table "t"`. A `NOT NULL ... NO INHERIT` on a partitioned table reports `0A000`. A later column's error waits for an earlier column's type to resolve, so `CREATE TABLE t (a nosuchtype, b int DEFAULT 1 DEFAULT 2)` reports the missing type. `DEFERRABLE` `PRIMARY KEY` and `UNIQUE` constraints are reported as unsupported after the statement's other checks pass. A column default is analyzed as PostgreSQL's `cookDefault` analyzes it, in `CREATE TABLE`, `ALTER TABLE ... ADD COLUMN`, `ALTER COLUMN ... SET DEFAULT` and `CREATE DOMAIN`. A string literal is read by the column type's input function when the default is defined, so `b int DEFAULT 'x'` reports `22P02` `invalid input syntax for type integer: "x"` and `b int DEFAULT '12'` stores the constant `12`, which `pg_get_expr` prints as `12`, as it prints `true` for `b bool DEFAULT 'yes'`, `'2026-01-02'::date` for a date and `'{1,2}'::integer[]` for an array; the input function runs without the column's type modifier, so `varchar(2) DEFAULT 'abc'` is accepted and fails when a row takes the default. A cast of a literal, `'7'::bigint`, is read by the cast's type and keeps its modifier. A literal of an OID alias type, `regclass`, `regtype`, `regproc`, `regprocedure` or `regnamespace`, such as `'seq'::regclass` or `'seq'` for a `regclass` column, `'int4'::regtype` or `'f(int)'::regprocedure`, and a literal of an array of one of them are read by the type's input function when the default, generation expression or `CHECK` constraint is defined, as parse analysis reads them: the stored constant is the object's OID, so a name no object has reports the input function's error at `CREATE TABLE`, `ADD COLUMN`, `SET DEFAULT`, `ADD CONSTRAINT` and `CREATE DOMAIN`, `42P01` `relation "seq" does not exist`, `42704` `type "t" does not exist`, `42883` `function "f" does not exist`, `42725` `more than one function named "abs"` or `3F000` `schema "s" does not exist`; `pg_get_expr` and `pg_get_constraintdef` print the constant as the type's output function prints the object, `'s2'::regclass` after `ALTER SEQUENCE s1 RENAME TO s2`, `'integer'::regtype` for `'int4'`, `'character varying'::regtype` without the written modifier, `'f(integer)'::regprocedure`, and schema-qualified when the object's schema is not on the search path; and `DROP TYPE`, `DROP FUNCTION` and `DROP SCHEMA` report `2BP01` for the defaults and constraints that depend on the object. A `regrole` constant reports `0A000` `constant of the type regrole cannot be used here`, while a `regrole[]` constant and a `regrole` computed from text are stored. Any other expression must have an assignment cast to the column's type, or the default reports `42804` `column "b" is of type integer but default expression is of type text` with the hint `You will need to rewrite or cast the expression.`; `1.5` is assignable to an integer column and rounds when a row takes it. A default that is a NULL constant, `DEFAULT NULL`, `NULL::int` or `(NULL)`, stores no default, and `SET DEFAULT NULL` removes one. Set-returning functions report `0A000` in defaults and generation expressions. A generation expression that cannot be assigned to its column reports the same `42804` message, a subquery `0A000`, an aggregate `42803`, a window function `42P20`, and a reference to another generated column `42P17` with the detail `A generated column cannot reference another generated column.`; a generation expression that is a string literal is read by the column type's input function and stored as its constant.

`CREATE TABLE` draws the OIDs of what it creates from the database's counter where PostgreSQL draws them. Once the columns are described and the relation's name is checked, the relation takes its OID, its array type the next and its row type the next, as `heap_create_with_catalog` allocates them, and the defaults, generation expressions and CHECK constraints the table inherits take theirs with the relation, the CHECKs in parent order and in name order within a parent. Then each of the table's own defaults and generation expressions takes a `pg_attrdef` OID in column order, a partition's cloned keys take an index OID and a constraint OID each and its cloned foreign keys a constraint OID, each CHECK takes its OID in written order while a CHECK merged with an inherited constraint keeps the inherited OID, each NOT NULL constraint takes its OID, each `PRIMARY KEY` or `UNIQUE` constraint takes an index OID and then a constraint OID, and each foreign key takes its constraint OID once its referenced key is found. A statement that fails after a step has used the OIDs allocated so far, as the counter advances outside the transaction: `CREATE TABLE t (a int DEFAULT 'x')` uses three OIDs, a table whose second CHECK names an unknown column uses four, and a statement that fails before the relation is created, on an unknown type or a key over a missing column, uses none. The triggers a foreign key creates and a TOAST table's relation and index take no OIDs.

`CREATE TABLE` creates its NOT NULL constraints as PostgreSQL's `AddRelationNotNullConstraints` creates them, after the CHECK constraints: the constraints the statement declares first, in the order it collected them, one per column, and then the constraints the parents give. A column's `NOT NULL` clause, or the constraint its `PRIMARY KEY`, SERIAL or identity implies, and a table constraint `NOT NULL column` are collected in written order; the columns of a table `PRIMARY KEY` are collected last, once every element is examined, and a `NO INHERIT` declaration of such a column reports `42601` `conflicting NO INHERIT declaration for not-null constraint on column "a"` there. A later declaration of a column that is already declared must agree on `NO INHERIT` (`42601`) and may name the constraint an earlier declaration left unnamed, while two different given names report `42601` `conflicting not-null constraint names "x" and "y"`; a declaration must name a column of the relation (`42703` `column "c" of relation "t" does not exist`, `0A000` for a system column), and a `NO INHERIT` declaration on a column whose parents give a constraint reports `42804` with the detail `The column has an inherited not-null constraint.`. A given name repeated by another NOT NULL constraint reports `42710`, and one a CHECK constraint of the table holds reports `23505` for `pg_constraint_conrelid_contypid_conname_index` with the relation's OID in the detail; a chosen name is `t_a_not_null`, then `t_a_not_null1` and so on, avoiding every constraint of the schema. An inherited constraint keeps its first parent's name unless the table holds it. Every NOT NULL constraint of a new table is validated, `NOT VALID` notwithstanding, and the declared constraints take their OIDs before the inherited ones.

`CREATE TABLE` checks a definition in `DefineRelation`'s order. The sequences of serial and identity columns are created first, before the parents' columns and CHECK constraints merge and before the relation's name is checked. The defaults and generation expressions follow in column order, then a partition's bound and the rows of the parent's default partition, then the partition key, then the keys a partition clones from its parent, and then the CHECK constraints in the order the statement writes them, column and table constraints alike; the declared keys and foreign keys come last, each foreign key's name checked before its referenced table. A statement with several errors therefore reports the one PostgreSQL reports: a default that calls an undefined function is reported before a partition key that names a missing column, and a CHECK that calls one after both. A generation expression that calls an undefined function reports `42883` with the call's argument types, as a default does.

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

Every ordinary table starts with a NULL ACL and default owner access to `SELECT`, `INSERT`, `UPDATE`, `DELETE`, `TRUNCATE`, `REFERENCES`, `TRIGGER`, and `MAINTAIN`. An owner may revoke these ordinary privileges from itself; subsequent access follows the ACL, including inherited roles and independent grants, while ownership still permits ALTER, DROP and privilege regrant through implicit grant options. A column grant permits access only to the granted columns unless a table-wide grant independently permits it. Table-wide `GRANT` and `REVOKE` support `ALL [PRIVILEGES]`, `PUBLIC`, inherited roles, independent grantors, `WITH GRANT OPTION`, `GRANT OPTION FOR`, `GRANTED BY`, dependency-aware `RESTRICT` and `CASCADE`, owner self-revocation and transfer, role dependencies, read-only transactions, temporary tables, transactions, savepoints, cross-engine refresh, and durable reopen. Explicit `ON TABLE` targets route sequences through their `SELECT`, `UPDATE`, and `USAGE` ACL rules, while `ON ALL TABLES IN SCHEMA` excludes sequences. Parent-targeted inheritance and partition operations check the named parent's privilege rather than each physical child, while relations added by foreign-key `TRUNCATE ... CASCADE` require their own `TRUNCATE` privilege.

The engine enforces table-wide privileges for query sources, `INSERT`, `UPDATE`, `DELETE`, `MERGE`, `COPY FROM`, `COPY TO`, `TRUNCATE`, foreign-key references, ordinary-table trigger creation, and `ANALYZE` or `VACUUM (ANALYZE)`. Catalog-wide maintenance processes only tables on which the active role has `MAINTAIN` and emits PostgreSQL-compatible warnings for skipped tables. DML requires `SELECT` only when an expression reads target-table values, matching PostgreSQL's constant-assignment and `RETURNING` boundaries. Ordinary-table column ACLs support `SELECT`, `INSERT`, `UPDATE`, and `REFERENCES`, NULL and explicitly empty `attacl` state, table-to-column privilege implication, independent rooted grant-option paths, dependent `RESTRICT` and `CASCADE`, and exact column checks across direct, joined, correlated, CTE, DML-source, target-expression, `COPY`, `MERGE`, and foreign-key paths. Implicit-width inserts check only supplied positions, `DEFAULT VALUES` accepts any insertable column, system columns require table-wide `SELECT`, and rename, drop, owner transfer, role dependencies, transactions, savepoints, cross-engine refresh, and durable reopen preserve the column ACL state. `pg_class.relacl` and `pg_attribute.attacl` expose PostgreSQL ACL text, all six `has_table_privilege` and all twelve `has_column_privilege` current-user or explicit-role name/OID and column-name/attnum overloads preserve comma-any, grant-option, strict-NULL, missing-object, sequence-column, system-column, and error-precedence behavior, and `information_schema.tables`, `columns`, `column_privileges`, and `role_column_grants` apply PostgreSQL role visibility. Foreign tables use the same relation and column ACL model while their built-in wrappers remain read-only; default privileges and row-level security remain open compatibility bugs.

Table and column privilege inquiries bind an explicit role name or OID to that role's identity before resolving the target; a replacement with the same name or OID cannot acquire that subject's authority. The lowercase `public` subject and role OIDs absent from the catalog receive applicable PUBLIC grants, including exact column grants, but no PUBLIC grant options. A quoted role named `"PUBLIC"` remains a distinct ordinary role. Named relation and column lookup errors retain their precedence, while an invalid privilege string is rejected before returning `NULL` for a missing relation OID or invalid attribute number.

Object `GRANT` and `REVOKE` distinguish the `PUBLIC` recipient from the quoted role name `"PUBLIC"`. Only the keyword applies to every role and disallows grant options; the quoted name must exist and can own grant options and grantor paths. Quoted `"CURRENT_USER"` and `"SESSION_USER"` likewise name literal roles, while unquoted `CURRENT_USER`, `CURRENT_ROLE` and `SESSION_USER` select the corresponding session identity. This distinction applies to table/column, view, materialized-view, foreign-table, sequence, schema, database, routine and system-catalog privileges, including `GRANTED BY`, and to the supported `OWNER TO` statements. Revocation, role dependencies, transaction undo, refresh and reopen preserve the distinction.

```sql execute
CREATE ROLE "PUBLIC";
CREATE ROLE acl_observer;
CREATE TABLE literal_acl_target(v integer);
GRANT SELECT ON literal_acl_target TO "PUBLIC" WITH GRANT OPTION;
SELECT has_table_privilege('PUBLIC', 'literal_acl_target', 'SELECT') AS named_role_access,
       has_table_privilege('acl_observer', 'literal_acl_target', 'SELECT') AS observer_access;
```

Built-in catalog relations use the same table and column privilege inquiries, `GRANT` and `REVOKE`, query authorization and explicit `LOCK` checks. `pg_authid` projects the current catalog view of logical role OIDs, names, attributes and connection limits; its private default ACL requires an explicit relation or column grant for non-superuser queries. `pg_roles` retains its public view and masked password column. Grants persist through native SQLite, SQLite Key/Value and redb transactions, savepoints and reopen. System base tables suppress non-superuser relation-level `INSERT`, `UPDATE`, `DELETE` and `TRUNCATE` checks even when granted; explicit attribute privileges and grant-option inquiries are separate, and views such as `pg_settings` retain their own ACLs. Relation and attribute ACLs have independent write identities: unrelated catalog relations or attributes can commit concurrently, while a concurrent replacement of the same ACL tuple reports PostgreSQL's `XX000` error; rollback releases the waiter without that conflict. Role dependencies include system table and attribute grantor paths. Revoking a table privilege also revokes its matching explicit column grants, with dependent grants following `RESTRICT` or `CASCADE`.

```sql execute
CREATE ROLE catalog_acl_reader;
GRANT SELECT (rolname) ON pg_catalog.pg_authid TO catalog_acl_reader;
SELECT has_column_privilege('catalog_acl_reader', 'pg_catalog.pg_authid', 'rolname', 'SELECT');
SET ROLE catalog_acl_reader;
SELECT rolname FROM pg_catalog.pg_authid ORDER BY rolname;
RESET ROLE;
REVOKE SELECT ON pg_catalog.pg_authid FROM catalog_acl_reader;
DROP ROLE catalog_acl_reader;
```

## Inheritance and partitioning

Ordinary `INHERITS` tables and declarative `LIST`, `RANGE`, and `HASH` partitioning retain independent physical rows while a parent scan includes descendants; `ONLY parent` scans only the parent's own storage. Partition inserts, updates, COPY streams, and hierarchy mutations route to the matching leaf, and bounds, local-versus-inherited column provenance, partition keys, default partitions, partitioned indexes, and inheritance edges are durable catalog state.

```sql
CREATE TABLE events (event_id INTEGER, occurred_on DATE) PARTITION BY RANGE (occurred_on);
CREATE TABLE events_2026 PARTITION OF events FOR VALUES FROM ('2026-01-01') TO ('2027-01-01');
CREATE TABLE events_other PARTITION OF events DEFAULT;
```

A new table's columns merge with its parents' as PostgreSQL's `MergeAttributes` merges them. A parent named twice reports `42P07` `relation "p" would be inherited from more than once`, and a column the statement declares twice reports `42701` `column "b" specified more than once`, before any parent is examined. A table cannot inherit from a partitioned table or a partition (`42809`), a permanent table cannot inherit from a temporary one or be its partition, and a temporary table cannot be a partition of a permanent one (`42809`); a partition's parent that is not partitioned reports `42P17` `"p" is not partitioned` once the defaults are analyzed. Columns that two parents share report the notice `merging multiple inherited definitions of column "a"` and must have the same type (`42804` `inherited column "a" has a type conflict`, with the detail `text versus character varying(10)`) and the same generation; a declared column that a parent's column takes reports `merging column "a" with inherited definition`, or `moving and merging column "a" with inherited definition` with the detail `User-specified column moved to the position of the inherited column.` when it was declared elsewhere, and must have the same type (`42804` `column "a" has a type conflict`). A child column of a generated parent column is generated, with its own expression when it declares one; it cannot declare a default, an identity or the other generation kind (`42611`), and a column whose parent is not generated cannot declare a generation expression (`42611`). Parents that give a column different defaults or generation expressions report `42611` `column "a" inherits conflicting default values` or `inherits conflicting generation expressions`, with PostgreSQL's hint, unless the new table declares its own. 

`CREATE TABLE ... PARTITION OF` takes column options without types, `(b DEFAULT 5, c NOT NULL CHECK (c <> 'x'))`, each naming a column of the parent as `MergeAttributes` merges them: an option on a column the parent lacks reports `42703` `column "nocol" does not exist`, an option named twice reports `42701`, and an option's `DEFAULT` or generation expression replaces the parent's for the partition and for the partitions below it, its `NOT NULL`, `CHECK`, `PRIMARY KEY`, `UNIQUE` and `REFERENCES` clauses apply to the partition, and no notice reports the merge. A generated parent column keeps its generation: the option may give another expression of the same kind but no `DEFAULT`, identity or other kind (`42611`), and a column the parent does not generate takes no generation expression (`42611`). An identity clause on a partition's column reports `0A000` `identity columns are not supported on partitions`. 

Each bound value is analyzed as a partition bound expression, coerced to its partition key's type in assignment context and evaluated once when the partition is created or attached, as PostgreSQL's `transformPartitionBound` does. A column reference or subquery reports `0A000`, an aggregate `42803`, a window function `42P20`, and a set-returning function `0A000`; a value with no assignment cast to the key type reports `42804` naming the key, invalid input reports the key type's input error, and a NULL range bound reports `42P17`. The stored bound holds the resulting typed constants: routing compares keys in the key type's order, a repeated list value is stored once, `pg_get_expr` spells each constant as PostgreSQL's constant deparser does for the key type, and an enum bound keeps its label identity when the label is renamed. Overlap, empty-range, default-conflict and hash modulus diagnostics name the partitions PostgreSQL names, and a row that no partition accepts reports `23514` with the failing row's partition key in DETAIL when the current role can read that key. Partition key expressions must be immutable and reference a column of the table; system columns, generated columns and constant keys report `42P17`. `HASH` keys use PostgreSQL's extended hash functions for `smallint`, `integer`, `bigint`, `text`, `name`, `varchar`, `character`, `uuid`, `date`, enum types, and domains over them, so rows reach the same remainders as in PostgreSQL; an enum key hashes its label OID, and other key types report `0A000`.

```sql execute
CREATE TYPE request_priority AS ENUM ('low', 'normal', 'urgent');
CREATE TABLE support_requests (request_id INTEGER, priority request_priority) PARTITION BY LIST (priority);
CREATE TABLE support_requests_routine PARTITION OF support_requests FOR VALUES IN ('low', 'normal');
CREATE TABLE support_requests_urgent PARTITION OF support_requests FOR VALUES IN ('urgent');
INSERT INTO support_requests VALUES (1, 'urgent'), (2, 'low');
ALTER TYPE request_priority RENAME VALUE 'urgent' TO 'critical';
SELECT tableoid::regclass::text AS partition, request_id, priority FROM support_requests ORDER BY request_id;
SELECT pg_get_expr(relpartbound, oid) AS bound FROM pg_class WHERE relname = 'support_requests_urgent';
```

Request 1 stays in `support_requests_urgent` and reads as `critical`, and that partition's bound reads `FOR VALUES IN ('critical')`.

Direct hierarchy changes use PostgreSQL 18 `ALTER TABLE` forms:

```sql
ALTER TABLE audit_events INHERIT events;
ALTER TABLE audit_events NO INHERIT events;
ALTER TABLE events ATTACH PARTITION events_2027 FOR VALUES FROM ('2027-01-01') TO ('2028-01-01');
ALTER TABLE events DETACH PARTITION events_2027;
```

`INHERIT` and `NO INHERIT` validate compatible row types, inherited checks, persistence, duplicate edges, and cycles while retaining ordered `pg_inherits.inhseqno` values. Parent names in `CREATE TABLE ... INHERITS`, `PARTITION OF`, `ALTER TABLE ... INHERIT`, and `ATTACH PARTITION` resolve through the active role's effective schema `USAGE` boundary before their definitions are inspected; a missing qualified schema reports `3F000`, and the resulting hierarchy stores exact canonical parent identities. `ATTACH PARTITION` validates the exact partition row type, existing rows, sibling and default bounds, inherited checks, keys, foreign keys, identity generation, and every descendant before publishing the edge. `DETACH PARTITION` localizes the inherited schema state and restores a partition's prior serial generator when an attached parent identity had temporarily overridden it. It preserves local key and foreign-key rows and their public OIDs, separates the detached subtree from its former parent's foreign-key enforcement family, and keeps attachments inside that subtree. Explicit deferred modes remain in effect on both sides; removing the former parent constraint leaves the detached constraint enforced. Reopening a legacy catalog repairs and canonicalizes hierarchy parents before materializing and synchronizing partition-inherited foreign-key object identities; later detach uses their provenance. These changes are atomic, survive rename and reopen, and roll back with an explicit transaction.

Ordinary `INHERITS` children do not inherit foreign keys, including inline `REFERENCES` on newly inherited columns and recursive `ADD COLUMN`. Declarative partitions retain foreign-key enforcement with an independent catalog row for each partition. `ALTER TABLE ... ADD FOREIGN KEY` on a partitioned table gives every partition below it the foreign key: a partition attaches its first foreign key by name that copies none of its parent's and references the same key through the same columns with the same deferrability, actions and match type, and the copies on its own partitions follow it; otherwise it receives a copy under the parent's name, or under that name with the first numeric suffix that no constraint of its schema holds when the partition already uses the name. `ATTACH PARTITION` applies the same rules to the attached partition and its partitions, reports `42P16` for a foreign key that differs from the parent's in enforceability alone, and validates the rows under each new copy of an enforced foreign key, whether the parent's foreign key is valid or not, and under each attached foreign key that is not valid when the parent's is. A `CREATE TABLE ... PARTITION OF` constraint named like a foreign key of the parent reports `42710`. A foreign key that references a partitioned table derives a constraint on each partition of it, as PostgreSQL does: a `pg_constraint` row of the referencing table that references the partition through the partition's index derived from the referenced key, with `conparentid` naming the foreign key or the parent partition's derived constraint, `conislocal = false`, `coninhcount = 1` and `connoinherit = false`, which the information schema lists as a foreign key. Creating the foreign key names its derived constraints from its own name with the first numeric suffix that no constraint of the referencing table's schema holds, parents before their partitions in partition order; a partition that joins later, through `CREATE TABLE ... PARTITION OF` or `ATTACH PARTITION`, takes its name from the constraint at the level it joins, the foreign key's for a partition of the referenced table and the parent partition's derived constraint below it, and `DETACH PARTITION` drops the derived constraints of the detached partitions. It first locks each referencing table Share and reads its latest committed rows with the transaction's own changes, and a row whose key a row of the detached subtree holds, or for a temporal foreign key a row whose period overlaps one there, reports `23503`, `removing partition "pk1" violates foreign key constraint "fk_a_fkey_1"`, with the detail `Key (a)=(1) is still referenced from table "fk".`, whether the foreign key is enforced or not, as PostgreSQL's `ATDetachCheckNoForeignKeyRefs` does. The partitions' copies of a partitioned referencing table's foreign key derive none; a detached referencing partition's foreign key derives its own and an attached one drops them. A derived constraint keeps its name when the foreign key is renamed, shares the referencing table's constraint names, renames with `RENAME CONSTRAINT`, reports `42P16` for `DROP CONSTRAINT` and `55000` for `ALTER CONSTRAINT`, and follows the foreign key's enforceability and deferrability; `VALIDATE CONSTRAINT` of one validates it and the derived constraints below it, reading a referencing table that is not partitioned against that partition alone, as PostgreSQL does. A delete or key update of a referenced row that the foreign key rejects names the partition that holds the row and the constraint derived on it, `update or delete on table "pk1" violates foreign key constraint "fk_a_fkey_1" on table "fk"`, and a row that moves to another partition names the relation the `UPDATE` names and its constraint, as PostgreSQL fires the update's root for a moved row. `SET CONSTRAINTS` of a derived constraint governs the checks of its partition and of the partitions below it, and that of the foreign key governs all of them. A foreign key that references a table which is not partitioned reads only that table's own rows, as PostgreSQL's referential queries read `ONLY` such a table: a key that only an inheritance child holds does not satisfy it, and an `UPDATE` or `DELETE` of a child's row runs none of its referential actions, while the partitions of a referenced partitioned table hold that table's rows. A foreign key declared on a partitioned referencing table checks and acts on the rows of all its partitions, while a foreign key declared on one partition checks and acts on that partition's rows alone, as PostgreSQL creates the referenced side's triggers for the constraint that has no parent constraint.

A row that no partition accepts reports `23514`, `no partition of relation "p" found for row`, naming the partitioned table at which routing failed, with the detail `Partition key of the failing row contains (k) = (1).`; the keys print as `pg_get_partkeydef` prints them, an expression in parentheses unless it is a function call, and a role sees the values only when it may read the table or each key is a column it may read. An `INSERT` that names a partition checks the partition's bound after its NOT NULL and CHECK constraints, and a partitioned table that is itself a partition checks its own bound before it routes a row; a row outside the bound reports `new row for relation "p1" violates partition constraint` with the `Failing row contains (...)` detail. An `UPDATE` that names a partition checks the bound first, so it cannot move a row out of the partition, while an `UPDATE` of a partitioned table moves the row to the partition that accepts it. `INSERT ... ON CONFLICT DO UPDATE` reports `0A000`, `invalid ON UPDATE specification`, when the new row would leave its partition.

Column- and table-level CHECK constraints inherit by name and bound expression. Equivalent local and inherited definitions merge; a different expression under the same name reports `42710`, and incompatible `NO INHERIT`, validation, or enforcement states report `42P17`. CHECKs that two parents supply under one name merge into one inherited constraint, enforced when either parent's is, when their expressions match; otherwise `CREATE TABLE` reports `42710` `check constraint name "x" appears multiple times but with different expressions` while it merges the parents, before it analyzes the defaults. `pg_constraint.conislocal` records an independent local declaration, while `coninhcount` counts direct parents supplying the named constraint. A newly created child validates an inherited enforced CHECK even when the parent's CHECK is `NOT VALID`; an inherited `NOT ENFORCED` CHECK remains unvalidated. Attaching a partition marks supplied CHECKs inherited, and removing the last supplying inheritance edge or detaching a partition makes retained CHECKs local.

Recursive CHECK additions stop below an unchanged merged constraint. A CHECK declared with `ADD COLUMN` propagates independently of whether a child already had that column. Recursive `DROP CONSTRAINT` removes a child's CHECK only after its last inherited source disappears and it has no local declaration; `ONLY` preserves the child constraint and makes it local when its last source disappears. Directly dropping or renaming an inherited CHECK reports `42P16`, including when it also has a local declaration. Recursive `RENAME CONSTRAINT` requires every supplying parent to participate, checks affected relation owners, and preserves each constraint's OID. CHECK validation reaches all descendants before marking the parent valid; `ONLY` on an unvalidated inheritable CHECK with children reports `42P16`. Statement and savepoint rollback restore names, origin, validation, and identities, and committed state survives reopen.

```sql execute
CREATE TABLE checked_measurements(score INTEGER CONSTRAINT score_positive CHECK(score >= 0));
CREATE TABLE locally_checked_measurements(score INTEGER CONSTRAINT score_positive CHECK(score >= 0)) INHERITS(checked_measurements);
ALTER TABLE checked_measurements DROP CONSTRAINT score_positive;
SELECT conname, conislocal, coninhcount FROM pg_constraint WHERE conrelid = 'locally_checked_measurements'::regclass AND contype = 'c';
```

The child's `score_positive` CHECK remains with `conislocal = true` and `coninhcount = 0`.

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

A generation expression can reference non-generated columns in the same row and must be immutable. Subqueries, aggregate or window functions, parameters, `DEFAULT`, whole-row references, and references to another generated column are rejected before the table is created. The implemented expression surface is statically typed before catalog mutation. Immutability follows PostgreSQL's function and cast volatility: a cast calls its `pg_cast` function or, for an I/O conversion, the source type's output and target type's input function, so casts involving `date`, timestamps, `interval`, arrays, ranges, records, the `reg*` types or enum types, such as `mood::text` or `date_value::timestamptz`, report `42P17`, while an `unknown` literal is converted during analysis and calls nothing at run time. A default or generation expression stores an `unknown` literal as the constant the selected operator's operand type reads, as parse analysis stores it, so `a + '1'` is stored and printed as `(a + 1)`, `1.5 + '1'` as `(1.5 + '1'::numeric)` and `'x'::bytea || 'y'` as a `bytea` pair; a domain's default is stored in `pg_type.typdefaultbin`, which `pg_get_expr` prints, with its text in `typdefault`. The same analysis applies to index expressions and partition key expressions with their own messages. Among the enum support functions, `enum_first`, `enum_last` and `enum_range` read the label list and are stable, while comparisons, `enum_cmp`, `enum_smaller`, `enum_larger`, `hashenum` and `hashenumextended` are immutable; an invalid enum label in a generation expression reports `22P02` when the table is created. Stored SQL routine calls bind and persist the exact overload signature used for later evaluation and dependency checks. A generated column cannot also have a default or identity definition.

Virtual generated values are absent from physical row storage and are evaluated only when a logical projection or enforced constraint requires them. Stored generated values are recomputed exactly once at the prepared-write boundary of every insert, update, upsert, merge, referential action, and direct document replacement. Assigning a generated column directly is rejected; `DEFAULT` requests recomputation and is the only accepted explicit assignment.

Virtual generated columns cannot use user-defined routines, user-defined types or UQA Engine engine-defined types and cannot own primary-key, unique, foreign-key, or index constraints. A virtual column declared with an enum or domain type reports `0A000` with `virtual generated column "name" cannot have a user-defined type`. After the immutability check, the expression is examined in pre-order as PostgreSQL 18's `check_virtual_generated_security` does: a user-defined routine call raises `0A000` with primary message `generation expression uses user-defined function`, and any subexpression of a user-defined type, such as a column of an enum type, raises `0A000` with `generation expression uses user-defined type`, each with a separate DETAIL explaining the virtual-column restriction, including through `ALTER TABLE`. Stored generated columns can participate in those constraints and indexes and may call immutable user-defined routines.

`ALTER TABLE ADD COLUMN` supports both generated kinds, and `ALTER COLUMN ... SET EXPRESSION AS (...)` replaces a generation expression. `DROP EXPRESSION` is available for a stored generated column and retains its last stored values; PostgreSQL 18 rejects that operation for a virtual generated column.

A statement that writes a value other than `DEFAULT` to a generated column fails with `428C9` before it reads or writes a row, as PostgreSQL's rewriter rejects it: an `INSERT`, including one whose source yields no row, reports `cannot insert a non-DEFAULT value into column "name"`, and an `UPDATE`, an `ON CONFLICT DO UPDATE` or a `MERGE` update reports `column "name" can only be updated to DEFAULT`, each with the detail `Column "name" is a generated column.`. `OVERRIDING SYSTEM VALUE` admits only an identity column's value, and a statement writing several such columns reports the first in table order. `COPY` naming a generated column reports `42P10`.

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

A table-level `PRIMARY KEY (...)` or `UNIQUE (...)` may end with `INCLUDE (column, ...)`, which adds those columns to the constraint's supporting index beside the key, as [`CREATE INDEX ... INCLUDE`](#relational-b-tree-indexes) does. Included columns take no part in uniqueness and do not become NOT NULL; they may repeat and may name key columns, must exist, and cannot be virtual generated columns. The default constraint name lists the key columns and then the included ones, as `orders_account_id_total_key`, and `pg_index` and `pg_get_indexdef` report them while `pg_constraint.conkey` lists the key columns only. Renaming an included column renames it in the constraint, and dropping one drops the constraint, so a foreign key that references the constraint blocks the drop unless `CASCADE` removes it too.

`CREATE TABLE` analyzes its keys as PostgreSQL 18 does, before it looks for an existing relation of the table's name. A key may name a column that the table inherits, or a system column; a column the table lacks reports `42703`, `column "x" named in key does not exist`, a column named twice `42701`, `column "a" appears twice in primary key constraint` or `in unique constraint`, and a second primary key `42P16`, `multiple primary keys for table "t" are not allowed`. The primary key's index is created first whatever the declaration order. A key whose index would repeat an earlier key's index, with the same key columns, included columns, NULL treatment and `WITHOUT OVERLAPS`, creates no second index: `a integer UNIQUE PRIMARY KEY` declares the primary key alone, and the key that remains takes the name of the repeated one when it has none of its own. A key's name is the name of its index, so a name that another relation holds reports `42P07`, `relation "c" already exists`, and a name that another constraint of the table holds `42710`. `ALTER TABLE ... ADD PRIMARY KEY` first gives each key column its NOT NULL constraint, so a missing column reports `column "x" of relation "t" does not exist`, where `ADD UNIQUE` reports `column "x" named in key does not exist`.

A key column or included column that is a system column reports `0A000`, `index creation on system columns is not supported`, after the key's other checks, as does `CREATE INDEX` over a system column, in a key, an included column, a key expression or the predicate. A key or index key over `xmin`, `xmax`, `cmin` or `cmax` reports `42704`, `data type xid has no default operator class for access method "btree"`, with a hint, when its attributes are resolved.

A primary key or unique constraint of a partitioned table must hold every column of the partition key among its key columns, because each partition enforces the key with an index of its own; included columns do not count. A key that lacks one reports `0A000`, `unique constraint on partitioned table must include all partitioning columns`, with the detail `PRIMARY KEY constraint on table "t" lacks column "a" which is part of the partition key.`. A partition key that is an expression reports `unsupported PRIMARY KEY constraint with partition key definition`, and a `WITHOUT OVERLAPS` period that is a partition key column `cannot match partition key to index on column "valid_at" using non-equal operator "&&"`. The same rule holds for a unique index, which the errors call a `UNIQUE` constraint, and for every partitioned table that builds an index for the key: `ALTER TABLE ... ADD PRIMARY KEY` and `ADD UNIQUE`, `CREATE UNIQUE INDEX`, `CREATE TABLE ... PARTITION OF` and `ATTACH PARTITION` check each partitioned partition against its own partition key, each partition after its parent with siblings in partition bound order, and name the partition that fails. A partition that already has an equivalent key, or a matching unique index for a unique index, adopts it and is not checked again, nor are the partitions below it. A partition that would receive a primary key beside one it declares or already has reports `42P16`.

```sql
CREATE TABLE readings (sensor_id INTEGER, taken_on DATE, value NUMERIC, PRIMARY KEY (sensor_id, taken_on)) PARTITION BY RANGE (taken_on);
```

Adding a key or a unique index to a table that has rows builds its index from them. A repeated key reports `23505`, `could not create unique index "t_a_key"`, with the detail `Key (a)=(1) is duplicated.`; for a partitioned table the index named is that of the leaf partition holding the rows, and the leaves are built in partition order. The key reported is that of the first row, in the table's physical order, that repeats the key of an earlier row. `ALTER TABLE ... ADD PRIMARY KEY` verifies the NOT NULL constraints of the key's columns after it has built the index, so a repeated key is reported before a NULL, which reports `23502`, `column "a" of relation "t" contains null values`. `ATTACH PARTITION` builds the indexes of the attached table before it checks its rows against the partition constraint and the inherited foreign keys.

## Check constraints

```sql
CREATE TABLE measurements (
    id INTEGER PRIMARY KEY,
    value DOUBLE PRECISION NOT NULL,
    CHECK (value >= 0 AND value <= 1)
);
```

A check rejects rows for which its predicate is false. Follow SQL three-valued logic: a NULL result does not replace a separate `NOT NULL` requirement.

A CHECK takes the name its `CONSTRAINT` clause gives it. An unnamed CHECK is named after the table, the one column its expression references when it references exactly one, and `check`, with the first numeric suffix that no constraint of the schema holds, whether the statement writes it on a column or on the table: `a int CHECK (b > 0)` is named `t_b_check` and `a int CHECK (a > b)` is named `t_check`. `CREATE TABLE` names its CHECKs in written order. A name that an earlier CHECK of the statement took, given or chosen, reports `42710` `check constraint "x" already exists`, and a name that a key or foreign key a partition clones from its parent holds reports `42710` `constraint "x" for relation "t" already exists`. A named CHECK that matches an inherited one merges with it, with the notice `merging constraint "x" with inherited definition`, and a `NO INHERIT` CHECK on a partitioned table reports `42P16` `cannot add NO INHERIT constraint to partitioned table "t"`. Names written on `DEFAULT`, `NULL`, generation and identity clauses are ignored, as PostgreSQL ignores them.

A row that violates a NOT NULL or CHECK constraint reports `23502` or `23514` with PostgreSQL's message, which names the relation without its schema, and the detail `Failing row contains (...)`; a row that the `WITH CHECK OPTION` of an auto-updatable view rejects reports `44000`, `new row violates check option for view "v"`, with the same detail. The detail prints each value in its type's text output form, `null` for a NULL and `virtual` for a virtual generated column, and cuts a value longer than 64 bytes at a character boundary, marking the cut with `...`. A row that a statement routes to a partition, or updates in an inheritance child through its parent, is described in the columns of the relation the statement names, and a row that the `INSTEAD OF` trigger of an underlying view returns is described in the columns of that view, to which the statement was rewritten. A role without `SELECT` on that relation sees only the columns it may read or the statement supplies, as `Failing row contains (a, b) = (1, x).`, and no detail when there is no such column; a referential action describes its row as the owner of the referencing table, who performs it. NOT NULL columns are checked in column order, those of virtual generated columns last, and CHECK constraints in the order of their names, so a row that violates several reports the first. `ALTER TABLE` reports an existing row that violates a constraint as `column "c" of relation "t" contains null values` or `check constraint "c" of relation "t" is violated by some row`, without a detail.

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

A foreign key that is not deferred is checked when the statement has written its rows, as PostgreSQL runs its internal referential triggers: a row may reference a row that the same statement writes after it, so `INSERT INTO tree VALUES (2, 1), (1, NULL)` succeeds on a self-referencing table, and a unique, NOT NULL or CHECK violation of any row of the statement is reported before a foreign key violation. A missing referenced row reports `23503` with the detail `Key (parent_id)=(5) is not present in table "parent".`, and `MATCH FULL` reports a key that mixes NULLs with values the same way with the detail `MATCH FULL does not allow mixing of null and nonnull key values.`. A delete or an update that removes a referenced key is checked at the same point: under `NO ACTION` the key is satisfied when another row of the referenced table holds it again by then, and a referencing row reports `23503` with the detail `Key (id)=(1) is still referenced from table "child".`; `RESTRICT` accepts no other row and reports `23001`, `update or delete on table "parent" violates RESTRICT setting of foreign key constraint "child_parent_id_fkey" on table "child"`, with the detail `Key (id)=(1) is referenced from table "child".`. A statement that deletes the referencing rows together with the referenced ones passes both. The checks take their `FOR KEY SHARE` lock on a referenced row when they run.

The checks run among a row's `AFTER` row triggers in the order of the triggers' names, as PostgreSQL's internal triggers named `RI_ConstraintTrigger_a_...` for a referenced key and `RI_ConstraintTrigger_c_...` for a referencing row do: a trigger whose name sorts before them, such as `"A_fill"`, runs before the check, and one named `a_fill` after it. A row that an `UPDATE` moves to another partition is checked as a row inserted into its new partition, the referenced keys it held as keys of the table the `UPDATE` names, and the keys of a foreign key that references its old partition itself as keys deleted from that partition. A deferred check reports the same errors at `COMMIT` or `SET CONSTRAINTS ... IMMEDIATE`.

The actions `CASCADE`, `SET NULL` and `SET DEFAULT` run at the same point and in the same order, as PostgreSQL's internal action triggers do, so an `AFTER` row trigger of the referenced table named `"A_log"` still sees the referencing rows that the action of the same row deletes. Each action runs as a statement of its own on the foreign key's table: that table's `BEFORE` statement triggers fire once for the statement, its `BEFORE` row triggers fire as the action writes each row, and the `AFTER` events of the rows it writes follow every event the statement has already queued, so the referencing table's `AFTER` row triggers fire after the referenced table's `AFTER` statement triggers. An action that finds no referencing row still fires its table's statement triggers, and a unique violation of a later row of the statement is reported before a violation that an action causes.

When the referenced column list is omitted, as in `REFERENCES parent`, the referenced table's primary-key columns are inferred in declaration order. Column and table foreign-key declarations in `CREATE TABLE`, plus `ALTER TABLE ... ADD ... FOREIGN KEY`, resolve the referenced table through the active role's effective schema `USAGE` boundary once and store its canonical identity; a missing qualified schema reports `3F000`. A later non-null child-key write keeps that exact identity but checks the executing role's current `USAGE` on the referenced schema before reading the parent, matching PostgreSQL 18. Explicit or inferred referenced columns must form a primary-key or unique key, the referencing and referenced column counts must match, and each aligned type pair must support equality comparison. Mutations validate referential actions as part of the same transaction.

Initial conversion of legacy foreign keys resolves an unqualified stored target against all stored tables before assigning its index incarnation. Exactly one table must match, independently of the session search path; missing or ambiguous targets fail the conversion before writes. The converted canonical target survives later same-name tables in other schemas. Current-format target or index corruption is rejected without reinterpreting the declaration as legacy metadata.

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

`NOT VALID` skips the existing-row scan while an enforced constraint still checks every new or changed row. `VALIDATE CONSTRAINT` scans existing rows and publishes `convalidated = true` only after the complete scan succeeds. On a partitioned table, `ADD FOREIGN KEY` and `VALIDATE CONSTRAINT` scan the rows of each leaf partition through its copy, report a violation under the partition's name and the copy's name, skip the partitions below a copy that is already valid, and mark every copy valid. Changing a foreign key from `NOT ENFORCED` to `ENFORCED` performs the same failure-atomic scan. PostgreSQL does not permit changing CHECK or named `NOT NULL` enforceability, and UQA Engine returns the corresponding error instead of approximating that operation. `ALTER CONSTRAINT` of a partitioned table's foreign key changes the copies on all its partitions, which it locks AccessExclusive, and an enforceability change locks the referenced table and its partitions, ShareRowExclusive for `ENFORCED` and AccessExclusive for `NOT ENFORCED`. `ALTER TABLE ONLY` of a partitioned table's constraint reports `42P16`, `constraint must be altered in child tables too`, and `ALTER CONSTRAINT` of a partition's copy reports `55000`, `cannot alter constraint`, with a detail that names the foreign key it derives from and a hint to alter that one instead.

A named `NOT NULL` constraint can be declared inline, in table-constraint form, or through `ALTER TABLE ... ADD CONSTRAINT name NOT NULL column [NOT VALID] [NO INHERIT]`. Its `pg_constraint` row uses `contype = 'n'`, its validation and inheritance flags survive reopen, and dropping it clears `pg_attribute.attnotnull`. Primary-key membership prevents removal with `42P16`. Identity columns reject named constraint removal with `55000` and `ALTER COLUMN ... DROP NOT NULL` with `42601`; serial columns retain their separate nullable behavior.

Removing an inheritable NOT NULL constraint, including through `ALTER COLUMN ... DROP NOT NULL`, recurses by column into children that have no local declaration or remaining supplying parent. CHECK removal follows the same rule using the constraint name. A child with another origin keeps its constraint, and traversal stops there. `ONLY` preserves each direct child constraint and makes it local when its last parent origin disappears. Directly removing a constraint that still has an inherited origin reports `42P16`. Removal retains AccessExclusive on each inspected child, follows its original identity through concurrent rename and name reuse, and leaves descendants below retained constraints unlocked. Transaction and savepoint rollback restore constraints and release the corresponding locks.

`ALTER TABLE ... RENAME CONSTRAINT` preserves the identity of CHECK and NOT NULL constraints and renames inherited constraints by their existing constraint name. Descendants with a different NOT NULL constraint name cause `42704`; `ONLY` on an inheritable constraint with children and direct renaming of an inherited constraint cause `42P16`. NO INHERIT constraints change only locally. Each selected child retains AccessExclusive on its original relation through waits, and rollback restores names and releases the corresponding locks. NOT NULL OIDs also survive column and table renames and persistent reopen; removing and recreating the same named constraint gives it a new identity.

PRIMARY KEY, UNIQUE and CHECK constraint OIDs survive table and column renames and persistent reopen. Dropping and recreating a constraint produces a new catalog identity; transaction and savepoint rollback restore the original identity. Each partition key has its own constraint identity, including keys added during attachment.

`ALTER TABLE ... RENAME CONSTRAINT` also renames column and table foreign keys while preserving their public OID and deferred checks. Each partition copy has an independent catalog identity; renaming a parent or child changes only the selected relation, including with `ONLY`. The operation retains AccessExclusive on that relation and does not add locks on the referenced table or other partitions. Name conflicts report `42710`. Statement and savepoint rollback restore the original name and identity; committed names and OIDs survive reopen, and removal/recreation allocates a new identity.

Validation of an unvalidated inheritable CHECK or NOT NULL constraint retains ShareUpdateExclusive on every descendant and validates the children before the parent. Child NOT NULL constraints are matched by column, preserving independently chosen constraint names. `ALTER TABLE ONLY ... VALIDATE CONSTRAINT` reports `42P16` when those children must also be validated; a NULL in a child reports `23502` and leaves validation changes rolled back. NO INHERIT constraints and already validated constraints skip descendant validation and locks. An unvalidated foreign key retains RowShare on its referenced table; a reference renamed during the wait keeps its original identity. These validation locks last until transaction completion or rollback to the savepoint preceding their acquisition.

An `INITIALLY DEFERRED` foreign key is checked exactly once before temporary-table `ON COMMIT` actions at the outer transaction commit. A deferred check that a delete or key update of a referenced row fired reports the referenced side when it fails, `update or delete on table "pk" violates foreign key constraint "fk_a_fkey" on table "fk"`, as PostgreSQL's deferred `NO ACTION` trigger does. The final transaction state may therefore insert the child before its parent or temporarily delete a referenced parent, and savepoint rollback removes pending checks introduced after that savepoint. `SET CONSTRAINTS { ALL | name [, ...] } { DEFERRED | IMMEDIATE }` changes implemented deferrable foreign-key modes for the current transaction; names may be schema-qualified, an unqualified name resolves every match in the first matching effective `search_path` schema including the configured position of `pg_temp`, an allocated temporary namespace remains resolvable after its last object is dropped unless its first allocation is rolled back, and `ALL` ignores non-deferrable constraints when selecting a new mode and remains the default for deferrable constraints created later in the transaction.

Changing a mode to `IMMEDIATE` checks pending row events retroactively in event order across nested transaction frames and leaves the prior mode in place if validation fails; `ALL IMMEDIATE` also fires events queued while a constraint was deferrable even if its catalog state later becomes non-deferrable. Each event remains bound to the durable identity of the exact originating constraint and follows its target row across a primary-key identity rewrite, so a same-name replacement in the current or another session cannot inherit its mode or capture its event. Partition-inherited events additionally retain their exact physical relation when unrelated constraint DDL reconciles transaction state.

Dropping a foreign key from a partitioned root removes every inherited clone, a direct drop on a clone reports `42P16`, and a root drop reports `55006` when a physical clone has a pending event. A dependency cascade caused by dropping the referenced key or table removes a child foreign key and its child-side pending events while retaining the child rows, matching PostgreSQL 18; an event fired by the DDL target itself still blocks the target rewrite.

Foreign-key removal retains AccessExclusive on the referencing relation, each removed partition clone, and the referenced relation, including for `NOT ENFORCED` constraints. Dropping a referenced key, column, or index with `CASCADE` retains the same locks on dependent foreign keys. Dependency waits follow the original relation and constraint identities through renames and name reuse; a replacement constraint is preserved, and subsequent publication retains refreshed metadata. `DROP INDEX` checks its current foreign-key dependencies after acquiring the parent table lock, so foreign keys added or removed during that wait affect both `RESTRICT` and `CASCADE`. Transaction and savepoint rollback restore removed constraints and release locks acquired within the reverted scope.

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

`ALTER TABLE [IF EXISTS] name action` takes a relation identifier and returns the `ALTER TABLE` command tag without rows. Target binding retains the requested name through relation-lock waits, rechecks current ownership and system-catalog protection, and dispatches actions using the resulting relation kind. If the source disappears, `IF EXISTS` emits one notice and makes no change; otherwise a missing relation reports `42P01` and a missing explicit schema reports `3F000`. An unqualified source is resolved again through the search path after a wait. `RENAME TO` additionally requires CREATE on the source schema, including current database TEMP authority for a temporary table, both before and after waiting. Column changes do not require that rename privilege.

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

`ALTER TABLE ... ADD COLUMN` first rejects a directly targeted partition with `42809` `cannot add column to a partition`, including when `IF NOT EXISTS` names an existing column. For an ordinary table, a system-column name or an existing column without `IF NOT EXISTS` reports `42701` before resolving the proposed type or checking its clauses. An existing ordinary column with `IF NOT EXISTS` emits a `42701` notice and skips those checks, even if the proposed type does not exist. Adding a column to the partitioned parent continues to propagate it to its partitions.

Adding a column publishes its declaration, physical fields, default values and key constraints in one transaction. A `serial` or identity column creates its sequence once, before the column is added, and fills the rows the table holds with that sequence's values; an existing column reports `42701` before any sequence is created. A partition's new identity column draws from its parent's sequence and an inheritance child's new `serial` column from its parent's default, while an identity column cannot be added to a table with inheritance children, because identity is not inherited, and reports `42P16`. `ADD COLUMN k TEXT UNIQUE DEFAULT 'key1'` preserves an existing row and enforces that default through reopen. If the default would create duplicate keys, the statement reports `23505` with `could not create unique index` and a separate duplicate-key DETAIL, leaving the original rows and schema intact. Savepoint rollback also removes the new column's analyzer and index state.

The historical `ALTER TABLE name OWNER TO role` spelling also accepts regular views, materialized views, sequences and foreign tables, with the same owner-transfer checks as their explicit ALTER commands. After a concurrent replacement of the requested name, the current relation kind selects that command's execution path.

Trigger enable/disable and `ADD FOREIGN KEY` retain SHARE ROW EXCLUSIVE on the target, while `VALIDATE CONSTRAINT` and `ATTACH PARTITION` use SHARE UPDATE EXCLUSIVE. Combined actions retain the strongest required mode. Foreign-key addition, including a new column's REFERENCES declaration, also retains SHARE ROW EXCLUSIVE on the referenced table. `INHERIT` requires ownership of its parent and takes SHARE UPDATE EXCLUSIVE there, plus ACCESS SHARE on existing child descendants for cycle validation; `NO INHERIT` requires only child ownership and takes ACCESS SHARE on the parent. Attachment requires ownership of the attached table and locks its subtree and the default partition subtree before publication. Inherited column and constraint addition, CHECK/NOT NULL validation and renaming retain selected child identities, so a concurrent rename cannot redirect a change to a replacement using the former name. Explicit addition, attachment and inheritance references are resolved again after waits; foreign-key validation retains its stored reference identity. All these locks participate in transaction completion and savepoint rollback.

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

`ALTER COLUMN` actions check their column as PostgreSQL does before they change anything: a name the relation lacks reports `42703` (`column "c" of relation "t" does not exist`) and a system column reports `0A000`. `SET DEFAULT` and `DROP DEFAULT` reject an identity column or a generated column with `42601` and PostgreSQL's hint to use `DROP IDENTITY`, `SET EXPRESSION` or `DROP EXPRESSION` instead. `SET EXPRESSION` and `DROP EXPRESSION` report `55000` for a column that is not generated, which `DROP EXPRESSION IF EXISTS` skips with a notice, and the expression of a virtual generated column cannot be dropped (`0A000`). `DROP NOT NULL` keeps an identity column NOT NULL (`42601`), and a partition's column while its parent's column is NOT NULL (`42P16`).

Column renames preserve creation-bound references in SQL-standard function and procedure bodies, including SELECT, INSERT, UPDATE, DELETE, and MERGE. Bound table columns change while relation aliases, CTE outputs, function parameters, declared result columns, and view outputs retain their identities. The table owner may rename a column used by a routine in an inaccessible schema. Recreating the old column name does not redirect stored references. String-literal SQL bodies continue resolving their original source text at execution. Missing source columns report `42703`, and duplicate destination names report `42701` before mutation. The changes follow transaction and savepoint rollback, sibling-engine catalog refresh, and SQLite reopen.

```sql execute
CREATE TABLE renamed_amounts (id integer, amount integer);
INSERT INTO renamed_amounts VALUES (1, 42);
CREATE FUNCTION renamed_amount_reader() RETURNS integer
    LANGUAGE SQL BEGIN ATOMIC SELECT amount FROM renamed_amounts; END;
ALTER TABLE renamed_amounts RENAME COLUMN amount TO total;
ALTER TABLE renamed_amounts ADD COLUMN amount integer DEFAULT 1000;
SELECT renamed_amount_reader();
```

`ALTER TABLE name DROP COLUMN [IF EXISTS] column [RESTRICT | CASCADE]` takes a column identifier and defaults to RESTRICT. It returns the `ALTER TABLE` command tag without rows. SQL-standard function and procedure bodies retain dependencies on columns read by queries and INSERT, UPDATE, DELETE, and MERGE expressions; INSERT and UPDATE destination columns also establish dependencies. A dependent routine blocks RESTRICT with `2BP01`. Missing columns report `42703`, while `IF EXISTS` skips a missing column after relation and owner validation.

CASCADE follows stored routine dependencies through generated columns, views, owned sequences, other routines, and domains, and removes dependent defaults, CHECK constraints, indexes, and inbound foreign keys through the corresponding object lifecycle. Unrelated columns, rows, and routines survive. The table owner's authority permits removal of a dependent routine in an inaccessible schema. Statement and savepoint failures roll back the column and dependent objects together, committed changes refresh sibling engines, and stored definitions survive SQLite reopen. The Rust `Engine::drop_column` API also protects stored readers with RESTRICT behavior.

Removing dependent columns, defaults or CHECK constraints through `DROP FUNCTION`, `DROP SCHEMA`, `DROP DOMAIN` or `DROP SEQUENCE ... CASCADE` retains an `ACCESS EXCLUSIVE` lock on each affected table until transaction end or rollback to a preceding savepoint. These changes wait for concurrent `ANALYZE` and prepare their storage writes after the lock is acquired.

Stored SQL-standard routines retain the creation-time input columns of ordinary and foreign table sources. Deleting an unread column removes its positional alias from table and enclosing join alias lists; adding columns, including reuse of a deleted name, does not change the surviving bindings or expanded projections. Renames update the retained physical names while preserving SQL aliases. This applies to nested joins, CTEs, subqueries, query and mutation-command bodies, and procedures. Source shape metadata alone does not create a read dependency: a routine that only counts rows can survive removal of every column.

```sql execute
CREATE TABLE alias_kept_source (discarded integer, amount integer);
INSERT INTO alias_kept_source VALUES (5, 42);
CREATE FUNCTION alias_kept_reader() RETURNS integer
    LANGUAGE SQL BEGIN ATOMIC SELECT s.kept FROM alias_kept_source AS s(unused, kept); END;
ALTER TABLE alias_kept_source DROP COLUMN discarded;
ALTER TABLE alias_kept_source ADD COLUMN discarded integer DEFAULT 1000;
SELECT alias_kept_reader();
```

A MERGE destination used only for writing does not establish a column dependency. A retained stored MERGE routine skips writes to that deleted column, including evaluation of their value expressions, while continuing to mutate surviving columns. Those stored expressions keep their routine and sequence dependencies. Non-DEFAULT assignments also retain the original destination domain dependencies, including domains inside arrays, after column removal; omitted destinations and DEFAULT assignments do not add those coercion dependencies. Reusing the old column name does not redirect the retired write. The same behavior applies to implicit INSERT destination lists, MERGE command CTEs, procedures, transaction rollback, catalog refresh, and durable reopen.

```sql execute
CREATE TABLE removed_amounts (id integer, amount integer);
INSERT INTO removed_amounts VALUES (1, 42);
CREATE FUNCTION removed_amount_reader() RETURNS integer
    LANGUAGE SQL BEGIN ATOMIC SELECT amount FROM removed_amounts; END;
CREATE FUNCTION retained_id_reader() RETURNS integer
    LANGUAGE SQL BEGIN ATOMIC SELECT id FROM removed_amounts; END;
ALTER TABLE removed_amounts DROP COLUMN amount CASCADE;
SELECT to_regprocedure('removed_amount_reader()') IS NULL AS reader_removed,
       retained_id_reader() AS retained_id;
```

`ALTER COLUMN name TYPE type [USING expression]` analyzes the transform against the original column definitions before checking the target or resolving the new type, even on an empty table. Missing columns and routines therefore precede target errors. Transforms reject subqueries and set-returning functions with `0A000`, aggregates with `42803`, window functions with `42P20` and parameters with `42P02`, preserving nested argument analysis order. Target checks then reject missing and system columns, `USING` on generated columns (`42611` with DETAIL), inherited columns (`42P16`) and columns referenced by partition keys (`42P16`) before resolving the new type. An identity column's sequence type is checked earlier, as PostgreSQL's preparatory `ALTER SEQUENCE` checks it. An incompatible assignment reports `42804` and PostgreSQL's hint; unknown literals pass through the target input function at analysis. Constant planning follows these checks, so `USING 1 / 0` fails on an empty table while an unreachable constant branch is discarded. Volatile calls run for actual rows, never just to validate an empty table.

Multiple `ALTER COLUMN ... TYPE` actions in one statement evaluate every analyzed transform against the same original typed row, in written order within the type-change actions. A transform can read a column dropped by that statement, but cannot read a newly added column. Repeated actions on a column retain PostgreSQL's original-type check: a second action after an actual type change reports `0A000`, while repeated same-type transforms each read the original value. Added defaults follow type transforms for each row, and stored generation expressions use the completed replacement row. Successful rewrites publish the complete relation replacement; self-modifying `USING` callbacks do not add rows to that relation's captured input. A callback's committed changes to a descendant that has not yet been rewritten enter that descendant's input, while rolled-back changes do not.

Recursive type changes lock and process each descendant once, including a relation reached through multiple inheritance paths. Each child's transform binds to that child's original columns, so different physical column layouts preserve the referenced values. `ONLY` on a relation with descendants and a changed column inherited from an outside parent report `42P16`; partition-key checks also apply to descendants. Original inputs, replacement rows and callback-change identities spill to encrypted temporary storage under one statement allowance instead of retaining complete row or identity vectors.

Validation checks each replacement row's validated NOT NULL and CHECK constraints before checking keys across the complete replacement relation. Rebuilt validated foreign keys involving changed columns are checked against the completed relations, whatever the replication role; unrelated foreign keys and `NOT VALID` constraints are not revalidated. A row may take a key value that another row gives up; a repeated key reports `23505`, `could not create unique index "t_pkey"`, with the detail `Key (id)=(0) is duplicated.`, and a primary key column left NULL reports its NOT NULL violation. A row whose integer primary key changes moves to the identity its new key names. Adding a stored generated column and changing its expression validate the same way, including when combined with a virtual-column type change. Built-in ranges can be rewritten to their paired multirange with `USING multirange(column)` while retaining `WITHOUT OVERLAPS`; changing one side of an existing `PERIOD` relationship to an incompatible range identity is rejected with PostgreSQL 18 datatype-mismatch SQLSTATE `42804`. Schema, rows and index changes share the enclosing statement and transaction rollback boundary and persist across reopen on durable providers.

```sql execute
CREATE TABLE rewrite_pair (id integer PRIMARY KEY, a integer, b integer);
INSERT INTO rewrite_pair VALUES (1, 3, 7);
ALTER TABLE rewrite_pair
    ALTER COLUMN a TYPE integer USING b,
    ALTER COLUMN b TYPE integer USING a;
SELECT a = 7 AND b = 3 AS original_inputs_preserved FROM rewrite_pair;
```

The role active at `CREATE TABLE` owns the ordinary table. ALTER requires the current role to be a superuser or to inherit the table owner, while DROP also permits the owning role of the containing schema. `ALTER TABLE name OWNER TO role` requires an existing target role; an actual change also requires a SET-enabled path to it and target-role `CREATE` on the containing schema unless the caller is a superuser. An unchanged owner skips the SET and CREATE checks but still requires table-owner authority. Owner transfer preserves relation and storage identities, rewrites owned serial and identity sequence ownership, updates table and index `pg_class.relowner` plus `pg_tables.tableowner`, blocks removal of dependent roles, and follows transaction, savepoint, temporary-table, cross-engine refresh, and durable-reopen lifecycle. Table ACLs and owner checks on the remaining relation-administration paths outside this standalone-index boundary are still open compatibility bugs.

## Relational B-tree indexes

The default access method is B-tree:

```sql
CREATE INDEX orders_state_idx ON orders (state);
CREATE UNIQUE INDEX orders_account_state_uq ON orders (account_id, state);
ALTER INDEX orders_state_idx RENAME TO orders_state_lookup;
DROP INDEX orders_state_lookup;
```

An index belongs to its table's schema and has no independent owner: creation requires inherited ownership of the table, and `pg_class.relowner` always follows that table's owner. `CREATE INDEX` first applies schema `USAGE` during table lookup, then checks table ownership, schema `CREATE`, and the index name and definition in PostgreSQL order. `DROP INDEX` permits inherited table-owner authority or ownership of the containing schema, validates every named index before deleting any, and does not let `IF EXISTS` bypass authorization for an existing index. The durable index identity stores its schema and local name as separate components. Distinct schemas may therefore contain indexes with the same local name, while an index cannot share one schema-local relation name with a table, view, materialized view, sequence, or foreign table. Unnamed indexes allocate PostgreSQL-style schema-local names such as `orders_state_idx` and then `orders_state_idx1`. `DROP INDEX` resolves one exact identity through the effective `search_path`, skips inaccessible unqualified schemas, and applies qualified schema `USAGE`, missing-schema, missing-index, wrong-relation-kind, and owner-error precedence before mutation. Index `regclass` values and the corresponding `pg_class`, `pg_index`, and `pg_indexes` rows use the same identity and remain stable across owner transfer, transaction rollback, catalog refresh, and durable reopen. Indexes on temporary tables stay session-local and are never written to the durable catalog.

`CREATE INDEX IF NOT EXISTS` still validates its declared expressions, predicate, access-method capabilities, options and columns before skipping an occupied relation name. Invalid definitions retain their ordinary PostgreSQL diagnostics; the clause does not hide missing columns, non-immutable expressions or invalid options. A valid skip emits a notice and leaves the existing relation unchanged without scanning existing rows, evaluating index expressions on data or rebuilding a physical index. B-tree and GIN option declarations preserve PostgreSQL Boolean/numeric parsing, duplicate-option, namespace and range diagnostics; UQA's vector-method and analyzer options retain their documented meanings.

`ALTER INDEX [IF EXISTS] name RENAME TO new_name` changes the schema-local relation name and returns the `ALTER INDEX` command tag. The target name is an identifier. An owned PRIMARY KEY or UNIQUE index also renames its constraint; the constraint/index OIDs, foreign-key dependencies, partition parent edges and physical keys remain unchanged. The current role must inherit the indexed table owner and have schema `CREATE`. A relation-name collision raises `42P07`; an owned constraint-name collision raises `42710`. The index retains ShareUpdateExclusive through the transaction, allowing independent document writes and renames of other indexes. An explicit name is resolved again after a wait, while dependent constraint and table removal follow the retained index incarnation. `ALTER TABLE index_name RENAME TO new_name` retains AccessExclusive. Renaming a partition parent leaves child names unchanged, and rollback, savepoints and reopen preserve the same identities.

`DROP INDEX` retains an exclusive definition lock on the index, the indexed table and, for a partitioned index, its descendant tables. After a lock wait it resolves each explicit name again and rechecks the index incarnation, indexed table, relation kind and current authority, including under REPEATABLE READ. A replaced index or renamed table releases the provisional lock on the previous table before acquiring the current target. A disappeared index follows the same missing-index and `IF EXISTS` behavior as an initially absent index; a replacement table or constraint-owned index keeps its respective wrong-kind or dependency error. Constraint-owned index removal acquires its table locks before reporting the dependency error. Savepoint undo restores removed catalog and physical index state and releases the locks acquired after the savepoint; ordinary inheritance does not propagate index locks.

`CREATE UNIQUE INDEX` checks existing rows before publishing the index and enforces the complete key during INSERT, UPDATE, COPY, and conflict handling. A repeated key reports `could not create unique index` with the repeated key, and a unique index of a partitioned table follows the partition key rule of [key constraints](#key-and-uniqueness-constraints). A failed build leaves no index behind, and a failed multirow mutation rolls back the statement. The default treats NULL key fields as distinct; `NULLS NOT DISTINCT` makes NULL compare equal for uniqueness. Multiple matching unique indexes all participate in `ON CONFLICT` arbitration.

B-tree keys may combine table columns and immutable scalar expressions, such as `lower(email)` or `(quantity * price)`. Expressions bind against the declared table row type and retain their result types and routine identities. Subqueries, aggregates, window functions, set-returning calls, and non-immutable functions are rejected. A unique expression index compares the complete evaluated key, including NULL handling, during builds and subsequent mutations. Stored expression keys survive reopen and rollback; replacing an immutable function's body does not retroactively recompute existing keys.

A partial index's `WHERE` predicate must be an immutable Boolean expression. Uniqueness applies only when the predicate is true for both rows; false and NULL exclude a row. Updates that change predicate columns recheck membership. `ON CONFLICT (columns_or_expressions) WHERE predicate` selects indexes with matching keys whose predicates are implied by the target, while a missing matching arbiter raises `42P10`, including for empty input. `ON CONFLICT ON CONSTRAINT name` selects the named constraint. Inference expressions and predicates resolve INSERT target aliases and projected or computed view columns in the public target row type; an inference predicate may contain additional volatile conditions without executing them during arbitration. Included columns contribute catalog attributes and reconstructed definitions without becoming unique key fields. Column order and NULL placement are preserved in index metadata. `pg_attribute` exposes creation-time index attribute names and declared types; renaming a source column retains the index attribute's original name. `pg_index.indkey` uses zero for an expression key, and `indexprs` contains its reconstructed expression.

`INCLUDE (column, ...)` names columns that a B-tree index carries beside its key, as in PostgreSQL. They take no part in ordering, uniqueness or search, may repeat, and may also be key columns. Other access methods reject them with `0A000`, as do an expression and a virtual generated column. An index holds the stored value of every plain key column and every included column for each row, including rows outside a partial index's predicate.

A query over one table whose predicate selects rows through an index, and whose referenced columns are all held by the table's indexes, is answered from the index entries without reading the rows, as PostgreSQL's index-only scan does. Key columns alone are enough, and the columns may come from different indexes of the table. A row-locking query, a query that references a column no index holds, a virtual generated column or `xmin`, and a query on a transaction snapshot other than the session's current table state read the rows; the results are the same either way. `SET enable_indexonlyscan = off` turns index-only reads off for the session, with PostgreSQL's setting name, Boolean values and `pg_settings` row.

```sql execute
CREATE TABLE covered_orders (order_id integer PRIMARY KEY, account_id integer, state text, total numeric(10, 2));
CREATE INDEX covered_orders_account ON covered_orders (account_id) INCLUDE (total);
INSERT INTO covered_orders VALUES (1, 7, 'open', 12.50), (2, 7, 'paid', 30.00), (3, 8, 'open', 4.25);
SELECT sum(total) AS account_total FROM covered_orders WHERE account_id = 7;
SELECT pg_get_indexdef('covered_orders_account'::regclass) AS definition;
SET enable_indexonlyscan = off;
SELECT sum(total) AS account_total FROM covered_orders WHERE account_id = 7;
RESET enable_indexonlyscan;
```

PRIMARY KEY and UNIQUE constraints expose their supporting indexes through `pg_class`, `pg_index`, and `pg_indexes`. `pg_constraint.conindid` identifies the constraint's supporting index or a foreign key's selected referenced index. Directly dropping a constraint-owned index raises `2BP01`; drop its constraint instead. A foreign key can reference a non-partial unique index whose keys are ordinary columns and retains that index dependency across reopen. `DROP INDEX` rejects a referenced index unless CASCADE removes the dependent foreign keys. Index key expressions and partial predicates retain routine identities across function renames; DROP FUNCTION RESTRICT protects dependent indexes and CASCADE removes them. Key expressions, predicates, and included-column references follow column renames and durable reopen in SQLite and key-value providers.

```sql execute
CREATE TABLE indexed_accounts (account_id integer, email text, active boolean);
CREATE UNIQUE INDEX active_email ON indexed_accounts (lower(email)) NULLS NOT DISTINCT WHERE active;
INSERT INTO indexed_accounts VALUES (1, 'One@example.test', true), (2, 'one@example.test', false);
INSERT INTO indexed_accounts VALUES (3, 'one@example.test', true)
ON CONFLICT (lower(email)) WHERE active DO UPDATE SET account_id = excluded.account_id;
SELECT account_id, email, active FROM indexed_accounts ORDER BY account_id;
SELECT pg_get_indexdef('active_email'::regclass) AS definition;
```

The complete expression-type, type-change, predicate-implication, optimizer, operator-class, collation, concurrent-build, index-administration, and partition-index lifecycle matrices remain open compatibility work. Expression keys for access methods other than B-tree remain open compatibility bugs.

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

HNSW option values are unsigned integers. Underscore and documented hyphenated aliases are accepted. A field can have only one physical IVF, HNSW or DiskANN index at a time.

## DiskANN vector indexes

UQA Engine 0.4.9 supports `USING diskann` on one `VECTOR(n)` or `TENSOR(n)` field. Opening an older supported database applies the [persistent format upgrades](../reference/10-upgrading.md#045-vector-indexes-and-storage-formats).

```sql execute
CREATE TABLE diskann_items (id INTEGER PRIMARY KEY, embedding VECTOR(2));
INSERT INTO diskann_items VALUES
    (1, ARRAY[1.0, 0.0]), (2, ARRAY[0.0, 1.0]), (3, ARRAY[0.9, 0.1]);
CREATE INDEX diskann_items_embedding ON diskann_items USING diskann (embedding)
WITH (max_degree = 2, search_list_size = 4, beam_width = 2, pq_bytes = 1);
SELECT id, _score FROM diskann_items
WHERE knn_match(embedding, ARRAY[1.0, 0.0], 2)
ORDER BY _score DESC, id;
```

| Option | Default | Constraint |
| --- | --- | --- |
| `max_degree` | 64 | Integer at least 2 |
| `build_list_size` | 128 | Integer at least `max_degree` |
| `search_list_size` | 64 | Positive integer |
| `alpha` | 1.2 | Finite real number at least 1 whose square is finite |
| `beam_width` | 4 | Integer from 1 through `search_list_size` |
| `pq_bytes` | `min(n, 32)` | Integer from 1 through the field dimension `n` |
| `seed` | 42 | Unsigned 64-bit integer, including zero |

Option names are case-insensitive; unknown, repeated or cross-method options are rejected. Defaults are resolved against the field dimension before construction, and buffer sizes must fit the platform. Configuration is retained in the index definition. PQ guides approximate candidate selection; final scores use canonical cosine, with the maximum element score for tensors. Threshold retrieval remains exact. See [retrieval and diagnostics](06-retrieval.md#diskann-plan-diagnostics).

Memory, native SQLite, SQLite Key/Value and redb preserve DiskANN publication, private writes, rollback and retained readers through their existing transaction interfaces. `DROP INDEX` removes physical index ownership and returns the field to exact search without deleting its canonical vectors. The [binding examples](../../../examples/README.md) use the same SQL configuration and typed parameters.

SQL `CREATE INDEX` accepts B-tree, GIN, IVF, HNSW and DiskANN. Other access methods, including R-tree, are not exposed by SQL DDL.

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

An optional view column-name list renames query outputs positionally and may name only a leading subset. It cannot contain more names than the query returns, the final names must be unique, and quoted names retain their exact spelling. `CREATE OR REPLACE VIEW` must preserve the name and declared type of every existing column in order, but it may append columns at the end. Creation analyzes the query without executing it, expands projection stars against the creation-time source row types, then validates the column list and target relation; the durable definition retains the fixed public names and row width across nested views, later base-column additions, transactions, and reopen. `TEMP` and `TEMPORARY` views are session-local, and a view over a temporary table or view becomes temporary as in PostgreSQL. The active creating role owns a view; inherited owner authority protects replacement and ALTER, while direct DROP also permits the owner of the containing schema. `ALTER VIEW ... OWNER TO` requires an existing target role and view-owner authority. An actual owner change additionally requires SET access to that role and its `CREATE` privilege on the containing schema unless the caller is a superuser. Ownership blocks dependent role removal, is shown by `pg_class.relowner` and `pg_views.viewowner`, and follows transaction, savepoint, temporary, cross-engine refresh, durable-reopen, and stable-OID lifecycle. View options `security_barrier`, `security_invoker`, and `check_option`, including `WITH [LOCAL | CASCADED] CHECK OPTION`, are validated, retained in `pg_class.reloptions`, and may be changed with owner-authorized `ALTER VIEW ... SET/RESET`.

Regular views support PostgreSQL table-shaped relation and column `GRANT` and `REVOKE`, including all eight relation privileges, column `SELECT`, `INSERT`, `UPDATE`, and `REFERENCES`, `PUBLIC`, independent rooted grant-option paths, dependent `RESTRICT` and `CASCADE`, default owner privileges and implicit grant options, role dependencies, owner-transfer grantor rewriting, `ALL TABLES IN SCHEMA`, and preservation through replacement, transactions, savepoints, temporary lifetime, cross-engine refresh, and reopen. `pg_class.relacl` and `pg_attribute.attacl` expose the durable ACLs, the name and OID forms of `has_table_privilege` and `has_column_privilege` accept views, and `information_schema.tables`, `views`, `columns`, `column_privileges`, and `role_column_grants` apply PostgreSQL visibility rules while excluding materialized views. A caller must hold the requested privilege on every regular-view boundary; the underlying query or automatically rewritten DML uses each view owner's privileges unless that view has `security_invoker=true`, in which case it retains the invoking privilege subject. SQL-visible `current_user` remains the caller while a regular view executes.

A single-source projection view is automatically updatable when its underlying table or view is automatically updatable and its target list does not contain a set-returning expression. `INSERT` values and queries, `ON CONFLICT`, `UPDATE`, `UPDATE FROM`, `DELETE`, `DELETE USING`, and `MERGE` write the base relation through its ordinary defaults, constraints, row and statement triggers, partition routing, and `RETURNING` path; an implicit INSERT may supply only the leading view columns and lets omitted writable columns take their base defaults. The public view row type is the DML name boundary, so unprojected base columns report `42703` even when the view shape itself is non-updatable; unqualified names owned only by a `FROM` or `USING` source remain source columns instead of colliding with hidden base columns, and ordinary source relations named `old` or `new` remain source relations outside explicit `RETURNING` row-image aliases. Computed and system-column projections, including `tableoid`, remain readable in predicates and `RETURNING` but are not writable; partitioned DML reports the physical leaf relation for current, `OLD`, `NEW`, and rule row images, including a partition-moving update. A view with no writable columns rejects INSERT and UPDATE with `55000` while remaining automatically deletable. A view that cannot be rewritten reports `55000` as PostgreSQL does, `cannot insert into view "v"`, `cannot update view "v"` or `cannot delete from view "v"`, with the reason `view_query_is_auto_updatable` gives as DETAIL, a conditional `INSTEAD` rule ahead of the view's query, and a HINT naming the `INSTEAD OF` trigger or unconditional rule that would allow the command, only the trigger for `MERGE`; a column it cannot write reports `0A000`, `cannot insert into column "c" of view "v"`, `cannot update column` or `cannot merge into column`, naming the first such column in view order with its reason as DETAIL. Each view is judged by its own query as the rewrite reaches it, so a view with `FOR UPDATE` is automatically updatable and a view over a materialized view is not, and the privileges of every view layer are checked once the rewrite is complete, so a rewrite error at any layer is reported before them. Normal PostgreSQL ambiguity checks apply between the target, `excluded`, and `FROM` or `USING` sources. Correlated scalar subqueries retain that complete containing DML namespace, including explicit `OLD` and `NEW` aliases in `RETURNING`. A bare `RETURNING *` from `UPDATE FROM` or `DELETE USING` emits view-target columns before source columns, while `MERGE` emits source columns before public view-target columns. Rewriting follows nested view aliases and predicates, permits a row to leave a view without a check option, and enforces nested `LOCAL` and `CASCADED` check options from the innermost view outward against the final row after `BEFORE` row triggers with statement atomicity, including `UPDATE FROM` and `MERGE`; a check option on a directly non-updatable view is rejected with `0A000` and the reason as HINT. `ALSO` and `INSTEAD` rules on every automatically rewritten view layer run in PostgreSQL rewrite order and may provide `RETURNING` for `INSERT`, `UPDATE`, and `DELETE`; `MERGE` rejects a user rewrite rule on any targeted view layer with `0A000`. An unconditional `INSTEAD` rule suppresses the original view mutation and its row and statement triggers, evaluates only input or assignment expressions required by matching rule conditions and actions, accepts supplied computed view columns when an action consumes them, and reports the affected-row count of the final executed action; a conditional `INSTEAD` rule without an unconditional `INSTEAD` rule does not make the view updatable. DML through an outer view may terminate at a nonautomatically updatable underlying view when that view has an applicable unconditional rule; outer view rule layers remain ordered around that boundary as in PostgreSQL. Rewriting checks each view it reaches only by that view's own query, as PostgreSQL's `rewriteTargetView` does, and `INSERT`, `UPDATE`, and `DELETE` through an outer view stop at an underlying view with an `INSTEAD OF` row trigger for the command, whether or not that view is automatically updatable; its trigger performs the command, and the outer views' check options validate the row the trigger returns. For `INSERT`, `UPDATE`, and `DELETE`, a defined `INSTEAD OF` row trigger selects the trigger path independently of whether `session_replication_role` suppresses that trigger; a suppressed trigger performs no automatic base write, while the row still counts, returns its proposed row, or its `OLD` row for `DELETE`, from `RETURNING`, and meets the outer check options, as PostgreSQL does. `information_schema.views`, `information_schema.tables`, and `information_schema.columns` expose automatic, trigger, and per-column updatability. Views with `TABLESAMPLE`, system columns other than `tableoid`, or whole-row columns cannot be defined yet, and complete optimizer effects for `security_barrier` remain open compatibility bugs.

For `MERGE`, every named mutation action must select one complete automatic or `INSTEAD OF` trigger path; PostgreSQL rejects a statement that mixes those paths, and a nonautomatically updatable view needs an action-specific trigger for each mutation kind. Automatic outer-view rewriting may stop at a trigger-updatable inner view. The final row returned by the trigger is then mapped back through the outer public row types and validated by their `LOCAL` or `CASCADED` check options. `DO NOTHING` requires no trigger, and a defined but replication-suppressed trigger path performs no fallback base write, although its actions still count and return their rows.

View creation holds `ACCESS SHARE` locks on its query sources, including `CREATE MATERIALIZED VIEW ... WITH NO DATA`. Queries also retain `ACCESS SHARE` on each regular or materialized view they read. `CREATE OR REPLACE VIEW`, `ALTER VIEW`, materialized-view owner changes and renames, and ordinary `REFRESH MATERIALIZED VIEW` acquire `ACCESS EXCLUSIVE` on the target before publishing changes. Materialized-view `fillfactor` SET/RESET uses `SHARE UPDATE EXCLUSIVE`, allowing readers while excluding concurrent refresh or option changes. A wait rechecks the selected relation identity and current ownership or maintenance privilege, and uses the refreshed definition. These locks last until transaction completion or rollback of the savepoint that acquired them. Concurrent creation of a previously absent view name waits for the preceding creator; its commit produces catalog uniqueness SQLSTATE `23505`, including for `CREATE OR REPLACE VIEW` and `CREATE MATERIALIZED VIEW IF NOT EXISTS`, while rollback allows creation to continue.

`CREATE MATERIALIZED VIEW` stores a query snapshot, supports `WITH [NO] DATA`, persists its rows and static schema across reopen, and remains stale until `REFRESH MATERIALIZED VIEW [WITH [NO] DATA]`; direct `INSERT`, `UPDATE`, and `DELETE` first enforce the requested materialized-view privilege and then report PostgreSQL's relation-kind SQLSTATE `42809`. The active creating role owns the materialized view; inherited owner authority protects ALTER, direct DROP also permits the owner of the containing schema, and `ALTER MATERIALIZED VIEW ... OWNER TO` applies the same target-role rules and durable lifecycle as regular views. Materialized views support the same durable relation and column ACL machinery as regular views; `SELECT` controls snapshot reads independently of source privileges, and `MAINTAIN` permits refresh without `SELECT` on either the snapshot or its sources. Owners hold `MAINTAIN` by default; ownership alone no longer permits refresh after they revoke it from themselves. Refresh evaluates the stored query with the owner's privileges and `current_user`, then restores the caller's session identity. `pg_class` exposes relation kind `m`, owner, ACL, population state, persistence, and reloptions, while `pg_attribute`, `pg_matviews`, `has_table_privilege`, and `has_column_privilege` expose the corresponding implemented metadata. The supported materialized-view option is `fillfactor`, including owner-authorized `ALTER MATERIALIZED VIEW ... SET/RESET`; temporary and unlogged materialized views, concurrent refresh, materialized-view indexes, access methods, tablespaces, and dependencies on temporary relations are not implemented.

Materialized-view creation analyzes the source query before resolving the target schema and checking for an existing relation. Any existing relation kind reports `42P07`; `IF NOT EXISTS` instead emits a `42P07` NOTICE, returns command tag `CREATE MATERIALIZED VIEW` and skips column-definition checks and source execution. For a new target, an excessive column-name list reports `42601` (`too many column names were specified`) before schema `CREATE` is checked. Schema authorization precedes duplicate column names, column-type `USAGE`, system-column names and pseudo-types. A partial column list renames the corresponding output positions and preserves the remaining source names. `WITH NO DATA` stores the analyzed definition without executing its source. Reading an unpopulated materialized view checks `SELECT` authority first, then reports `55000` with the unqualified relation name and HINT `Use the REFRESH MATERIALIZED VIEW command.`, including through a stored view or a `LIMIT 0` query.

`ALTER VIEW [IF EXISTS] name RENAME TO new_name` and `ALTER MATERIALIZED VIEW [IF EXISTS] name RENAME TO new_name` accept a relation identifier and an unqualified new identifier; the historical `ALTER TABLE` spelling accepts either relation kind. Rename returns no rows, preserves relation and row-type OIDs, owner, ACLs, stored columns, and materialized rows, and rewrites dependent view and rewrite-rule references in the same transaction. Transaction and savepoint rollback restore the previous name, and committed changes survive reopen. The caller must own the relation and have `CREATE` on its containing schema; for a temporary view, namespace CREATE follows current database `TEMP` authority. Actual-relation ownership and system-catalog protection precede the requested-kind check, and rename additionally checks source CREATE before that kind check. These checks repeat after a relation-lock wait, including when the original name now identifies a different relation. View option changes do not require source CREATE. An occupied target name reports `42P07`, a mismatched explicit relation kind reports `42809`, and a missing relation or schema fails unless `IF EXISTS` requests a notice and no change.

```sql execute
CREATE TABLE view_rename_source (id integer);
INSERT INTO view_rename_source VALUES (7);
CREATE VIEW view_rename_original AS SELECT id FROM view_rename_source;
CREATE VIEW view_rename_dependent AS SELECT id FROM view_rename_original;
ALTER VIEW view_rename_original RENAME TO view_rename_current;
SELECT id FROM view_rename_dependent;
CREATE MATERIALIZED VIEW view_rename_snapshot AS SELECT id FROM view_rename_current;
ALTER TABLE view_rename_snapshot RENAME TO view_rename_saved;
SELECT id FROM view_rename_saved;
```

View and materialized-view definitions can be reconstructed with [`pg_get_viewdef`](04-expressions-and-functions.md#view-definition-functions). Their catalog definitions contain SQL, and the `_RETURN` rewrite rule exposes the same query through `pg_get_ruledef`; that rule's OID follows the view object's rename and replacement lifecycle.

## CREATE TABLE AS

```sql
CREATE TABLE pending_orders (order_id, account_id, total) AS
SELECT order_id, account_id, total
FROM orders
WHERE state = 'pending';
```

CTAS creates and populates a table from a query, preserves the query's declared output types for implemented SQL types, and creates nullable columns without copying source constraints. An optional column-name list replaces output names positionally and may be shorter than the query output, in which case remaining names come from the query; quoted case is preserved. More names than output columns raise `42601`, while duplicate names and PostgreSQL system-column names raise `42701`, before the query is executed. `WITH NO DATA` creates and durably persists the same typed schema, including vector and tensor field metadata, without evaluating row-producing expressions or volatile functions; relation, column, function, type-input, and column-name-list analysis still occurs in PostgreSQL order. CTAS supports ordinary, temporary, and unlogged targets, and temporary targets support all three `ON COMMIT` actions. Top-level `SELECT ... INTO [TEMPORARY | TEMP | UNLOGGED] [TABLE] name` creates the same corresponding table and executes the query; PL/pgSQL `SELECT ... INTO` remains variable assignment. Storage options, access methods, and tablespaces are not implemented.

## Sequences

A column default such as `nextval('ticket_ids')` binds its unknown string argument to a `regclass` OID when the table or default is defined. `currval` and `setval`, ordinary and foreign tables, and `ALTER TABLE ... ADD COLUMN` or `SET DEFAULT` use the same selected-function input rules. `pg_get_expr` renders the retained OID as `'ticket_ids'::regclass` when visible and qualifies the name when needed; normal catalog dependencies protect it from `DROP ... RESTRICT`. Renaming the sequence, reusing its old name, changing `search_path`, or reopening a persistent database does not retarget the default. An explicit `nextval('ticket_ids'::text)` instead keeps a runtime text-to-regclass conversion and no static sequence dependency. A selected user-defined `nextval(text)` follows that routine's text signature. A missing relation is rejected at definition with `42P01`; a relation of another kind can be stored as a regclass constant and is rejected with `42809` only when the sequence function runs.

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

Rename and schema-move operations preserve the sequence's `pg_class.oid`, numeric and literal `regclass` bindings, reserved cache block, `currval`, and `lastval` in the current session and in sessions that observe the new name later. Stored column-default and view references retain that identity while catalog output follows the new visible name; serial and identity ownership continues to name the same object. These bindings remain durable across reopen and follow transaction and savepoint rollback without reclaiming values already returned by `nextval`. A serial or identity sequence may be renamed and `pg_get_serial_sequence` follows it, but moving an owned sequence to another schema reports `0A000`. Moving a sequence to its current schema is a no-op. A missing sequence reports `42P01`, another relation kind reports `42809`, a target-name collision reports `42P07`, a missing target schema reports `3F000`, and a read-only transaction reports `25006` before target lookup; `IF EXISTS` converts only a missing source into a notice.

The role active at `CREATE SEQUENCE` owns the sequence. Sequence definition, persistence, name, namespace, and drop operations require the current user to be a superuser or to inherit the owning role. `ALTER SEQUENCE name OWNER TO role` and its historical `ALTER TABLE` spelling require that authority and an existing target role. An actual change additionally requires SET access to the target role and its CREATE privilege on the containing schema unless the caller is a superuser; changing the owner of an attached serial or identity sequence reports `0A000`. An unchanged owner skips SET and CREATE checks and also succeeds for an attached sequence. A transfer changes `pg_class.relowner` and `pg_sequences.sequenceowner` without changing the stable relation OID, definition generation, value or session cache, and it follows transaction, savepoint, temporary-object, rename, and durable-reopen lifecycle. SQL role owners prevent `DROP ROLE` until the sequence is reassigned or removed.

Sequence ACLs support `USAGE`, `SELECT`, and `UPDATE`, `ALL [PRIVILEGES]`, `PUBLIC`, `WITH GRANT OPTION`, `GRANT OPTION FOR`, `GRANTED BY`, `RESTRICT`, and `CASCADE` through `GRANT` and `REVOKE`. Explicit `ON SEQUENCE` targets, the historical `ON TABLE sequence_name` spelling, and `ON ALL SEQUENCES IN SCHEMA` are supported. Owners retain implicit grant options even after revoking their own ordinary privileges; value operations and parameter inspection then require the applicable ACL privilege, while ownership still permits definition changes and information-schema metadata visibility. Denied value operations do not consume cached sequence reservations. Nonowners may delegate through direct or inherited rooted grant-option paths; alternate paths survive a cascading revoke; and ACL grantors or grantees prevent `DROP ROLE`. Owner transfer rewrites owner-issued paths, and explicit ACLs appear in `pg_class.relacl` with PostgreSQL's `r`, `w`, and `U` codes. ACL changes follow statement, transaction, savepoint, temporary-object, rename, cross-engine refresh, and durable-reopen lifecycle.

`nextval` requires `USAGE` or `UPDATE`, `currval` and `lastval` require `USAGE` or `SELECT`, and `setval` requires `UPDATE`. The six current-user or explicit-role name/OID `has_sequence_privilege` overloads accept comma-separated privilege checks, including `WITH GRANT OPTION`. Explicit `public` and absent role OIDs receive only applicable PUBLIC privileges. Privilege-string validation follows explicit-role lookup but precedes target lookup; a missing sequence name reports `42P01`, a missing sequence OID returns `NULL`, and another relation kind reports `42809`. A durable `CREATE SEQUENCE` requires `CREATE` on its target schema but not `USAGE`; definition errors precede that check, while the check precedes collision and `IF NOT EXISTS` handling. Name-based value functions, direct scans, ALTER, DROP, sequence grants, and privilege inquiry require schema `USAGE` before sequence privileges or relation lookup; qualified inaccessible names report `42501`, while unqualified lookup skips inaccessible search-path schemas. `OWNER TO` additionally requires the new owner to have `CREATE` on the current schema, and `SET SCHEMA` requires the acting owner to have `CREATE` on the target schema without requiring target `USAGE`. Read-only rejection and target, role, schema, relation-kind, and invalid-privilege resolution follow the tested PostgreSQL 18 precedence.

A sequence is directly selectable as a one-row relation with the PostgreSQL columns `last_value bigint`, `log_cnt bigint`, and `is_called boolean`; direct scans require `SELECT`. `pg_class` exposes the sequence as relkind `S` with three attributes and no row type, while `pg_attribute` exposes those three non-null, positive-numbered physical columns in PostgreSQL order. The physical value and log counter follow bounded cache reservations, survive durable reopen, and reset on `setval`, `RESTART`, or an allocation-affecting definition change. `pg_sequence_parameters(oid)` returns the configured start, bounds, increment, cycle flag, cache size, and data-type OID to a role with any sequence privilege; `pg_get_sequence_data(regclass)` returns the physical value and called state only with `SELECT` and otherwise returns a null record; `pg_sequence_last_value(regclass)` and `pg_sequences.last_value` return the physical value only after the sequence is called and only with `SELECT` or `USAGE`. These functions expose their PostgreSQL 18 OIDs, argument modes and names, strictness, volatility, and parallel-safety through `pg_proc`.

A persistent sequence makes its durable record cover a run of values instead of writing it for each value, as PostgreSQL writes a sequence to its log 32 fetches ahead of the values it returns: the record holds the value `log_cnt` fetches past `last_value`, and `nextval` writes it again only when those values are used up. Every session and every process that has the database open draws from the same exact position, so the values and the `last_value`, `log_cnt` and `is_called` they read are the ones PostgreSQL shows, and a database that is closed and reopened continues each sequence at its next value. When the last process that has the database open ends without closing it, or the machine fails, a sequence continues after the value its record holds: up to 32 values and the rest of a cached block are skipped and none is repeated, and `last_value`, `log_cnt` and `is_called` then show the recorded value, zero and true, as PostgreSQL shows them after crash recovery. Sequence values are therefore unique but not gap-free, as in PostgreSQL. A temporary sequence, and a sequence the current transaction created or changed, writes each value.

Definition-option changes retain ShareRowExclusive on the source sequence. `OWNER TO`, `SET LOGGED`, `SET UNLOGGED`, `RENAME TO` and `SET SCHEMA` retain AccessExclusive, including unchanged persistence requests. These alterations bind the source relation and recheck its original requested name after a wait. Actual relation ownership and pinned-system protection are checked before the requested sequence kind. Rename additionally requires CREATE on the source schema before kind validation, including current database TEMP authority for an existing temporary namespace; definition changes and schema moves do not require source-schema CREATE. Every authority check is repeated after a source wait. `SET SCHEMA` then checks and retains the destination namespace through transaction completion or savepoint undo, including when it is already the source schema. Destination deletion, recreation and CREATE privileges are rechecked after a namespace wait; name collisions use the resulting catalog. Owned-sequence rejection precedes destination lookup, while temporary-schema restrictions follow destination existence and authority checks. Explicit `SET SCHEMA public` remains independent of the search path. `DROP SEQUENCE` also retains AccessExclusive and rebinds the requested name after waits, so an uncommitted deletion cannot be bypassed by a competing ALTER and a committed rename cannot redirect deletion to the old object.

`OWNED BY` requires the sequence and its ordinary, inherited, or partitioned owner table to be in the same schema. It records an automatic dependency without creating or changing the column default, survives table and column renames through stable object identities, appears through `pg_get_serial_sequence(text, text)`, and causes an owner-column or owner-table drop to remove the sequence. If another default or view depends on that sequence, an owner drop with `RESTRICT` reports `2BP01`, while `CASCADE` removes the dependent default and complete view closure. Multiple sequences may own one column. `TRUNCATE ... CONTINUE IDENTITY` preserves their values, while `TRUNCATE ... RESTART IDENTITY` restarts them. `OWNED BY NONE` detaches the dependency; assigning a new owner moves it. These changes follow statement, transaction, savepoint, and durable-reopen semantics.

An ownership-only `ALTER SEQUENCE ... OWNED BY` discards the issuing session's unused cached values while preserving the allocation generation and other sessions' reserved cache blocks. Values allocated after that ownership change survive transaction or savepoint rollback. The ownership dependency itself rolls back. Explicit value-generation options, including an unchanged `CACHE` or `INCREMENT` value, retain their transactional replacement behavior.

`SERIAL` columns use the same automatic dependency, so their sequence may be reassigned or detached and may be dropped directly subject to ordinary default-expression dependencies. Identity columns use an internal dependency: their generated sequence cannot be reassigned with `ALTER SEQUENCE ... OWNED BY` or dropped directly even with `CASCADE`, and dropping the identity column removes it. Both forms choose their implicit sequence name with PostgreSQL's schema-local `table_column_seq` rule: table and column components are balanced and clipped at UTF-8 boundaries to fit the 63-byte identifier limit, and a collision with any existing relation retries with `seq1`, `seq2`, and later numeric labels.

`smallserial` and `serial2` backing sequences use `smallint`, `serial` and `serial4` use `integer`, and `bigserial` and `serial8` use `bigint`; identity backing sequences use their declared integer column type. `information_schema.sequences` reports the declared type and numeric precision plus exact start, minimum, maximum, increment, and cycle state, excludes internally owned identity sequences, and exposes a row only when the current user has schema `USAGE` and either inherits the owner or holds `SELECT`, `UPDATE`, or `USAGE` on the sequence. Both it and `pg_sequences` hide temporary sequences owned by other sessions. These types, dependencies, metadata, and visibility survive durable reopen.

An identity declaration, `GENERATED { ALWAYS | BY DEFAULT } AS IDENTITY [ ( options ) ]`, creates its sequence with the options `CREATE SEQUENCE` accepts, read and checked as `CREATE SEQUENCE` reads them, and two more: `SEQUENCE NAME name` names the sequence, in the table's schema unless the name gives another, and `LOGGED` or `UNLOGGED` gives it that persistence instead of its table's, which a temporary table's sequence rejects with `42P16`. The sequence counts in the column's type, so a written `AS` conflicts with it and reports `42601`, as a repeated option does, and a column of another type than `smallint`, `integer` or `bigint` reports `22023`. A repeated `SEQUENCE NAME`, `LOGGED` or `UNLOGGED` is reported while the statement is analyzed, the other option errors when the sequence is created, after any analysis error later in the statement. An `OWNED BY` option is checked as `CREATE SEQUENCE` checks it and then replaced: the identity column owns its sequence. `information_schema.columns` reports the sequence's start, increment, bounds and cycling as `identity_start`, `identity_increment`, `identity_maximum`, `identity_minimum` and `identity_cycle`; a partition, whose identity column draws from its parent's sequence, reports none.

`ALTER TABLE ... ALTER COLUMN name ADD GENERATED { ALWAYS | BY DEFAULT } AS IDENTITY [ ( options ) ]` creates the column's sequence as an identity declaration does and makes a `NOT NULL` column without a default an identity column; a nullable column, an identity column and a column with a default or a generation expression report `55000`. `SET GENERATED { ALWAYS | BY DEFAULT }`, `RESTART [ [ WITH ] value ]` and `SET sequence_option`, in any combination, change the generation and the sequence, whose options are read as `ALTER SEQUENCE` reads its own, and `DROP IDENTITY [ IF EXISTS ]` drops the sequence and keeps the column's `NOT NULL`. These report a column that is not an identity column with `55000`, which `IF EXISTS` turns into a notice. A partitioned table's partitions share its identity sequence and follow each action, while an action on a partition, or on `ONLY` the partitioned table, reports `42P16`. `ALTER COLUMN ... TYPE` changes an identity sequence's type with its column's, as `ALTER SEQUENCE ... AS` would, and `SET DEFAULT` and `DROP DEFAULT` on an identity or generated column report `42601` with PostgreSQL's hint.

Sequence definition changes are transactional. A parameter-changing `ALTER SEQUENCE`, including a same-value `CACHE` change, invalidates outstanding blocks in every session. An actual logged-state change does the same, while `SET LOGGED` on an already logged sequence or `SET UNLOGGED` on an already unlogged sequence preserves every session's block. An allocation made after an uncommitted `ALTER SEQUENCE` or `RESTART` follows that definition's transaction or savepoint ownership, while an earlier reservation against the retained definition remains nontransactional. Rolling back an unused logged-state change leaves that earlier block usable; once the changed definition allocates a value, rollback discards its block and resumes from the retained definition without reviving an abandoned block. The affected session `currval` and `lastval` still retain the most recently returned value across rollback, matching PostgreSQL 18.

`DROP SEQUENCE [ IF EXISTS ] name [, ...] [ CASCADE | RESTRICT ]` resolves relation names through the current `search_path`, validates every target before mutation, ignores duplicate targets, and uses `RESTRICT` by default. A missing target reports `42P01` unless `IF EXISTS` requests a notice and continuation, a target of another relation kind reports `42809` even with `IF EXISTS`, a dependency rejected by `RESTRICT` reports `2BP01`, and a read-only transaction reports `25006` before target lookup.

`CASCADE` removes referencing column defaults, column- and table-level `CHECK` constraints, stored or virtual generated columns, routines that read those removed columns, and the complete closure of dependent views while retaining the underlying tables; the same expression-granular behavior applies to foreign-table defaults and `CHECK` constraints. Dropping a serial sequence with `CASCADE` removes its column default and serial ownership metadata; if the serial default was replaced first, an ordinary drop succeeds and preserves the replacement expression. Sequence rename rewrites these stored schema expressions to the new exact relation identity, and recreating the old name cannot retarget them. String literals explicitly cast to `regclass` in stored schema expressions bind the original relation identity. A sequence rename or recreation of the old name cannot redirect these constants, including generated values and defaults evaluated by later inserts. Surviving routine source aliases are updated when a sequence cascade removes generated columns. Sequence drops are transactional: transaction and savepoint rollback restore the catalog object and its session-local `currval` and `lastval` identity, while a committed drop remains absent after reopen and does not transfer session values to a same-named replacement.

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

`CREATE SERVER [IF NOT EXISTS] name [TYPE 'type'] [VERSION 'version' | VERSION NULL] FOREIGN DATA WRAPPER wrapper [OPTIONS (name 'value', ...)]` creates one server owned by the current role and returns the command tag `CREATE SERVER`. TYPE and VERSION are retained independently of connection options; omitted values and VERSION NULL are NULL, while an explicit empty string remains empty. Ownership keeps the role's identity across renames and blocks `DROP ROLE` with `2BP01` and an `owner of server ...` detail. `pg_shdepend` and `pg_describe_object` expose this ownership dependency. Transaction rollback, savepoint undo, peer refresh and reopen retain the corresponding server identity and ownership.

An existing name reports `42710` before looking up the requested wrapper or checking options; `IF NOT EXISTS` instead emits a `42710` notice and preserves the original definition. A missing wrapper reports `42704`. Repeated option names report `42710`; once duplicates have been checked, an option name containing `=` reports `22023`. SQL-standard unquoted routine bodies reject CREATE SERVER with `0A000`; quoted SQL bodies compile and execute the utility at their normal boundaries. The direct Rust registration API retains its map-option behavior, including arbitrary keys and the last value for a repeated key; an empty server name is rejected before publication.

`DROP SERVER [IF EXISTS] name [, ...] [RESTRICT | CASCADE]` returns the command tag `DROP SERVER`. The current role must inherit each server owner's authority; a failure reports `42501`. Names are resolved and authorized in written order before checking the combined dependency closure. A missing name reports `42704`; `IF EXISTS` emits a separate `00000` notice for each missing occurrence. Repeated existing targets are deleted once. `RESTRICT`, the default, reports `2BP01` with the dependent objects and a CASCADE hint. `CASCADE` removes dependent foreign tables, their views and stored routine dependencies in the same transaction, without requiring ownership of each dependent. Read-only transactions reject deletion with `25006`. SQL-standard unquoted routine bodies reject DROP SERVER with `0A000`; quoted bodies execute it normally.

Server object locks survive through the transaction and its savepoints. After a wait, deletion resolves a replaced name again and checks the replacement owner's authority. Rollback restores the original identities and complete dependency state. A foreign table retains the server OID and incarnation selected when it was created. As in PostgreSQL, concurrent foreign-table creation and server deletion can both commit while leaving the table's old server reference absent. The database remains reopenable and that table remains removable; SELECT, including LIMIT 0, and EXPLAIN report `XX000` with `cache lookup failed for foreign server <oid>`. Recreating the same server name does not redirect the old table to the replacement.

```sql execute
CREATE SERVER foreign_drop_memory FOREIGN DATA WRAPPER memory_fdw;
CREATE FOREIGN TABLE foreign_drop_rows (value integer) SERVER foreign_drop_memory;
DROP SERVER foreign_drop_memory CASCADE;
```

The engine stores one canonical SQL schema for each foreign table and projects only names and physical types when calling an FDW handler. `NOT NULL`, column defaults, column- and table-level `CHECK` constraints, stored generated columns, `SERIAL`, and identity columns therefore remain visible through `information_schema.columns`, `pg_attribute`, `pg_attrdef`, and `pg_constraint` and survive transactions, catalog refresh, and durable reopen. Routine and sequence references in those expressions bind at creation, follow exact-object rename, and participate in `DROP ... RESTRICT` and expression-granular `CASCADE`; dropping a routine-dependent generated column follows PostgreSQL and retains the foreign table. Foreign-table `SERIAL` and identity declarations create automatic and internal owned-sequence dependencies respectively, work with `pg_get_serial_sequence`, move sequence ownership with the foreign-table owner, and drop their sequences with the owning foreign table while preserving PostgreSQL's external-dependency `RESTRICT` and `CASCADE` behavior. Legacy column-array catalog rows, including rows whose generated sequences were not materialized, are upgraded only during the initial open, while ordinary reload validates the versioned schema without repairing it. `CREATE FOREIGN TABLE IF NOT EXISTS` applies the same namespace and shared-name preflight before analyzing its columns, types, constraints, expressions, server, or generated sequences. For an analyzed definition, primary-key, unique, foreign-key and exclusion constraints report PostgreSQL's `0A000` diagnostics in declaration order. Each column's type lookup precedes its constraint attributes and clauses; attribute placement is checked before clause conflicts, and later expressions do not override an earlier forbidden constraint. Table-level `CONSTRAINT name NOT NULL column` declarations retain the column nullability, constraint name and `pg_constraint` metadata through rollback and reopen. A rejected definition leaves no foreign relation or generated sequence.

```sql execute
CREATE SERVER foreign_declaration_memory FOREIGN DATA WRAPPER memory_fdw OPTIONS (kind 'memory');
CREATE FOREIGN TABLE foreign_declaration_example (
    id integer,
    CONSTRAINT required_id NOT NULL id
) SERVER foreign_declaration_memory OPTIONS (source 'memory');
SELECT attname, attnotnull FROM pg_attribute
WHERE attrelid = 'foreign_declaration_example'::regclass AND attnum > 0;
DROP FOREIGN TABLE foreign_declaration_example;
```

The role active at `CREATE FOREIGN TABLE` owns the foreign table and its implicit `SERIAL` and identity sequences. `ALTER FOREIGN TABLE name OWNER TO role` and the historical `ALTER TABLE name OWNER TO role` spelling require inherited owner authority, a SET-enabled path to the target role, and `CREATE` for that role on the containing schema; superusers bypass the latter two privilege restrictions, and the target does not need `USAGE` on the foreign server. Owner transfer moves every owned sequence in the same transaction. The owner or containing-schema owner may use `DROP FOREIGN TABLE`, `RESTRICT` rejects dependent views and external dependents of owned sequences, and `CASCADE` removes their complete closure. Ownership appears in `pg_class.relowner`, blocks `DROP ROLE`, and follows transaction, savepoint, cross-engine refresh, explicit catalog migration, corruption rejection, and durable-reopen lifecycle.

`ALTER FOREIGN TABLE [IF EXISTS] name RENAME TO new_name`, including its historical `ALTER TABLE` spelling, follows the same actual-owner, source-schema CREATE, system-catalog and post-wait authority checks, shared-namespace collision errors, missing-target notices, and transactional rename behavior as views. An unchanged owner assignment does not require source CREATE. Relation and row-type OIDs, column definitions, owned-sequence dependencies, ACLs, dependent views, and trigger and rewrite-rule references remain attached to the same foreign table across rename and reopen.

```sql execute
CREATE SERVER foreign_rename_memory FOREIGN DATA WRAPPER memory_fdw OPTIONS (kind 'memory');
CREATE FOREIGN TABLE foreign_rename_original (id integer) SERVER foreign_rename_memory OPTIONS (source 'memory');
CREATE VIEW foreign_rename_dependent AS SELECT id FROM foreign_rename_original;
ALTER FOREIGN TABLE foreign_rename_original RENAME TO foreign_rename_current;
SELECT definition FROM pg_views WHERE viewname = 'foreign_rename_dependent';
```

Foreign tables use the same nullable relation ACL and per-column ACL model as other table-shaped relations. `GRANT` and `REVOKE ... ON TABLE` support all eight relation privileges and column `SELECT`, `INSERT`, `UPDATE`, and `REFERENCES`, including `PUBLIC`, independent rooted grant-option paths, dependent `RESTRICT` and `CASCADE`, implicit owner rights, `ALL TABLES IN SCHEMA`, owner-transfer grantor rewriting, and role dependencies. SQL scans enforce table or exact-column `SELECT` across direct, joined, stored definer-view, and `security_invoker` paths; the built-in foreign wrappers expose a read-only scan interface, so `information_schema.tables.is_insertable_into` and foreign columns' `is_updatable` are `NO`. `pg_class.relacl`, `pg_attribute.attacl`, all name/OID `has_table_privilege` and `has_column_privilege` forms, `information_schema.tables`, `columns`, `column_privileges`, and `role_column_grants` expose the same durable state through transactions, savepoints, cross-engine refresh, migration, corruption validation, and reopen.

Foreign tables accept ordinary `BEFORE` and `AFTER` row and statement trigger definitions for `INSERT`, `UPDATE`, `DELETE`, and `TRUNCATE`, including `UPDATE OF` and `WHEN`; constraint triggers, transition relations, and `INSTEAD OF` timing are rejected with PostgreSQL's foreign-table diagnostic. Creation enforces the foreign table's `TRIGGER` privilege and the function's `EXECUTE` privilege. The foreign table owner controls trigger rename and `ALTER FOREIGN TABLE` or historical `ALTER TABLE` enable modes, while `DROP TRIGGER` derives authority from the same live owner. `pg_trigger`, `pg_class.relhastriggers`, function dependencies, owner transfer, rollback, cross-engine refresh, durable reopen, and automatic trigger removal with `DROP FOREIGN TABLE` use the durable trigger catalog. The built-in foreign wrappers remain read-only, so writable foreign-table DML and trigger execution remain compatibility work.

## Stored relation and routine dependencies

`DROP TABLE`, `DROP FOREIGN TABLE`, `DROP VIEW`, `DROP MATERIALIZED VIEW`, and `DROP SEQUENCE` accept relation identifiers and use `RESTRICT` unless `CASCADE` is specified. SQL-standard `RETURN` and `BEGIN ATOMIC` routine bodies retain dependencies on referenced relations. A dependent routine prevents removal with SQLSTATE `2BP01`; a successful command returns its `DROP` tag without rows.

Explicit table, foreign-table, view and materialized-view DROP targets acquire `ACCESS EXCLUSIVE` in statement order. Target kind and ownership are checked before waiting and again against the refreshed catalog after acquisition; a renamed or replaced name is resolved again before deletion. `IF EXISTS` skips a missing object or schema with a notice and still rejects a different relation kind. Multiple references to the same canonical target remove it once. The direct Rust `drop_table`, `drop_foreign_table` and `drop_view` APIs also retain transaction-owned definition locks, recheck targets after waits and apply the dependency rules of `RESTRICT` before removal. They return `false` for a missing target without an SQL notice, and reject wrong-kind targets and prohibited read-only mutations.

`DROP TABLE`, `DROP FOREIGN TABLE`, `DROP VIEW` and `DROP MATERIALIZED VIEW` also lock views whose stored queries reference the selected relations, including transitive view dependencies, and every deletion locks the other relations it removes or changes before it removes anything. Both `RESTRICT` and `CASCADE` wait for concurrent changes to those definitions and then search the dependencies again. A view that no longer references the target survives; a renamed dependent is followed by object identity, so reusing its old name does not cause the new view to be removed. Dependency checks finish before catalog mutation, and cancellation leaves the target and its dependents intact.

`CASCADE` follows the [catalog dependencies](#catalog-dependencies) through stored views, routines, domain defaults, and typed or generated columns until no additional objects depend on the removed objects. This includes routines reached through a view in another schema and cycles between a view and a routine. Dropping a function or schema follows the same dependencies. The owner of the requested object authorizes the cascade; dependent objects do not require separate ownership or schema access. Referencing tables and unrelated columns remain. Multi-target failures are atomic, and dependency removal follows transaction and savepoint rollback, catalog refresh, and durable reopen.

`regclass` casts of string literals, literal sequence arguments to `nextval`, `currval`, and `setval`, and string literals supplied to `regclass` routine arguments or parameter defaults retain the relation OID chosen at routine creation. Named arguments and scalar domains over `regclass` use the same binding. Sequence rename and recreation of the old name do not retarget these bindings. String-literal SQL and PL/pgSQL bodies retain execution-time body lookup. Explicit `text` sequence arguments also retain execution-time lookup. Integer-to-`regclass` conversions and implicit conversion at the SQL routine result boundary do not establish a stored relation dependency; parameter defaults remain creation-bound regardless of body syntax. [Column renames and deletion](#alter-table) also preserve SQL-standard body identities and enforce the corresponding read and write dependencies, including after reuse of the old column name. See [routine lifecycle](08-transactions-and-routines.md#routine-lifecycle) and [compatibility accounting](09-compatibility.md) for the remaining dependency implementation work.

```sql execute
CREATE TABLE routine_drop_source (id integer);
CREATE VIEW routine_drop_bridge AS SELECT id FROM routine_drop_source;
CREATE FUNCTION routine_drop_reader() RETURNS integer LANGUAGE SQL
BEGIN ATOMIC
    SELECT id FROM routine_drop_bridge LIMIT 1;
END;
DROP TABLE routine_drop_source CASCADE;
SELECT to_regclass('routine_drop_bridge') IS NULL AS view_removed,
       to_regprocedure('routine_drop_reader()') IS NULL AS routine_removed;

CREATE SEQUENCE routine_drop_sequence START 7;
CREATE FUNCTION routine_drop_next() RETURNS bigint LANGUAGE SQL
RETURN nextval('routine_drop_sequence');
ALTER SEQUENCE routine_drop_sequence RENAME TO routine_drop_sequence_moved;
SELECT routine_drop_next();
DROP SEQUENCE routine_drop_sequence_moved CASCADE;
```

## Catalog dependencies

`pg_depend` records what each user object depends on, as PostgreSQL records it when the object is created: its schema, the types of its columns, arguments and results, its row type and array type, inheritance parents and partitioned parents, a sequence's owning column, and for constraints, column defaults, generation expressions, index keys and predicates, triggers, rules, views and SQL-standard routine bodies, the columns, types, routines and relations their expressions use. The dependency type tells what a drop does: an object that another references normally (`n`) is removed only by `CASCADE`; one that goes with it automatically (`a`), is part of it (`i`), or belongs to a partitioned parent (`P`, `S`) is removed with it. Dependencies on built-in objects, which cannot be dropped, are not recorded. `pg_shdepend` records the roles each object depends on: its owner (`o`) and every role its privileges name as a grantee or grantor (`a`), including the columns of a relation, and a role membership's grantor. `pg_describe_object(classid, objid, objsubid)` names an object as the dependency reports do, and returns `NULL` for an object that does not exist.

```sql execute
CREATE TYPE catalog_mood AS ENUM ('calm', 'busy');
CREATE TABLE catalog_diary (id integer PRIMARY KEY, mood catalog_mood DEFAULT 'calm');
SELECT pg_describe_object(classid, objid, objsubid) AS object,
       pg_describe_object(refclassid, refobjid, refobjsubid) AS referenced,
       deptype
FROM pg_depend
WHERE refobjid = 'catalog_mood'::regtype
ORDER BY 1;
DROP TYPE catalog_mood CASCADE;
DROP TABLE catalog_diary;
```

`DROP ROLE` reports `2BP01` while any object depends on the role; the detail lists each dependent object once per dependency type, by ascending OID, as `owner of` or `privileges for` the object, as PostgreSQL's `checkSharedDependencies` does.

## Dependency-aware deletion

`DROP TABLE`, `DROP FOREIGN TABLE`, `DROP VIEW`, `DROP MATERIALIZED VIEW`, `DROP SEQUENCE`, `DROP INDEX`, `DROP SCHEMA`, `DROP DOMAIN`, `DROP TYPE`, `DROP FUNCTION`, `DROP PROCEDURE`, `ALTER TABLE ... DROP COLUMN`, `ALTER TABLE ... DROP CONSTRAINT` and `ON COMMIT DROP` remove objects as PostgreSQL's `performDeletion` does: the named objects and everything that depends on them are found through the dependencies `pg_depend` shows. An object that depends on a target normally (`n`) is removed only with `CASCADE`. Without it the statement fails with `2BP01` and the message `cannot drop <object> because other objects depend on it`, or `cannot drop desired object(s) because other objects depend on them` for several targets; the detail has a line `<dependent> depends on <object>` for each such dependency, and the hint is `Use DROP ... CASCADE to drop the dependent objects too.` Objects that go with a target automatically (`a`), are part of it (`i`), or belong to its partitions (`P`, `S`) are removed without `CASCADE` and are not reported.

An object that is part of another cannot be dropped by itself. Dropping the index that implements a constraint, the sequence of an identity column, or a partition's copy of its parent's index or constraint fails with `2BP01`, the message `cannot drop <object> because <owner> requires it`, and the hint `You can drop <owner> instead.`

With `CASCADE`, one notice reports what is removed besides the targets: `drop cascades to <object>` for a single object, or `drop cascades to N other objects` with a detail line `drop cascades to <object>` for each of them, of which at most 100 are listed. Objects are removed one at a time, each before the objects it depends on, so no remaining object names a removed one at any point. Before removing anything, the statement locks every relation it removes or changes with `ACCESS EXCLUSIVE`, and after waiting for a lock it searches the dependencies again. `ON COMMIT DROP` removes a temporary table and its dependents without a notice. Every change follows transaction and savepoint rollback, catalog refresh, and durable reopen.

```sql execute
CREATE TABLE deletion_orders (id integer PRIMARY KEY, amount integer);
CREATE VIEW deletion_totals AS SELECT sum(amount) AS total FROM deletion_orders;
CREATE VIEW deletion_report AS SELECT total FROM deletion_totals;
DROP TABLE deletion_orders CASCADE;
SELECT to_regclass('deletion_totals') IS NULL AS totals_removed,
       to_regclass('deletion_report') IS NULL AS report_removed;
```

The `DROP TABLE` above reports the notice `drop cascades to 2 other objects` with the details `drop cascades to view deletion_totals` and `drop cascades to view deletion_report`; the primary key's constraint and index go with the table unreported.

## TRUNCATE and DROP

```sql
TRUNCATE TABLE staging_a, staging_b;
DROP TABLE IF EXISTS staging_a;
```

`TRUNCATE` removes all rows from its listed tables under a transaction boundary. A table that a table outside the list references, directly or through a partitioned table it is a partition of, is rejected with `0A000`, `cannot truncate a table referenced in a foreign key constraint`, with the detail `Table "fk" references "pk1".` and the hint to truncate both or use `CASCADE`; `TRUNCATE ... CASCADE` adds such referencing tables and reports `truncate cascades to table "fk"` for each. SQL drop supports implemented table, foreign-table, index, view, materialized-view, sequence, schema, function, and procedure targets. `DROP TABLE ... CASCADE` and `DROP FOREIGN TABLE ... CASCADE` remove everything that depends on the tables, as [dependency-aware deletion](#dependency-aware-deletion) describes, without requiring separate ownership of those dependent objects; that includes a foreign key that references a dropped table, or a partitioned table a dropped partition belongs to, as PostgreSQL drops the constraint it derived on the partition, and without `CASCADE` such a foreign key rejects the drop with `2BP01`. Relation-kind mismatches return PostgreSQL-compatible errors.
