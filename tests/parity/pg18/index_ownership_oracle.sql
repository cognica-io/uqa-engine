\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned

DROP SCHEMA IF EXISTS uqa_index_ownership_oracle CASCADE;
DROP ROLE IF EXISTS uqa_index_owner_schema;
DROP ROLE IF EXISTS uqa_index_owner_table;
DROP ROLE IF EXISTS uqa_index_owner_member;
DROP ROLE IF EXISTS uqa_index_owner_creator;
DROP ROLE IF EXISTS uqa_index_owner_outsider;
DROP ROLE IF EXISTS uqa_index_owner_next;

CREATE ROLE uqa_index_owner_schema;
CREATE ROLE uqa_index_owner_table;
CREATE ROLE uqa_index_owner_member INHERIT;
CREATE ROLE uqa_index_owner_creator;
CREATE ROLE uqa_index_owner_outsider;
CREATE ROLE uqa_index_owner_next;
GRANT uqa_index_owner_table TO uqa_index_owner_member;
CREATE SCHEMA uqa_index_ownership_oracle AUTHORIZATION uqa_index_owner_schema;
GRANT USAGE, CREATE ON SCHEMA uqa_index_ownership_oracle TO uqa_index_owner_table, uqa_index_owner_creator, uqa_index_owner_next;
GRANT USAGE ON SCHEMA uqa_index_ownership_oracle TO uqa_index_owner_member, uqa_index_owner_outsider;

CREATE OR REPLACE FUNCTION pg_temp.index_owner_probe(label text, role_name text, command text)
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

SET ROLE uqa_index_owner_table;
CREATE TABLE uqa_index_ownership_oracle.items(id integer, value integer);
CREATE TABLE uqa_index_ownership_oracle.transfer_items(id integer);
CREATE TABLE uqa_index_ownership_oracle.rollback_items(id integer);
CREATE INDEX existing_idx ON uqa_index_ownership_oracle.items(id);
CREATE INDEX owner_drop_idx ON uqa_index_ownership_oracle.items(id);
CREATE INDEX member_drop_idx ON uqa_index_ownership_oracle.items(id);
CREATE INDEX schema_drop_idx ON uqa_index_ownership_oracle.items(id);
CREATE INDEX outsider_drop_idx ON uqa_index_ownership_oracle.items(id);
CREATE INDEX multi_owner_idx ON uqa_index_ownership_oracle.items(id);
CREATE INDEX transfer_idx ON uqa_index_ownership_oracle.transfer_items(id);
CREATE INDEX rollback_idx ON uqa_index_ownership_oracle.rollback_items(id);
RESET ROLE;

SET ROLE uqa_index_owner_creator;
CREATE TABLE uqa_index_ownership_oracle.creator_items(id integer);
CREATE INDEX multi_creator_idx ON uqa_index_ownership_oracle.creator_items(id);
RESET ROLE;

SELECT 'initial-owner|' || pg_get_userbyid(c.relowner)
FROM pg_class AS c
JOIN pg_namespace AS n ON n.oid = c.relnamespace
WHERE n.nspname = 'uqa_index_ownership_oracle' AND c.relname = 'existing_idx';
SELECT pg_temp.index_owner_probe('creator-create', 'uqa_index_owner_creator', 'CREATE INDEX creator_denied_idx ON uqa_index_ownership_oracle.items(value)');
SELECT pg_temp.index_owner_probe('schema-owner-create', 'uqa_index_owner_schema', 'CREATE INDEX schema_denied_idx ON uqa_index_ownership_oracle.items(value)');
SELECT pg_temp.index_owner_probe('member-create', 'uqa_index_owner_member', 'CREATE INDEX member_created_idx ON uqa_index_ownership_oracle.items(value)');
SELECT 'member-created-owner|' || pg_get_userbyid(c.relowner)
FROM pg_class AS c
JOIN pg_namespace AS n ON n.oid = c.relnamespace
WHERE n.nspname = 'uqa_index_ownership_oracle' AND c.relname = 'member_created_idx';

REVOKE CREATE ON SCHEMA uqa_index_ownership_oracle FROM uqa_index_owner_table;
SELECT pg_temp.index_owner_probe('owner-without-create', 'uqa_index_owner_table', 'CREATE INDEX owner_no_create_idx ON uqa_index_ownership_oracle.items(value)');
GRANT CREATE ON SCHEMA uqa_index_ownership_oracle TO uqa_index_owner_table;
REVOKE USAGE ON SCHEMA uqa_index_ownership_oracle FROM uqa_index_owner_table;
SELECT pg_temp.index_owner_probe('owner-without-usage', 'uqa_index_owner_table', 'CREATE INDEX owner_no_usage_idx ON uqa_index_ownership_oracle.items(value)');
GRANT USAGE ON SCHEMA uqa_index_ownership_oracle TO uqa_index_owner_table;

