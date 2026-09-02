\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned

DROP SCHEMA IF EXISTS uqa_sequence_schema_security_source CASCADE;
DROP SCHEMA IF EXISTS uqa_sequence_schema_security_target CASCADE;
DROP ROLE IF EXISTS uqa_sequence_schema_security_schema_owner;
DROP ROLE IF EXISTS uqa_sequence_schema_security_sequence_owner;
DROP ROLE IF EXISTS uqa_sequence_schema_security_new_owner;
DROP ROLE IF EXISTS uqa_sequence_schema_security_outsider;
DROP ROLE IF EXISTS uqa_sequence_schema_security_delegate;
DROP ROLE IF EXISTS uqa_sequence_schema_security_leaf;

CREATE ROLE uqa_sequence_schema_security_schema_owner;
CREATE ROLE uqa_sequence_schema_security_sequence_owner;
CREATE ROLE uqa_sequence_schema_security_new_owner;
CREATE ROLE uqa_sequence_schema_security_outsider;
CREATE ROLE uqa_sequence_schema_security_delegate;
CREATE ROLE uqa_sequence_schema_security_leaf;
CREATE SCHEMA uqa_sequence_schema_security_source AUTHORIZATION uqa_sequence_schema_security_schema_owner;
CREATE SCHEMA uqa_sequence_schema_security_target AUTHORIZATION uqa_sequence_schema_security_schema_owner;

CREATE OR REPLACE FUNCTION pg_temp.sequence_schema_probe(label text, command text)
RETURNS text
LANGUAGE plpgsql
AS $oracle$
DECLARE
    state text;
BEGIN
    EXECUTE command;
    RETURN label || '|ok';
EXCEPTION WHEN OTHERS THEN
    GET STACKED DIAGNOSTICS state = RETURNED_SQLSTATE;
    RETURN label || '|' || state;
END
$oracle$;

SELECT 'default|' || nspname || '|' || pg_get_userbyid(nspowner) || '|' || coalesce(nspacl::text, 'NULL') FROM pg_catalog.pg_namespace WHERE nspname IN ('uqa_sequence_schema_security_source', 'uqa_sequence_schema_security_target') ORDER BY nspname;
SELECT pg_temp.sequence_schema_probe('value-missing-schema', 'SELECT nextval(''uqa_sequence_schema_security_missing.ids'')');
SELECT pg_temp.sequence_schema_probe('scan-missing-schema', 'SELECT * FROM uqa_sequence_schema_security_missing.ids');
SELECT pg_temp.sequence_schema_probe('alter-missing-schema', 'ALTER SEQUENCE uqa_sequence_schema_security_missing.ids CACHE 2');
SELECT pg_temp.sequence_schema_probe('alter-missing-schema-if-exists', 'ALTER SEQUENCE IF EXISTS uqa_sequence_schema_security_missing.ids CACHE 2');
SELECT pg_temp.sequence_schema_probe('drop-missing-schema', 'DROP SEQUENCE uqa_sequence_schema_security_missing.ids');
SELECT pg_temp.sequence_schema_probe('drop-missing-schema-if-exists', 'DROP SEQUENCE IF EXISTS uqa_sequence_schema_security_missing.ids');
SELECT pg_temp.sequence_schema_probe('grant-missing-schema', 'GRANT USAGE ON SEQUENCE uqa_sequence_schema_security_missing.ids TO PUBLIC');
SELECT pg_temp.sequence_schema_probe('inquiry-missing-schema', 'SELECT has_sequence_privilege(''uqa_sequence_schema_security_missing.ids'', ''USAGE'')');
SELECT pg_temp.sequence_schema_probe('schema-grant-missing-target', 'GRANT SELECT ON SCHEMA uqa_sequence_schema_security_missing TO uqa_sequence_schema_security_missing_role');
SELECT pg_temp.sequence_schema_probe('schema-grant-missing-role', 'GRANT SELECT ON SCHEMA uqa_sequence_schema_security_source TO uqa_sequence_schema_security_missing_role');
SELECT pg_temp.sequence_schema_probe('schema-grant-invalid-privilege', 'GRANT SELECT ON SCHEMA uqa_sequence_schema_security_source TO uqa_sequence_schema_security_outsider');
SELECT pg_temp.sequence_schema_probe('schema-grant-public-option', 'GRANT USAGE ON SCHEMA uqa_sequence_schema_security_source TO PUBLIC WITH GRANT OPTION');
SELECT pg_temp.sequence_schema_probe('create-no-create', 'SET ROLE uqa_sequence_schema_security_sequence_owner; CREATE SEQUENCE uqa_sequence_schema_security_source.ids');
RESET ROLE;
SELECT pg_temp.sequence_schema_probe('create-invalid-before-privilege', 'SET ROLE uqa_sequence_schema_security_sequence_owner; CREATE SEQUENCE uqa_sequence_schema_security_source.invalid_ids INCREMENT 0');
RESET ROLE;
GRANT CREATE ON SCHEMA uqa_sequence_schema_security_source TO uqa_sequence_schema_security_sequence_owner;
SELECT pg_temp.sequence_schema_probe('create-with-create-only', 'SET ROLE uqa_sequence_schema_security_sequence_owner; CREATE SEQUENCE uqa_sequence_schema_security_source.ids');
RESET ROLE;
GRANT USAGE ON SCHEMA uqa_sequence_schema_security_source TO uqa_sequence_schema_security_sequence_owner;
SELECT pg_temp.sequence_schema_probe('create-owned-sequence', 'SET ROLE uqa_sequence_schema_security_sequence_owner; CREATE TABLE uqa_sequence_schema_security_source.owned_rows(id serial)');
RESET ROLE;
REVOKE CREATE ON SCHEMA uqa_sequence_schema_security_source FROM uqa_sequence_schema_security_sequence_owner;
SELECT pg_temp.sequence_schema_probe('create-existing-no-create', 'SET ROLE uqa_sequence_schema_security_sequence_owner; CREATE SEQUENCE IF NOT EXISTS uqa_sequence_schema_security_source.ids');
RESET ROLE;

