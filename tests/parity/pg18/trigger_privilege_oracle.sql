\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned

DROP SCHEMA IF EXISTS uqa_trigger_privilege_oracle CASCADE;
DROP ROLE IF EXISTS uqa_trigger_schema_owner;
DROP ROLE IF EXISTS uqa_trigger_table_owner;
DROP ROLE IF EXISTS uqa_trigger_table_member;
DROP ROLE IF EXISTS uqa_trigger_function_owner;
DROP ROLE IF EXISTS uqa_trigger_creator;
DROP ROLE IF EXISTS uqa_trigger_outsider;
DROP ROLE IF EXISTS uqa_trigger_next_owner;

CREATE ROLE uqa_trigger_schema_owner;
CREATE ROLE uqa_trigger_table_owner;
CREATE ROLE uqa_trigger_table_member INHERIT;
CREATE ROLE uqa_trigger_function_owner;
CREATE ROLE uqa_trigger_creator;
CREATE ROLE uqa_trigger_outsider;
CREATE ROLE uqa_trigger_next_owner;
GRANT uqa_trigger_table_owner TO uqa_trigger_table_member;
CREATE SCHEMA uqa_trigger_privilege_oracle AUTHORIZATION uqa_trigger_schema_owner;
GRANT USAGE, CREATE ON SCHEMA uqa_trigger_privilege_oracle TO uqa_trigger_table_owner, uqa_trigger_function_owner;
GRANT USAGE ON SCHEMA uqa_trigger_privilege_oracle TO uqa_trigger_table_member, uqa_trigger_creator, uqa_trigger_outsider, uqa_trigger_next_owner;

CREATE OR REPLACE FUNCTION pg_temp.trigger_privilege_probe(label text, role_name text, command text)
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

SET ROLE uqa_trigger_function_owner;
CREATE FUNCTION uqa_trigger_privilege_oracle.allowed_trigger()
RETURNS trigger
LANGUAGE plpgsql
AS $function$
BEGIN
    RETURN NEW;
END
$function$;
CREATE FUNCTION uqa_trigger_privilege_oracle.denied_trigger()
RETURNS trigger
LANGUAGE plpgsql
AS $function$
BEGIN
    RETURN NEW;
END
$function$;
CREATE FUNCTION uqa_trigger_privilege_oracle.not_a_trigger()
RETURNS integer
LANGUAGE sql
AS 'SELECT 1';
REVOKE ALL ON FUNCTION uqa_trigger_privilege_oracle.allowed_trigger() FROM PUBLIC;
REVOKE ALL ON FUNCTION uqa_trigger_privilege_oracle.denied_trigger() FROM PUBLIC;
REVOKE ALL ON FUNCTION uqa_trigger_privilege_oracle.not_a_trigger() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION uqa_trigger_privilege_oracle.allowed_trigger() TO uqa_trigger_table_owner, uqa_trigger_creator, uqa_trigger_schema_owner, uqa_trigger_outsider;
RESET ROLE;

