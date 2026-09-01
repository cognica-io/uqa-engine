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

`BEGIN` and `START TRANSACTION` accept `ISOLATION LEVEL`, `READ ONLY` or `READ WRITE`, and `DEFERRABLE` or `NOT DEFERRABLE` characteristics. `SET TRANSACTION` changes the active transaction subject to PostgreSQL's first-snapshot and savepoint restrictions, `SET SESSION CHARACTERISTICS AS TRANSACTION` changes later transaction defaults, and `COMMIT AND CHAIN` or `ROLLBACK AND CHAIN` starts the next transaction with the current characteristics. Read-only transactions reject permanent-relation DML, every DDL command including temporary-object DDL, and `TRUNCATE` with `25006`; they allow DML against an existing temporary relation, `nextval` and `setval` on an existing temporary sequence, `ANALYZE`, and session-local effects. The four isolation-level names and transaction settings are retained and exposed, but the complete PostgreSQL concurrent-isolation anomaly matrix and imported snapshots remain compatibility bugs.

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
| `default_transaction_isolation` | Mutable transaction default, `read committed` |
| `default_transaction_read_only` | Mutable transaction default, `off` |
| `default_transaction_deferrable` | Mutable transaction default, `off` |
| `transaction_isolation` | Current transaction value |
| `transaction_read_only` | Current transaction value |
| `transaction_deferrable` | Current transaction value |

```sql
SET search_path TO application, public;
SHOW search_path;
SET timezone TO 'UTC';
SHOW timezone;
```

`SET ROLE name` changes `current_user` for the session while preserving `session_user`; `RESET ROLE`, `SET ROLE NONE`, and `SET ROLE DEFAULT` restore the session identity. The embedded connection starts as the durable bootstrap superuser role `uqa`, so it may assume any defined role. A non-superuser session may assume a directly or transitively granted role only through membership edges whose `SET` option is true. A successful `SET ROLE` executed by a `SECURITY INVOKER` routine remains visible to the session, while an error restores the prior identity and any `SET ROLE` attempted inside a `SECURITY DEFINER` context fails with `42501`, including through a nested invoker.

`DISCARD ALL`, `DISCARD PLANS`, and `DISCARD SEQUENCES` reset their implemented session state. `DISCARD TEMP` removes the session's temporary tables, views, sequences, and sequence state; PostgreSQL rejects it inside a transaction, and UQA Engine does the same.

`LOAD 'age'` (also `age.so`, `$libdir/age`, and `$libdir/age.so`) succeeds without side effects because the Apache AGE surface is embedded; any other library name fails as `could not access file "$libdir/name": No such file or directory` (`58P01`). See [Graph SQL and Cypher](07-graph.md) for the AGE session bootstrap.

## SQL cursors

```sql
BEGIN;
DECLARE tasks_cursor SCROLL CURSOR WITH HOLD FOR
SELECT task_id, title FROM tasks ORDER BY task_id;
FETCH FORWARD 10 FROM tasks_cursor;
MOVE BACKWARD 2 FROM tasks_cursor;
COMMIT;
FETCH NEXT FROM tasks_cursor;
CLOSE tasks_cursor;
```

Top-level `DECLARE`, `FETCH`, `MOVE`, and `CLOSE` use session portals with PostgreSQL 18 cursor SQLSTATEs, forward, backward, absolute, and relative positioning, `SCROLL` and `NO SCROLL`, and `WITH HOLD`. Scrollable scan, `VALUES`, filter, projection, sort, and limit pipelines re-execute volatile expressions in the requested direction when PostgreSQL does; physical plans without backwards execution materialize at their supported semantic boundary so revisits retain the values PostgreSQL freezes. `UNION ALL` keeps backwards-capable child plans live and crosses `Append` boundaries in child order or reverse child order; if any child cannot scan backwards, the complete `Append` output is materialized incrementally. A non-holdable portal closes at transaction end, a holdable portal survives commit, and rolling back to a savepoint closes portals declared after that savepoint without rewinding older portal positions. A PostgreSQL simple-query command string may declare and fetch a cursor inside its shared implicit transaction, after which a non-holdable cursor closes. Cursor behavior for PostgreSQL physical node families not implemented by UQA remains open.

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

