# PostgreSQL 18.4 routine security and cursor oracle

This transcript was recorded on 2026-08-25 from the local `uqa-pg18-age:1.8.0` image `sha256:93e3ba7b4cde8eb1a2172e744623449357fb3cd85a0b88e6bf5910161d5902e3`, reporting PostgreSQL `18.4 (Debian 18.4-1.pgdg13+1)`, `server_version_num = 180004`, `aarch64-unknown-linux-gnu`, and Apache AGE `1.8.0`. The complete probe ran inside one transaction and ended with `ROLLBACK`, including its two temporary roles.

## Owner, ACL, execution identity, and pg_proc

```sql
BEGIN;
CREATE ROLE uqa_oracle_owner;
CREATE ROLE uqa_oracle_caller LOGIN;
CREATE FUNCTION pg_temp.uqa_secured()
RETURNS text
LANGUAGE SQL
SECURITY DEFINER
LEAKPROOF
PARALLEL SAFE
SUPPORT textlike_support
SET search_path TO pg_catalog
AS 'SELECT current_user || ''/'' || session_user || ''/'' || current_schema';
ALTER FUNCTION pg_temp.uqa_secured() OWNER TO uqa_oracle_owner;
REVOKE ALL ON FUNCTION pg_temp.uqa_secured() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION pg_temp.uqa_secured() TO uqa_oracle_caller WITH GRANT OPTION;
SET ROLE uqa_oracle_caller;
SELECT current_user, session_user, pg_temp.uqa_secured();
RESET ROLE;
SELECT owner_role.rolname, proc.prosecdef, proc.proleakproof, proc.proparallel, proc.prosupport::regproc, proc.proconfig, proc.proacl
FROM pg_catalog.pg_proc AS proc
JOIN pg_catalog.pg_roles AS owner_role ON owner_role.oid = proc.proowner
WHERE proc.oid = 'pg_temp.uqa_secured()'::regprocedure;
```

| current_user | session_user | function result |
| --- | --- | --- |
| `uqa_oracle_caller` | `postgres` | `uqa_oracle_owner/postgres/pg_catalog` |

| owner | prosecdef | proleakproof | proparallel | prosupport | proconfig | proacl |
| --- | --- | --- | --- | --- | --- | --- |
| `uqa_oracle_owner` | `t` | `t` | `s` | `textlike_support` | `{search_path=pg_catalog}` | `{uqa_oracle_owner=X/uqa_oracle_owner,uqa_oracle_caller=X*/uqa_oracle_owner}` |

## REFCURSOR session portal

```sql
CREATE FUNCTION pg_temp.uqa_cursor_return() RETURNS refcursor LANGUAGE plpgsql AS $$
DECLARE c CURSOR FOR SELECT 11 AS value UNION ALL SELECT 22;
BEGIN OPEN c; RETURN c; END
$$;
CREATE FUNCTION pg_temp.uqa_cursor_input(c refcursor) RETURNS integer LANGUAGE plpgsql AS $$
DECLARE value integer;
BEGIN FETCH c INTO value; RETURN value; END
$$;
SELECT pg_typeof('named'::refcursor)::text,
       pg_temp.uqa_cursor_input(pg_temp.uqa_cursor_return()),
       pg_temp.uqa_cursor_input('<unnamed portal 1>'),
       pg_temp.uqa_cursor_input('<unnamed portal 1>') IS NULL;
```

The result was `refcursor|11|22|t`, proving that the returned name addresses one portal whose position survives the first routine call and is shared by later calls in the same transaction.

A second transaction opened portal 1, created a savepoint, fetched `11`, opened portal 2, rolled back to the savepoint, and fetched `22` from portal 1. This establishes PostgreSQL 18's boundary used by the engine regression: rollback closes a portal created after the savepoint but does not rewind an earlier portal's fetch position.

## SUPPORT NONE boundary

PostgreSQL 18.4 parses `ALTER FUNCTION pg_temp.uqa_secured() SUPPORT NONE` as a request for a function named `none(internal)` and rejects it with SQLSTATE `42883`; it does not clear the support function. UQA Engine keeps the same boundary and leaves the prior routine definition unchanged.