SET ROLE uqa_trigger_table_owner;
CREATE TABLE uqa_trigger_privilege_oracle.items(id integer);
CREATE TABLE uqa_trigger_privilege_oracle.denied_items(id integer);
CREATE TABLE uqa_trigger_privilege_oracle.execute_items(id integer);
CREATE TABLE uqa_trigger_privilege_oracle.member_items(id integer);
CREATE TABLE uqa_trigger_privilege_oracle.creator_drop_items(id integer);
CREATE TABLE uqa_trigger_privilege_oracle.schema_drop_items(id integer);
CREATE TABLE uqa_trigger_privilege_oracle.member_drop_items(id integer);
CREATE TABLE uqa_trigger_privilege_oracle.owner_drop_items(id integer);
CREATE TABLE uqa_trigger_privilege_oracle.missing_drop_items(id integer);
CREATE TABLE uqa_trigger_privilege_oracle.alter_creator_items(id integer);
CREATE TABLE uqa_trigger_privilege_oracle.alter_member_items(id integer);
CREATE TABLE uqa_trigger_privilege_oracle.transfer_items(id integer);
CREATE TABLE uqa_trigger_privilege_oracle.runtime_items(id integer);
CREATE TABLE uqa_trigger_privilege_oracle.view_base(id integer);
CREATE VIEW uqa_trigger_privilege_oracle.item_view AS SELECT id FROM uqa_trigger_privilege_oracle.view_base;
GRANT TRIGGER ON TABLE uqa_trigger_privilege_oracle.items, uqa_trigger_privilege_oracle.execute_items, uqa_trigger_privilege_oracle.creator_drop_items, uqa_trigger_privilege_oracle.alter_creator_items, uqa_trigger_privilege_oracle.runtime_items, uqa_trigger_privilege_oracle.item_view TO uqa_trigger_creator;
GRANT INSERT, SELECT ON TABLE uqa_trigger_privilege_oracle.runtime_items TO uqa_trigger_creator;
CREATE TRIGGER creator_drop_trigger BEFORE INSERT ON uqa_trigger_privilege_oracle.creator_drop_items FOR EACH ROW EXECUTE FUNCTION uqa_trigger_privilege_oracle.allowed_trigger();
CREATE TRIGGER schema_drop_trigger BEFORE INSERT ON uqa_trigger_privilege_oracle.schema_drop_items FOR EACH ROW EXECUTE FUNCTION uqa_trigger_privilege_oracle.allowed_trigger();
CREATE TRIGGER member_drop_trigger BEFORE INSERT ON uqa_trigger_privilege_oracle.member_drop_items FOR EACH ROW EXECUTE FUNCTION uqa_trigger_privilege_oracle.allowed_trigger();
CREATE TRIGGER owner_drop_trigger BEFORE INSERT ON uqa_trigger_privilege_oracle.owner_drop_items FOR EACH ROW EXECUTE FUNCTION uqa_trigger_privilege_oracle.allowed_trigger();
CREATE TRIGGER alter_creator_trigger BEFORE INSERT ON uqa_trigger_privilege_oracle.alter_creator_items FOR EACH ROW EXECUTE FUNCTION uqa_trigger_privilege_oracle.allowed_trigger();
CREATE TRIGGER alter_member_trigger BEFORE INSERT ON uqa_trigger_privilege_oracle.alter_member_items FOR EACH ROW EXECUTE FUNCTION uqa_trigger_privilege_oracle.allowed_trigger();
CREATE TRIGGER transfer_trigger BEFORE INSERT ON uqa_trigger_privilege_oracle.transfer_items FOR EACH ROW EXECUTE FUNCTION uqa_trigger_privilege_oracle.allowed_trigger();
CREATE TRIGGER view_drop_trigger INSTEAD OF INSERT ON uqa_trigger_privilege_oracle.item_view FOR EACH ROW EXECUTE FUNCTION uqa_trigger_privilege_oracle.allowed_trigger();
RESET ROLE;