GRANT USAGE, SELECT, UPDATE ON SEQUENCE uqa_sequence_schema_security_source.ids TO uqa_sequence_schema_security_outsider;
SELECT pg_temp.sequence_schema_probe('nextval-no-schema-usage', 'SET ROLE uqa_sequence_schema_security_outsider; SELECT nextval(''uqa_sequence_schema_security_source.ids'')');
RESET ROLE;
SELECT pg_temp.sequence_schema_probe('scan-no-schema-usage', 'SET ROLE uqa_sequence_schema_security_outsider; SELECT * FROM uqa_sequence_schema_security_source.ids');
RESET ROLE;
GRANT USAGE ON SCHEMA uqa_sequence_schema_security_source TO uqa_sequence_schema_security_outsider;
SELECT pg_temp.sequence_schema_probe('nextval-with-schema-usage', 'SET ROLE uqa_sequence_schema_security_outsider; SELECT nextval(''uqa_sequence_schema_security_source.ids'')');
RESET ROLE;
SELECT pg_temp.sequence_schema_probe('scan-with-schema-usage', 'SET ROLE uqa_sequence_schema_security_outsider; SELECT * FROM uqa_sequence_schema_security_source.ids');
RESET ROLE;
REVOKE USAGE ON SCHEMA uqa_sequence_schema_security_source FROM uqa_sequence_schema_security_outsider;
SELECT pg_temp.sequence_schema_probe('alter-missing-no-schema-usage', 'SET ROLE uqa_sequence_schema_security_outsider; ALTER SEQUENCE uqa_sequence_schema_security_source.missing_ids CACHE 2');
RESET ROLE;
GRANT USAGE ON SCHEMA uqa_sequence_schema_security_source TO uqa_sequence_schema_security_outsider;
SELECT pg_temp.sequence_schema_probe('alter-missing-with-schema-usage', 'SET ROLE uqa_sequence_schema_security_outsider; ALTER SEQUENCE uqa_sequence_schema_security_source.missing_ids CACHE 2');
RESET ROLE;

SET ROLE uqa_sequence_schema_security_schema_owner;
GRANT USAGE ON SCHEMA uqa_sequence_schema_security_source TO uqa_sequence_schema_security_delegate WITH GRANT OPTION;
RESET ROLE;
SET ROLE uqa_sequence_schema_security_delegate;
GRANT USAGE ON SCHEMA uqa_sequence_schema_security_source TO uqa_sequence_schema_security_leaf;
RESET ROLE;
GRANT USAGE ON SEQUENCE uqa_sequence_schema_security_source.ids TO uqa_sequence_schema_security_leaf;
SELECT pg_temp.sequence_schema_probe('dependent-schema-usage', 'SET ROLE uqa_sequence_schema_security_leaf; SELECT nextval(''uqa_sequence_schema_security_source.ids'')');
RESET ROLE;
SELECT pg_temp.sequence_schema_probe('schema-revoke-restrict', 'SET ROLE uqa_sequence_schema_security_schema_owner; REVOKE GRANT OPTION FOR USAGE ON SCHEMA uqa_sequence_schema_security_source FROM uqa_sequence_schema_security_delegate RESTRICT');
RESET ROLE;
SELECT pg_temp.sequence_schema_probe('schema-revoke-cascade', 'SET ROLE uqa_sequence_schema_security_schema_owner; REVOKE GRANT OPTION FOR USAGE ON SCHEMA uqa_sequence_schema_security_source FROM uqa_sequence_schema_security_delegate CASCADE');
RESET ROLE;
SELECT pg_temp.sequence_schema_probe('dependent-schema-usage-after-cascade', 'SET ROLE uqa_sequence_schema_security_leaf; SELECT nextval(''uqa_sequence_schema_security_source.ids'')');
RESET ROLE;

