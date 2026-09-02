\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned

DROP SCHEMA IF EXISTS uqa_table_ownership_oracle CASCADE;
DROP ROLE IF EXISTS uqa_table_schema_owner;
DROP ROLE IF EXISTS uqa_table_owner_member;
DROP ROLE IF EXISTS uqa_table_owner_source;
DROP ROLE IF EXISTS uqa_table_owner_target;
DROP ROLE IF EXISTS uqa_table_owner_no_create;
DROP ROLE IF EXISTS uqa_table_owner_no_set;
DROP ROLE IF EXISTS uqa_table_owner_outsider;

CREATE ROLE uqa_table_owner_source;
CREATE ROLE uqa_table_schema_owner;
CREATE ROLE uqa_table_owner_member INHERIT;
CREATE ROLE uqa_table_owner_target;
CREATE ROLE uqa_table_owner_no_create;
CREATE ROLE uqa_table_owner_no_set;
CREATE ROLE uqa_table_owner_outsider;
GRANT uqa_table_owner_source TO uqa_table_owner_member;
GRANT uqa_table_owner_target, uqa_table_owner_no_create TO uqa_table_owner_source;
GRANT uqa_table_owner_no_set TO uqa_table_owner_source WITH SET FALSE;
CREATE SCHEMA uqa_table_ownership_oracle AUTHORIZATION uqa_table_schema_owner;
GRANT USAGE ON SCHEMA uqa_table_ownership_oracle TO uqa_table_owner_member, uqa_table_owner_no_create, uqa_table_owner_outsider;
GRANT USAGE, CREATE ON SCHEMA uqa_table_ownership_oracle TO uqa_table_owner_source, uqa_table_owner_target, uqa_table_owner_no_set;

CREATE OR REPLACE FUNCTION pg_temp.table_owner_probe(label text, role_name text, command text)
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

SET ROLE uqa_table_owner_source;
CREATE TABLE uqa_table_ownership_oracle.items(id integer, serial_id serial);
CREATE TABLE uqa_table_ownership_oracle.rollback_items(id integer);
CREATE TABLE uqa_table_ownership_oracle.schema_drop_items(id integer);
RESET ROLE;

SELECT 'initial-owner|' || pg_get_userbyid(c.relowner)
FROM pg_class AS c
JOIN pg_namespace AS n ON n.oid = c.relnamespace
WHERE n.nspname = 'uqa_table_ownership_oracle' AND c.relname = 'items';
SELECT 'initial-sequence-owner|' || pg_get_userbyid(c.relowner)
FROM pg_class AS c
JOIN pg_namespace AS n ON n.oid = c.relnamespace
WHERE n.nspname = 'uqa_table_ownership_oracle' AND c.relname = 'items_serial_id_seq';

SELECT pg_temp.table_owner_probe('outsider-alter', 'uqa_table_owner_outsider', 'ALTER TABLE uqa_table_ownership_oracle.items ADD COLUMN outsider_value integer');
SELECT pg_temp.table_owner_probe('outsider-drop', 'uqa_table_owner_outsider', 'DROP TABLE uqa_table_ownership_oracle.items');
SELECT pg_temp.table_owner_probe('schema-owner-drop', 'uqa_table_schema_owner', 'DROP TABLE uqa_table_ownership_oracle.schema_drop_items');
SELECT pg_temp.table_owner_probe('member-alter', 'uqa_table_owner_member', 'ALTER TABLE uqa_table_ownership_oracle.items ADD COLUMN member_value integer');
SELECT pg_temp.table_owner_probe('missing-owner', 'uqa_table_owner_source', 'ALTER TABLE uqa_table_ownership_oracle.items OWNER TO uqa_table_owner_missing');
SELECT pg_temp.table_owner_probe('owner-without-create', 'uqa_table_owner_source', 'ALTER TABLE uqa_table_ownership_oracle.items OWNER TO uqa_table_owner_no_create');
SELECT pg_temp.table_owner_probe('owner-without-set', 'uqa_table_owner_source', 'ALTER TABLE uqa_table_ownership_oracle.items OWNER TO uqa_table_owner_no_set');
SELECT pg_temp.table_owner_probe('owner-transfer', 'uqa_table_owner_source', 'ALTER TABLE uqa_table_ownership_oracle.items OWNER TO uqa_table_owner_target');

SELECT 'transferred-owner|' || pg_get_userbyid(c.relowner)
FROM pg_class AS c
JOIN pg_namespace AS n ON n.oid = c.relnamespace
WHERE n.nspname = 'uqa_table_ownership_oracle' AND c.relname = 'items';
SELECT 'transferred-sequence-owner|' || pg_get_userbyid(c.relowner)
FROM pg_class AS c
JOIN pg_namespace AS n ON n.oid = c.relnamespace
WHERE n.nspname = 'uqa_table_ownership_oracle' AND c.relname = 'items_serial_id_seq';

REVOKE uqa_table_owner_target FROM uqa_table_owner_source;
SELECT pg_temp.table_owner_probe('former-owner-alter', 'uqa_table_owner_source', 'ALTER TABLE uqa_table_ownership_oracle.items ADD COLUMN former_value integer');
SELECT pg_temp.table_owner_probe('new-owner-alter', 'uqa_table_owner_target', 'ALTER TABLE uqa_table_ownership_oracle.items ADD COLUMN target_value integer');
SELECT pg_temp.table_owner_probe('new-owner-no-set', 'uqa_table_owner_target', 'ALTER TABLE uqa_table_ownership_oracle.items OWNER TO uqa_table_owner_outsider');

BEGIN;
ALTER TABLE uqa_table_ownership_oracle.rollback_items OWNER TO uqa_table_owner_target;
SELECT 'transaction-owner|' || pg_get_userbyid(c.relowner)
FROM pg_class AS c
JOIN pg_namespace AS n ON n.oid = c.relnamespace
WHERE n.nspname = 'uqa_table_ownership_oracle' AND c.relname = 'rollback_items';
ROLLBACK;
SELECT 'rollback-owner|' || pg_get_userbyid(c.relowner)
FROM pg_class AS c
JOIN pg_namespace AS n ON n.oid = c.relnamespace
WHERE n.nspname = 'uqa_table_ownership_oracle' AND c.relname = 'rollback_items';

SELECT pg_temp.table_owner_probe('owner-drop-dependent', 'postgres', 'DROP ROLE uqa_table_owner_target');
SELECT pg_temp.table_owner_probe('superuser-transfer-no-create', 'postgres', 'ALTER TABLE uqa_table_ownership_oracle.items OWNER TO uqa_table_owner_no_create');
SELECT 'superuser-transferred-owner|' || pg_get_userbyid(c.relowner)
FROM pg_class AS c
JOIN pg_namespace AS n ON n.oid = c.relnamespace
WHERE n.nspname = 'uqa_table_ownership_oracle' AND c.relname = 'items';
ALTER TABLE uqa_table_ownership_oracle.items OWNER TO postgres;
REVOKE ALL ON SCHEMA uqa_table_ownership_oracle FROM uqa_table_owner_target;
SELECT pg_temp.table_owner_probe('owner-drop-released', 'postgres', 'DROP ROLE uqa_table_owner_target');

DROP SCHEMA uqa_table_ownership_oracle CASCADE;
DROP ROLE uqa_table_schema_owner;
DROP ROLE uqa_table_owner_member;
DROP ROLE uqa_table_owner_source;
DROP ROLE uqa_table_owner_no_create;
DROP ROLE uqa_table_owner_no_set;
DROP ROLE uqa_table_owner_outsider;
