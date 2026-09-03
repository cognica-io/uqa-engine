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
GRANT USAGE ON SCHEMA uqa_rule_privilege_oracle TO uqa_rule_member, uqa_rule_caller;
GRANT USAGE, CREATE ON SCHEMA uqa_rule_privilege_oracle TO uqa_rule_next_owner, uqa_rule_resource_owner;

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
CREATE TABLE uqa_rule_privilege_oracle.expression_log(kind text, value bigint);
CREATE TABLE uqa_rule_privilege_oracle.routine_owner_event(id bigint);
CREATE TABLE uqa_rule_privilege_oracle.routine_caller_event(id bigint);
CREATE TABLE uqa_rule_privilege_oracle.condition_owner_event(id bigint);
CREATE TABLE uqa_rule_privilege_oracle.condition_caller_event(id bigint);
CREATE TABLE uqa_rule_privilege_oracle.nextval_owner_event(id bigint);
CREATE TABLE uqa_rule_privilege_oracle.nextval_caller_event(id bigint);
CREATE TABLE uqa_rule_privilege_oracle.currval_owner_event(id bigint);
CREATE TABLE uqa_rule_privilege_oracle.currval_caller_event(id bigint);
CREATE TABLE uqa_rule_privilege_oracle.lastval_event(id bigint);
CREATE TABLE uqa_rule_privilege_oracle.setval_owner_event(id bigint);
CREATE TABLE uqa_rule_privilege_oracle.setval_caller_event(id bigint);
CREATE TABLE uqa_rule_privilege_oracle.sequence_owner_scan_event(id bigint);
CREATE TABLE uqa_rule_privilege_oracle.sequence_caller_scan_event(id bigint);
CREATE TABLE uqa_rule_privilege_oracle.default_owner_event(id bigint);
CREATE TABLE uqa_rule_privilege_oracle.default_caller_event(id bigint);
INSERT INTO uqa_rule_privilege_oracle.rule_owner_source VALUES (50);
CREATE TABLE uqa_rule_privilege_oracle.view_base(id integer);
CREATE VIEW uqa_rule_privilege_oracle.item_view AS SELECT id FROM uqa_rule_privilege_oracle.view_base;
CREATE RULE existing_rule AS ON INSERT TO uqa_rule_privilege_oracle.items DO NOTHING;
CREATE RULE alter_rule AS ON INSERT TO uqa_rule_privilege_oracle.alter_items DO NOTHING;
CREATE RULE member_rule AS ON INSERT TO uqa_rule_privilege_oracle.member_items DO NOTHING;
CREATE RULE transfer_rule AS ON INSERT TO uqa_rule_privilege_oracle.transfer_items DO NOTHING;
CREATE RULE view_insert_rule AS ON INSERT TO uqa_rule_privilege_oracle.item_view DO INSTEAD INSERT INTO uqa_rule_privilege_oracle.view_base VALUES (NEW.id);
GRANT ALL ON TABLE uqa_rule_privilege_oracle.items, uqa_rule_privilege_oracle.missing_items, uqa_rule_privilege_oracle.alter_items, uqa_rule_privilege_oracle.runtime_items, uqa_rule_privilege_oracle.view_action_items, uqa_rule_privilege_oracle.item_view TO uqa_rule_caller;
GRANT INSERT ON uqa_rule_privilege_oracle.routine_owner_event, uqa_rule_privilege_oracle.routine_caller_event, uqa_rule_privilege_oracle.condition_owner_event, uqa_rule_privilege_oracle.condition_caller_event, uqa_rule_privilege_oracle.nextval_owner_event, uqa_rule_privilege_oracle.nextval_caller_event, uqa_rule_privilege_oracle.currval_owner_event, uqa_rule_privilege_oracle.currval_caller_event, uqa_rule_privilege_oracle.lastval_event, uqa_rule_privilege_oracle.setval_owner_event, uqa_rule_privilege_oracle.setval_caller_event, uqa_rule_privilege_oracle.sequence_owner_scan_event, uqa_rule_privilege_oracle.sequence_caller_scan_event, uqa_rule_privilege_oracle.default_owner_event, uqa_rule_privilege_oracle.default_caller_event TO uqa_rule_caller;
GRANT SELECT ON uqa_rule_privilege_oracle.default_caller_event TO uqa_rule_caller;
RESET ROLE;