The implemented PL/pgSQL surface includes declarations, assignment, `IF` and `CASE`, basic loops, `WHILE`, integer, static-query, dynamic-query, and bound-cursor `FOR`, array `FOREACH`, labeled blocks and exits, `RETURN`, `RETURN NEXT`, `RETURN QUERY`, `PERFORM`, static SQL, dynamic `EXECUTE`, nested blocks, recursive calls with a depth limit, diagnostics, exception handlers, and cursors covered by the routine tests.

### Query FOR loops

The implemented forms are `[ <<label>> ] FOR target IN query LOOP statements END LOOP [ label ];`, `[ <<label>> ] FOR target IN EXECUTE text_expression [ USING expression [, ...] ] LOOP statements END LOOP [ label ];`, and `[ <<label>> ] FOR record_variable IN bound_cursor [ ( [ argument_name => ] argument_expression [, ...] ) ] LOOP statements END LOOP [ label ];`.

Static and dynamic query targets can be a record variable, a row variable, or a comma-separated scalar target list. A bound-cursor loop declares its record variable for the loop, so a same-named outer variable is shadowed only within that loop. Cursor arguments can be positional or named; named arguments are matched in declaration order.

The dynamic query expression and each `USING` expression are evaluated exactly once when the loop is entered, with the query text evaluated before the parameters. Static and dynamic loops use an implicit `NO SCROLL` portal and fetch up to 10 rows initially and up to 50 on later fetches, so volatile row expressions in the fetched batch can run before an early `EXIT`; a bound-cursor loop fetches one row at a time because its cursor remains visible to the routine. Every loop pins its portal while the body runs and closes it on normal completion, `EXIT`, `RETURN`, or error. A bound cursor whose variable was NULL receives an automatically generated portal name during the loop and becomes NULL again afterward; an explicit cursor name is retained, including a name assigned by the body.

The target receives each row before the body runs. After a nonempty static or dynamic loop, the target retains the last assigned row; if the query returns no rows, the target is assigned NULL. `FOUND` is not changed by ordinary loop iteration and is set only when the loop exits: true if at least one row was assigned and false otherwise. Evaluating a nonempty bound-cursor argument list follows PostgreSQL's internal single-row `SELECT` behavior, so it sets `FOUND` to true and `ROW_COUNT` to 1 before the first loop body execution.

A NULL dynamic query string fails with SQLSTATE `22004`. A dynamic string containing multiple statements or a command without result rows fails with `42P11`, and the latter is rejected before changing data. Opening a bound cursor that is already in use fails with `42P03`; attempting to `CLOSE` the cursor from inside its loop fails with `24000` because the active portal is pinned.

```sql execute
CREATE FUNCTION manual_dynamic_for(limit_value INTEGER)
RETURNS TEXT
AS $$
DECLARE
    loop_row RECORD;
    output TEXT := '';
BEGIN
    FOR loop_row IN EXECUTE
        'SELECT value FROM generate_series(1, $1) AS source(value) ORDER BY value'
        USING limit_value
    LOOP
        output := output || loop_row.value;
    END LOOP;
    RETURN output || ':found=' || FOUND;
END;
$$ LANGUAGE plpgsql;

SELECT manual_dynamic_for(3);
```

### Array FOREACH

The implemented syntax is `[ <<label>> ] FOREACH target [ SLICE number ] IN ARRAY expression LOOP statements END LOOP [ label ];`. Omitting `SLICE` is equivalent to `SLICE 0`.

The array expression is evaluated exactly once. With `SLICE 0`, the target must not have an array type and receives each scalar element in storage order regardless of declared array dimensions or lower bounds. With a positive `SLICE`, the target must be an array variable and receives successive subarrays whose trailing dimensions and lower bounds are preserved; the slice number cannot exceed the input array's dimension count.

`FOREACH` produces no SQL result rows. Its body can use the ordinary labeled `CONTINUE` and `EXIT` forms, and after the loop `FOUND` is true when at least one iteration began and false for an empty array.

