# Transactions, Sessions, and Routines

UQA Engine implements SQL transaction control, savepoints, prepared statements, session variables, SQL-language routines, and a tested PL/pgSQL subset.

## Transaction control

```sql
BEGIN;
UPDATE accounts SET balance = balance - 10 WHERE account_id = 1;
UPDATE accounts SET balance = balance + 10 WHERE account_id = 2;
COMMIT;
```

`START TRANSACTION` is accepted as the explicit begin form. `ROLLBACK` discards the active transaction.

A statement that fails inside an explicit transaction aborts the transaction as in PostgreSQL 18: every later statement, including typed engine mutations and failing savepoint commands, reports `25P02` until `ROLLBACK` or `ROLLBACK TO SAVEPOINT` ends the aborted state, and `COMMIT` of an aborted transaction rolls back. A failure inside a nested `BEGIN` aborts only that nested frame; the enclosing frames keep their writes and row locks.

## Savepoints

```sql
BEGIN;
INSERT INTO audit_log (message) VALUES ('begin import');
SAVEPOINT optional_rows;
INSERT INTO optional_data (id) VALUES (1);
ROLLBACK TO SAVEPOINT optional_rows;
RELEASE SAVEPOINT optional_rows;
COMMIT;
```

Rolling back to a savepoint keeps the transaction active and discards changes after that point. Releasing removes the savepoint marker without committing the outer transaction.

## Session isolation

Every `Engine::new_session()` has independent transaction state, parameters, prepared statements, statement cache, sequence session state, open portals, effective role, random seed, notices, and cancellation token. Sessions share durable catalog and row state plus runtime function registries.

Do not use one session concurrently as if it were multiple transaction contexts. Create a session per independent SQL conversation.

## Prepared statements

```sql
PREPARE find_task (text) AS
SELECT task_id, title
FROM tasks
WHERE state = $1
ORDER BY task_id;

EXECUTE find_task('open');
DEALLOCATE find_task;
```

Prepared plans are session-local. Catalog or function registry changes trigger the engine's plan cache invalidation and rebind rules. Applications must still handle a prepare or execute error when an incompatible change makes a plan invalid.

## SET and SHOW

Known settings include:

| Setting | Default or behavior |
| --- | --- |
| `search_path` | Schema resolution path |
| `server_version` | Read-only compatibility value `18.0-uqa` |
| `server_encoding` | Read-only `UTF8` |
| `client_encoding` | Mutable, default `UTF8` |
| `datestyle` | Mutable, default `ISO, MDY` |
| `timezone` | Mutable, default `UTC` |
| `work_mem` | Mutable, default `64MB` |

```sql
SET search_path TO application, public;
SHOW search_path;
SET timezone TO 'UTC';
SHOW timezone;
```

`SET ROLE name` changes `current_user` for the session while preserving `session_user`; `RESET ROLE`, `SET ROLE NONE`, and `SET ROLE DEFAULT` restore the session identity. The embedded connection starts as the durable bootstrap superuser role `uqa`, so it may assume any defined role. Role membership and non-superuser role assumption are not implemented.

`DISCARD ALL`, `DISCARD PLANS`, and `DISCARD SEQUENCES` reset their implemented session state. `DISCARD TEMP` removes the session's temporary tables, views, sequences, and sequence state; PostgreSQL rejects it inside a transaction, and UQA Engine does the same.

`LOAD 'age'` (also `age.so`, `$libdir/age`, and `$libdir/age.so`) succeeds without side effects because the Apache AGE surface is embedded; any other library name fails as `could not access file "$libdir/name": No such file or directory` (`58P01`). See [Graph SQL and Cypher](07-graph.md) for the AGE session bootstrap.

## SQL-language scalar function

```sql
CREATE FUNCTION add_tax(amount NUMERIC, rate NUMERIC)
RETURNS NUMERIC
AS $$
    SELECT amount + amount * rate
$$
LANGUAGE sql
IMMUTABLE;

SELECT add_tax(100.00, 0.10);
```