SET ROLE uqa_rule_resource_owner;
CREATE TABLE uqa_rule_privilege_oracle.runtime_log(actor text, seen integer);
CREATE TABLE uqa_rule_privilege_oracle.runtime_secret(payload integer);
CREATE TABLE uqa_rule_privilege_oracle.view_action_base(value integer);
CREATE VIEW uqa_rule_privilege_oracle.view_action AS SELECT value FROM uqa_rule_privilege_oracle.view_action_base;
CREATE FUNCTION uqa_rule_privilege_oracle.owner_only() RETURNS bigint LANGUAGE SQL AS 'SELECT 101::bigint';
CREATE FUNCTION uqa_rule_privilege_oracle.caller_only() RETURNS bigint LANGUAGE SQL AS 'SELECT 102::bigint';
REVOKE ALL ON FUNCTION uqa_rule_privilege_oracle.owner_only() FROM PUBLIC;
REVOKE ALL ON FUNCTION uqa_rule_privilege_oracle.caller_only() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION uqa_rule_privilege_oracle.owner_only() TO uqa_rule_owner;
GRANT EXECUTE ON FUNCTION uqa_rule_privilege_oracle.caller_only() TO uqa_rule_caller;
CREATE SEQUENCE uqa_rule_privilege_oracle.owner_sequence;
CREATE SEQUENCE uqa_rule_privilege_oracle.caller_sequence;
REVOKE ALL ON SEQUENCE uqa_rule_privilege_oracle.owner_sequence, uqa_rule_privilege_oracle.caller_sequence FROM PUBLIC;
GRANT USAGE, SELECT, UPDATE ON SEQUENCE uqa_rule_privilege_oracle.owner_sequence TO uqa_rule_owner;
GRANT USAGE, SELECT, UPDATE ON SEQUENCE uqa_rule_privilege_oracle.caller_sequence TO uqa_rule_caller;
CREATE TABLE uqa_rule_privilege_oracle.default_owner_target(id bigint DEFAULT nextval('uqa_rule_privilege_oracle.owner_sequence'));
CREATE TABLE uqa_rule_privilege_oracle.default_caller_target(id bigint DEFAULT nextval('uqa_rule_privilege_oracle.caller_sequence'));
CREATE TABLE uqa_rule_privilege_oracle.direct_default_target(id bigserial PRIMARY KEY, payload integer DEFAULT 9);
CREATE TABLE uqa_rule_privilege_oracle.direct_identity_target(id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, payload integer DEFAULT 11);
GRANT INSERT ON uqa_rule_privilege_oracle.default_owner_target, uqa_rule_privilege_oracle.default_caller_target TO uqa_rule_owner, uqa_rule_next_owner;
INSERT INTO uqa_rule_privilege_oracle.runtime_secret VALUES (41);
RESET ROLE;

