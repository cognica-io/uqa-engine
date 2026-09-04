\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned

DROP SCHEMA IF EXISTS uqa_rule_whole_row_oracle CASCADE;
CREATE SCHEMA uqa_rule_whole_row_oracle;
SET search_path = uqa_rule_whole_row_oracle, pg_catalog;

CREATE OR REPLACE FUNCTION pg_temp.rule_whole_row_state(label text, command text)
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

CREATE TABLE scalar_event(a integer PRIMARY KEY, b text);
CREATE TABLE scalar_log(stage integer, payload jsonb);
CREATE RULE scalar_row AS ON INSERT TO scalar_event DO ALSO INSERT INTO scalar_log VALUES (NEW.a, to_jsonb(NEW.*));
ALTER TABLE scalar_event ADD COLUMN c integer DEFAULT 7;
INSERT INTO scalar_event(a, b) VALUES (1, 'one');
SELECT 'scalar-add|' || payload::text FROM scalar_log WHERE stage = 1;
ALTER TABLE scalar_event RENAME COLUMN b TO renamed;
INSERT INTO scalar_event(a, renamed, c) VALUES (2, 'two', 8);
SELECT 'scalar-rename|' || payload::text FROM scalar_log WHERE stage = 2;
SELECT pg_temp.rule_whole_row_state('scalar-drop', 'ALTER TABLE scalar_event DROP COLUMN renamed RESTRICT');
INSERT INTO scalar_event(a, c) VALUES (3, 9);
SELECT 'scalar-drop-row|' || payload::text FROM scalar_log WHERE stage = 3;
SELECT 'scalar-definition|' || (pg_get_ruledef(oid, true) LIKE '%to_jsonb(new.*)%') FROM pg_rewrite WHERE rulename = 'scalar_row';

CREATE TABLE condition_event(a integer PRIMARY KEY, b text);
CREATE TABLE condition_log(payload jsonb);
INSERT INTO condition_event VALUES (1, 'one'), (2, 'two');
CREATE RULE condition_row AS ON UPDATE TO condition_event WHERE OLD IS DISTINCT FROM NEW DO ALSO INSERT INTO condition_log VALUES (to_jsonb(NEW));
UPDATE condition_event SET b = b WHERE a = 1;
SELECT 'condition-same|' || count(*) FROM condition_log;
UPDATE condition_event SET b = b || '!';
SELECT 'condition-change|' || string_agg(payload::text, ',' ORDER BY payload->>'a') FROM condition_log;
TRUNCATE condition_log;
ALTER TABLE condition_event ADD COLUMN c integer DEFAULT 7;
UPDATE condition_event SET c = 8 WHERE a = 1;
SELECT 'condition-add|' || payload::text FROM condition_log;

CREATE TABLE scope_event(a integer, b text);
CREATE TABLE scope_log(payload jsonb);
CREATE TABLE scope_local(new text);
CREATE TABLE scope_old(old text);
INSERT INTO scope_local VALUES ('table-local');
INSERT INTO scope_old VALUES ('old-local');
SELECT pg_temp.rule_whole_row_state('invalid-insert-old', 'CREATE RULE invalid_insert_old AS ON INSERT TO scope_event DO ALSO INSERT INTO scope_log VALUES (to_jsonb(OLD))');
SELECT pg_temp.rule_whole_row_state('invalid-insert-old-star', 'CREATE RULE invalid_insert_old_star AS ON INSERT TO scope_event DO ALSO INSERT INTO scope_log VALUES (to_jsonb(OLD.*))');
SELECT pg_temp.rule_whole_row_state('invalid-delete-new', 'CREATE RULE invalid_delete_new AS ON DELETE TO scope_event DO ALSO INSERT INTO scope_log VALUES (to_jsonb(NEW))');
SELECT pg_temp.rule_whole_row_state('invalid-delete-new-star', 'CREATE RULE invalid_delete_new_star AS ON DELETE TO scope_event DO ALSO INSERT INTO scope_log VALUES (to_jsonb(NEW.*))');
CREATE RULE local_bare_row AS ON INSERT TO scope_event DO ALSO INSERT INTO scope_log VALUES ((SELECT to_jsonb(new) FROM (VALUES (42)) AS new(x)));
CREATE RULE local_star_row AS ON INSERT TO scope_event DO ALSO INSERT INTO scope_log VALUES ((SELECT to_jsonb(new.*) FROM (VALUES (43)) AS new(x)));
CREATE RULE local_column AS ON INSERT TO scope_event DO ALSO INSERT INTO scope_log VALUES ((SELECT to_jsonb(new) FROM (VALUES ('local')) AS value(new)));
CREATE RULE local_table_column AS ON INSERT TO scope_event DO ALSO INSERT INTO scope_log SELECT to_jsonb(new) FROM scope_local;
CREATE RULE local_invalid_side_column AS ON INSERT TO scope_event DO ALSO INSERT INTO scope_log SELECT to_jsonb(old) FROM scope_old;
CREATE RULE local_derived_column AS ON INSERT TO scope_event DO ALSO INSERT INTO scope_log VALUES ((SELECT to_jsonb(new) FROM (SELECT 'derived'::text AS new) AS value));
CREATE RULE local_cte_column AS ON INSERT TO scope_event DO ALSO WITH value(new) AS (VALUES ('cte')) INSERT INTO scope_log SELECT to_jsonb(new) FROM value;
INSERT INTO scope_event VALUES (1, 'event');
SELECT 'local-shadow|' || string_agg(payload::text, ',' ORDER BY payload::text) FROM scope_log;

