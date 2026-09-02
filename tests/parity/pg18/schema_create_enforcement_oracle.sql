\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned

DROP SCHEMA IF EXISTS uqa_schema_create_enforcement CASCADE;
DROP SERVER IF EXISTS uqa_schema_create_enforcement_server CASCADE;
DROP ROLE IF EXISTS uqa_schema_create_enforcement_leaf;
DROP ROLE IF EXISTS uqa_schema_create_enforcement_group;
DROP ROLE IF EXISTS uqa_schema_create_enforcement_worker;
DROP ROLE IF EXISTS uqa_schema_create_enforcement_owner;

CREATE ROLE uqa_schema_create_enforcement_owner;
CREATE ROLE uqa_schema_create_enforcement_worker;
CREATE ROLE uqa_schema_create_enforcement_group;
CREATE ROLE uqa_schema_create_enforcement_leaf INHERIT;
CREATE SCHEMA uqa_schema_create_enforcement AUTHORIZATION uqa_schema_create_enforcement_owner;
REVOKE ALL ON SCHEMA uqa_schema_create_enforcement FROM PUBLIC;
SET ROLE uqa_schema_create_enforcement_owner;
CREATE TABLE uqa_schema_create_enforcement.existing_table(id integer);
CREATE TABLE uqa_schema_create_enforcement.index_target(id integer);
CREATE TABLE uqa_schema_create_enforcement.alter_target(id integer);
CREATE VIEW uqa_schema_create_enforcement.existing_view AS SELECT 1 AS id;
CREATE MATERIALIZED VIEW uqa_schema_create_enforcement.existing_matview AS SELECT 1 AS id WITH NO DATA;
CREATE FUNCTION uqa_schema_create_enforcement.existing_function() RETURNS integer LANGUAGE sql AS 'SELECT 1';
RESET ROLE;
ALTER TABLE uqa_schema_create_enforcement.index_target OWNER TO uqa_schema_create_enforcement_worker;
ALTER TABLE uqa_schema_create_enforcement.alter_target OWNER TO uqa_schema_create_enforcement_worker;
GRANT USAGE ON SCHEMA uqa_schema_create_enforcement TO uqa_schema_create_enforcement_worker;
CREATE EXTENSION IF NOT EXISTS postgres_fdw;
CREATE SERVER uqa_schema_create_enforcement_server FOREIGN DATA WRAPPER postgres_fdw OPTIONS (host 'localhost');
GRANT USAGE ON FOREIGN SERVER uqa_schema_create_enforcement_server TO uqa_schema_create_enforcement_worker, uqa_schema_create_enforcement_group;

CREATE OR REPLACE FUNCTION pg_temp.schema_create_enforcement_probe(label text, role_name text, command text)
RETURNS text
LANGUAGE plpgsql
AS $oracle$
DECLARE
    state text;
BEGIN
    EXECUTE format('SET ROLE %I', role_name);
    EXECUTE command;
    RESET ROLE;
    RESET search_path;
    RETURN label || '|ok';
EXCEPTION WHEN OTHERS THEN
    GET STACKED DIAGNOSTICS state = RETURNED_SQLSTATE;
    RESET ROLE;
    RESET search_path;
    RETURN label || '|' || state;
END
$oracle$;

