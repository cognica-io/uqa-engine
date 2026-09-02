\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned

DROP SCHEMA IF EXISTS uqa_rel_hidden CASCADE;
DROP SCHEMA IF EXISTS uqa_rel_visible CASCADE;
DROP ROLE IF EXISTS uqa_rel_caller;
DROP ROLE IF EXISTS uqa_rel_group;
DROP ROLE IF EXISTS uqa_rel_member;

CREATE ROLE uqa_rel_caller;
CREATE ROLE uqa_rel_group;
CREATE ROLE uqa_rel_member INHERIT;
GRANT uqa_rel_group TO uqa_rel_member;
CREATE SCHEMA uqa_rel_hidden;
CREATE SCHEMA uqa_rel_visible;
REVOKE ALL ON SCHEMA uqa_rel_hidden, uqa_rel_visible FROM PUBLIC;

CREATE OR REPLACE FUNCTION pg_temp.rel_scalar_probe(label text, role_name text, path text, command text)
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

CREATE OR REPLACE FUNCTION pg_temp.rel_command_probe(label text, role_name text, path text, command text)
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

CREATE TABLE uqa_rel_hidden.pick(id integer);
INSERT INTO uqa_rel_hidden.pick VALUES (1);
CREATE TABLE uqa_rel_visible.pick(id integer);
INSERT INTO uqa_rel_visible.pick VALUES (2);
CREATE TABLE uqa_rel_hidden.only_table(id integer);
INSERT INTO uqa_rel_hidden.only_table VALUES (7);
CREATE TABLE uqa_rel_hidden.ddl_table(id integer);
ALTER TABLE uqa_rel_hidden.ddl_table OWNER TO uqa_rel_caller;
CREATE VIEW uqa_rel_visible.bound_view AS SELECT id FROM uqa_rel_hidden.only_table;
CREATE VIEW uqa_rel_visible.invoker_view WITH (security_invoker=true) AS SELECT id FROM uqa_rel_hidden.only_table;
CREATE MATERIALIZED VIEW uqa_rel_visible.bound_matview AS SELECT id FROM uqa_rel_hidden.only_table;
CREATE FUNCTION uqa_rel_visible.bound_function() RETURNS integer LANGUAGE SQL RETURN (SELECT id FROM uqa_rel_hidden.only_table);
CREATE FUNCTION uqa_rel_visible.dynamic_invoker() RETURNS integer LANGUAGE SQL SECURITY INVOKER AS 'SELECT id FROM uqa_rel_hidden.only_table';
CREATE FUNCTION uqa_rel_visible.dynamic_definer() RETURNS integer LANGUAGE SQL SECURITY DEFINER AS 'SELECT id FROM uqa_rel_hidden.only_table';

GRANT SELECT, INSERT, UPDATE, DELETE, TRUNCATE ON uqa_rel_hidden.pick, uqa_rel_hidden.only_table TO uqa_rel_caller;
GRANT SELECT ON uqa_rel_visible.pick, uqa_rel_visible.bound_view, uqa_rel_visible.invoker_view, uqa_rel_visible.bound_matview TO uqa_rel_caller;
GRANT USAGE ON SCHEMA uqa_rel_visible TO uqa_rel_caller;