A NULL expression fails with SQLSTATE `22004`; a non-array expression or target whose scalar-versus-array shape does not match `SLICE` fails with `42804`; and a slice number outside `0..array_dimensions` fails with `2202E`. The dimension check precedes the target-shape check, including for a zero-dimensional empty array.

```sql execute
CREATE FUNCTION manual_foreach_sum(items INTEGER[])
RETURNS INTEGER
AS $$
DECLARE
    item INTEGER;
    total INTEGER := 0;
BEGIN
    FOREACH item IN ARRAY items LOOP
        total := total + item;
    END LOOP;
    RETURN total;
END;
$$ LANGUAGE plpgsql IMMUTABLE;

SELECT manual_foreach_sum(ARRAY[1, 2, 3]);
```

PL/pgSQL cursors support bound `CURSOR [(arguments)] FOR query` declarations with positional or named `OPEN` arguments, unbound static `OPEN ... FOR query`, dynamic `OPEN ... FOR EXECUTE ... USING`, explicit `SCROLL` and `NO SCROLL`, directional single-row `FETCH ... INTO`, `MOVE` directions and counts including `ALL`, `FOUND`, `ROW_COUNT`, and `CLOSE`. Static and dynamic cursor plans may be relational queries, `SHOW`, `EXPLAIN`, `CALL` with output parameters, or `INSERT`, `UPDATE`, `DELETE`, and `MERGE` with `RETURNING`; a mutation command without a result row is rejected before it can change data. Query cursors use query-dependent default scrollability, while command cursors default to `NO SCROLL`; PostgreSQL 18's explicit-`SCROLL` DML behavior for `INSERT`, `UPDATE`, and `DELETE`, which retains the returned row count while exposing `NULL` values, is preserved, `CALL` output cursors may scroll explicitly, and explicit-`SCROLL` `MERGE` cursors fail with SQLSTATE `0A000`.

A row-returning command cursor does not execute at `OPEN`. Its complete command runs when the portal is first used, including by `MOVE 0`, and execution errors are reported by that operation; closing the unread cursor has no command side effects. `CALL` returns its output-parameter row, and `MERGE ... RETURNING` completes every selected action before the first row is fetched. Query cursors evaluate target expressions for rows discarded by `OFFSET`, stop before rows excluded by `LIMIT`, and remain one-row incremental. Scrollable scan, `VALUES`, filter, projection, sort, and limit pipelines re-evaluate volatile expressions in the requested direction, including PostgreSQL's `FETCH 0` back-and-forward execution and execution-free `MOVE 0`; plans without backwards execution materialize below re-evaluated parent expressions or at the completed plan boundary according to the supported physical shape. Native `UNION ALL` branches retain independent directional state across boundaries, nested unions, and surrounding limits, while one unsupported branch causes the complete `Append` output to retain its first-evaluation values. An opened cursor is a session portal, so a routine may return its `refcursor` name and a later routine or SQL statement in the same session and transaction may continue fetching. Cursor behavior for PostgreSQL physical node families not implemented by UQA remains open.

This is a deliberate subset. Validate every routine body during migration instead of assuming all PostgreSQL PL/pgSQL statements or diagnostics exist.

## Triggers

### Syntax

The implemented ordinary creation form is `CREATE [OR REPLACE] TRIGGER name { BEFORE | AFTER } event [ OR event ... ] ON relation [ REFERENCING { OLD TABLE [ AS ] old_transition_name | NEW TABLE [ AS ] new_transition_name } [...] ] [ FOR EACH { ROW | STATEMENT } ] [ WHEN (condition) ] EXECUTE FUNCTION function_name([argument, ...])`, where an event is `INSERT`, `UPDATE [ OF column [, ...] ]`, `DELETE`, or `TRUNCATE`. The implemented constraint form is `CREATE CONSTRAINT TRIGGER name AFTER event [ OR event ... ] ON relation [ FROM referenced_relation ] [ NOT DEFERRABLE | DEFERRABLE ] [ INITIALLY IMMEDIATE | INITIALLY DEFERRED ] FOR EACH ROW [ WHEN (condition) ] EXECUTE FUNCTION function_name([argument, ...])`, where the event is `INSERT`, `UPDATE [ OF column [, ...] ]`, or `DELETE`. Lifecycle forms are `DROP TRIGGER [ IF EXISTS ] name ON relation`, `ALTER TRIGGER name ON relation RENAME TO new_name`, `ALTER TABLE relation RENAME CONSTRAINT name TO new_name`, and `ALTER TABLE relation { ENABLE | DISABLE } TRIGGER { name | ALL | USER }`; `SET CONSTRAINTS { ALL | name [, ...] } { DEFERRED | IMMEDIATE }` controls deferrable constraint triggers inside a transaction.