SELECT pg_temp.schema_create_enforcement_probe('table-no-create', 'uqa_schema_create_enforcement_worker', 'CREATE TABLE uqa_schema_create_enforcement.denied_table(id integer)');
SELECT pg_temp.schema_create_enforcement_probe('ctas-no-create', 'uqa_schema_create_enforcement_worker', 'CREATE TABLE uqa_schema_create_enforcement.denied_ctas AS SELECT 1 AS id');
SELECT pg_temp.schema_create_enforcement_probe('ctas-source-precedence', 'uqa_schema_create_enforcement_worker', 'CREATE TABLE uqa_schema_create_enforcement.denied_ctas AS SELECT * FROM uqa_schema_create_enforcement.missing_source');
SELECT pg_temp.schema_create_enforcement_probe('select-into-no-create', 'uqa_schema_create_enforcement_worker', 'SELECT 1 AS id INTO uqa_schema_create_enforcement.denied_into');
SELECT pg_temp.schema_create_enforcement_probe('view-no-create', 'uqa_schema_create_enforcement_worker', 'CREATE VIEW uqa_schema_create_enforcement.denied_view AS SELECT 1 AS id');
SELECT pg_temp.schema_create_enforcement_probe('view-source-precedence', 'uqa_schema_create_enforcement_worker', 'CREATE VIEW uqa_schema_create_enforcement.denied_view AS SELECT * FROM uqa_schema_create_enforcement.missing_source');
SELECT pg_temp.schema_create_enforcement_probe('matview-no-create', 'uqa_schema_create_enforcement_worker', 'CREATE MATERIALIZED VIEW uqa_schema_create_enforcement.denied_matview AS SELECT 1 AS id');
SELECT pg_temp.schema_create_enforcement_probe('matview-source-precedence', 'uqa_schema_create_enforcement_worker', 'CREATE MATERIALIZED VIEW uqa_schema_create_enforcement.denied_matview AS SELECT * FROM uqa_schema_create_enforcement.missing_source');
SELECT pg_temp.schema_create_enforcement_probe('sequence-no-create', 'uqa_schema_create_enforcement_worker', 'CREATE SEQUENCE uqa_schema_create_enforcement.denied_sequence');
SELECT pg_temp.schema_create_enforcement_probe('foreign-table-no-create', 'uqa_schema_create_enforcement_worker', 'CREATE FOREIGN TABLE uqa_schema_create_enforcement.denied_foreign(id integer) SERVER uqa_schema_create_enforcement_server');
SELECT pg_temp.schema_create_enforcement_probe('function-no-create', 'uqa_schema_create_enforcement_worker', 'CREATE FUNCTION uqa_schema_create_enforcement.denied_function() RETURNS integer LANGUAGE sql AS ''SELECT 1''');
SELECT pg_temp.schema_create_enforcement_probe('procedure-no-create', 'uqa_schema_create_enforcement_worker', 'CREATE PROCEDURE uqa_schema_create_enforcement.denied_procedure() LANGUAGE sql AS ''SELECT 1''');
SELECT pg_temp.schema_create_enforcement_probe('index-definition-precedence', 'uqa_schema_create_enforcement_worker', 'CREATE INDEX denied_index ON uqa_schema_create_enforcement.index_target USING missing_method(id)');
SELECT pg_temp.schema_create_enforcement_probe('alter-key-definition-precedence', 'uqa_schema_create_enforcement_worker', 'ALTER TABLE uqa_schema_create_enforcement.alter_target ADD UNIQUE (missing_column)');
SELECT pg_temp.schema_create_enforcement_probe('alter-column-collision-precedence', 'uqa_schema_create_enforcement_worker', 'ALTER TABLE uqa_schema_create_enforcement.alter_target ADD COLUMN id integer UNIQUE');
SELECT pg_temp.schema_create_enforcement_probe('table-existing-ifne', 'uqa_schema_create_enforcement_worker', 'CREATE TABLE IF NOT EXISTS uqa_schema_create_enforcement.existing_table(id integer)');
SELECT pg_temp.schema_create_enforcement_probe('ctas-existing', 'uqa_schema_create_enforcement_worker', 'CREATE TABLE uqa_schema_create_enforcement.existing_table AS SELECT 1 AS id');
SELECT pg_temp.schema_create_enforcement_probe('ctas-existing-ifne', 'uqa_schema_create_enforcement_worker', 'CREATE TABLE IF NOT EXISTS uqa_schema_create_enforcement.existing_table AS SELECT 1 AS id');
SELECT pg_temp.schema_create_enforcement_probe('ctas-existing-source-precedence', 'uqa_schema_create_enforcement_worker', 'CREATE TABLE uqa_schema_create_enforcement.existing_table AS SELECT * FROM uqa_schema_create_enforcement.missing_source');
SELECT pg_temp.schema_create_enforcement_probe('ctas-existing-ifne-source-precedence', 'uqa_schema_create_enforcement_worker', 'CREATE TABLE IF NOT EXISTS uqa_schema_create_enforcement.existing_table AS SELECT * FROM uqa_schema_create_enforcement.missing_source');
SELECT pg_temp.schema_create_enforcement_probe('view-replace-existing', 'uqa_schema_create_enforcement_worker', 'CREATE OR REPLACE VIEW uqa_schema_create_enforcement.existing_view AS SELECT 2 AS id');
SELECT pg_temp.schema_create_enforcement_probe('matview-existing-ifne', 'uqa_schema_create_enforcement_worker', 'CREATE MATERIALIZED VIEW IF NOT EXISTS uqa_schema_create_enforcement.existing_table AS SELECT 1 AS id');
SELECT pg_temp.schema_create_enforcement_probe('matview-existing-ifne-source-precedence', 'uqa_schema_create_enforcement_worker', 'CREATE MATERIALIZED VIEW IF NOT EXISTS uqa_schema_create_enforcement.existing_table AS SELECT * FROM uqa_schema_create_enforcement.missing_source');
SELECT pg_temp.schema_create_enforcement_probe('function-replace-existing', 'uqa_schema_create_enforcement_worker', 'CREATE OR REPLACE FUNCTION uqa_schema_create_enforcement.existing_function() RETURNS integer LANGUAGE sql AS ''SELECT 2''');
SELECT pg_temp.schema_create_enforcement_probe('missing-schema', 'uqa_schema_create_enforcement_worker', 'CREATE TABLE uqa_schema_create_enforcement_missing.denied_table(id integer)');
SELECT pg_temp.schema_create_enforcement_probe('system-schema', 'uqa_schema_create_enforcement_worker', 'CREATE TABLE pg_catalog.denied_table(id integer)');