SQL functions can return scalar, `SETOF`, or `TABLE` results according to their declaration. Positional parameters and named parameters are resolved by the routine compiler. SQL-standard `RETURN expression` and `BEGIN ATOMIC ... END` bodies are also implemented for supported statement shapes.

## PL/pgSQL scalar function

```sql
CREATE FUNCTION classify_priority(value INTEGER)
RETURNS TEXT
AS $$
BEGIN
    IF value <= 1 THEN
        RETURN 'urgent';
    ELSIF value <= 3 THEN
        RETURN 'normal';
    ELSE
        RETURN 'low';
    END IF;
END;
$$ LANGUAGE plpgsql IMMUTABLE;
```

The implemented PL/pgSQL surface includes declarations, assignment, `IF` and `CASE`, basic loops, `WHILE`, integer and query `FOR`, labeled blocks and exits, `RETURN`, `RETURN NEXT`, `RETURN QUERY`, `PERFORM`, static SQL, dynamic `EXECUTE`, nested blocks, recursive calls with a depth limit, diagnostics, exception handlers, and bound cursors covered by the routine tests.

Bound cursors support `CURSOR [(arguments)] FOR query`, positional or named `OPEN` arguments, repeated `FETCH NEXT ... INTO`, `FOUND`, and `CLOSE`. An opened cursor is a session portal: a routine may return its `refcursor` name, a later routine in the same session and transaction may accept that name and continue fetching, and an outer transaction end closes the remaining portals. Rolling back to a savepoint closes portals opened after it without rewinding the position of an older portal, matching PostgreSQL 18. `OPEN ... FOR`, dynamic cursor queries, `MOVE`, fetch directions other than `NEXT`, holdable cursors, and top-level SQL `FETCH` remain unsupported.

This is a deliberate subset. Validate every routine body during migration instead of assuming all PostgreSQL PL/pgSQL statements or diagnostics exist.

## Triggers

```sql
CREATE FUNCTION normalize_item() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    NEW.title := upper(NEW.title);
    RETURN NEW;
END;
$$;

CREATE TRIGGER normalize_item_before
BEFORE INSERT OR UPDATE OF title ON items
FOR EACH ROW
WHEN (NEW.title IS NOT NULL)
EXECUTE FUNCTION normalize_item();
```

The implemented trigger surface supports durable PL/pgSQL `BEFORE` and `AFTER` row and statement triggers for `INSERT`, `UPDATE`, `DELETE`, and `TRUNCATE`, including `UPDATE OF`, boolean-typed `WHEN`, multiple events, string arguments, executable `CREATE OR REPLACE TRIGGER`, `DROP TRIGGER`, `ALTER TRIGGER ... RENAME`, and `ALTER TABLE ... ENABLE` or `DISABLE TRIGGER`. Row triggers fire in name order. Returning `NULL` from a `BEFORE` row trigger skips that row; returning a modified `NEW` row changes an insert or update. An `AFTER` row trigger's `WHEN` predicate is decided immediately after its logical row change, while the selected function invocation runs after all rows have been changed and before `AFTER` statement triggers. Statement triggers fire even when the command changes zero rows, foreign-key referential actions run the recursive child row and statement trigger lifecycle, and `TRUNCATE` triggers cover every explicit, descendant, and `CASCADE`-added relation in statement discovery order.

Trigger functions declare no ordinary parameters and return `trigger`. They receive `OLD`, `NEW`, `TG_NAME`, `TG_WHEN`, `TG_LEVEL`, `TG_OP`, `TG_RELID`, `TG_TABLE_NAME`, `TG_TABLE_SCHEMA`, `TG_NARGS`, and zero-based `TG_ARGV`; calling a trigger function directly fails with SQLSTATE `0A000`. Stored generated columns are recomputed after `BEFORE` triggers, so their fields in `NEW` are `NULL` during a `BEFORE` trigger and available afterward, while virtual generated fields are `NULL` in every trigger `OLD` and `NEW` image as in PostgreSQL 18. `INSERT ... ON CONFLICT` and `MERGE` invoke the applicable root and referential-action triggers in PostgreSQL order, and a trigger-suppressed `MERGE` action does not consume that target's once-per-row modification identity.

