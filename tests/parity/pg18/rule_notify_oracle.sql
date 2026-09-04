\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned
SET client_min_messages = warning;

DROP SCHEMA IF EXISTS uqa_rule_notify_oracle CASCADE;
CREATE SCHEMA uqa_rule_notify_oracle;

CREATE OR REPLACE FUNCTION pg_temp.rule_notify_probe(label text, command text)
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

LISTEN uqa_rule_notify_events;
BEGIN;
NOTIFY uqa_rule_notify_late, 'before-listen';
LISTEN uqa_rule_notify_late;
COMMIT;
SELECT 'final-listen|done';
BEGIN;
LISTEN uqa_rule_notify_removed;
NOTIFY uqa_rule_notify_removed, 'removed';
UNLISTEN uqa_rule_notify_removed;
COMMIT;
SELECT 'final-unlisten|done';
UNLISTEN uqa_rule_notify_late;
LISTEN "*";
NOTIFY "*", 'quoted-star';
UNLISTEN "*";
SELECT 'quoted-star|done';
CREATE TABLE uqa_rule_notify_oracle.items(id integer);
CREATE TABLE uqa_rule_notify_oracle.empty_items(id integer);
CREATE RULE item_inserted AS ON INSERT TO uqa_rule_notify_oracle.items DO ALSO NOTIFY uqa_rule_notify_events, 'inserted';
CREATE RULE item_updated AS ON UPDATE TO uqa_rule_notify_oracle.items DO ALSO NOTIFY uqa_rule_notify_events, 'updated';
CREATE RULE item_deleted AS ON DELETE TO uqa_rule_notify_oracle.items DO ALSO NOTIFY uqa_rule_notify_events, 'deleted';
SELECT 'definition|' || (pg_get_ruledef(oid, true) LIKE '%NOTIFY uqa_rule_notify_events, ''inserted''%') FROM pg_rewrite WHERE rulename = 'item_inserted';

INSERT INTO uqa_rule_notify_oracle.items VALUES (1), (2);
SELECT 'multi-insert|' || count(*) FROM uqa_rule_notify_oracle.items;
INSERT INTO uqa_rule_notify_oracle.items SELECT id FROM uqa_rule_notify_oracle.empty_items;
SELECT 'zero-insert|' || count(*) FROM uqa_rule_notify_oracle.items;
UPDATE uqa_rule_notify_oracle.items SET id = id WHERE false;
SELECT 'zero-update|' || count(*) FROM uqa_rule_notify_oracle.items;
DELETE FROM uqa_rule_notify_oracle.items WHERE false;
SELECT 'zero-delete|' || count(*) FROM uqa_rule_notify_oracle.items;

BEGIN;
NOTIFY uqa_rule_notify_events, 'deduplicated';
NOTIFY uqa_rule_notify_events, 'deduplicated';
COMMIT;
SELECT 'dedupe|done';

BEGIN;
NOTIFY uqa_rule_notify_events, 'kept';
SAVEPOINT notification_point;
NOTIFY uqa_rule_notify_events, 'discarded';
ROLLBACK TO SAVEPOINT notification_point;
COMMIT;
SELECT 'savepoint|done';

BEGIN;
INSERT INTO uqa_rule_notify_oracle.items VALUES (3);
ROLLBACK;
SELECT 'rollback|' || count(*) FROM uqa_rule_notify_oracle.items;

CREATE TABLE uqa_rule_notify_oracle.replaced_items(id integer);
CREATE RULE replace_insert AS ON INSERT TO uqa_rule_notify_oracle.replaced_items DO INSTEAD NOTIFY uqa_rule_notify_events, 'replaced';
INSERT INTO uqa_rule_notify_oracle.replaced_items VALUES (1), (2);
SELECT 'instead|' || count(*) FROM uqa_rule_notify_oracle.replaced_items;

SELECT pg_temp.rule_notify_probe('conditional', 'CREATE RULE conditional_notification AS ON INSERT TO uqa_rule_notify_oracle.items WHERE NEW.id > 0 DO ALSO NOTIFY uqa_rule_notify_events, ''conditional''');
SELECT pg_temp.rule_notify_probe('payload-limit', 'NOTIFY uqa_rule_notify_events, ' || quote_literal(repeat('x', 8000)));

UNLISTEN *;
DROP SCHEMA uqa_rule_notify_oracle CASCADE;
