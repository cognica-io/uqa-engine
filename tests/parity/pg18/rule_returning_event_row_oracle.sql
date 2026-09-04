\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned

DROP SCHEMA IF EXISTS uqa_rule_returning_event_row_oracle CASCADE;
CREATE SCHEMA uqa_rule_returning_event_row_oracle;
SET search_path = uqa_rule_returning_event_row_oracle, public;

CREATE OR REPLACE FUNCTION pg_temp.rule_returning_state(label text, command text)
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

CREATE OR REPLACE FUNCTION pg_temp.rule_returning_error(label text, command text)
RETURNS text
LANGUAGE plpgsql
AS $oracle$
DECLARE
    state text;
    message text;
BEGIN
    EXECUTE command;
    RETURN label || '|ok';
EXCEPTION WHEN OTHERS THEN
    GET STACKED DIAGNOSTICS state = RETURNED_SQLSTATE, message = MESSAGE_TEXT;
    RETURN label || '|' || state || '|' || message;
END
$oracle$;

CREATE TABLE insert_event(e integer, x text);
CREATE TABLE insert_target(i integer, y text);
CREATE RULE insert_provider AS ON INSERT TO insert_event DO INSTEAD INSERT INTO insert_target VALUES (NEW.e + 100, NEW.x || '-action') RETURNING NEW.*;
INSERT INTO insert_event VALUES (1, 'event') RETURNING 'insert', e, x, coalesce(old.x, '<null>'), new.x;
SELECT 'insert-target|' || i || ':' || y FROM insert_target;

CREATE TABLE insert_old_event(e integer PRIMARY KEY, x text);
CREATE TABLE insert_old_target(i integer, y text);
INSERT INTO insert_old_event VALUES (6, 'event-old');
CREATE RULE insert_old_provider AS ON UPDATE TO insert_old_event DO INSTEAD INSERT INTO insert_old_target VALUES (NEW.e + 10, NEW.x || '-action') RETURNING OLD.*;
UPDATE insert_old_event SET x = 'event-new' RETURNING 'insert-old', coalesce(e::text, '<null>'), coalesce(x, '<null>'), coalesce(old.x, '<null>'), coalesce(new.x, '<null>');
SELECT 'insert-old-target|' || i || ':' || y FROM insert_old_target;

CREATE TABLE update_event(e integer PRIMARY KEY, x text);
CREATE TABLE update_target(i integer PRIMARY KEY, y text);
INSERT INTO update_event VALUES (2, 'event-old');
INSERT INTO update_target VALUES (2, 'action-old');
CREATE RULE update_provider AS ON UPDATE TO update_event DO INSTEAD UPDATE update_target AS trgt SET y = NEW.x || '-action' WHERE trgt.i = OLD.e RETURNING NEW.*;
UPDATE update_event SET x = 'event-new' WHERE e = 2 RETURNING 'update', e, x, old.x, new.x;
SELECT 'update-event|' || e || ':' || x FROM update_event;
SELECT 'update-target|' || i || ':' || y FROM update_target;

CREATE TABLE delete_event(e integer PRIMARY KEY, x text);
CREATE TABLE delete_target(i integer PRIMARY KEY, y text);
INSERT INTO delete_event VALUES (3, 'event-old');
INSERT INTO delete_target VALUES (3, 'action-old');
CREATE RULE delete_provider AS ON DELETE TO delete_event DO INSTEAD DELETE FROM delete_target AS trgt WHERE trgt.i = OLD.e RETURNING OLD.*;
DELETE FROM delete_event WHERE e = 3 RETURNING 'delete', e, x, old.x, coalesce(new.x, '<null>');
SELECT 'delete-event|' || count(*) FROM delete_event;
SELECT 'delete-target|' || count(*) FROM delete_target;

CREATE TABLE explicit_event(e integer PRIMARY KEY, x text);
CREATE TABLE explicit_target(i integer PRIMARY KEY, y text);
INSERT INTO explicit_event VALUES (4, 'event-old');
INSERT INTO explicit_target VALUES (4, 'action-old');
CREATE RULE explicit_provider AS ON UPDATE TO explicit_event DO INSTEAD UPDATE explicit_target AS trgt SET y = NEW.x || '-action' WHERE trgt.i = OLD.e RETURNING WITH (NEW AS action_new) action_new.*;
UPDATE explicit_event SET x = 'event-new' WHERE e = 4 RETURNING 'explicit', e, x, old.x, new.x;
SELECT 'explicit-target|' || i || ':' || y FROM explicit_target;

CREATE TABLE cardinality_event(e integer, x text);
CREATE TABLE cardinality_target(i integer, y text);
INSERT INTO cardinality_event VALUES (1, 'one'), (2, 'two');
INSERT INTO cardinality_target VALUES (10, 'ten');
CREATE RULE cardinality_provider AS ON UPDATE TO cardinality_event DO INSTEAD UPDATE cardinality_target SET y = y RETURNING NEW.*;
UPDATE cardinality_event SET x = x || '!' RETURNING 'cardinality', e, x;
SELECT 'cardinality-event|' || string_agg(e || ':' || x, ',' ORDER BY e) FROM cardinality_event;
SELECT 'cardinality-target|' || string_agg(i || ':' || y, ',' ORDER BY i) FROM cardinality_target;