### Arguments

The relation, optional referenced relation, trigger, constraint, function, transition-table, and `UPDATE OF` column names are identifiers. The `WHEN` clause is a Boolean expression over the event row images allowed at that timing and level. Function arguments are stored string values exposed through `TG_ARGV`; the trigger function itself has no declared ordinary arguments and returns `trigger`.

### Result, effects, and errors

Creation, replacement, rename, enable state, and removal update the durable trigger catalog transactionally and invalidate affected prepared plans. Trigger DDL has no row result. Invalid timing/event or transition-relation combinations, non-Boolean or out-of-scope `WHEN` expressions, wrong function signatures, dependency violations, and unsupported trigger kinds fail before partial catalog publication with the PostgreSQL-shaped SQLSTATE documented by the compatibility tests.

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

The implemented trigger surface supports durable PL/pgSQL `BEFORE` and `AFTER` row and statement triggers for `INSERT`, `UPDATE`, `DELETE`, and `TRUNCATE`, including `UPDATE OF`, boolean-typed `WHEN`, multiple events, string arguments, executable `CREATE OR REPLACE TRIGGER`, `DROP TRIGGER`, `ALTER TRIGGER ... RENAME`, and `ALTER TABLE ... ENABLE` or `DISABLE TRIGGER`. Row triggers fire in name order. Returning `NULL` from a `BEFORE` row trigger skips that row; returning a modified `NEW` row changes an insert or update. An `AFTER` row trigger's `WHEN` predicate is decided immediately after its logical row change, while the selected function invocation runs after all rows have been changed and before `AFTER` statement triggers. Statement triggers fire even when the command changes zero rows, foreign-key referential actions run the recursive child row and statement trigger lifecycle, and `TRUNCATE` triggers cover every explicit, descendant, and `CASCADE`-added relation in statement discovery order. The superuser-only `session_replication_role` setting accepts `origin`, `local`, and `replica`: `origin` and `local` execute Origin and Always triggers, `replica` executes Replica and Always triggers, Disabled triggers never execute, and replica mode suppresses foreign-key check and referential-action triggers as in PostgreSQL 18.

Views support durable `INSTEAD OF` row triggers for `INSERT`, `UPDATE`, and `DELETE` together with `BEFORE` and `AFTER` statement triggers. Multiple row triggers run alphabetically; `NULL` suppresses later triggers, the affected-row count, and `RETURNING`, while non-NULL `INSERT` and `UPDATE` results flow into the next trigger and final `RETURNING` row. For `DELETE`, PostgreSQL preserves the original `OLD` image for later triggers and `RETURNING` while using a non-NULL result only as the performed-row signal. Table and view `INSERT SELECT`, `UPDATE`, `DELETE`, and trigger-backed `MERGE` source and target scans retain the statement-start snapshot across writes performed by `BEFORE` statement triggers. The view-trigger path covers multi-row `INSERT`, `INSERT SELECT`, `UPDATE FROM`, `DELETE USING`, all `MERGE` candidate and action kinds, source columns and current, `OLD`, and `NEW` row images in `RETURNING`, zero-row statement triggers, catalog deparsing, rename and drop lifecycle, and durable reopen. PostgreSQL's invalid table/view timing, statement level, `WHEN`, `UPDATE OF`, transition-table, `TRUNCATE`, and enable-mode definitions retain their SQLSTATEs.