REVOKE USAGE ON SCHEMA uqa_schema_create_enforcement FROM uqa_schema_create_enforcement_worker;
GRANT CREATE ON SCHEMA uqa_schema_create_enforcement TO uqa_schema_create_enforcement_worker;
SELECT pg_temp.schema_create_enforcement_probe('qualified-table-create-only', 'uqa_schema_create_enforcement_worker', 'CREATE TABLE uqa_schema_create_enforcement.granted_table(id integer)');
SELECT pg_temp.schema_create_enforcement_probe('qualified-view-create-only', 'uqa_schema_create_enforcement_worker', 'CREATE VIEW uqa_schema_create_enforcement.granted_view AS SELECT 1 AS id');
SELECT pg_temp.schema_create_enforcement_probe('qualified-function-create-only', 'uqa_schema_create_enforcement_worker', 'CREATE FUNCTION uqa_schema_create_enforcement.granted_function() RETURNS integer LANGUAGE sql AS ''SELECT 1''');
SELECT pg_temp.schema_create_enforcement_probe('unqualified-create-only', 'uqa_schema_create_enforcement_worker', 'SET search_path = uqa_schema_create_enforcement; CREATE TABLE unqualified_without_usage(id integer)');
SELECT pg_temp.schema_create_enforcement_probe('index-create-only', 'uqa_schema_create_enforcement_worker', 'CREATE INDEX index_without_usage ON uqa_schema_create_enforcement.index_target(id)');
GRANT USAGE ON SCHEMA uqa_schema_create_enforcement TO uqa_schema_create_enforcement_worker;
SELECT pg_temp.schema_create_enforcement_probe('unqualified-create-and-usage', 'uqa_schema_create_enforcement_worker', 'SET search_path = uqa_schema_create_enforcement; CREATE TABLE unqualified_with_usage(id integer)');
SELECT pg_temp.schema_create_enforcement_probe('index-create-and-usage', 'uqa_schema_create_enforcement_worker', 'CREATE INDEX index_target_id_idx ON uqa_schema_create_enforcement.index_target(id)');
SELECT pg_temp.schema_create_enforcement_probe('alter-key-create-and-usage', 'uqa_schema_create_enforcement_worker', 'ALTER TABLE uqa_schema_create_enforcement.alter_target ADD UNIQUE (id)');