CREATE TABLE scope_event(e integer, x text);
CREATE TABLE scope_target(i integer, y text);
SELECT pg_temp.rule_returning_state('unused-target-new', 'CREATE RULE unused_target_new AS ON UPDATE TO scope_event DO ALSO UPDATE scope_target AS new SET y = ''literal''');
SELECT pg_temp.rule_returning_state('unused-target-old', 'CREATE RULE unused_target_old AS ON DELETE TO scope_event DO ALSO DELETE FROM scope_target AS old');
SELECT pg_temp.rule_returning_state('ambiguous-body', 'CREATE RULE ambiguous_body AS ON UPDATE TO scope_event DO ALSO UPDATE scope_target AS new SET y = new.y');
SELECT pg_temp.rule_returning_state('nested-target-alias', 'CREATE RULE nested_target_alias AS ON UPDATE TO scope_event DO ALSO UPDATE scope_target AS new SET y = (SELECT new.x FROM (VALUES (''nested'')) AS new(x))');
SELECT pg_temp.rule_returning_state('invalid-old', 'CREATE RULE invalid_old AS ON INSERT TO scope_event DO INSTEAD UPDATE scope_target SET y = ''x'' RETURNING OLD.*');
SELECT pg_temp.rule_returning_state('invalid-new', 'CREATE RULE invalid_new AS ON DELETE TO scope_event DO INSTEAD DELETE FROM scope_target RETURNING NEW.*');
SELECT pg_temp.rule_returning_state('missing-event', 'CREATE RULE missing_event AS ON UPDATE TO scope_event DO INSTEAD UPDATE scope_target SET y = ''x'' RETURNING NEW.missing, NEW.x');
SELECT pg_temp.rule_returning_state('missing-action', 'CREATE RULE missing_action AS ON UPDATE TO scope_event DO INSTEAD INSERT INTO scope_target VALUES (NEW.e, NEW.x) RETURNING NEW.e, NEW.x');
SELECT pg_temp.rule_returning_state('ambiguous-new', 'CREATE RULE ambiguous_new AS ON UPDATE TO scope_event DO INSTEAD UPDATE scope_target AS new SET y = ''x'' RETURNING i');
SELECT pg_temp.rule_returning_state('ambiguous-old', 'CREATE RULE ambiguous_old AS ON DELETE TO scope_event DO INSTEAD DELETE FROM scope_target AS old RETURNING i');
SELECT pg_temp.rule_returning_state('inaccessible-insert-event', 'CREATE RULE inaccessible_insert_event AS ON UPDATE TO scope_event DO INSTEAD INSERT INTO scope_target VALUES (NEW.e, NEW.x) RETURNING WITH (NEW AS action_new) NEW.*');

CREATE TABLE lifecycle_event(z integer PRIMARY KEY, a text);
CREATE TABLE lifecycle_target(i integer PRIMARY KEY, y text);
INSERT INTO lifecycle_event VALUES (1, 'event-old');
INSERT INTO lifecycle_target VALUES (1, 'action-old');
CREATE RULE lifecycle_provider AS ON UPDATE TO lifecycle_event DO INSTEAD UPDATE lifecycle_target AS trgt SET y = NEW.a WHERE trgt.i = OLD.z RETURNING NEW.*;
ALTER TABLE lifecycle_event RENAME COLUMN a TO renamed;
SELECT 'rename-definition|' || (pg_get_ruledef(oid, true) LIKE '%new.renamed%') || '|' || (pg_get_ruledef(oid, true) LIKE '%new.renamed AS a%') FROM pg_rewrite WHERE rulename = 'lifecycle_provider';
UPDATE lifecycle_event SET renamed = 'event-new' RETURNING 'rename-row', z, renamed;
SELECT pg_temp.rule_returning_state('drop-restrict', 'ALTER TABLE lifecycle_event DROP COLUMN renamed');
ALTER TABLE lifecycle_event DROP COLUMN renamed CASCADE;
SELECT 'drop-cascade|' || count(*) FROM pg_rewrite WHERE rulename = 'lifecycle_provider';

CREATE TABLE add_event(z integer PRIMARY KEY, a text);
CREATE TABLE add_target(i integer PRIMARY KEY, y text);
INSERT INTO add_event VALUES (1, 'event-old');
INSERT INTO add_target VALUES (1, 'action-old');
CREATE RULE add_provider AS ON UPDATE TO add_event DO INSTEAD UPDATE add_target AS trgt SET y = NEW.a WHERE trgt.i = OLD.z RETURNING NEW.*;
ALTER TABLE add_event ADD COLUMN later integer DEFAULT 7;
SELECT pg_temp.rule_returning_error('add-width', 'UPDATE add_event SET a = ''event-new'' RETURNING *');
SELECT 'add-event|' || z || ':' || a || ':' || later FROM add_event;
SELECT 'add-target|' || i || ':' || y FROM add_target;

RESET search_path;
DROP SCHEMA uqa_rule_returning_event_row_oracle CASCADE;
