\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned

DROP SCHEMA IF EXISTS uqa_view_privilege_oracle CASCADE;
DROP ROLE IF EXISTS uqa_view_acl_next_owner;
DROP ROLE IF EXISTS uqa_view_acl_owner;
DROP ROLE IF EXISTS uqa_view_acl_delegate;
DROP ROLE IF EXISTS uqa_view_acl_reader;
DROP ROLE IF EXISTS uqa_view_acl_column_reader;
DROP ROLE IF EXISTS uqa_view_acl_maintainer;
DROP ROLE IF EXISTS uqa_view_acl_outsider;

CREATE ROLE uqa_view_acl_owner;
CREATE ROLE uqa_view_acl_next_owner;
CREATE ROLE uqa_view_acl_delegate;
CREATE ROLE uqa_view_acl_reader;
CREATE ROLE uqa_view_acl_column_reader;
CREATE ROLE uqa_view_acl_maintainer;
CREATE ROLE uqa_view_acl_outsider;
GRANT uqa_view_acl_next_owner TO uqa_view_acl_owner WITH INHERIT FALSE, SET TRUE;
CREATE SCHEMA uqa_view_privilege_oracle AUTHORIZATION uqa_view_acl_owner;
GRANT USAGE ON SCHEMA uqa_view_privilege_oracle TO uqa_view_acl_next_owner, uqa_view_acl_delegate, uqa_view_acl_reader, uqa_view_acl_column_reader, uqa_view_acl_maintainer, uqa_view_acl_outsider;
GRANT CREATE ON SCHEMA uqa_view_privilege_oracle TO uqa_view_acl_next_owner;

CREATE OR REPLACE FUNCTION pg_temp.view_privilege_probe(label text, role_name text, command text)
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

SET ROLE uqa_view_acl_owner;
CREATE TABLE uqa_view_privilege_oracle.base(id integer PRIMARY KEY, value integer);
INSERT INTO uqa_view_privilege_oracle.base VALUES (1, 10), (2, 20);
CREATE VIEW uqa_view_privilege_oracle.items AS SELECT id, value FROM uqa_view_privilege_oracle.base;
CREATE VIEW uqa_view_privilege_oracle.identity_items AS SELECT id, current_user AS who FROM uqa_view_privilege_oracle.base;
CREATE VIEW uqa_view_privilege_oracle.invoker_items WITH (security_invoker=true) AS SELECT id, value FROM uqa_view_privilege_oracle.base;
CREATE VIEW uqa_view_privilege_oracle.writable AS SELECT id, value FROM uqa_view_privilege_oracle.base;
CREATE VIEW uqa_view_privilege_oracle.all_items AS SELECT id, value FROM uqa_view_privilege_oracle.base;
CREATE VIEW uqa_view_privilege_oracle.cascade_items AS SELECT id FROM uqa_view_privilege_oracle.base;
CREATE VIEW uqa_view_privilege_oracle.rule_low AS SELECT id, value FROM uqa_view_privilege_oracle.base;
CREATE VIEW uqa_view_privilege_oracle.rule_stop AS SELECT id, value FROM uqa_view_privilege_oracle.rule_low;
CREATE RULE rule_stop_insert AS ON INSERT TO uqa_view_privilege_oracle.rule_stop DO INSTEAD NOTHING;
CREATE RULE rule_stop_update AS ON UPDATE TO uqa_view_privilege_oracle.rule_stop DO INSTEAD NOTHING;
CREATE RULE rule_stop_delete AS ON DELETE TO uqa_view_privilege_oracle.rule_stop DO INSTEAD NOTHING;
CREATE VIEW uqa_view_privilege_oracle.rule_top AS SELECT id, value FROM uqa_view_privilege_oracle.rule_stop;
CREATE MATERIALIZED VIEW uqa_view_privilege_oracle.snapshot AS SELECT id, value FROM uqa_view_privilege_oracle.base;
CREATE MATERIALIZED VIEW uqa_view_privilege_oracle.identity_snapshot AS SELECT current_user AS who;
CREATE MATERIALIZED VIEW uqa_view_privilege_oracle.empty_snapshot AS SELECT id FROM uqa_view_privilege_oracle.base WITH NO DATA;
CREATE SEQUENCE uqa_view_privilege_oracle.ids;
RESET ROLE;