SELECT pg_temp.trigger_privilege_probe('table-denied', 'uqa_trigger_creator', 'CREATE TRIGGER denied_table_trigger BEFORE INSERT ON uqa_trigger_privilege_oracle.denied_items FOR EACH ROW EXECUTE FUNCTION uqa_trigger_privilege_oracle.allowed_trigger()');
SELECT pg_temp.trigger_privilege_probe('schema-owner-denied', 'uqa_trigger_schema_owner', 'CREATE TRIGGER schema_owner_trigger BEFORE INSERT ON uqa_trigger_privilege_oracle.denied_items FOR EACH ROW EXECUTE FUNCTION uqa_trigger_privilege_oracle.allowed_trigger()');
SELECT pg_temp.trigger_privilege_probe('member-create', 'uqa_trigger_table_member', 'CREATE TRIGGER member_create_trigger BEFORE INSERT ON uqa_trigger_privilege_oracle.member_items FOR EACH ROW EXECUTE FUNCTION uqa_trigger_privilege_oracle.allowed_trigger()');
SELECT pg_temp.trigger_privilege_probe('granted-create', 'uqa_trigger_creator', 'CREATE TRIGGER granted_trigger BEFORE INSERT ON uqa_trigger_privilege_oracle.items FOR EACH ROW EXECUTE FUNCTION uqa_trigger_privilege_oracle.allowed_trigger()');
SELECT pg_temp.trigger_privilege_probe('constraint-granted-create', 'uqa_trigger_creator', 'CREATE CONSTRAINT TRIGGER granted_constraint_trigger AFTER INSERT ON uqa_trigger_privilege_oracle.items DEFERRABLE INITIALLY IMMEDIATE FOR EACH ROW EXECUTE FUNCTION uqa_trigger_privilege_oracle.allowed_trigger()');
SELECT pg_temp.trigger_privilege_probe('view-granted-create', 'uqa_trigger_creator', 'CREATE TRIGGER view_granted_trigger BEFORE INSERT ON uqa_trigger_privilege_oracle.item_view FOR EACH STATEMENT EXECUTE FUNCTION uqa_trigger_privilege_oracle.allowed_trigger()');
SELECT pg_temp.trigger_privilege_probe('view-denied-create', 'uqa_trigger_outsider', 'CREATE TRIGGER view_denied_trigger BEFORE INSERT ON uqa_trigger_privilege_oracle.item_view FOR EACH STATEMENT EXECUTE FUNCTION uqa_trigger_privilege_oracle.allowed_trigger()');
SELECT pg_temp.trigger_privilege_probe('function-denied', 'uqa_trigger_creator', 'CREATE TRIGGER denied_function_trigger BEFORE INSERT ON uqa_trigger_privilege_oracle.execute_items FOR EACH ROW EXECUTE FUNCTION uqa_trigger_privilege_oracle.denied_trigger()');
SELECT pg_temp.trigger_privilege_probe('table-before-missing-function', 'uqa_trigger_outsider', 'CREATE TRIGGER missing_function_trigger BEFORE INSERT ON uqa_trigger_privilege_oracle.denied_items FOR EACH ROW EXECUTE FUNCTION uqa_trigger_privilege_oracle.missing_trigger()');
SELECT pg_temp.trigger_privilege_probe('wrong-return-before-table', 'uqa_trigger_creator', 'CREATE TRIGGER wrong_return_trigger BEFORE INSERT ON uqa_trigger_privilege_oracle.denied_items FOR EACH ROW EXECUTE FUNCTION uqa_trigger_privilege_oracle.not_a_trigger()');
SELECT pg_temp.trigger_privilege_probe('wrong-return-before-execute', 'uqa_trigger_creator', 'CREATE TRIGGER wrong_return_trigger BEFORE INSERT ON uqa_trigger_privilege_oracle.execute_items FOR EACH ROW EXECUTE FUNCTION uqa_trigger_privilege_oracle.not_a_trigger()');
SELECT pg_temp.trigger_privilege_probe('replace-granted', 'uqa_trigger_creator', 'CREATE OR REPLACE TRIGGER granted_trigger AFTER INSERT ON uqa_trigger_privilege_oracle.items FOR EACH ROW EXECUTE FUNCTION uqa_trigger_privilege_oracle.allowed_trigger()');
REVOKE EXECUTE ON FUNCTION uqa_trigger_privilege_oracle.allowed_trigger() FROM uqa_trigger_creator;
SELECT pg_temp.trigger_privilege_probe('replace-function-denied', 'uqa_trigger_creator', 'CREATE OR REPLACE TRIGGER granted_trigger BEFORE INSERT ON uqa_trigger_privilege_oracle.items FOR EACH ROW EXECUTE FUNCTION uqa_trigger_privilege_oracle.allowed_trigger()');
SELECT pg_temp.trigger_privilege_probe('duplicate-before-function-execute', 'uqa_trigger_creator', 'CREATE TRIGGER granted_trigger BEFORE INSERT ON uqa_trigger_privilege_oracle.items FOR EACH ROW EXECUTE FUNCTION uqa_trigger_privilege_oracle.allowed_trigger()');
GRANT EXECUTE ON FUNCTION uqa_trigger_privilege_oracle.allowed_trigger() TO uqa_trigger_creator;

SELECT pg_temp.trigger_privilege_probe('runtime-create', 'uqa_trigger_creator', 'CREATE TRIGGER runtime_trigger BEFORE INSERT ON uqa_trigger_privilege_oracle.runtime_items FOR EACH ROW EXECUTE FUNCTION uqa_trigger_privilege_oracle.allowed_trigger()');
REVOKE EXECUTE ON FUNCTION uqa_trigger_privilege_oracle.allowed_trigger() FROM uqa_trigger_creator;
SELECT pg_temp.trigger_privilege_probe('runtime-after-revoke', 'uqa_trigger_creator', 'INSERT INTO uqa_trigger_privilege_oracle.runtime_items VALUES (1)');
SELECT 'runtime-state|' || string_agg(id::text, ',' ORDER BY id) FROM uqa_trigger_privilege_oracle.runtime_items;

