\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned

DROP SCHEMA IF EXISTS uqa_routine_schema_hidden CASCADE;
DROP SCHEMA IF EXISTS uqa_routine_schema_visible CASCADE;
DROP ROLE IF EXISTS uqa_routine_schema_member;
DROP ROLE IF EXISTS uqa_routine_schema_group;
DROP ROLE IF EXISTS uqa_routine_schema_caller;
DROP ROLE IF EXISTS uqa_routine_schema_owner;

CREATE ROLE uqa_routine_schema_owner;
CREATE ROLE uqa_routine_schema_caller;
CREATE ROLE uqa_routine_schema_group;
CREATE ROLE uqa_routine_schema_member INHERIT;
GRANT uqa_routine_schema_group TO uqa_routine_schema_member;
CREATE SCHEMA uqa_routine_schema_hidden AUTHORIZATION uqa_routine_schema_owner;
CREATE SCHEMA uqa_routine_schema_visible AUTHORIZATION uqa_routine_schema_owner;
REVOKE ALL ON SCHEMA uqa_routine_schema_hidden, uqa_routine_schema_visible FROM PUBLIC;

CREATE OR REPLACE FUNCTION pg_temp.routine_schema_scalar_probe(label text, role_name text, path text, command text)
RETURNS text
LANGUAGE plpgsql
AS $oracle$
DECLARE
    state text;
    message text;
    result text;
BEGIN
    EXECUTE format('SET ROLE %I', role_name);
    PERFORM set_config('search_path', path, true);
    EXECUTE command INTO result;
    RESET ROLE;
    RESET search_path;
    RETURN label || '|ok|' || COALESCE(result, 'NULL');
EXCEPTION WHEN OTHERS THEN
    GET STACKED DIAGNOSTICS state = RETURNED_SQLSTATE, message = MESSAGE_TEXT;
    RESET ROLE;
    RESET search_path;
    RETURN label || '|' || state || '|' || message;
END
$oracle$;

CREATE OR REPLACE FUNCTION pg_temp.routine_schema_command_probe(label text, role_name text, path text, command text)
RETURNS text
LANGUAGE plpgsql
AS $oracle$
DECLARE
    state text;
    message text;
BEGIN
    EXECUTE format('SET ROLE %I', role_name);
    PERFORM set_config('search_path', path, true);
    EXECUTE command;
    RESET ROLE;
    RESET search_path;
    RETURN label || '|ok';
EXCEPTION WHEN OTHERS THEN
    GET STACKED DIAGNOSTICS state = RETURNED_SQLSTATE, message = MESSAGE_TEXT;
    RESET ROLE;
    RESET search_path;
    RETURN label || '|' || state || '|' || message;
END
$oracle$;