SELECT 'defaults|' || (SELECT relacl IS NULL FROM pg_catalog.pg_class WHERE oid = 'uqa_view_privilege_oracle.items'::regclass) || '|' || (SELECT relacl IS NULL FROM pg_catalog.pg_class WHERE oid = 'uqa_view_privilege_oracle.snapshot'::regclass) || '|' || (SELECT bool_and(attacl IS NULL) FROM pg_catalog.pg_attribute WHERE attrelid = 'uqa_view_privilege_oracle.items'::regclass AND attnum > 0);

SET ROLE uqa_view_acl_owner;
GRANT SELECT ON TABLE uqa_view_privilege_oracle.items TO uqa_view_acl_delegate WITH GRANT OPTION;
GRANT SELECT(id) ON TABLE uqa_view_privilege_oracle.items TO uqa_view_acl_column_reader;
GRANT SELECT ON TABLE uqa_view_privilege_oracle.identity_items, uqa_view_privilege_oracle.invoker_items TO uqa_view_acl_reader;
GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE uqa_view_privilege_oracle.writable TO uqa_view_acl_reader;
GRANT MAINTAIN ON TABLE uqa_view_privilege_oracle.identity_snapshot, uqa_view_privilege_oracle.items TO uqa_view_acl_maintainer;
GRANT SELECT ON TABLE uqa_view_privilege_oracle.empty_snapshot TO uqa_view_acl_reader;
GRANT ALL PRIVILEGES ON TABLE uqa_view_privilege_oracle.all_items TO uqa_view_acl_outsider;
GRANT SELECT ON TABLE uqa_view_privilege_oracle.cascade_items TO uqa_view_acl_delegate WITH GRANT OPTION;
GRANT INSERT, UPDATE, DELETE ON TABLE uqa_view_privilege_oracle.rule_top TO uqa_view_acl_reader;
RESET ROLE;

SET ROLE uqa_view_acl_delegate;
GRANT SELECT ON TABLE uqa_view_privilege_oracle.items, uqa_view_privilege_oracle.cascade_items TO uqa_view_acl_reader;
RESET ROLE;

SELECT 'relation-acl|' || relacl::text FROM pg_catalog.pg_class WHERE oid = 'uqa_view_privilege_oracle.items'::regclass;
SELECT 'column-acl|' || attacl::text FROM pg_catalog.pg_attribute WHERE attrelid = 'uqa_view_privilege_oracle.items'::regclass AND attname = 'id';
SELECT 'all-acl|' || relacl::text FROM pg_catalog.pg_class WHERE oid = 'uqa_view_privilege_oracle.all_items'::regclass;
SELECT 'inquiry|' || has_table_privilege('uqa_view_acl_reader', 'uqa_view_privilege_oracle.items', 'SELECT') || '|' || has_table_privilege('uqa_view_acl_reader'::regrole::oid, 'uqa_view_privilege_oracle.items'::regclass::oid, 'SELECT') || '|' || has_column_privilege('uqa_view_acl_column_reader', 'uqa_view_privilege_oracle.items', 'id', 'SELECT') || '|' || has_column_privilege('uqa_view_acl_column_reader'::regrole::oid, 'uqa_view_privilege_oracle.items'::regclass::oid, 1::smallint, 'SELECT') || '|' || coalesce(has_column_privilege('uqa_view_acl_column_reader', 'uqa_view_privilege_oracle.items', -1::smallint, 'SELECT')::text, 'NULL');
SELECT pg_temp.view_privilege_probe('view-system-column', 'uqa_view_acl_column_reader', 'SELECT has_column_privilege(''uqa_view_privilege_oracle.items'', ''ctid'', ''SELECT'')');