GRANT INSERT ON uqa_rule_privilege_oracle.view_action TO uqa_rule_owner;
GRANT INSERT ON uqa_rule_privilege_oracle.runtime_log TO uqa_rule_owner;
GRANT SELECT ON uqa_rule_privilege_oracle.runtime_secret TO uqa_rule_owner;
SET ROLE uqa_rule_owner;
CREATE RULE runtime_rule AS ON INSERT TO uqa_rule_privilege_oracle.runtime_items DO ALSO INSERT INTO uqa_rule_privilege_oracle.runtime_log(actor, seen) SELECT current_user, payload + NEW.id FROM uqa_rule_privilege_oracle.runtime_secret;
CREATE RULE view_action_rule AS ON INSERT TO uqa_rule_privilege_oracle.view_action_items DO ALSO INSERT INTO uqa_rule_privilege_oracle.view_action SELECT payload + NEW.id FROM uqa_rule_privilege_oracle.rule_owner_source;
CREATE RULE routine_owner_rule AS ON INSERT TO uqa_rule_privilege_oracle.routine_owner_event DO ALSO INSERT INTO uqa_rule_privilege_oracle.expression_log VALUES ('routine-owner', uqa_rule_privilege_oracle.owner_only());
CREATE RULE routine_caller_rule AS ON INSERT TO uqa_rule_privilege_oracle.routine_caller_event DO ALSO INSERT INTO uqa_rule_privilege_oracle.expression_log VALUES ('routine-caller', uqa_rule_privilege_oracle.caller_only());
CREATE RULE condition_owner_rule AS ON INSERT TO uqa_rule_privilege_oracle.condition_owner_event WHERE uqa_rule_privilege_oracle.owner_only() = 101 DO ALSO INSERT INTO uqa_rule_privilege_oracle.expression_log VALUES ('condition-owner', NEW.id);
CREATE RULE condition_caller_rule AS ON INSERT TO uqa_rule_privilege_oracle.condition_caller_event WHERE uqa_rule_privilege_oracle.caller_only() = 102 DO ALSO INSERT INTO uqa_rule_privilege_oracle.expression_log VALUES ('condition-caller', NEW.id);
CREATE RULE nextval_owner_rule AS ON INSERT TO uqa_rule_privilege_oracle.nextval_owner_event DO ALSO INSERT INTO uqa_rule_privilege_oracle.expression_log VALUES ('nextval-owner', nextval('uqa_rule_privilege_oracle.owner_sequence'));
CREATE RULE nextval_caller_rule AS ON INSERT TO uqa_rule_privilege_oracle.nextval_caller_event DO ALSO INSERT INTO uqa_rule_privilege_oracle.expression_log VALUES ('nextval-caller', nextval('uqa_rule_privilege_oracle.caller_sequence'));
CREATE RULE currval_owner_rule AS ON INSERT TO uqa_rule_privilege_oracle.currval_owner_event DO ALSO INSERT INTO uqa_rule_privilege_oracle.expression_log VALUES ('currval-owner', currval('uqa_rule_privilege_oracle.owner_sequence'));
CREATE RULE currval_caller_rule AS ON INSERT TO uqa_rule_privilege_oracle.currval_caller_event DO ALSO INSERT INTO uqa_rule_privilege_oracle.expression_log VALUES ('currval-caller', currval('uqa_rule_privilege_oracle.caller_sequence'));
CREATE RULE lastval_rule AS ON INSERT TO uqa_rule_privilege_oracle.lastval_event DO ALSO INSERT INTO uqa_rule_privilege_oracle.expression_log VALUES ('lastval', lastval());
CREATE RULE setval_owner_rule AS ON INSERT TO uqa_rule_privilege_oracle.setval_owner_event DO ALSO INSERT INTO uqa_rule_privilege_oracle.expression_log VALUES ('setval-owner', setval('uqa_rule_privilege_oracle.owner_sequence', NEW.id));
CREATE RULE setval_caller_rule AS ON INSERT TO uqa_rule_privilege_oracle.setval_caller_event DO ALSO INSERT INTO uqa_rule_privilege_oracle.expression_log VALUES ('setval-caller', setval('uqa_rule_privilege_oracle.caller_sequence', NEW.id));
CREATE RULE sequence_owner_scan_rule AS ON INSERT TO uqa_rule_privilege_oracle.sequence_owner_scan_event DO ALSO INSERT INTO uqa_rule_privilege_oracle.expression_log SELECT 'sequence-owner-scan', last_value FROM uqa_rule_privilege_oracle.owner_sequence;
CREATE RULE sequence_caller_scan_rule AS ON INSERT TO uqa_rule_privilege_oracle.sequence_caller_scan_event DO ALSO INSERT INTO uqa_rule_privilege_oracle.expression_log SELECT 'sequence-caller-scan', last_value FROM uqa_rule_privilege_oracle.caller_sequence;
CREATE RULE default_owner_rule AS ON INSERT TO uqa_rule_privilege_oracle.default_owner_event DO ALSO INSERT INTO uqa_rule_privilege_oracle.default_owner_target DEFAULT VALUES;
CREATE RULE default_caller_rule AS ON INSERT TO uqa_rule_privilege_oracle.default_caller_event DO ALSO INSERT INTO uqa_rule_privilege_oracle.default_caller_target DEFAULT VALUES;
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