SET ROLE uqa_routine_schema_owner;
CREATE FUNCTION uqa_routine_schema_hidden.pick(integer) RETURNS text LANGUAGE sql AS 'SELECT ''hidden''::text';
CREATE FUNCTION uqa_routine_schema_visible.pick(integer) RETURNS text LANGUAGE sql AS 'SELECT ''visible''::text';
CREATE FUNCTION uqa_routine_schema_hidden.only_probe() RETURNS integer LANGUAGE sql AS 'SELECT 7';
CREATE FUNCTION uqa_routine_schema_hidden.denied_probe() RETURNS integer LANGUAGE sql AS 'SELECT 8';
REVOKE EXECUTE ON FUNCTION uqa_routine_schema_hidden.denied_probe() FROM PUBLIC;
CREATE PROCEDURE uqa_routine_schema_hidden.procedure_probe() LANGUAGE plpgsql AS 'BEGIN NULL; END';
CREATE FUNCTION uqa_routine_schema_hidden.bound_probe() RETURNS integer LANGUAGE sql IMMUTABLE AS 'SELECT 9';
CREATE VIEW uqa_routine_schema_visible.bound_view AS SELECT uqa_routine_schema_hidden.bound_probe() AS value;
CREATE TABLE uqa_routine_schema_visible.bound_generated(source integer, value integer GENERATED ALWAYS AS (uqa_routine_schema_hidden.bound_probe()) STORED);
CREATE FUNCTION uqa_routine_schema_visible.atomic_probe() RETURNS integer LANGUAGE sql RETURN uqa_routine_schema_hidden.bound_probe();
CREATE FUNCTION uqa_routine_schema_visible.invoker_probe() RETURNS integer LANGUAGE sql SECURITY INVOKER AS 'SELECT uqa_routine_schema_hidden.bound_probe()';
CREATE FUNCTION uqa_routine_schema_visible.definer_probe() RETURNS integer LANGUAGE sql SECURITY DEFINER AS 'SELECT uqa_routine_schema_hidden.bound_probe()';
CREATE FUNCTION uqa_routine_schema_hidden.ddl_probe() RETURNS integer LANGUAGE sql AS 'SELECT 10';
RESET ROLE;
ALTER FUNCTION uqa_routine_schema_hidden.ddl_probe() OWNER TO uqa_routine_schema_caller;
GRANT USAGE ON SCHEMA uqa_routine_schema_visible TO uqa_routine_schema_caller;
GRANT SELECT ON uqa_routine_schema_visible.bound_view TO uqa_routine_schema_caller;
GRANT INSERT, SELECT ON uqa_routine_schema_visible.bound_generated TO uqa_routine_schema_caller;

SELECT pg_temp.routine_schema_scalar_probe('unqualified-skip', 'uqa_routine_schema_caller', 'uqa_routine_schema_hidden,uqa_routine_schema_visible,pg_catalog', 'SELECT pick(1)');
SELECT pg_temp.routine_schema_scalar_probe('unqualified-missing', 'uqa_routine_schema_caller', 'uqa_routine_schema_hidden,pg_catalog', 'SELECT only_probe()');
SELECT pg_temp.routine_schema_scalar_probe('argument-column-precedence', 'uqa_routine_schema_caller', 'pg_catalog', 'SELECT uqa_routine_schema_hidden.pick(missing_column) FROM (SELECT 1 AS present) source');
SELECT pg_temp.routine_schema_scalar_probe('argument-type-precedence', 'uqa_routine_schema_caller', 'pg_catalog', 'SELECT uqa_routine_schema_hidden.pick(CAST(NULL AS missing_routine_type))');
SELECT pg_temp.routine_schema_scalar_probe('where-argument-column-precedence', 'uqa_routine_schema_caller', 'pg_catalog', 'SELECT 1 FROM (SELECT 1 AS present) source WHERE uqa_routine_schema_hidden.pick(missing_column) IS NOT NULL');
SELECT pg_temp.routine_schema_scalar_probe('where-argument-type-precedence', 'uqa_routine_schema_caller', 'pg_catalog', 'SELECT 1 WHERE uqa_routine_schema_hidden.pick(CAST(NULL AS missing_routine_type)) IS NOT NULL');
SELECT pg_temp.routine_schema_scalar_probe('qualified-existing', 'uqa_routine_schema_caller', 'pg_catalog', 'SELECT uqa_routine_schema_hidden.only_probe()');
SELECT pg_temp.routine_schema_scalar_probe('qualified-missing', 'uqa_routine_schema_caller', 'pg_catalog', 'SELECT uqa_routine_schema_hidden.missing_probe()');
SELECT pg_temp.routine_schema_scalar_probe('missing-schema', 'uqa_routine_schema_caller', 'pg_catalog', 'SELECT uqa_routine_schema_absent.missing_probe()');
SELECT pg_temp.routine_schema_scalar_probe('schema-before-execute', 'uqa_routine_schema_caller', 'pg_catalog', 'SELECT uqa_routine_schema_hidden.denied_probe()');
SELECT pg_temp.routine_schema_command_probe('procedure-schema', 'uqa_routine_schema_caller', 'pg_catalog', 'CALL uqa_routine_schema_hidden.procedure_probe()');
SELECT pg_temp.routine_schema_command_probe('alter-schema', 'uqa_routine_schema_caller', 'pg_catalog', 'ALTER FUNCTION uqa_routine_schema_hidden.ddl_probe() IMMUTABLE');
SELECT pg_temp.routine_schema_command_probe('grant-schema', 'uqa_routine_schema_caller', 'pg_catalog', 'GRANT EXECUTE ON FUNCTION uqa_routine_schema_hidden.ddl_probe() TO uqa_routine_schema_group');
SELECT pg_temp.routine_schema_command_probe('revoke-schema', 'uqa_routine_schema_caller', 'pg_catalog', 'REVOKE EXECUTE ON FUNCTION uqa_routine_schema_hidden.ddl_probe() FROM PUBLIC');
SELECT pg_temp.routine_schema_command_probe('drop-schema', 'uqa_routine_schema_caller', 'pg_catalog', 'DROP FUNCTION uqa_routine_schema_hidden.ddl_probe()');