GRANT USAGE ON SCHEMA uqa_sequence_schema_security_source TO uqa_sequence_schema_security_sequence_owner;
GRANT uqa_sequence_schema_security_new_owner TO uqa_sequence_schema_security_sequence_owner WITH INHERIT FALSE, SET TRUE;
SELECT pg_temp.sequence_schema_probe('owner-same-no-create', 'SET ROLE uqa_sequence_schema_security_sequence_owner; ALTER SEQUENCE uqa_sequence_schema_security_source.ids OWNER TO uqa_sequence_schema_security_sequence_owner');
RESET ROLE;
SELECT pg_temp.sequence_schema_probe('owned-owner-transfer-precedence', 'SET ROLE uqa_sequence_schema_security_sequence_owner; ALTER SEQUENCE uqa_sequence_schema_security_source.owned_rows_id_seq OWNER TO uqa_sequence_schema_security_new_owner');
RESET ROLE;
SELECT pg_temp.sequence_schema_probe('set-schema-same-no-create', 'SET ROLE uqa_sequence_schema_security_sequence_owner; ALTER SEQUENCE uqa_sequence_schema_security_source.ids SET SCHEMA uqa_sequence_schema_security_source');
RESET ROLE;
SELECT pg_temp.sequence_schema_probe('owned-set-schema-precedence', 'SET ROLE uqa_sequence_schema_security_sequence_owner; ALTER SEQUENCE uqa_sequence_schema_security_source.owned_rows_id_seq SET SCHEMA uqa_sequence_schema_security_target');
RESET ROLE;
SELECT pg_temp.sequence_schema_probe('owned-set-schema-missing-precedence', 'SET ROLE uqa_sequence_schema_security_sequence_owner; ALTER SEQUENCE uqa_sequence_schema_security_source.owned_rows_id_seq SET SCHEMA uqa_sequence_schema_security_missing');
RESET ROLE;
SELECT pg_temp.sequence_schema_probe('owner-target-no-create', 'SET ROLE uqa_sequence_schema_security_sequence_owner; ALTER SEQUENCE uqa_sequence_schema_security_source.ids OWNER TO uqa_sequence_schema_security_new_owner');
RESET ROLE;
GRANT CREATE ON SCHEMA uqa_sequence_schema_security_source TO uqa_sequence_schema_security_new_owner;
SELECT pg_temp.sequence_schema_probe('owner-target-with-create', 'SET ROLE uqa_sequence_schema_security_sequence_owner; ALTER SEQUENCE uqa_sequence_schema_security_source.ids OWNER TO uqa_sequence_schema_security_new_owner');
RESET ROLE;
GRANT USAGE ON SCHEMA uqa_sequence_schema_security_source TO uqa_sequence_schema_security_new_owner;
SELECT pg_temp.sequence_schema_probe('set-schema-target-no-create', 'SET ROLE uqa_sequence_schema_security_new_owner; ALTER SEQUENCE uqa_sequence_schema_security_source.ids SET SCHEMA uqa_sequence_schema_security_target');
RESET ROLE;
GRANT CREATE ON SCHEMA uqa_sequence_schema_security_target TO uqa_sequence_schema_security_new_owner;
SELECT pg_temp.sequence_schema_probe('set-schema-target-with-create-only', 'SET ROLE uqa_sequence_schema_security_new_owner; ALTER SEQUENCE uqa_sequence_schema_security_source.ids SET SCHEMA uqa_sequence_schema_security_target');
RESET ROLE;

SELECT 'catalog|' || nspname || '|' || pg_get_userbyid(nspowner) || '|' || coalesce(nspacl::text, 'NULL') FROM pg_catalog.pg_namespace WHERE nspname IN ('uqa_sequence_schema_security_source', 'uqa_sequence_schema_security_target') ORDER BY nspname;

DROP SCHEMA uqa_sequence_schema_security_source CASCADE;
DROP SCHEMA uqa_sequence_schema_security_target CASCADE;
DROP ROLE uqa_sequence_schema_security_leaf;
DROP ROLE uqa_sequence_schema_security_delegate;
DROP ROLE uqa_sequence_schema_security_outsider;
DROP ROLE uqa_sequence_schema_security_sequence_owner;
DROP ROLE uqa_sequence_schema_security_new_owner;
DROP ROLE uqa_sequence_schema_security_schema_owner;
