-- Unified Query Algebra
-- Copyright (c) 2023-2026 Cognica, Inc.
-- Captured with PostgreSQL 18.4 (postgres:18.4-alpine).
\set ON_ERROR_STOP on
BEGIN;
CREATE TEMP TABLE trigger_source(id integer);
CREATE FUNCTION pg_temp.saved() RETURNS trigger LANGUAGE plpgsql AS $$BEGIN RETURN NEW; END$$;
CREATE TRIGGER retained BEFORE INSERT ON trigger_source FOR EACH ROW EXECUTE FUNCTION pg_temp.saved('event argument');
ALTER FUNCTION pg_temp.saved() RENAME TO renamed;
CREATE FUNCTION pg_temp.saved() RETURNS integer LANGUAGE plpgsql AS $$BEGIN RETURN 1; END$$;
SELECT 'renamed_trigger_identity', tgfoid = 'pg_temp.renamed()'::regprocedure FROM pg_trigger WHERE tgname = 'retained' AND tgrelid = 'trigger_source'::regclass;
INSERT INTO trigger_source VALUES(7);
SELECT 'renamed_trigger_effect', id FROM trigger_source;
SAVEPOINT definitions;
\set ON_ERROR_STOP off
CREATE TRIGGER wrong_return BEFORE INSERT ON trigger_source FOR EACH ROW EXECUTE FUNCTION pg_temp.saved();
\echo wrong_return :SQLSTATE
ROLLBACK TO definitions;
CREATE FUNCTION pg_temp.with_argument(integer) RETURNS trigger LANGUAGE plpgsql AS $$BEGIN RETURN NEW; END$$;
\echo declared_argument :SQLSTATE
ROLLBACK TO definitions;
CREATE FUNCTION pg_temp.sql_trigger() RETURNS trigger LANGUAGE sql AS 'SELECT NULL';
\echo sql_trigger :SQLSTATE
ROLLBACK TO definitions;
CREATE FUNCTION pg_temp.set_trigger() RETURNS SETOF trigger LANGUAGE plpgsql AS $$BEGIN RETURN NEW; END$$;
\echo set_return :SQLSTATE
CREATE TRIGGER set_return BEFORE INSERT ON trigger_source FOR EACH ROW EXECUTE FUNCTION pg_temp.set_trigger();
\echo set_trigger :SQLSTATE
INSERT INTO trigger_source VALUES(8);
\echo set_trigger_execution :SQLSTATE
SELECT 'set_trigger_effect', array_agg(id ORDER BY id) FROM trigger_source;
SELECT pg_temp.set_trigger();
\echo direct_set_call :SQLSTATE
ROLLBACK TO definitions;
ROLLBACK;