GRANT USAGE ON SCHEMA uqa_routine_schema_hidden TO uqa_routine_schema_caller;
SELECT pg_temp.routine_schema_scalar_probe('unqualified-first-visible', 'uqa_routine_schema_caller', 'uqa_routine_schema_hidden,uqa_routine_schema_visible,pg_catalog', 'SELECT pick(1)');
SELECT pg_temp.routine_schema_scalar_probe('execute-after-usage', 'uqa_routine_schema_caller', 'pg_catalog', 'SELECT uqa_routine_schema_hidden.denied_probe()');
SELECT pg_temp.routine_schema_command_probe('prepare-with-usage', 'uqa_routine_schema_caller', 'pg_catalog', 'PREPARE uqa_routine_schema_prepared AS SELECT uqa_routine_schema_hidden.only_probe()');
REVOKE USAGE ON SCHEMA uqa_routine_schema_hidden FROM uqa_routine_schema_caller;
SELECT pg_temp.routine_schema_command_probe('prepared-after-revoke', 'uqa_routine_schema_caller', 'pg_catalog', 'EXECUTE uqa_routine_schema_prepared');

SELECT pg_temp.routine_schema_scalar_probe('bound-view', 'uqa_routine_schema_caller', 'pg_catalog', 'SELECT value FROM uqa_routine_schema_visible.bound_view');
SELECT pg_temp.routine_schema_scalar_probe('bound-generated', 'uqa_routine_schema_caller', 'pg_catalog', 'INSERT INTO uqa_routine_schema_visible.bound_generated(source) VALUES (1) RETURNING value');
SELECT pg_temp.routine_schema_scalar_probe('bound-atomic', 'uqa_routine_schema_caller', 'pg_catalog', 'SELECT uqa_routine_schema_visible.atomic_probe()');
SELECT pg_temp.routine_schema_scalar_probe('source-invoker', 'uqa_routine_schema_caller', 'pg_catalog', 'SELECT uqa_routine_schema_visible.invoker_probe()');
SELECT pg_temp.routine_schema_scalar_probe('source-definer', 'uqa_routine_schema_caller', 'pg_catalog', 'SELECT uqa_routine_schema_visible.definer_probe()');

GRANT USAGE ON SCHEMA uqa_routine_schema_hidden TO uqa_routine_schema_group;
SELECT pg_temp.routine_schema_scalar_probe('inherited-usage', 'uqa_routine_schema_member', 'pg_catalog', 'SELECT uqa_routine_schema_hidden.only_probe()');

DEALLOCATE uqa_routine_schema_prepared;
DROP SCHEMA uqa_routine_schema_visible CASCADE;
DROP SCHEMA uqa_routine_schema_hidden CASCADE;
DROP OWNED BY uqa_routine_schema_member;
DROP OWNED BY uqa_routine_schema_group;
DROP OWNED BY uqa_routine_schema_caller;
DROP OWNED BY uqa_routine_schema_owner;
DROP ROLE uqa_routine_schema_member;
DROP ROLE uqa_routine_schema_group;
DROP ROLE uqa_routine_schema_caller;
DROP ROLE uqa_routine_schema_owner;