For a trigger-backed view `MERGE`, every named mutation action must resolve through an `INSTEAD OF` trigger at the same selected layer; a statement cannot mix automatic and trigger-backed action paths, while `DO NOTHING` needs no mutation trigger. `BEFORE` statement events run in INSERT, UPDATE, DELETE order and `AFTER` statement events run in DELETE, UPDATE, INSERT order even when no row selects an action. A `NULL` row-trigger result suppresses that action, a final non-NULL result drives the affected count and `RETURNING`, and repeated source candidates may invoke the trigger repeatedly because no physical view row is modified. Automatic outer-view rewriting can terminate at a trigger-updatable inner view, after which outer check options validate the trigger-returned final row. Trigger definitions continue to select this path when `session_replication_role` suppresses their execution.

Without an `INSTEAD OF` trigger definition, automatically updatable single-source projection views rewrite `INSERT`, `ON CONFLICT`, `UPDATE`, `DELETE`, and `MERGE` variants to the underlying table or nested view. The base relation owns statement and row triggers, defaults, constraints, partition routing, and row images; statement triggers declared on the automatically rewritten view do not fire. Direct projected columns are writable, omitted trailing implicit-INSERT columns take base defaults, computed and system columns are read-only but available to predicates and `RETURNING`, and partitioned DML retains the physical leaf `tableoid` in current, `OLD`, `NEW`, and rule images. The creation-time public view row type plus ordinary target/source ambiguity checks are enforced before automatic-updatability shape errors, including INSERT `RETURNING` and correlated scalar-subquery references across the complete containing DML namespace, including target and `excluded` rows, `FROM` or `USING` sources, and explicit `OLD` and `NEW` `RETURNING` aliases; `UPDATE FROM` and `DELETE USING` preserve source-only hidden names even for an unaliased derived source, preserve source relations named `old` or `new`, and emit target columns before source columns for bare `RETURNING *`; `MERGE` instead emits source columns before public view-target columns. A target-list set-returning expression prevents automatic updatability, and a view with no writable columns still supports automatic DELETE but not INSERT or UPDATE. Nested `LOCAL` or `CASCADED` check options validate post-`BEFORE` rows atomically from the innermost view outward for ordinary and `UPDATE FROM` paths, while creating or altering a directly non-updatable view with a check option is rejected without persisting the option. For `INSERT`, `UPDATE`, and `DELETE`, `ALSO` and `INSTEAD` rules on every automatic-view rewrite layer execute in PostgreSQL order, evaluate qualification columns before projecting columns needed only by matching actions, and can provide `RETURNING` with its provider-level scalar subqueries from the actual provider layer; conditional `INSTEAD`-only paths are rejected at every layer, outer suppression prevents inner actions, and INSERT actions run from the base layer outward. `MERGE` rejects a user rewrite rule on any targeted view layer with `0A000`. When rewriting reaches a nonautomatically updatable underlying view with an applicable unconditional rule, that rule handles the mutation while the outer rule layers retain PostgreSQL ordering. An unconditional `INSTEAD` rule suppresses the original view mutation and its row and statement triggers, evaluates only input or assignment expressions required by matching rule conditions and actions, accepts supplied computed view columns when an action consumes them, and reports the affected-row count of the final executed action. A rule-suppressed view INSERT does not evaluate base defaults or generated columns, route a partition, or consume an identity, and its rule image contains NULL for omitted defaulted and identity columns plus NULL `tableoid` before routing; once view rules permit the base rewrite, base-table rules receive applied defaults and identities. When a directly targeted view has both rewrite rules and an `INSTEAD OF` trigger, PostgreSQL event-specific ordering and suppression are preserved for INSERT, UPDATE, and DELETE. Repeated ordinary user `id` values in rule action tables remain distinct from internal document identities across reopen. A defined `INSTEAD OF` row trigger retains the trigger path even when `session_replication_role` suppresses its invocation, so suppression does not fall back to an automatic base write.

