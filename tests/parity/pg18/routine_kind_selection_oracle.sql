-- Captured from PostgreSQL 18.4 (aarch64), 2026-09-28.
-- Image: postgres@sha256:a02db8cac496f15b094798a38254f14d6e00741f709360e5e00bb6668ea31636
-- Run: docker exec -i uqa-pg18-comparisons psql -U postgres -d postgres -X -qAt < tests/parity/pg18/routine_kind_selection_oracle.sql
-- All catalog changes are rolled back; output is routine_kind_selection_oracle.expected.txt.
\set ON_ERROR_STOP on
BEGIN;
CREATE SCHEMA uqa_routine_kind;
CREATE FUNCTION pg_temp.routine_kind_probe(label text, command text)
RETURNS text LANGUAGE plpgsql AS $oracle$
DECLARE
    result text;
    state text;
    message text;
    hint text;
BEGIN
    EXECUTE command INTO result;
    RETURN label || '|00000|' || coalesce(result, 'NULL');
EXCEPTION WHEN OTHERS THEN
    GET STACKED DIAGNOSTICS state = RETURNED_SQLSTATE,
        message = MESSAGE_TEXT, hint = PG_EXCEPTION_HINT;
    RETURN label || '|' || state || '|' || message || '|' || coalesce(hint, '');
END
$oracle$;
CREATE PROCEDURE uqa_routine_kind.md5(value text)
LANGUAGE plpgsql AS $$ BEGIN NULL; END $$;
SET LOCAL search_path = uqa_routine_kind, pg_catalog, public;
SELECT pg_temp.routine_kind_probe('scalar', 'SELECT md5(''abc''::text)');
SELECT pg_temp.routine_kind_probe('table', 'SELECT * FROM md5(''abc''::text)');
SELECT pg_temp.routine_kind_probe('named-scalar', 'SELECT md5(value => ''abc''::text)');
SELECT pg_temp.routine_kind_probe('named-table', 'SELECT * FROM md5(value => ''abc''::text)');
SELECT pg_temp.routine_kind_probe('qualified', 'SELECT uqa_routine_kind.md5(''abc''::text)');
SET LOCAL search_path = pg_catalog, uqa_routine_kind, public;
SELECT pg_temp.routine_kind_probe('catalog-first', 'SELECT md5(''abc''::text)');
SET LOCAL search_path = uqa_routine_kind, public;
SELECT pg_temp.routine_kind_probe('implicit-catalog', 'SELECT md5(''abc''::text)');
SET LOCAL search_path = uqa_routine_kind, pg_catalog, public;
CREATE FUNCTION uqa_routine_kind.pick(value integer) RETURNS integer
LANGUAGE SQL AS 'SELECT 7';
CREATE PROCEDURE uqa_routine_kind.pick(value bigint)
LANGUAGE plpgsql AS $$ BEGIN NULL; END $$;
SELECT pg_temp.routine_kind_probe('integer-function', 'SELECT pick(1::integer)');
SELECT pg_temp.routine_kind_probe('bigint-procedure', 'SELECT pick(1::bigint)');
SELECT pg_temp.routine_kind_probe('unknown-ambiguity', 'SELECT pick(''1'')');
SELECT pg_temp.routine_kind_probe('call-function', 'CALL pick(1::integer)');
SELECT pg_temp.routine_kind_probe('named-call-function', 'CALL pick(value => 1::integer)');
ROLLBACK;