REVOKE CREATE ON SCHEMA uqa_index_ownership_oracle FROM uqa_index_owner_creator;
SELECT pg_temp.index_owner_probe('nonowner-before-create-and-definition', 'uqa_index_owner_creator', 'CREATE INDEX creator_method_idx ON uqa_index_ownership_oracle.items USING missing_method(value)');
SELECT pg_temp.index_owner_probe('missing-table-before-create', 'uqa_index_owner_creator', 'CREATE INDEX creator_missing_idx ON uqa_index_ownership_oracle.missing(id)');
SELECT pg_temp.index_owner_probe('if-not-exists-still-checks-owner', 'uqa_index_owner_creator', 'CREATE INDEX IF NOT EXISTS existing_idx ON uqa_index_ownership_oracle.items(id)');

SELECT pg_temp.index_owner_probe('outsider-drop', 'uqa_index_owner_outsider', 'DROP INDEX uqa_index_ownership_oracle.outsider_drop_idx');
SELECT pg_temp.index_owner_probe('creator-drop', 'uqa_index_owner_creator', 'DROP INDEX uqa_index_ownership_oracle.owner_drop_idx');
SELECT pg_temp.index_owner_probe('member-drop', 'uqa_index_owner_member', 'DROP INDEX uqa_index_ownership_oracle.member_drop_idx');
SELECT pg_temp.index_owner_probe('schema-owner-drop', 'uqa_index_owner_schema', 'DROP INDEX uqa_index_ownership_oracle.schema_drop_idx');
SELECT pg_temp.index_owner_probe('owner-drop', 'uqa_index_owner_table', 'DROP INDEX uqa_index_ownership_oracle.owner_drop_idx');
SELECT pg_temp.index_owner_probe('multi-target-atomic', 'uqa_index_owner_table', 'DROP INDEX uqa_index_ownership_oracle.multi_owner_idx, uqa_index_ownership_oracle.multi_creator_idx');
SELECT 'after-multi-target|' || string_agg(c.relname, ',' ORDER BY c.relname)
FROM pg_class AS c
JOIN pg_namespace AS n ON n.oid = c.relnamespace
WHERE n.nspname = 'uqa_index_ownership_oracle' AND c.relname IN ('multi_owner_idx', 'multi_creator_idx');
SELECT pg_temp.index_owner_probe('if-exists-existing-checks-owner', 'uqa_index_owner_table', 'DROP INDEX IF EXISTS uqa_index_ownership_oracle.multi_creator_idx');
SELECT pg_temp.index_owner_probe('schema-owner-multi-drop', 'uqa_index_owner_schema', 'DROP INDEX uqa_index_ownership_oracle.multi_owner_idx, uqa_index_ownership_oracle.multi_creator_idx');
SELECT 'after-schema-owner-multi|' || count(*)
FROM pg_class AS c
JOIN pg_namespace AS n ON n.oid = c.relnamespace
WHERE n.nspname = 'uqa_index_ownership_oracle' AND c.relname IN ('multi_owner_idx', 'multi_creator_idx');

GRANT uqa_index_owner_next TO uqa_index_owner_table WITH INHERIT FALSE, SET TRUE;
SET ROLE uqa_index_owner_table;
BEGIN;
ALTER TABLE uqa_index_ownership_oracle.rollback_items OWNER TO uqa_index_owner_next;
SELECT 'transaction-owner|' || pg_get_userbyid(c.relowner)
FROM pg_class AS c
JOIN pg_namespace AS n ON n.oid = c.relnamespace
WHERE n.nspname = 'uqa_index_ownership_oracle' AND c.relname = 'rollback_idx';
ROLLBACK;
SELECT 'rollback-owner|' || pg_get_userbyid(c.relowner)
FROM pg_class AS c
JOIN pg_namespace AS n ON n.oid = c.relnamespace
WHERE n.nspname = 'uqa_index_ownership_oracle' AND c.relname = 'rollback_idx';
ALTER TABLE uqa_index_ownership_oracle.transfer_items OWNER TO uqa_index_owner_next;
RESET ROLE;
SELECT 'transferred-owner|' || pg_get_userbyid(c.relowner)
FROM pg_class AS c
JOIN pg_namespace AS n ON n.oid = c.relnamespace
WHERE n.nspname = 'uqa_index_ownership_oracle' AND c.relname = 'transfer_idx';
REVOKE uqa_index_owner_next FROM uqa_index_owner_table;
SELECT pg_temp.index_owner_probe('former-owner-drop', 'uqa_index_owner_table', 'DROP INDEX uqa_index_ownership_oracle.transfer_idx');
SELECT pg_temp.index_owner_probe('new-owner-drop', 'uqa_index_owner_next', 'DROP INDEX uqa_index_ownership_oracle.transfer_idx');

DROP SCHEMA uqa_index_ownership_oracle CASCADE;
DROP ROLE uqa_index_owner_schema;
DROP ROLE uqa_index_owner_member;
DROP ROLE uqa_index_owner_table;
DROP ROLE uqa_index_owner_creator;
DROP ROLE uqa_index_owner_outsider;
DROP ROLE uqa_index_owner_next;
