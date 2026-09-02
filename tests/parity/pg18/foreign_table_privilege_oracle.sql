\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned

CREATE EXTENSION IF NOT EXISTS file_fdw;
DROP SCHEMA IF EXISTS uqa_foreign_acl_oracle CASCADE;
DROP SERVER IF EXISTS uqa_foreign_acl_server CASCADE;
DROP ROLE IF EXISTS uqa_foreign_acl_next_owner;
DROP ROLE IF EXISTS uqa_foreign_acl_owner;
DROP ROLE IF EXISTS uqa_foreign_acl_delegate;
DROP ROLE IF EXISTS uqa_foreign_acl_reader;
DROP ROLE IF EXISTS uqa_foreign_acl_column_reader;
DROP ROLE IF EXISTS uqa_foreign_acl_outsider;

CREATE ROLE uqa_foreign_acl_next_owner;
CREATE ROLE uqa_foreign_acl_owner;
CREATE ROLE uqa_foreign_acl_delegate;
CREATE ROLE uqa_foreign_acl_reader;
CREATE ROLE uqa_foreign_acl_column_reader;
CREATE ROLE uqa_foreign_acl_outsider;
GRANT uqa_foreign_acl_next_owner TO uqa_foreign_acl_owner WITH INHERIT FALSE, SET TRUE;
CREATE SCHEMA uqa_foreign_acl_oracle AUTHORIZATION uqa_foreign_acl_owner;
GRANT USAGE ON SCHEMA uqa_foreign_acl_oracle TO uqa_foreign_acl_next_owner, uqa_foreign_acl_delegate, uqa_foreign_acl_reader, uqa_foreign_acl_column_reader, uqa_foreign_acl_outsider;
GRANT CREATE ON SCHEMA uqa_foreign_acl_oracle TO uqa_foreign_acl_next_owner;
CREATE SERVER uqa_foreign_acl_server FOREIGN DATA WRAPPER file_fdw;
CREATE TEMP TABLE uqa_foreign_acl_seed(id integer, value text);
INSERT INTO uqa_foreign_acl_seed VALUES (1, 'one'), (2, 'two');
COPY uqa_foreign_acl_seed TO '/tmp/uqa_foreign_acl_oracle.csv' WITH (FORMAT csv);
CREATE FOREIGN TABLE uqa_foreign_acl_oracle.items(id integer, value text) SERVER uqa_foreign_acl_server OPTIONS (filename '/tmp/uqa_foreign_acl_oracle.csv', format 'csv');
CREATE FOREIGN TABLE uqa_foreign_acl_oracle.all_items(id integer, value text) SERVER uqa_foreign_acl_server OPTIONS (filename '/tmp/uqa_foreign_acl_oracle.csv', format 'csv');
CREATE FOREIGN TABLE uqa_foreign_acl_oracle.cascade_items(id integer, value text) SERVER uqa_foreign_acl_server OPTIONS (filename '/tmp/uqa_foreign_acl_oracle.csv', format 'csv');
ALTER FOREIGN TABLE uqa_foreign_acl_oracle.items OWNER TO uqa_foreign_acl_owner;
ALTER FOREIGN TABLE uqa_foreign_acl_oracle.all_items OWNER TO uqa_foreign_acl_owner;
ALTER FOREIGN TABLE uqa_foreign_acl_oracle.cascade_items OWNER TO uqa_foreign_acl_owner;

CREATE OR REPLACE FUNCTION pg_temp.foreign_acl_probe(label text, role_name text, command text)
RETURNS text
LANGUAGE plpgsql
AS $oracle$
DECLARE
    state text;
    message text;
BEGIN
    EXECUTE format('SET ROLE %I', role_name);
    EXECUTE command;
    RESET ROLE;
    RETURN label || '|ok';
EXCEPTION WHEN OTHERS THEN
    GET STACKED DIAGNOSTICS state = RETURNED_SQLSTATE, message = MESSAGE_TEXT;
    RESET ROLE;
    RETURN label || '|' || state || '|' || message;
END
$oracle$;

