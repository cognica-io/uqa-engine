# Transactions, Sessions, and Routines

UQA-RS implements SQL transaction control, savepoints, prepared statements, session variables, SQL-language routines, and a tested PL/pgSQL subset.

## Transaction control

```sql
BEGIN;
UPDATE accounts SET balance = balance - 10 WHERE account_id = 1;
UPDATE accounts SET balance = balance + 10 WHERE account_id = 2;
COMMIT;
```

`START TRANSACTION` is accepted as the explicit begin form. `ROLLBACK` discards the active transaction.

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

Every `Engine::new_session()` has independent transaction state, parameters, prepared statements, statement cache, sequence session state, random seed, notices, and cancellation token. Sessions share durable catalog and row state plus runtime function registries.

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

`DISCARD ALL`, `DISCARD PLANS`, and `DISCARD SEQUENCES` reset their implemented session state. `DISCARD TEMP` is rejected because temporary relations are not implemented.

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

Bound cursors support `CURSOR [(arguments)] FOR query`, positional or named `OPEN` arguments, repeated `FETCH NEXT ... INTO`, `FOUND`, and `CLOSE` within one routine activation. The query result and cursor position are owned by the PL/pgSQL interpreter. `OPEN ... FOR`, dynamic cursor queries, `MOVE`, fetch directions other than `NEXT`, `refcursor` parameters or returns, and cursors left open when a routine exits are rejected because those forms require session-level portal state that is not implemented.

This is a deliberate subset. Validate every routine body during migration instead of assuming all PostgreSQL PL/pgSQL statements or diagnostics exist.

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

Routine identity includes schema, name, kind, and input argument types. The engine resolves overloaded calls with implemented coercion rules and supports default and named arguments.

`CREATE OR REPLACE` replaces a compatible routine identity. It does not permit an incompatible return contract to masquerade as the same routine. Use qualified names when `search_path` could make an overload ambiguous.

## Volatility and mutation

Declare `IMMUTABLE`, `STABLE`, or `VOLATILE` accurately. A SQL or runtime callback that changes database or session state must be volatile. An immutable function must depend only on its arguments and may be duplicated, reordered, or eliminated by safe optimizer rewrites.

## Routine lifecycle

```sql
DROP FUNCTION add_tax(NUMERIC, NUMERIC);
DROP PROCEDURE record_message(TEXT);
```

Signatures disambiguate overloads. Durable SQL and PL/pgSQL routine definitions are restored with the catalog. Rust, Python, Node.js, and browser WASM runtime callbacks are not durable and must be registered after process start.

## Cancellation and failure

Cancellation is session-local and cooperative. Routine errors propagate through SQL. An exception handler can catch implemented SQL-state categories inside PL/pgSQL; unhandled errors abort the current statement and should lead the caller to roll back an explicit transaction when its invariant is no longer satisfiable.
