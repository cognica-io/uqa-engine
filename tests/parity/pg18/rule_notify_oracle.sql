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
SELECT pg_temp.rule_notify_probe('payload-accepted', 'SELECT pg_notify(''uqa_rule_notify_unobserved'', repeat(''x'', 7999))');
SELECT pg_temp.rule_notify_probe('payload-limit', 'NOTIFY uqa_rule_notify_events, ' || quote_literal(repeat('x', 8000)));
SELECT pg_temp.rule_notify_probe('channel-accepted', 'SELECT pg_notify(repeat(''c'', 63), '''')');
SELECT pg_temp.rule_notify_probe('channel-limit', 'SELECT pg_notify(repeat(''c'', 64), '''')');
SELECT pg_temp.rule_notify_probe('null-channel', 'SELECT pg_notify(NULL, '''')');
SELECT 'backend-pid|' || (pg_backend_pid() > 0)::text;
SELECT 'listening|' || string_agg(channel, ',' ORDER BY channel) FROM pg_listening_channels() AS channels(channel);
SELECT 'pg-notify|' || pg_typeof(pg_notify('uqa_rule_notify_unobserved', NULL))::text || '|' || (pg_notify('uqa_rule_notify_unobserved', NULL) IS NULL)::text;
SELECT 'void|' || length(('ignored'::void)::text)::text || '|' || to_json('ignored'::void)::text || '|' || ('ignored'::void IS NULL)::text;
SELECT 'queue-usage|' || (pg_notification_queue_usage() BETWEEN 0.0 AND 1.0)::text;
SELECT 'catalog|' || string_agg(oid::text || ':' || proname || ':' || prorettype::regtype::text || ':' || proretset::text || ':' || prorows::integer::text || ':' || provolatile::text || ':' || proparallel::text || ':' || proisstrict::text, ',' ORDER BY oid) FROM pg_proc WHERE oid IN (2026, 3035, 3036, 3296);
SELECT pg_temp.rule_notify_probe('void-equality', 'SELECT ''left''::void = ''right''::void');
SELECT pg_temp.rule_notify_probe('void-column', 'CREATE TABLE uqa_rule_notify_oracle.bad_void(value void)');

UNLISTEN *;
DROP SCHEMA uqa_rule_notify_oracle CASCADE;