SELECT pg_temp.view_privilege_probe('select-denied', 'uqa_view_acl_outsider', 'SELECT value FROM uqa_view_privilege_oracle.items');
SELECT pg_temp.view_privilege_probe('select-granted', 'uqa_view_acl_reader', 'SELECT value FROM uqa_view_privilege_oracle.items');
SELECT pg_temp.view_privilege_probe('column-select-granted', 'uqa_view_acl_column_reader', 'SELECT id FROM uqa_view_privilege_oracle.items');
SELECT pg_temp.view_privilege_probe('column-select-denied', 'uqa_view_acl_column_reader', 'SELECT value FROM uqa_view_privilege_oracle.items');
SELECT pg_temp.view_privilege_probe('column-count-granted', 'uqa_view_acl_column_reader', 'SELECT count(*) FROM uqa_view_privilege_oracle.items');

SET ROLE uqa_view_acl_reader;
SELECT 'definer-current-user|' || who FROM uqa_view_privilege_oracle.identity_items ORDER BY id LIMIT 1;
RESET ROLE;
SELECT pg_temp.view_privilege_probe('invoker-base-denied', 'uqa_view_acl_reader', 'SELECT value FROM uqa_view_privilege_oracle.invoker_items');
SET ROLE uqa_view_acl_owner;
GRANT SELECT ON TABLE uqa_view_privilege_oracle.base TO uqa_view_acl_reader;
RESET ROLE;
SELECT pg_temp.view_privilege_probe('invoker-base-granted', 'uqa_view_acl_reader', 'SELECT value FROM uqa_view_privilege_oracle.invoker_items');

SELECT pg_temp.view_privilege_probe('view-insert', 'uqa_view_acl_reader', 'INSERT INTO uqa_view_privilege_oracle.writable VALUES (3, 30)');
SELECT pg_temp.view_privilege_probe('view-update', 'uqa_view_acl_reader', 'UPDATE uqa_view_privilege_oracle.writable SET value = 31 WHERE id = 3');
SELECT pg_temp.view_privilege_probe('view-delete', 'uqa_view_acl_reader', 'DELETE FROM uqa_view_privilege_oracle.writable WHERE id = 2');
SELECT 'view-dml-state|' || string_agg(id::text || ':' || value::text, ',' ORDER BY id) FROM uqa_view_privilege_oracle.base;

SELECT pg_temp.view_privilege_probe('maintainer-refresh', 'uqa_view_acl_maintainer', 'REFRESH MATERIALIZED VIEW uqa_view_privilege_oracle.identity_snapshot');
SELECT pg_temp.view_privilege_probe('maintainer-select-denied', 'uqa_view_acl_maintainer', 'SELECT who FROM uqa_view_privilege_oracle.identity_snapshot');
SELECT 'refresh-owner-context|' || who FROM uqa_view_privilege_oracle.identity_snapshot;
SELECT 'maintain-regular-view|' || has_table_privilege('uqa_view_acl_maintainer', 'uqa_view_privilege_oracle.items', 'MAINTAIN');
SELECT pg_temp.view_privilege_probe('unpopulated-no-select', 'uqa_view_acl_outsider', 'SELECT id FROM uqa_view_privilege_oracle.empty_snapshot');
SELECT pg_temp.view_privilege_probe('unpopulated-with-select', 'uqa_view_acl_reader', 'SELECT id FROM uqa_view_privilege_oracle.empty_snapshot');
SELECT pg_temp.view_privilege_probe('matview-insert-no-privilege', 'uqa_view_acl_reader', 'INSERT INTO uqa_view_privilege_oracle.snapshot VALUES (4, 40)');
SET ROLE uqa_view_acl_owner;
GRANT INSERT ON TABLE uqa_view_privilege_oracle.snapshot TO uqa_view_acl_reader;
RESET ROLE;
SELECT pg_temp.view_privilege_probe('matview-insert-with-privilege', 'uqa_view_acl_reader', 'INSERT INTO uqa_view_privilege_oracle.snapshot VALUES (4, 40)');