PostgreSQL 18 transition relations are available to ordinary `AFTER` row and statement triggers through `REFERENCING OLD TABLE AS name` and `REFERENCING NEW TABLE AS name`. An `INSERT` trigger may name only the new table, a `DELETE` trigger only the old table, and an `UPDATE` trigger either or both; a transition trigger must name exactly one mutation event and cannot use `UPDATE OF`, `TRUNCATE`, `BEFORE`, constraint-trigger syntax, duplicate transition kinds, identical old and new names, or `OLD ROW` and `NEW ROW`. Each invocation sees the complete typed row set for its statement, including an empty set for a zero-row statement, post-`BEFORE` values and stored generated values, separate action sets for `ON CONFLICT` and `MERGE`, `UPDATE FROM`, recursive foreign-key cascades, and root-shaped rows aggregated across partition and inheritance descendants. Multiple direct foreign-key actions on the same relation and event preserve cumulative row changes in one transition set: `BEFORE` statement triggers use the first action's updated columns, while `AFTER` statement triggers use the union and transition relations contain every action row image. Recursive foreign-key actions follow PostgreSQL's transition-set boundaries: without an `AFTER ROW` transition trigger their row images remain coalesced, while such a row trigger can close the current set and cause deeper cascade steps to receive separate transition relations and statement-trigger invocations; multi-row `ON CONFLICT DO UPDATE` preserves one statement-global event order while combining independently prepared cascade trees. Row-level transition triggers are rejected on a partitioned table, a partition, or an inheritance child. Transition relations are scoped to their declaring trigger routine, masked from nested ordinary routines and triggers, and cannot be captured by a persistent view or materialized view.

Constraint triggers implement PostgreSQL 18 `AFTER ROW` execution for `INSERT`, `UPDATE`, and `DELETE`, including multiple events, `UPDATE OF`, optional `FROM`, `NOT DEFERRABLE`, `DEFERRABLE INITIALLY IMMEDIATE`, and `DEFERRABLE INITIALLY DEFERRED`. A deferred event captures the row image after the row change and evaluates its `WHEN` predicate at that time, then invokes the stored trigger at `SET CONSTRAINTS ... IMMEDIATE` or outer commit; savepoint rollback, trigger removal, and removal of the referenced `FROM` relation discard the affected queued events. `SET CONSTRAINTS` name resolution shares PostgreSQL constraint namespaces with foreign keys, applies `ALL` and per-constraint modes, and immediately drains already queued matching events when a mode changes to `IMMEDIATE`.

Trigger functions declare no ordinary parameters and return `trigger`. They receive `OLD`, `NEW`, `TG_NAME`, `TG_WHEN`, `TG_LEVEL`, `TG_OP`, `TG_RELID`, `TG_TABLE_NAME`, `TG_TABLE_SCHEMA`, `TG_NARGS`, and zero-based `TG_ARGV`; calling a trigger function directly fails with SQLSTATE `0A000`. Stored generated columns are recomputed after `BEFORE` triggers, so their fields in `NEW` are `NULL` during a `BEFORE` trigger and available afterward, while virtual generated fields are `NULL` in every trigger `OLD` and `NEW` image as in PostgreSQL 18. `INSERT ... ON CONFLICT` and `MERGE` invoke the applicable root and referential-action triggers in PostgreSQL order, and a trigger-suppressed `MERGE` action does not consume that target's once-per-row modification identity. A partition-moving `UPDATE` uses source DELETE and destination INSERT row-trigger lifecycles after source UPDATE `BEFORE` triggers, preserves UPDATE statement triggers and transition rows, and applies PostgreSQL's distinct empty UPDATE transition sets for partition-moving MERGE actions.

Trigger definitions participate in table, referenced-table, column, and exact zero-argument function dependencies, survive storage reopen, roll back with catalog transactions, preserve temporary triggers across unrelated catalog reloads, and invalidate prepared plans. `pg_trigger`, including `tgoldtable` and `tgnewtable`, trigger-owned `pg_constraint` rows, `pg_class.relhastriggers`, `pg_tables.hastriggers`, and both PostgreSQL 18 `pg_get_triggerdef` overloads expose the implemented catalog; the boolean overload selects compact or pretty rendering and propagates `NULL`, and trigger deparsing includes transition-table clauses. Trigger and constraint names have independent rename lifecycles and stable independent OIDs. A row trigger created on a partitioned table fires for leaf rows and appears as a parent-linked leaf clone with its own trigger and constraint identities.

Remaining PostgreSQL automatic-updatability shapes involving CTE-backed definitions or other unverified query forms, direct ALTER or DROP operations on generated partition clones, trigger privileges, exact PostgreSQL `pg_node_tree` serialization, dump and restore, and the complete upstream trigger regression schedule remain compatibility bugs.