SELECT pg_temp.rel_scalar_probe('unqualified-skip', 'uqa_rel_caller', 'uqa_rel_hidden,uqa_rel_visible,pg_catalog', 'SELECT id FROM pick');
SELECT pg_temp.rel_scalar_probe('unqualified-missing', 'uqa_rel_caller', 'uqa_rel_hidden,pg_catalog', 'SELECT id FROM only_table');
SELECT pg_temp.rel_scalar_probe('qualified-existing', 'uqa_rel_caller', 'pg_catalog', 'SELECT id FROM uqa_rel_hidden.only_table');
SELECT pg_temp.rel_scalar_probe('qualified-missing', 'uqa_rel_caller', 'pg_catalog', 'SELECT id FROM uqa_rel_hidden.missing_table');
SELECT pg_temp.rel_scalar_probe('missing-schema', 'uqa_rel_caller', 'pg_catalog', 'SELECT id FROM uqa_rel_absent.missing_table');
SELECT pg_temp.rel_scalar_probe('column-precedence', 'uqa_rel_caller', 'pg_catalog', 'SELECT missing_column FROM uqa_rel_hidden.only_table');
SELECT pg_temp.rel_scalar_probe('where-column-precedence', 'uqa_rel_caller', 'pg_catalog', 'SELECT id FROM uqa_rel_hidden.only_table WHERE missing_column = 1');
SELECT pg_temp.rel_command_probe('insert-schema', 'uqa_rel_caller', 'pg_catalog', 'INSERT INTO uqa_rel_hidden.only_table VALUES (9)');
SELECT pg_temp.rel_command_probe('update-schema', 'uqa_rel_caller', 'pg_catalog', 'UPDATE uqa_rel_hidden.only_table SET id = 9');
SELECT pg_temp.rel_command_probe('delete-schema', 'uqa_rel_caller', 'pg_catalog', 'DELETE FROM uqa_rel_hidden.only_table');
SELECT pg_temp.rel_command_probe('truncate-schema', 'uqa_rel_caller', 'pg_catalog', 'TRUNCATE uqa_rel_hidden.only_table');
SELECT pg_temp.rel_command_probe('alter-schema', 'uqa_rel_caller', 'pg_catalog', 'ALTER TABLE uqa_rel_hidden.ddl_table ADD COLUMN marker integer');
SELECT pg_temp.rel_command_probe('drop-schema', 'uqa_rel_caller', 'pg_catalog', 'DROP TABLE uqa_rel_hidden.ddl_table');
SELECT pg_temp.rel_scalar_probe('regclass-hard', 'uqa_rel_caller', 'pg_catalog', $$SELECT 'uqa_rel_hidden.only_table'::regclass::oid::text$$);
SELECT pg_temp.rel_scalar_probe('regclass-soft', 'uqa_rel_caller', 'pg_catalog', $$SELECT to_regclass('uqa_rel_hidden.only_table')::oid::text$$);
SELECT pg_temp.rel_scalar_probe('bound-view', 'uqa_rel_caller', 'pg_catalog', 'SELECT id FROM uqa_rel_visible.bound_view');
SELECT pg_temp.rel_scalar_probe('invoker-view', 'uqa_rel_caller', 'pg_catalog', 'SELECT id FROM uqa_rel_visible.invoker_view');
SELECT pg_temp.rel_scalar_probe('bound-matview', 'uqa_rel_caller', 'pg_catalog', 'SELECT id FROM uqa_rel_visible.bound_matview');
SELECT pg_temp.rel_scalar_probe('bound-function', 'uqa_rel_caller', 'pg_catalog', 'SELECT uqa_rel_visible.bound_function()');
SELECT pg_temp.rel_scalar_probe('dynamic-invoker', 'uqa_rel_caller', 'pg_catalog', 'SELECT uqa_rel_visible.dynamic_invoker()');
SELECT pg_temp.rel_scalar_probe('dynamic-definer', 'uqa_rel_caller', 'pg_catalog', 'SELECT uqa_rel_visible.dynamic_definer()');

GRANT USAGE ON SCHEMA uqa_rel_hidden TO uqa_rel_caller;
SELECT pg_temp.rel_scalar_probe('first-visible', 'uqa_rel_caller', 'uqa_rel_hidden,uqa_rel_visible,pg_catalog', 'SELECT id FROM pick');
SELECT pg_temp.rel_command_probe('prepare-with-usage', 'uqa_rel_caller', 'pg_catalog', 'PREPARE uqa_rel_prepared AS SELECT id FROM uqa_rel_hidden.only_table');
REVOKE USAGE ON SCHEMA uqa_rel_hidden FROM uqa_rel_caller;
SELECT pg_temp.rel_scalar_probe('prepared-after-revoke', 'uqa_rel_caller', 'pg_catalog', 'EXECUTE uqa_rel_prepared');

GRANT USAGE ON SCHEMA uqa_rel_hidden TO uqa_rel_caller;
SET ROLE uqa_rel_caller;
BEGIN;
DECLARE uqa_rel_cursor CURSOR FOR SELECT id FROM uqa_rel_hidden.only_table;
RESET ROLE;
REVOKE USAGE ON SCHEMA uqa_rel_hidden FROM uqa_rel_caller;
SET ROLE uqa_rel_caller;
FETCH uqa_rel_cursor \gset uqa_rel_cursor_
SELECT 'bound-cursor|ok|' || :'uqa_rel_cursor_id';
ROLLBACK;
RESET ROLE;
REVOKE USAGE ON SCHEMA uqa_rel_hidden FROM uqa_rel_caller;

GRANT USAGE ON SCHEMA uqa_rel_hidden TO uqa_rel_group;
GRANT SELECT ON uqa_rel_hidden.only_table TO uqa_rel_member;
SELECT pg_temp.rel_scalar_probe('inherited-usage', 'uqa_rel_member', 'pg_catalog', 'SELECT id FROM uqa_rel_hidden.only_table');

DEALLOCATE uqa_rel_prepared;
DROP SCHEMA uqa_rel_visible CASCADE;
DROP SCHEMA uqa_rel_hidden CASCADE;
DROP OWNED BY uqa_rel_member;
DROP OWNED BY uqa_rel_group;
DROP OWNED BY uqa_rel_caller;
DROP ROLE uqa_rel_member;
DROP ROLE uqa_rel_group;
DROP ROLE uqa_rel_caller;