SET ROLE uqa_view_acl_column_reader;
SELECT 'information-schema|' || (SELECT count(*) FROM information_schema.views WHERE table_schema = 'uqa_view_privilege_oracle') || '|' || (SELECT string_agg(column_name, ',' ORDER BY ordinal_position) FROM information_schema.columns WHERE table_schema = 'uqa_view_privilege_oracle' AND table_name = 'items') || '|' || (SELECT string_agg(column_name || ':' || privilege_type, ',' ORDER BY column_name, privilege_type) FROM information_schema.column_privileges WHERE table_schema = 'uqa_view_privilege_oracle' AND table_name = 'items') || '|' || (SELECT count(*) FROM information_schema.columns WHERE table_schema = 'uqa_view_privilege_oracle' AND table_name = 'snapshot');
RESET ROLE;

SELECT pg_temp.view_privilege_probe('dependent-restrict', 'uqa_view_acl_owner', 'REVOKE GRANT OPTION FOR SELECT ON TABLE uqa_view_privilege_oracle.cascade_items FROM uqa_view_acl_delegate RESTRICT');
SET ROLE uqa_view_acl_owner;
REVOKE GRANT OPTION FOR SELECT ON TABLE uqa_view_privilege_oracle.cascade_items FROM uqa_view_acl_delegate CASCADE;
RESET ROLE;
SELECT 'cascade|' || has_table_privilege('uqa_view_acl_delegate', 'uqa_view_privilege_oracle.cascade_items', 'SELECT') || '|' || has_table_privilege('uqa_view_acl_delegate', 'uqa_view_privilege_oracle.cascade_items', 'SELECT WITH GRANT OPTION') || '|' || has_table_privilege('uqa_view_acl_reader', 'uqa_view_privilege_oracle.cascade_items', 'SELECT');

SET ROLE uqa_view_acl_owner;
ALTER VIEW uqa_view_privilege_oracle.rule_low OWNER TO uqa_view_acl_next_owner;
ALTER VIEW uqa_view_privilege_oracle.rule_stop OWNER TO uqa_view_acl_next_owner;
RESET ROLE;
SELECT pg_temp.view_privilege_probe('nested-rule-insert-stop', 'uqa_view_acl_reader', 'INSERT INTO uqa_view_privilege_oracle.rule_top VALUES (4, 40)');
SELECT pg_temp.view_privilege_probe('nested-rule-update-stop', 'uqa_view_acl_reader', 'UPDATE uqa_view_privilege_oracle.rule_top SET value = 99');
SELECT pg_temp.view_privilege_probe('nested-rule-delete-stop', 'uqa_view_acl_reader', 'DELETE FROM uqa_view_privilege_oracle.rule_top');

GRANT SELECT ON ALL TABLES IN SCHEMA uqa_view_privilege_oracle TO uqa_view_acl_outsider;
SELECT 'all-tables-schema|' || has_table_privilege('uqa_view_acl_outsider', 'uqa_view_privilege_oracle.items', 'SELECT') || '|' || has_table_privilege('uqa_view_acl_outsider', 'uqa_view_privilege_oracle.snapshot', 'SELECT') || '|' || has_sequence_privilege('uqa_view_acl_outsider', 'uqa_view_privilege_oracle.ids', 'SELECT');

SET ROLE uqa_view_acl_owner;
GRANT SELECT ON TABLE uqa_view_privilege_oracle.base TO uqa_view_acl_next_owner;
ALTER VIEW uqa_view_privilege_oracle.items OWNER TO uqa_view_acl_next_owner;
RESET ROLE;
SELECT 'owner-transfer|' || relowner::regrole || '|' || relacl::text FROM pg_catalog.pg_class WHERE oid = 'uqa_view_privilege_oracle.items'::regclass;
SELECT 'owner-transfer-column|' || attacl::text FROM pg_catalog.pg_attribute WHERE attrelid = 'uqa_view_privilege_oracle.items'::regclass AND attname = 'id';
SELECT pg_temp.view_privilege_probe('grant-role-dependent', 'postgres', 'DROP ROLE uqa_view_acl_delegate');

DROP SCHEMA uqa_view_privilege_oracle CASCADE;
DROP ROLE uqa_view_acl_next_owner;
DROP ROLE uqa_view_acl_owner;
DROP ROLE uqa_view_acl_delegate;
DROP ROLE uqa_view_acl_reader;
DROP ROLE uqa_view_acl_column_reader;
DROP ROLE uqa_view_acl_maintainer;
DROP ROLE uqa_view_acl_outsider;