SELECT nextval('uqa_rule_privilege_oracle.owner_sequence') AS owner_sequence_seed \gset
SELECT pg_temp.rule_privilege_probe('routine-owner-only', 'uqa_rule_caller', 'INSERT INTO uqa_rule_privilege_oracle.routine_owner_event VALUES (1)');
SELECT pg_temp.rule_privilege_probe('routine-caller-only', 'uqa_rule_caller', 'INSERT INTO uqa_rule_privilege_oracle.routine_caller_event VALUES (1)');
SELECT pg_temp.rule_privilege_probe('condition-owner-only', 'uqa_rule_caller', 'INSERT INTO uqa_rule_privilege_oracle.condition_owner_event VALUES (1)');
SELECT pg_temp.rule_privilege_probe('condition-caller-only', 'uqa_rule_caller', 'INSERT INTO uqa_rule_privilege_oracle.condition_caller_event VALUES (7)');
SELECT pg_temp.rule_privilege_probe('nextval-owner-only', 'uqa_rule_caller', 'INSERT INTO uqa_rule_privilege_oracle.nextval_owner_event VALUES (1)');
SELECT pg_temp.rule_privilege_probe('currval-owner-only', 'uqa_rule_caller', 'INSERT INTO uqa_rule_privilege_oracle.currval_owner_event VALUES (1)');
SELECT pg_temp.rule_privilege_probe('lastval-owner-only', 'uqa_rule_caller', 'INSERT INTO uqa_rule_privilege_oracle.lastval_event VALUES (1)');
SELECT pg_temp.rule_privilege_probe('nextval-caller-only', 'uqa_rule_caller', 'INSERT INTO uqa_rule_privilege_oracle.nextval_caller_event VALUES (1)');
SELECT pg_temp.rule_privilege_probe('currval-caller-only', 'uqa_rule_caller', 'INSERT INTO uqa_rule_privilege_oracle.currval_caller_event VALUES (1)');
SELECT pg_temp.rule_privilege_probe('lastval-caller-only', 'uqa_rule_caller', 'INSERT INTO uqa_rule_privilege_oracle.lastval_event VALUES (1)');
SELECT pg_temp.rule_privilege_probe('setval-owner-only', 'uqa_rule_caller', 'INSERT INTO uqa_rule_privilege_oracle.setval_owner_event VALUES (20)');
SELECT pg_temp.rule_privilege_probe('setval-caller-only', 'uqa_rule_caller', 'INSERT INTO uqa_rule_privilege_oracle.setval_caller_event VALUES (20)');
SELECT pg_temp.rule_privilege_probe('sequence-owner-scan', 'uqa_rule_caller', 'INSERT INTO uqa_rule_privilege_oracle.sequence_owner_scan_event VALUES (1)');
SELECT pg_temp.rule_privilege_probe('sequence-caller-scan', 'uqa_rule_caller', 'INSERT INTO uqa_rule_privilege_oracle.sequence_caller_scan_event VALUES (1)');
SELECT pg_temp.rule_privilege_probe('default-owner-sequence', 'uqa_rule_caller', 'INSERT INTO uqa_rule_privilege_oracle.default_owner_event VALUES (1)');
SELECT pg_temp.rule_privilege_probe('default-caller-sequence', 'uqa_rule_caller', 'INSERT INTO uqa_rule_privilege_oracle.default_caller_event VALUES (1)');
SELECT 'expression-log|' || string_agg(kind || ':' || value, ',' ORDER BY kind) FROM uqa_rule_privilege_oracle.expression_log;
SELECT 'default-targets|' || (SELECT count(*) FROM uqa_rule_privilege_oracle.default_owner_target) || '|' || (SELECT string_agg(id::text, ',' ORDER BY id) FROM uqa_rule_privilege_oracle.default_caller_target);
SELECT pg_temp.rule_privilege_probe('default-multi-row', 'uqa_rule_caller', 'INSERT INTO uqa_rule_privilege_oracle.default_caller_event VALUES (2), (3)');
SELECT pg_temp.rule_privilege_probe('default-empty-source', 'uqa_rule_caller', 'INSERT INTO uqa_rule_privilege_oracle.default_caller_event SELECT id FROM uqa_rule_privilege_oracle.default_caller_event WHERE false');
SELECT 'default-cardinality|' || count(*) || '|' || string_agg(id::text, ',' ORDER BY id) FROM uqa_rule_privilege_oracle.default_caller_target;
SET ROLE uqa_rule_resource_owner;
INSERT INTO uqa_rule_privilege_oracle.direct_default_target DEFAULT VALUES RETURNING 'direct-default|' || id || '|' || payload;
INSERT INTO uqa_rule_privilege_oracle.direct_default_target DEFAULT VALUES RETURNING 'direct-default|' || id || '|' || payload;
INSERT INTO uqa_rule_privilege_oracle.direct_identity_target DEFAULT VALUES RETURNING 'direct-identity|' || id || '|' || payload;
RESET ROLE;