CREATE TABLE insert_return_event(payload jsonb);
CREATE TABLE insert_return_target(i integer);
CREATE RULE insert_return_row AS ON INSERT TO insert_return_event DO INSTEAD INSERT INTO insert_return_target VALUES (42) RETURNING to_jsonb(NEW);
INSERT INTO insert_return_event VALUES ('{}') RETURNING 'return-insert|' || payload::text;

CREATE TABLE update_return_event(payload jsonb);
CREATE TABLE update_return_target(i integer);
INSERT INTO update_return_event VALUES ('{"before": 1}');
INSERT INTO update_return_target VALUES (7);
CREATE RULE update_return_row AS ON UPDATE TO update_return_event DO INSTEAD UPDATE update_return_target SET i = i + 1 RETURNING to_jsonb(NEW);
UPDATE update_return_event SET payload = '{"after": 2}' RETURNING 'return-update|' || payload::text;

CREATE TABLE alias_return_event(payload jsonb);
CREATE TABLE alias_return_target(i integer);
INSERT INTO alias_return_event VALUES ('{"before": 1}');
INSERT INTO alias_return_target VALUES (8);
CREATE RULE alias_return_row AS ON UPDATE TO alias_return_event DO INSTEAD UPDATE alias_return_target SET i = i + 1 RETURNING WITH (NEW AS action_new) to_jsonb(action_new);
UPDATE alias_return_event SET payload = '{"after": 2}' RETURNING 'return-alias|' || payload::text;

CREATE TABLE star_event(a integer PRIMARY KEY, b text);
CREATE TABLE star_target(i integer PRIMARY KEY, y text);
CREATE RULE star_provider AS ON INSERT TO star_event DO INSTEAD INSERT INTO star_target VALUES (NEW.a, NEW.b) RETURNING *;
ALTER TABLE star_target ADD COLUMN later integer DEFAULT 9;
INSERT INTO star_event VALUES (1, 'one') RETURNING 'star-add|' || a || ':' || b;
SELECT 'star-target|' || i || ':' || y || ':' || later FROM star_target;
ALTER TABLE star_target RENAME COLUMN y TO renamed;
SELECT 'star-rename-definition|' || (pg_get_ruledef(oid, true) LIKE '%renamed AS y%') FROM pg_rewrite WHERE rulename = 'star_provider';
INSERT INTO star_event VALUES (2, 'two') RETURNING 'star-rename-row|' || a || ':' || b;
SELECT pg_temp.rule_whole_row_state('star-drop-restrict', 'ALTER TABLE star_target DROP COLUMN renamed RESTRICT');
ALTER TABLE star_target DROP COLUMN renamed CASCADE;
SELECT 'star-drop-cascade|' || count(*) FROM pg_rewrite WHERE rulename = 'star_provider';

CREATE TABLE new_star_event(a integer, b text);
CREATE TABLE new_star_target(i integer, y text);
CREATE RULE new_star_provider AS ON INSERT TO new_star_event DO INSTEAD INSERT INTO new_star_target VALUES (NEW.a, NEW.b) RETURNING NEW.*;
ALTER TABLE new_star_target ADD COLUMN later integer DEFAULT 9;
INSERT INTO new_star_event VALUES (3, 'new-image') RETURNING 'star-new|' || a || ':' || b;

CREATE TABLE target_star_event(a integer, b text);
CREATE TABLE target_star_target(i integer, y text);
CREATE RULE target_star_provider AS ON INSERT TO target_star_event DO INSTEAD INSERT INTO target_star_target AS target VALUES (NEW.a, NEW.b) RETURNING target.*;
ALTER TABLE target_star_target ADD COLUMN later integer DEFAULT 9;
INSERT INTO target_star_event VALUES (4, 'target-alias') RETURNING 'star-target-alias|' || a || ':' || b;

CREATE TABLE image_star_event(a integer, b text);
CREATE TABLE image_star_target(i integer, y text);
CREATE RULE image_star_provider AS ON INSERT TO image_star_event DO INSTEAD INSERT INTO image_star_target VALUES (NEW.a, NEW.b) RETURNING WITH (NEW AS action_new) action_new.*;
ALTER TABLE image_star_target ADD COLUMN later integer DEFAULT 9;
INSERT INTO image_star_event VALUES (5, 'explicit-image') RETURNING 'star-explicit-image|' || a || ':' || b;

CREATE TABLE system_image(i integer);
INSERT INTO system_image VALUES (6) RETURNING 'system-image|' || to_jsonb(NEW)::text || '|' || (NEW.xmin IS NOT NULL)::text || '|' || (NEW.tableoid = 'system_image'::regclass)::text;

RESET search_path;
DROP SCHEMA uqa_rule_whole_row_oracle CASCADE;