SELECT 'defaults|' || (SELECT relacl IS NULL FROM pg_catalog.pg_class WHERE oid = 'uqa_foreign_acl_oracle.items'::regclass) || '|' || (SELECT bool_and(attacl IS NULL) FROM pg_catalog.pg_attribute WHERE attrelid = 'uqa_foreign_acl_oracle.items'::regclass AND attnum > 0);

SET ROLE uqa_foreign_acl_owner;
GRANT SELECT ON TABLE uqa_foreign_acl_oracle.items TO uqa_foreign_acl_delegate WITH GRANT OPTION;
GRANT SELECT(id) ON TABLE uqa_foreign_acl_oracle.items TO uqa_foreign_acl_column_reader;
GRANT ALL PRIVILEGES ON TABLE uqa_foreign_acl_oracle.all_items TO uqa_foreign_acl_outsider;
GRANT INSERT(id), UPDATE(id), REFERENCES(id) ON TABLE uqa_foreign_acl_oracle.all_items TO uqa_foreign_acl_column_reader;
GRANT SELECT ON TABLE uqa_foreign_acl_oracle.cascade_items TO uqa_foreign_acl_delegate WITH GRANT OPTION;
RESET ROLE;
SET ROLE uqa_foreign_acl_delegate;
GRANT SELECT ON TABLE uqa_foreign_acl_oracle.items, uqa_foreign_acl_oracle.cascade_items TO uqa_foreign_acl_reader;
RESET ROLE;

SELECT 'relation-acl|' || relacl::text FROM pg_catalog.pg_class WHERE oid = 'uqa_foreign_acl_oracle.items'::regclass;
SELECT 'column-acl|' || attacl::text FROM pg_catalog.pg_attribute WHERE attrelid = 'uqa_foreign_acl_oracle.items'::regclass AND attname = 'id';
SELECT 'all-acl|' || relacl::text FROM pg_catalog.pg_class WHERE oid = 'uqa_foreign_acl_oracle.all_items'::regclass;
SELECT 'column-write-acl|' || attacl::text FROM pg_catalog.pg_attribute WHERE attrelid = 'uqa_foreign_acl_oracle.all_items'::regclass AND attname = 'id';
SELECT 'inquiry|' || has_table_privilege('uqa_foreign_acl_reader', 'uqa_foreign_acl_oracle.items', 'SELECT') || '|' || has_table_privilege('uqa_foreign_acl_reader'::regrole::oid, 'uqa_foreign_acl_oracle.items'::regclass::oid, 'SELECT') || '|' || has_column_privilege('uqa_foreign_acl_column_reader', 'uqa_foreign_acl_oracle.items', 'id', 'SELECT') || '|' || has_column_privilege('uqa_foreign_acl_column_reader'::regrole::oid, 'uqa_foreign_acl_oracle.items'::regclass::oid, 1::smallint, 'SELECT') || '|' || has_column_privilege('uqa_foreign_acl_column_reader', 'uqa_foreign_acl_oracle.items', 'ctid', 'SELECT');
SELECT pg_temp.foreign_acl_probe('select-denied', 'uqa_foreign_acl_outsider', 'SELECT value FROM uqa_foreign_acl_oracle.items');
SELECT pg_temp.foreign_acl_probe('select-granted', 'uqa_foreign_acl_reader', 'SELECT value FROM uqa_foreign_acl_oracle.items');
SELECT pg_temp.foreign_acl_probe('column-select-granted', 'uqa_foreign_acl_column_reader', 'SELECT id FROM uqa_foreign_acl_oracle.items');
SELECT pg_temp.foreign_acl_probe('column-select-denied', 'uqa_foreign_acl_column_reader', 'SELECT value FROM uqa_foreign_acl_oracle.items');
SELECT pg_temp.foreign_acl_probe('column-count-granted', 'uqa_foreign_acl_column_reader', 'SELECT count(*) FROM uqa_foreign_acl_oracle.items');