## Rewrite rules

### Syntax

The implemented creation form is `CREATE [OR REPLACE] RULE name AS ON { INSERT | UPDATE | DELETE } TO relation [ WHERE condition ] DO [ ALSO | INSTEAD ] { NOTHING | action | (action; ...) }`. Lifecycle forms are `DROP RULE [ IF EXISTS ] name ON relation`, `ALTER RULE name ON relation RENAME TO new_name`, and `ALTER TABLE relation { ENABLE | DISABLE } RULE { name | ALL | USER }`.

### Arguments

The rule and relation names are identifiers. `OLD` and `NEW` expose the event-row fields valid for the selected event, the optional condition is a Boolean expression over the supported event scope, and each action is an implemented `INSERT`, `UPDATE`, or `DELETE` statement. `ALSO` preserves the original command; `INSTEAD` conditionally or unconditionally replaces it; `NOTHING` suppresses it for matching rows.

### Result and effects

Rule DDL has no row result. Definitions, enable state, rename, replacement, and removal are durable and transactional, update `pg_rewrite` and `pg_rules`, participate in relation and column lifecycle, and invalidate affected prepared plans. Active actions execute once over the qualified OLD/NEW row set rather than once as an independent SQL statement per event row.

### Errors

Creation rejects invalid OLD/NEW scope, result-column or type mismatches for a DML `RETURNING` provider, multiple providers, recursive definitions, reserved `_RETURN` misuse, and unsupported targets before publishing the definition. Active rules reject `ON CONFLICT` and `MERGE`; unsupported conditional set-operation and multi-row rewritten action shapes report their PostgreSQL 18 SQLSTATEs.

### Example

```sql execute
CREATE TABLE manual_rule_items (id INTEGER PRIMARY KEY, value TEXT);
CREATE TABLE manual_rule_log (item_id INTEGER, value TEXT);
CREATE RULE manual_rule_insert_log AS ON INSERT TO manual_rule_items DO ALSO INSERT INTO manual_rule_log VALUES (NEW.id, NEW.value);
INSERT INTO manual_rule_items VALUES (1, 'created');
SELECT item_id, value FROM manual_rule_log ORDER BY item_id;
```

Durable table rewrite rules execute INSERT VALUES, INSERT SELECT, UPDATE, and DELETE actions with PostgreSQL 18 cardinality: OLD- or NEW-referencing actions consume the matching event rows set-oriented, while row-independent UPDATE and DELETE actions follow the original command's qualification tuples, including no-predicate, false-predicate, empty-target, `UPDATE FROM`, and `DELETE USING` cases. Each bound action remains one statement, so its statement trigger fires once rather than once per source row. The implemented surface includes collision-free internal row sources, correlated LATERAL action sources, PostgreSQL query-scope rejection for event-row references in CTEs, set-operation members, and `ON CONFLICT DO UPDATE`, unqualified INSERT and DELETE conditions, alphabetical `ALSO` and conditional or unconditional `INSTEAD` ordering, `NOTHING`, recursion protection, replacement with enable-state retention, rename/drop and target-column lifecycle, view `_RETURN` replacement and protection, and PostgreSQL-shaped `pg_rewrite`, `pg_rules`, rule flags, and `pg_get_ruledef`. As in PostgreSQL 18, an unconditional `ON INSERT` action containing `UNION`, `INTERSECT`, or `EXCEPT` succeeds for a one-row insert but returns `0A000` when a multi-row insert makes the rewritten action conditional. An unconditional `INSTEAD` rule can provide DML `RETURNING` positionally with PostgreSQL-compatible creation-time row type and type-modifier checks, single-provider restrictions, lazily evaluated provider projections, provider action current/OLD/NEW images, outer row-image aliases, outer `UPDATE FROM` source columns for UPDATE-provider actions, and outer `DELETE USING` source columns for DELETE-provider actions. Active rules reject `ON CONFLICT` and `MERGE` with the PostgreSQL SQLSTATEs. Rule enable modes follow `session_replication_role`: Origin and Always rules run for `origin` and `local`, Replica and Always rules run for `replica`, and Disabled rules never run. Rule condition subqueries, OLD/NEW record expansion, `NOTIFY` actions, complete dependencies, inheritance and partition behavior, privileges, exact node trees and deparsing, dump/restore, and the complete upstream rule schedule remain compatibility bugs.

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

