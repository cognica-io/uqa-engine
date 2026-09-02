\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned

DROP SCHEMA IF EXISTS uqa_rule_privilege_oracle CASCADE;
DROP ROLE IF EXISTS uqa_rule_owner;
DROP ROLE IF EXISTS uqa_rule_member;
DROP ROLE IF EXISTS uqa_rule_caller;
DROP ROLE IF EXISTS uqa_rule_next_owner;
DROP ROLE IF EXISTS uqa_rule_resource_owner;

CREATE ROLE uqa_rule_owner;
CREATE ROLE uqa_rule_member INHERIT;
CREATE ROLE uqa_rule_caller;
CREATE ROLE uqa_rule_next_owner;
CREATE ROLE uqa_rule_resource_owner;
GRANT uqa_rule_owner TO uqa_rule_member;
CREATE SCHEMA uqa_rule_privilege_oracle AUTHORIZATION uqa_rule_owner;
GRANT USAGE ON SCHEMA uqa_rule_privilege_oracle TO uqa_rule_member, uqa_rule_caller, uqa_rule_next_owner;
GRANT USAGE, CREATE ON SCHEMA uqa_rule_privilege_oracle TO uqa_rule_resource_owner;

CREATE OR REPLACE FUNCTION pg_temp.rule_privilege_probe(label text, role_name text, command text)
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

SET ROLE uqa_rule_owner;
CREATE TABLE uqa_rule_privilege_oracle.items(id integer);
CREATE TABLE uqa_rule_privilege_oracle.missing_items(id integer);
CREATE TABLE uqa_rule_privilege_oracle.alter_items(id integer);
CREATE TABLE uqa_rule_privilege_oracle.member_items(id integer);
CREATE TABLE uqa_rule_privilege_oracle.transfer_items(id integer);
CREATE TABLE uqa_rule_privilege_oracle.runtime_items(id integer);
CREATE TABLE uqa_rule_privilege_oracle.view_action_items(id integer);
CREATE TABLE uqa_rule_privilege_oracle.rule_owner_source(payload integer);
INSERT INTO uqa_rule_privilege_oracle.rule_owner_source VALUES (50);
CREATE TABLE uqa_rule_privilege_oracle.view_base(id integer);
CREATE VIEW uqa_rule_privilege_oracle.item_view AS SELECT id FROM uqa_rule_privilege_oracle.view_base;
CREATE RULE existing_rule AS ON INSERT TO uqa_rule_privilege_oracle.items DO NOTHING;
CREATE RULE alter_rule AS ON INSERT TO uqa_rule_privilege_oracle.alter_items DO NOTHING;
CREATE RULE member_rule AS ON INSERT TO uqa_rule_privilege_oracle.member_items DO NOTHING;
CREATE RULE transfer_rule AS ON INSERT TO uqa_rule_privilege_oracle.transfer_items DO NOTHING;
CREATE RULE view_insert_rule AS ON INSERT TO uqa_rule_privilege_oracle.item_view DO INSTEAD INSERT INTO uqa_rule_privilege_oracle.view_base VALUES (NEW.id);
GRANT ALL ON TABLE uqa_rule_privilege_oracle.items, uqa_rule_privilege_oracle.missing_items, uqa_rule_privilege_oracle.alter_items, uqa_rule_privilege_oracle.runtime_items, uqa_rule_privilege_oracle.view_action_items, uqa_rule_privilege_oracle.item_view TO uqa_rule_caller;
RESET ROLE;

SET ROLE uqa_rule_resource_owner;
CREATE TABLE uqa_rule_privilege_oracle.runtime_log(actor text, seen integer);
CREATE TABLE uqa_rule_privilege_oracle.runtime_secret(payload integer);
CREATE TABLE uqa_rule_privilege_oracle.view_action_base(value integer);
CREATE VIEW uqa_rule_privilege_oracle.view_action AS SELECT value FROM uqa_rule_privilege_oracle.view_action_base;
INSERT INTO uqa_rule_privilege_oracle.runtime_secret VALUES (41);
RESET ROLE;

GRANT INSERT ON uqa_rule_privilege_oracle.view_action TO uqa_rule_owner;
GRANT INSERT ON uqa_rule_privilege_oracle.runtime_log TO uqa_rule_owner;
GRANT SELECT ON uqa_rule_privilege_oracle.runtime_secret TO uqa_rule_owner;
SET ROLE uqa_rule_owner;
CREATE RULE runtime_rule AS ON INSERT TO uqa_rule_privilege_oracle.runtime_items DO ALSO INSERT INTO uqa_rule_privilege_oracle.runtime_log(actor, seen) SELECT current_user, payload + NEW.id FROM uqa_rule_privilege_oracle.runtime_secret;
CREATE RULE view_action_rule AS ON INSERT TO uqa_rule_privilege_oracle.view_action_items DO ALSO INSERT INTO uqa_rule_privilege_oracle.view_action SELECT payload + NEW.id FROM uqa_rule_privilege_oracle.rule_owner_source;
RESET ROLE;