SET ROLE uqa_foreign_acl_column_reader;
SELECT 'information-schema|' || (SELECT count(*) FROM information_schema.tables WHERE table_schema = 'uqa_foreign_acl_oracle') || '|' || (SELECT string_agg(column_name, ',' ORDER BY ordinal_position) FROM information_schema.columns WHERE table_schema = 'uqa_foreign_acl_oracle' AND table_name = 'items') || '|' || (SELECT string_agg(column_name || ':' || privilege_type, ',' ORDER BY column_name, privilege_type) FROM information_schema.column_privileges WHERE table_schema = 'uqa_foreign_acl_oracle' AND table_name = 'items') || '|' || (SELECT table_type || ':' || is_insertable_into FROM information_schema.tables WHERE table_schema = 'uqa_foreign_acl_oracle' AND table_name = 'items') || '|' || (SELECT is_updatable FROM information_schema.columns WHERE table_schema = 'uqa_foreign_acl_oracle' AND table_name = 'items' AND column_name = 'id');
RESET ROLE;

SELECT pg_temp.foreign_acl_probe('dependent-restrict', 'uqa_foreign_acl_owner', 'REVOKE GRANT OPTION FOR SELECT ON TABLE uqa_foreign_acl_oracle.cascade_items FROM uqa_foreign_acl_delegate RESTRICT');
SET ROLE uqa_foreign_acl_owner;
REVOKE GRANT OPTION FOR SELECT ON TABLE uqa_foreign_acl_oracle.cascade_items FROM uqa_foreign_acl_delegate CASCADE;
RESET ROLE;
SELECT 'cascade|' || has_table_privilege('uqa_foreign_acl_delegate', 'uqa_foreign_acl_oracle.cascade_items', 'SELECT') || '|' || has_table_privilege('uqa_foreign_acl_delegate', 'uqa_foreign_acl_oracle.cascade_items', 'SELECT WITH GRANT OPTION') || '|' || has_table_privilege('uqa_foreign_acl_reader', 'uqa_foreign_acl_oracle.cascade_items', 'SELECT');

SET ROLE uqa_foreign_acl_owner;
BEGIN;
GRANT DELETE ON TABLE uqa_foreign_acl_oracle.items TO uqa_foreign_acl_reader;
SELECT 'transaction-grant|' || has_table_privilege('uqa_foreign_acl_reader', 'uqa_foreign_acl_oracle.items', 'DELETE');
ROLLBACK;
SELECT 'rollback-grant|' || has_table_privilege('uqa_foreign_acl_reader', 'uqa_foreign_acl_oracle.items', 'DELETE');
GRANT SELECT ON ALL TABLES IN SCHEMA uqa_foreign_acl_oracle TO uqa_foreign_acl_outsider;
RESET ROLE;
SELECT 'all-tables-schema|' || has_table_privilege('uqa_foreign_acl_outsider', 'uqa_foreign_acl_oracle.items', 'SELECT') || '|' || has_table_privilege('uqa_foreign_acl_outsider', 'uqa_foreign_acl_oracle.cascade_items', 'SELECT');

SET ROLE uqa_foreign_acl_owner;
ALTER FOREIGN TABLE uqa_foreign_acl_oracle.items OWNER TO uqa_foreign_acl_next_owner;
RESET ROLE;
SELECT 'owner-transfer|' || relowner::regrole || '|' || relacl::text FROM pg_catalog.pg_class WHERE oid = 'uqa_foreign_acl_oracle.items'::regclass;
SELECT 'owner-transfer-column|' || attacl::text FROM pg_catalog.pg_attribute WHERE attrelid = 'uqa_foreign_acl_oracle.items'::regclass AND attname = 'id';
SELECT pg_temp.foreign_acl_probe('grant-role-dependent', 'postgres', 'DROP ROLE uqa_foreign_acl_delegate');

DROP SCHEMA uqa_foreign_acl_oracle CASCADE;
DROP SERVER uqa_foreign_acl_server;
DROP ROLE uqa_foreign_acl_next_owner;
DROP ROLE uqa_foreign_acl_owner;
DROP ROLE uqa_foreign_acl_delegate;
DROP ROLE uqa_foreign_acl_reader;
DROP ROLE uqa_foreign_acl_column_reader;
DROP ROLE uqa_foreign_acl_outsider;