Trigger definitions participate in table, column, and exact zero-argument function dependencies, survive storage reopen, roll back with catalog transactions, preserve temporary triggers across unrelated catalog reloads, and invalidate prepared plans. `pg_trigger`, `pg_class.relhastriggers`, `pg_tables.hastriggers`, and both PostgreSQL 18 `pg_get_triggerdef` overloads expose the implemented catalog; the boolean overload selects compact or pretty rendering and propagates `NULL`. A row trigger created on a partitioned table fires for leaf rows and appears as parent-linked leaf clones in `pg_trigger`.

Constraint and deferred triggers, transition relations, `INSTEAD OF` view triggers, partition-moving update semantics, direct ALTER or DROP operations on generated partition clones, `session_replication_role`, trigger privileges, exact PostgreSQL `pg_node_tree` serialization, dump and restore, and the complete upstream trigger regression schedule remain compatibility bugs.

Durable table rewrite rules execute INSERT VALUES, INSERT SELECT, UPDATE, and DELETE actions with OLD and NEW binding, unqualified INSERT and DELETE conditions, alphabetical `ALSO` and conditional or unconditional `INSTEAD` ordering, `NOTHING`, recursion protection, replacement with enable-state retention, rename/drop and target-column lifecycle, view `_RETURN` replacement and protection, and PostgreSQL-shaped `pg_rewrite`, `pg_rules`, rule flags, and `pg_get_ruledef`. Active rules reject `ON CONFLICT` and `MERGE` with the PostgreSQL SQLSTATEs. Set-oriented action execution and statement-trigger cardinality, writable-view DML rules, condition subqueries, record expansion, `RETURNING`, `NOTIFY`, complete dependencies, inheritance and partition behavior, `session_replication_role`, privileges, exact node trees and deparsing, dump/restore, and the complete upstream rule schedule remain compatibility bugs.

## Procedures and CALL

```sql
CREATE PROCEDURE record_message(IN message TEXT)
AS $$
BEGIN
    INSERT INTO messages (body) VALUES (message);
END;
$$ LANGUAGE plpgsql;

CALL record_message('manual generated');
```

Procedures support input, output, and in-out parameters in implemented forms. `CALL` returns output values according to the parameter declaration.

## Anonymous DO block

```sql
DO $$
BEGIN
    INSERT INTO audit_log (message) VALUES ('maintenance started');
END;
$$ LANGUAGE plpgsql;
```

An anonymous block executes without creating a durable routine identity.

## Overloads, defaults, and replacement

Routine identity includes schema, name, and input identity argument types. Function or procedure kind is not a separate identity component, so a function and a procedure cannot coexist with the same schema, name, and input identity signature. `IN`, `INOUT`, and `VARIADIC` parameters participate in identity, pure `OUT` and `TABLE` parameters do not, and a `VARIADIC` parameter contributes its declared array type rather than its expanded element calls.

Ordinary scalar SQL and PL/pgSQL, table-returning, and `SETOF` function overloads retain declared argument types from direct casts and scalar subqueries, while procedures retain direct-cast and concrete PL/pgSQL datum declarations, including typed NULL variables, until candidate selection. Named and default arguments are matched before effective-signature search-path shadowing, typed routine identities survive catalog reopen, and exact implemented information-schema domain overloads outrank their base-type overloads, including when a nested cast restores the domain at the call boundary. PostgreSQL's unqualified `COALESCE`, `GREATEST`, `LEAST`, and `NULLIF` syntax keeps built-in identity, while quoted or schema-qualified calls and ordinary function names such as `upper` and `concat` remain catalog routines selected through the search path. `SETOF` calls in a select list keep the selected scalar or set-returning signature stable through projection expansion, including when a visible user overload shadows a built-in. Table-function sources and every member of a `ROWS FROM` group keep their exact binding through UPDATE, DELETE, MERGE, correlated LATERAL execution, and stored-view serialization and reopen. A stored view owns an exact dependency on each bound user-function signature, so the default `DROP FUNCTION` RESTRICT behavior rejects removal with dependent-objects-still-exist (`2BP01`) instead of leaving the view to fall through to another overload or same-named built-in. `CALL` uses PostgreSQL's visible signature for OUT and TABLE parameters: omitting a required output placeholder reports undefined procedure (`42883`), while supplying the placeholder to a structurally matching function reports wrong object type (`42809`); PostgreSQL also rejects subqueries in `CALL` arguments.