GRANT USAGE, CREATE ON SCHEMA uqa_schema_create_enforcement TO uqa_schema_create_enforcement_group;
GRANT uqa_schema_create_enforcement_group TO uqa_schema_create_enforcement_leaf;
SELECT pg_temp.schema_create_enforcement_probe('inherited-create', 'uqa_schema_create_enforcement_leaf', 'CREATE TABLE uqa_schema_create_enforcement.inherited_table(id integer)');
REVOKE CREATE ON SCHEMA uqa_schema_create_enforcement FROM uqa_schema_create_enforcement_group;
SELECT pg_temp.schema_create_enforcement_probe('inherited-create-revoked', 'uqa_schema_create_enforcement_leaf', 'CREATE TABLE uqa_schema_create_enforcement.revoked_table(id integer)');

REVOKE CREATE ON SCHEMA uqa_schema_create_enforcement FROM uqa_schema_create_enforcement_worker;
SELECT pg_temp.schema_create_enforcement_probe('inferred-temporary-view', 'uqa_schema_create_enforcement_worker', 'CREATE TEMP TABLE schema_create_temp_source(id integer); CREATE VIEW schema_create_temp_view AS SELECT * FROM schema_create_temp_source');
SELECT pg_temp.schema_create_enforcement_probe('qualified-inferred-temporary-view', 'uqa_schema_create_enforcement_worker', 'CREATE VIEW uqa_schema_create_enforcement.qualified_temp_view AS SELECT * FROM schema_create_temp_source');
SELECT pg_temp.schema_create_enforcement_probe('drop-inferred-temporary-view', 'uqa_schema_create_enforcement_worker', 'DROP VIEW schema_create_temp_view; DROP TABLE schema_create_temp_source');

BEGIN;
GRANT CREATE ON SCHEMA uqa_schema_create_enforcement TO uqa_schema_create_enforcement_worker;
SELECT pg_temp.schema_create_enforcement_probe('transactional-grant-visible', 'uqa_schema_create_enforcement_worker', 'CREATE TABLE uqa_schema_create_enforcement.transaction_table(id integer)');
ROLLBACK;
SELECT pg_temp.schema_create_enforcement_probe('transactional-grant-rolled-back', 'uqa_schema_create_enforcement_worker', 'CREATE TABLE uqa_schema_create_enforcement.transaction_after_rollback(id integer)');
GRANT CREATE ON SCHEMA uqa_schema_create_enforcement TO uqa_schema_create_enforcement_worker;
BEGIN;
SAVEPOINT before_revoke;
REVOKE CREATE ON SCHEMA uqa_schema_create_enforcement FROM uqa_schema_create_enforcement_worker;
SELECT pg_temp.schema_create_enforcement_probe('savepoint-revoke-visible', 'uqa_schema_create_enforcement_worker', 'CREATE TABLE uqa_schema_create_enforcement.savepoint_denied(id integer)');
ROLLBACK TO SAVEPOINT before_revoke;
SELECT pg_temp.schema_create_enforcement_probe('savepoint-revoke-restored', 'uqa_schema_create_enforcement_worker', 'CREATE TABLE uqa_schema_create_enforcement.savepoint_restored(id integer)');
ROLLBACK;

DROP SCHEMA uqa_schema_create_enforcement CASCADE;
DROP SERVER uqa_schema_create_enforcement_server CASCADE;
DROP OWNED BY uqa_schema_create_enforcement_leaf;
DROP OWNED BY uqa_schema_create_enforcement_group;
DROP OWNED BY uqa_schema_create_enforcement_worker;
DROP OWNED BY uqa_schema_create_enforcement_owner;
DROP ROLE uqa_schema_create_enforcement_leaf;
DROP ROLE uqa_schema_create_enforcement_group;
DROP ROLE uqa_schema_create_enforcement_worker;
DROP ROLE uqa_schema_create_enforcement_owner;