ALTER TABLE uqa_rule_privilege_oracle.routine_owner_event OWNER TO uqa_rule_next_owner;
ALTER TABLE uqa_rule_privilege_oracle.nextval_owner_event OWNER TO uqa_rule_next_owner;
GRANT INSERT ON uqa_rule_privilege_oracle.expression_log TO uqa_rule_next_owner;
GRANT EXECUTE ON FUNCTION uqa_rule_privilege_oracle.owner_only() TO uqa_rule_next_owner;
GRANT USAGE ON SEQUENCE uqa_rule_privilege_oracle.owner_sequence TO uqa_rule_next_owner;
SELECT pg_temp.rule_privilege_probe('transfer-routine-still-caller', 'uqa_rule_caller', 'INSERT INTO uqa_rule_privilege_oracle.routine_owner_event VALUES (2)');
SELECT pg_temp.rule_privilege_probe('transfer-nextval-still-caller', 'uqa_rule_caller', 'INSERT INTO uqa_rule_privilege_oracle.nextval_owner_event VALUES (2)');
GRANT EXECUTE ON FUNCTION uqa_rule_privilege_oracle.owner_only() TO uqa_rule_caller;
GRANT USAGE ON SEQUENCE uqa_rule_privilege_oracle.owner_sequence TO uqa_rule_caller;
SELECT pg_temp.rule_privilege_probe('transfer-routine-caller-granted', 'uqa_rule_caller', 'INSERT INTO uqa_rule_privilege_oracle.routine_owner_event VALUES (3)');
SELECT pg_temp.rule_privilege_probe('transfer-nextval-caller-granted', 'uqa_rule_caller', 'INSERT INTO uqa_rule_privilege_oracle.nextval_owner_event VALUES (3)');
SELECT 'expression-transfer|' || string_agg(kind || ':' || value, ',' ORDER BY kind) FROM uqa_rule_privilege_oracle.expression_log WHERE kind IN ('routine-owner', 'nextval-owner');

DROP SCHEMA uqa_rule_privilege_oracle CASCADE;
DROP ROLE uqa_rule_member;
DROP ROLE uqa_rule_owner;
DROP ROLE uqa_rule_caller;
DROP ROLE uqa_rule_next_owner;
DROP ROLE uqa_rule_resource_owner;