### Polymorphic and VARIADIC routines

The implemented simple polymorphic family resolves `anyelement`, `anyarray`, and `anynonarray`, and the implemented compatible family resolves `anycompatible`, `anycompatiblearray`, and `anycompatiblenonarray`, for scalar and array value carriers represented by the engine. One call derives a concrete substitution shared by its input, return, `OUT`, and `TABLE` positions; compatible-family arguments first select their PostgreSQL common type. That concrete type is retained through SQL and PL/pgSQL scalar execution, `TABLE` and `SETOF` rows, `RETURN NEXT`, `CALL`, nested overloads, generated columns, stored views, and catalog reopen.

A `VARIADIC` parameter must be the final input parameter and must have an array declaration. Ordinary positional calls pack trailing element arguments, while `VARIADIC array_expression` passes one array through without packing; named notation for the variadic slot likewise requires the explicit `VARIADIC` keyword. A defaulted variadic array can serve a zero-argument call, but PostgreSQL candidate ranking can still make that call ambiguous with a fixed zero-argument overload. Fixed candidates, expanded variadic candidates, explicit array calls, defaults, and search-path visibility participate in the same overload selection before execution.

An unresolved polymorphic NULL reports datatype mismatch (`42804`), incompatible substitutions or an ineligible call shape report undefined function or procedure (`42883`), and equally ranked candidates report ambiguous function (`42725`). Invalid pseudo-type or variadic declarations fail before registry mutation, commonly as invalid function definition (`42P13`). User enum, range, and multirange value carriers are not yet implemented, so actual substitutions for `anyenum`, `anyrange`, `anymultirange`, `anycompatiblerange`, and `anycompatiblemultirange` remain type-system compatibility bugs rather than being treated as supported routine cases.

```sql execute
CREATE FUNCTION manual_pack(VARIADIC items INTEGER[])
RETURNS INTEGER[]
LANGUAGE sql
IMMUTABLE
AS 'SELECT $1';

SELECT manual_pack(1, 2);
SELECT manual_pack(VARIADIC ARRAY[3, 4]);
```

`CREATE OR REPLACE` replaces a compatible routine identity. It does not permit an incompatible return contract to masquerade as the same routine. Use qualified names when `search_path` could make an overload ambiguous.

### Altering routine attributes

`ALTER FUNCTION` and kind-neutral `ALTER ROUTINE` can change volatility, null-input behavior, `SECURITY DEFINER` or `SECURITY INVOKER`, leakproofness, parallel-safety metadata, planner-support metadata, and routine-local `SET` configuration without changing identity or replacing the compiled body. `ALTER FUNCTION`, `ALTER PROCEDURE`, and `ALTER ROUTINE` can transfer an exact routine to another existing owner. An explicit signature selects one exact input identity, `()` selects only the zero-input identity, and an omitted signature succeeds only when one visible routine of the requested kind remains unambiguous. Signature types can use implemented `%TYPE` references, and resolution follows `search_path` before checking whether the selected object has the requested kind.

The changed attributes are visible in `pg_proc` and survive catalog reopen. Only the bootstrap superuser may mark a function leakproof or select one of the recognized PostgreSQL planner-support functions; UQA Engine records the corresponding PostgreSQL support OID but does not yet apply every planner consequence of those support callbacks. Missing, ambiguous, wrong-kind, privilege, and configuration failures leave the prior definition intact; function-only attributes applied through `ALTER PROCEDURE` or to a procedure selected by `ALTER ROUTINE` report invalid function definition (`42P13`).