SELECT pg_temp.trigger_privilege_probe('creator-drop-denied', 'uqa_trigger_creator', 'DROP TRIGGER creator_drop_trigger ON uqa_trigger_privilege_oracle.creator_drop_items');
SELECT pg_temp.trigger_privilege_probe('schema-owner-drop-denied', 'uqa_trigger_schema_owner', 'DROP TRIGGER schema_drop_trigger ON uqa_trigger_privilege_oracle.schema_drop_items');
SELECT pg_temp.trigger_privilege_probe('outsider-missing', 'uqa_trigger_outsider', 'DROP TRIGGER missing_trigger ON uqa_trigger_privilege_oracle.missing_drop_items');
SELECT pg_temp.trigger_privilege_probe('outsider-if-exists-denied', 'uqa_trigger_outsider', 'DROP TRIGGER IF EXISTS missing_trigger ON uqa_trigger_privilege_oracle.missing_drop_items');
SELECT pg_temp.trigger_privilege_probe('member-drop', 'uqa_trigger_table_member', 'DROP TRIGGER member_drop_trigger ON uqa_trigger_privilege_oracle.member_drop_items');
SELECT pg_temp.trigger_privilege_probe('owner-drop', 'uqa_trigger_table_owner', 'DROP TRIGGER owner_drop_trigger ON uqa_trigger_privilege_oracle.owner_drop_items');
SELECT pg_temp.trigger_privilege_probe('view-drop-denied', 'uqa_trigger_creator', 'DROP TRIGGER view_drop_trigger ON uqa_trigger_privilege_oracle.item_view');
SELECT pg_temp.trigger_privilege_probe('view-owner-drop', 'uqa_trigger_table_owner', 'DROP TRIGGER view_drop_trigger ON uqa_trigger_privilege_oracle.item_view');

SELECT pg_temp.trigger_privilege_probe('creator-alter-denied', 'uqa_trigger_creator', 'ALTER TRIGGER alter_creator_trigger ON uqa_trigger_privilege_oracle.alter_creator_items RENAME TO creator_renamed_trigger');
SELECT pg_temp.trigger_privilege_probe('member-alter', 'uqa_trigger_table_member', 'ALTER TRIGGER alter_member_trigger ON uqa_trigger_privilege_oracle.alter_member_items RENAME TO member_renamed_trigger');
SELECT pg_temp.trigger_privilege_probe('view-creator-alter-denied', 'uqa_trigger_creator', 'ALTER TRIGGER view_granted_trigger ON uqa_trigger_privilege_oracle.item_view RENAME TO view_creator_renamed_trigger');
SELECT pg_temp.trigger_privilege_probe('view-owner-alter', 'uqa_trigger_table_owner', 'ALTER TRIGGER view_granted_trigger ON uqa_trigger_privilege_oracle.item_view RENAME TO view_owner_renamed_trigger');

ALTER TABLE uqa_trigger_privilege_oracle.transfer_items OWNER TO uqa_trigger_next_owner;
SELECT pg_temp.trigger_privilege_probe('former-owner-drop-denied', 'uqa_trigger_table_owner', 'DROP TRIGGER transfer_trigger ON uqa_trigger_privilege_oracle.transfer_items');
SELECT pg_temp.trigger_privilege_probe('new-owner-drop', 'uqa_trigger_next_owner', 'DROP TRIGGER transfer_trigger ON uqa_trigger_privilege_oracle.transfer_items');

DROP SCHEMA uqa_trigger_privilege_oracle CASCADE;
DROP ROLE uqa_trigger_schema_owner;
DROP ROLE uqa_trigger_table_member;
DROP ROLE uqa_trigger_table_owner;
DROP ROLE uqa_trigger_function_owner;
DROP ROLE uqa_trigger_creator;
DROP ROLE uqa_trigger_outsider;
DROP ROLE uqa_trigger_next_owner;