Role membership accepts PostgreSQL 18 `GRANT role [, ...] TO role [, ...] [WITH ADMIN {TRUE|FALSE}, INHERIT {TRUE|FALSE}, SET {TRUE|FALSE}] [GRANTED BY role]` and the corresponding full or option-only `REVOKE ... [CASCADE|RESTRICT]`. New edges default `ADMIN` to false and `SET` to true, while `INHERIT` follows the grantee role's `INHERIT` attribute; a re-grant changes only named options. Independent grantors retain independent rows, revoking the last ADMIN path observes dependent grants and `CASCADE`, cycles fail with `0LP01`, role drops remove member edges but reject a surviving grantor dependency, and every mutation is transactional and durable. `CREATE ROLE ... IN ROLE ... ROLE ... ADMIN ...` and legacy `ALTER GROUP ... ADD|DROP USER` use the same graph. `pg_auth_members` exposes PostgreSQL 18's `oid`, `roleid`, `member`, `grantor`, `admin_option`, `inherit_option`, and `set_option` columns. `pg_has_role` implements PostgreSQL's six current-user or explicit-user name/OID overloads: `MEMBER` checks direct or transitive membership, `USAGE` follows `INHERIT`-enabled paths, `SET` follows `SET`-enabled paths, comma-separated checks use OR semantics, and either `WITH ADMIN OPTION` or `WITH GRANT OPTION` tests ADMIN privilege.

```sql
GRANT reporting TO analyst WITH ADMIN FALSE, INHERIT TRUE, SET TRUE;
REVOKE SET OPTION FOR reporting FROM analyst;
```

A `CREATEROLE` user receives an ADMIN, non-inheritable, non-settable membership in each role it creates and can administer that role, but it may create or alter `CREATEDB`, `REPLICATION`, `BYPASSRLS`, `CREATEROLE`, or `SUPERUSER` attributes only when its own current role holds the corresponding authority. `SUPERUSER` changes remain superuser-only.

New routines are owned by `current_user` and grant `EXECUTE` to `PUBLIC` by default. `GRANT` and `REVOKE` accept `EXECUTE` or `ALL [PRIVILEGES]`, exact function, procedure, or routine signatures, `PUBLIC`, `CURRENT_USER`, `SESSION_USER`, grant-option changes, `GRANTED BY`, and `CASCADE` or `RESTRICT`. Owners, superusers, and roles with a direct or inherited grant-option path may alter ACLs; each grantee and grantor path remains independent in `pg_proc.proacl`, while `GRANTED BY` currently accepts only the effective current user. A grant attempted without grant option emits PostgreSQL's warning and changes nothing, `RESTRICT` rejects dependent grants with `2BP01`, and `CASCADE` recursively removes paths that no longer have a rooted grant option while preserving paths with independent authority. Grant option to `PUBLIC` is rejected with `0LP01`; execution checks occur before `STRICT` null short-circuiting. `CREATE OR REPLACE` preserves the existing owner and ACL, and ownership transfer rewrites owner-issued grantor paths to the new owner.

Routine ownership and EXECUTE privileges follow transitive membership edges whose `INHERIT` option is true. An inherited owner may alter, replace, grant on, or drop the routine and retains the owner's implicit EXECUTE privilege; a `SET`-only member must first assume the owner role. Ownership transfer additionally requires the current user to be able to `SET ROLE` to the new owner. Every explicit DROP target is ownership-checked before dependency expansion, so unauthorized multi-target replacement or removal cannot mutate the routine graph.

`SECURITY INVOKER` runs with the caller's `current_user`; `SECURITY DEFINER` temporarily uses the routine owner while `session_user` remains unchanged. Routine `SET`, `SET ... FROM CURRENT`, `RESET`, and `RESET ALL` configuration is applied only during the call and restored on every return or error. Passwords, per-role settings, schema and database privileges, default privileges, row-level-security consequences, extension languages, and the complete PostgreSQL object privilege model remain compatibility bugs.

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