```sql execute
CREATE FUNCTION manual_identity(value INTEGER)
RETURNS INTEGER
LANGUAGE sql
VOLATILE
CALLED ON NULL INPUT
AS 'SELECT $1';

ALTER FUNCTION manual_identity(INTEGER) STABLE STRICT;
SELECT manual_identity(7);
```

### Routine ownership, roles, and EXECUTE

The durable bootstrap role is `uqa`. The implemented role lifecycle includes `CREATE ROLE` or `CREATE USER`, `ALTER ROLE`, and `DROP ROLE` for the `SUPERUSER`, `INHERIT`, `CREATEROLE`, `CREATEDB`, `LOGIN`, `REPLICATION`, `BYPASSRLS`, and connection-limit attributes. `pg_roles` and `pg_user` expose this state, and a role that owns a routine or appears in its ACL cannot be dropped until that dependency is removed.

New routines are owned by `current_user` and grant `EXECUTE` to `PUBLIC` by default. `GRANT` and `REVOKE` accept `EXECUTE` or `ALL [PRIVILEGES]`, exact function, procedure, or routine signatures, `PUBLIC`, `CURRENT_USER`, `SESSION_USER`, and grant-option changes. An owner or superuser may transfer ownership and alter ACLs; execution checks occur before `STRICT` null short-circuiting. `CREATE OR REPLACE` preserves the existing owner and ACL.

`SECURITY INVOKER` runs with the caller's `current_user`; `SECURITY DEFINER` temporarily uses the routine owner while `session_user` remains unchanged. Routine `SET`, `SET ... FROM CURRENT`, `RESET`, and `RESET ALL` configuration is applied only during the call and restored on every return or error. Role memberships, passwords, per-role settings, schema and database privileges, default privileges, non-owner grantor chains, row-level-security consequences, extension languages, and the complete PostgreSQL object privilege model remain compatibility bugs.

## Volatility and mutation

Declare `IMMUTABLE`, `STABLE`, or `VOLATILE` accurately. A SQL or runtime callback that changes database or session state must be volatile. An immutable function must depend only on its arguments and may be duplicated, reordered, or eliminated by safe optimizer rewrites.

## Routine lifecycle

```sql
DROP FUNCTION add_tax(NUMERIC, NUMERIC);
DROP PROCEDURE record_message(TEXT);
```

Signatures disambiguate overloads. `DROP FUNCTION` uses RESTRICT behavior: an unrelated overload may be dropped, but an exact user-function signature referenced by a stored scalar expression, table-function source, `ROWS FROM` member, nested query, generated column, trigger, or SQL-standard routine query body is retained and reports `2BP01` with its dependents. SQL-standard query bodies bind exact identities when the routine is created and rebuild those bindings on reopen, while string-literal SQL and PL/pgSQL bodies retain PostgreSQL's dynamic dependency behavior. Multi-target drops preflight the whole set, so an internal dependency is satisfied when both routines are explicit targets, and `CREATE OR REPLACE VIEW` or routine replacement atomically replaces the prior dependency set.

For the implemented CASCADE graph, `DROP FUNCTION signature CASCADE` removes the exact routine, triggers, generated columns and stored views bound to it, transitive stored views, and transitive SQL-standard query-body functions or procedures while retaining unrelated overloads and objects. A single dependent emits PostgreSQL's `drop cascades to ...` notice and multiple dependents emit the object-count notice. Wrong-kind, missing, and RESTRICT failures are atomic, committed dependency bindings survive reopen, and SQL string bodies remain callable until their dynamically resolved target is actually needed. SQL-standard command-body calls and mutations, parameter-default dependencies, and PostgreSQL object kinds outside routines, triggers, generated columns, and stored views remain compatibility bugs.

Durable SQL and PL/pgSQL routine definitions are restored with the catalog. Rust, Python, Node.js, and browser WASM runtime callbacks are not durable and must be registered after process start.

## Cancellation and failure

Cancellation is session-local and cooperative. Routine errors propagate through SQL. An exception handler can catch implemented SQL-state categories inside PL/pgSQL; unhandled errors abort the current statement and should lead the caller to roll back an explicit transaction when its invariant is no longer satisfiable.