SELECT pg_temp.rule_privilege_probe('create-denied', 'uqa_rule_caller', 'CREATE RULE denied_rule AS ON INSERT TO uqa_rule_privilege_oracle.items DO NOTHING');
SELECT pg_temp.rule_privilege_probe('replace-denied', 'uqa_rule_caller', 'CREATE OR REPLACE RULE existing_rule AS ON INSERT TO uqa_rule_privilege_oracle.items DO NOTHING');
SELECT pg_temp.rule_privilege_probe('member-create', 'uqa_rule_member', 'CREATE RULE member_created AS ON INSERT TO uqa_rule_privilege_oracle.member_items DO NOTHING');
SELECT pg_temp.rule_privilege_probe('drop-denied', 'uqa_rule_caller', 'DROP RULE existing_rule ON uqa_rule_privilege_oracle.items');
SELECT pg_temp.rule_privilege_probe('drop-missing-denied', 'uqa_rule_caller', 'DROP RULE missing_rule ON uqa_rule_privilege_oracle.missing_items');
SELECT pg_temp.rule_privilege_probe('drop-missing-if-exists', 'uqa_rule_caller', 'DROP RULE IF EXISTS missing_rule ON uqa_rule_privilege_oracle.missing_items');
SELECT pg_temp.rule_privilege_probe('rename-denied', 'uqa_rule_caller', 'ALTER RULE alter_rule ON uqa_rule_privilege_oracle.alter_items RENAME TO renamed_rule');
SELECT pg_temp.rule_privilege_probe('rename-missing-denied', 'uqa_rule_caller', 'ALTER RULE missing_rule ON uqa_rule_privilege_oracle.alter_items RENAME TO renamed_rule');
SELECT pg_temp.rule_privilege_probe('disable-denied', 'uqa_rule_caller', 'ALTER TABLE uqa_rule_privilege_oracle.alter_items DISABLE RULE alter_rule');
SELECT pg_temp.rule_privilege_probe('view-create-denied', 'uqa_rule_caller', 'CREATE RULE denied_view_rule AS ON INSERT TO uqa_rule_privilege_oracle.item_view DO INSTEAD NOTHING');
SELECT pg_temp.rule_privilege_probe('view-rename-denied', 'uqa_rule_caller', 'ALTER RULE view_insert_rule ON uqa_rule_privilege_oracle.item_view RENAME TO renamed_view_rule');

SELECT pg_temp.rule_privilege_probe('runtime-owner-subject', 'uqa_rule_caller', 'INSERT INTO uqa_rule_privilege_oracle.runtime_items VALUES (1)');
SELECT 'runtime-row|' || actor || '|' || seen FROM uqa_rule_privilege_oracle.runtime_log ORDER BY seen;
SELECT pg_temp.rule_privilege_probe('runtime-view-target', 'uqa_rule_caller', 'INSERT INTO uqa_rule_privilege_oracle.view_action_items VALUES (1)');
SELECT 'view-target|' || value FROM uqa_rule_privilege_oracle.view_action_base;
REVOKE INSERT ON uqa_rule_privilege_oracle.runtime_log FROM uqa_rule_owner;
REVOKE SELECT ON uqa_rule_privilege_oracle.runtime_secret FROM uqa_rule_owner;
GRANT INSERT ON uqa_rule_privilege_oracle.runtime_log TO uqa_rule_caller;
GRANT SELECT ON uqa_rule_privilege_oracle.runtime_secret TO uqa_rule_caller;
SELECT pg_temp.rule_privilege_probe('runtime-caller-grants-ignored', 'uqa_rule_caller', 'INSERT INTO uqa_rule_privilege_oracle.runtime_items VALUES (2)');
SELECT 'runtime-count|' || count(*) FROM uqa_rule_privilege_oracle.runtime_log;
GRANT INSERT ON uqa_rule_privilege_oracle.runtime_log TO uqa_rule_owner;
SELECT pg_temp.rule_privilege_probe('runtime-owner-source-denied', 'uqa_rule_caller', 'INSERT INTO uqa_rule_privilege_oracle.runtime_items VALUES (3)');
GRANT SELECT ON uqa_rule_privilege_oracle.runtime_secret TO uqa_rule_owner;
SELECT pg_temp.rule_privilege_probe('runtime-owner-restored', 'uqa_rule_caller', 'INSERT INTO uqa_rule_privilege_oracle.runtime_items VALUES (4)');
SELECT 'runtime-rows|' || string_agg(actor || ':' || seen, ',' ORDER BY seen) FROM uqa_rule_privilege_oracle.runtime_log;

ALTER TABLE uqa_rule_privilege_oracle.transfer_items OWNER TO uqa_rule_next_owner;
SELECT pg_temp.rule_privilege_probe('former-owner-rename-denied', 'uqa_rule_owner', 'ALTER RULE transfer_rule ON uqa_rule_privilege_oracle.transfer_items RENAME TO former_rule');
SELECT pg_temp.rule_privilege_probe('new-owner-rename', 'uqa_rule_next_owner', 'ALTER RULE transfer_rule ON uqa_rule_privilege_oracle.transfer_items RENAME TO next_rule');

ALTER TABLE uqa_rule_privilege_oracle.runtime_items OWNER TO uqa_rule_next_owner;
SELECT pg_temp.rule_privilege_probe('runtime-new-owner-denied', 'uqa_rule_caller', 'INSERT INTO uqa_rule_privilege_oracle.runtime_items VALUES (5)');
GRANT INSERT ON uqa_rule_privilege_oracle.runtime_log TO uqa_rule_next_owner;
GRANT SELECT ON uqa_rule_privilege_oracle.runtime_secret TO uqa_rule_next_owner;
SELECT pg_temp.rule_privilege_probe('runtime-new-owner-granted', 'uqa_rule_caller', 'INSERT INTO uqa_rule_privilege_oracle.runtime_items VALUES (6)');
SELECT 'runtime-final|' || string_agg(actor || ':' || seen, ',' ORDER BY seen) FROM uqa_rule_privilege_oracle.runtime_log;

DROP SCHEMA uqa_rule_privilege_oracle CASCADE;
DROP ROLE uqa_rule_member;
DROP ROLE uqa_rule_owner;
DROP ROLE uqa_rule_caller;
DROP ROLE uqa_rule_next_owner;
DROP ROLE uqa_rule_resource_owner;
