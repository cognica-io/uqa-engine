\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned

DROP SCHEMA IF EXISTS uqa_view_ownership_oracle CASCADE;
DROP ROLE IF EXISTS uqa_view_schema_owner;
DROP ROLE IF EXISTS uqa_view_owner_member;
DROP ROLE IF EXISTS uqa_view_owner_source;
DROP ROLE IF EXISTS uqa_view_owner_target;
DROP ROLE IF EXISTS uqa_view_owner_no_create;
DROP ROLE IF EXISTS uqa_view_owner_no_set;
DROP ROLE IF EXISTS uqa_view_owner_outsider;

CREATE ROLE uqa_view_owner_source;
CREATE ROLE uqa_view_schema_owner;
CREATE ROLE uqa_view_owner_member INHERIT;
CREATE ROLE uqa_view_owner_target;
CREATE ROLE uqa_view_owner_no_create;
CREATE ROLE uqa_view_owner_no_set;
CREATE ROLE uqa_view_owner_outsider;
GRANT uqa_view_owner_source TO uqa_view_owner_member;
GRANT uqa_view_owner_target, uqa_view_owner_no_create TO uqa_view_owner_source;
GRANT uqa_view_owner_no_set TO uqa_view_owner_source WITH SET FALSE;
CREATE SCHEMA uqa_view_ownership_oracle AUTHORIZATION uqa_view_schema_owner;
GRANT USAGE ON SCHEMA uqa_view_ownership_oracle TO uqa_view_owner_member, uqa_view_owner_no_create;
GRANT USAGE, CREATE ON SCHEMA uqa_view_ownership_oracle TO uqa_view_owner_source, uqa_view_owner_target, uqa_view_owner_no_set, uqa_view_owner_outsider;

CREATE OR REPLACE FUNCTION pg_temp.view_owner_probe(label text, role_name text, command text)
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

SET ROLE uqa_view_owner_source;
CREATE TABLE uqa_view_ownership_oracle.base(id integer PRIMARY KEY, value integer);
INSERT INTO uqa_view_ownership_oracle.base VALUES (1, 10);
GRANT SELECT ON TABLE uqa_view_ownership_oracle.base TO uqa_view_owner_target, uqa_view_owner_outsider;
CREATE VIEW uqa_view_ownership_oracle.items AS SELECT id, value FROM uqa_view_ownership_oracle.base;
CREATE VIEW uqa_view_ownership_oracle.items_child AS SELECT id FROM uqa_view_ownership_oracle.items;
CREATE MATERIALIZED VIEW uqa_view_ownership_oracle.snapshot AS SELECT id, value FROM uqa_view_ownership_oracle.base;
CREATE VIEW uqa_view_ownership_oracle.rollback_items AS SELECT id FROM uqa_view_ownership_oracle.base;
CREATE VIEW uqa_view_ownership_oracle.schema_drop_view AS SELECT id FROM uqa_view_ownership_oracle.base;
CREATE MATERIALIZED VIEW uqa_view_ownership_oracle.schema_drop_snapshot AS SELECT id FROM uqa_view_ownership_oracle.base;
RESET ROLE;

SELECT 'initial-view-owner|' || pg_get_userbyid(c.relowner)
FROM pg_class AS c
JOIN pg_namespace AS n ON n.oid = c.relnamespace
WHERE n.nspname = 'uqa_view_ownership_oracle' AND c.relname = 'items';
SELECT 'initial-matview-owner|' || pg_get_userbyid(c.relowner)
FROM pg_class AS c
JOIN pg_namespace AS n ON n.oid = c.relnamespace
WHERE n.nspname = 'uqa_view_ownership_oracle' AND c.relname = 'snapshot';
SELECT 'pg-views-owner|' || viewowner FROM pg_views WHERE schemaname = 'uqa_view_ownership_oracle' AND viewname = 'items';
SELECT 'pg-matviews-owner|' || matviewowner FROM pg_matviews WHERE schemaname = 'uqa_view_ownership_oracle' AND matviewname = 'snapshot';

SELECT pg_temp.view_owner_probe('outsider-alter', 'uqa_view_owner_outsider', 'ALTER VIEW uqa_view_ownership_oracle.items SET (security_barrier=true)');
SELECT pg_temp.view_owner_probe('outsider-drop', 'uqa_view_owner_outsider', 'DROP VIEW uqa_view_ownership_oracle.items');
SELECT pg_temp.view_owner_probe('outsider-refresh', 'uqa_view_owner_outsider', 'REFRESH MATERIALIZED VIEW uqa_view_ownership_oracle.snapshot');
SELECT pg_temp.view_owner_probe('outsider-replace', 'uqa_view_owner_outsider', 'CREATE OR REPLACE VIEW uqa_view_ownership_oracle.items AS SELECT id, value FROM uqa_view_ownership_oracle.base WHERE id > 0');
SELECT pg_temp.view_owner_probe('schema-owner-drop-view', 'uqa_view_schema_owner', 'DROP VIEW uqa_view_ownership_oracle.schema_drop_view');
SELECT pg_temp.view_owner_probe('schema-owner-drop-matview', 'uqa_view_schema_owner', 'DROP MATERIALIZED VIEW uqa_view_ownership_oracle.schema_drop_snapshot');
SELECT pg_temp.view_owner_probe('member-alter', 'uqa_view_owner_member', 'ALTER VIEW uqa_view_ownership_oracle.items SET (security_barrier=true)');
SELECT pg_temp.view_owner_probe('member-refresh', 'uqa_view_owner_member', 'REFRESH MATERIALIZED VIEW uqa_view_ownership_oracle.snapshot');
SELECT pg_temp.view_owner_probe('member-replace', 'uqa_view_owner_member', 'CREATE OR REPLACE VIEW uqa_view_ownership_oracle.items AS SELECT id, value FROM uqa_view_ownership_oracle.base WHERE id > 0');

SELECT pg_temp.view_owner_probe('missing-owner', 'uqa_view_owner_source', 'ALTER VIEW uqa_view_ownership_oracle.items OWNER TO uqa_view_owner_missing');
SELECT pg_temp.view_owner_probe('owner-without-create', 'uqa_view_owner_source', 'ALTER VIEW uqa_view_ownership_oracle.items OWNER TO uqa_view_owner_no_create');
SELECT pg_temp.view_owner_probe('owner-without-set', 'uqa_view_owner_source', 'ALTER VIEW uqa_view_ownership_oracle.items OWNER TO uqa_view_owner_no_set');
SELECT pg_temp.view_owner_probe('view-owner-transfer', 'uqa_view_owner_source', 'ALTER VIEW uqa_view_ownership_oracle.items OWNER TO uqa_view_owner_target');
SELECT pg_temp.view_owner_probe('matview-owner-transfer', 'uqa_view_owner_source', 'ALTER MATERIALIZED VIEW uqa_view_ownership_oracle.snapshot OWNER TO uqa_view_owner_target');

SELECT 'transferred-view-owner|' || viewowner FROM pg_views WHERE schemaname = 'uqa_view_ownership_oracle' AND viewname = 'items';
SELECT 'transferred-matview-owner|' || matviewowner FROM pg_matviews WHERE schemaname = 'uqa_view_ownership_oracle' AND matviewname = 'snapshot';

REVOKE uqa_view_owner_target FROM uqa_view_owner_source;
SELECT pg_temp.view_owner_probe('former-owner-alter', 'uqa_view_owner_source', 'ALTER VIEW uqa_view_ownership_oracle.items RESET (security_barrier)');
SELECT pg_temp.view_owner_probe('new-owner-alter', 'uqa_view_owner_target', 'ALTER VIEW uqa_view_ownership_oracle.items RESET (security_barrier)');
SELECT pg_temp.view_owner_probe('new-owner-refresh', 'uqa_view_owner_target', 'REFRESH MATERIALIZED VIEW uqa_view_ownership_oracle.snapshot');

SET ROLE uqa_view_owner_source;
CREATE TABLE uqa_view_ownership_oracle.cascade_base(id integer PRIMARY KEY);
GRANT SELECT ON TABLE uqa_view_ownership_oracle.cascade_base TO uqa_view_owner_target;
RESET ROLE;
SET ROLE uqa_view_owner_target;
CREATE VIEW uqa_view_ownership_oracle.cascade_items AS SELECT id FROM uqa_view_ownership_oracle.cascade_base;
RESET ROLE;
SELECT pg_temp.view_owner_probe('cascade-owner-drop', 'uqa_view_owner_source', 'DROP TABLE uqa_view_ownership_oracle.cascade_base CASCADE');

BEGIN;
ALTER VIEW uqa_view_ownership_oracle.rollback_items OWNER TO uqa_view_owner_target;
SELECT 'transaction-owner|' || viewowner FROM pg_views WHERE schemaname = 'uqa_view_ownership_oracle' AND viewname = 'rollback_items';
ROLLBACK;
SELECT 'rollback-owner|' || viewowner FROM pg_views WHERE schemaname = 'uqa_view_ownership_oracle' AND viewname = 'rollback_items';

SELECT pg_temp.view_owner_probe('owner-drop-dependent', 'postgres', 'DROP ROLE uqa_view_owner_target');
SELECT pg_temp.view_owner_probe('superuser-view-transfer-no-create', 'postgres', 'ALTER VIEW uqa_view_ownership_oracle.items OWNER TO uqa_view_owner_no_create');
SELECT pg_temp.view_owner_probe('superuser-matview-transfer-no-create', 'postgres', 'ALTER MATERIALIZED VIEW uqa_view_ownership_oracle.snapshot OWNER TO uqa_view_owner_no_create');
SELECT 'superuser-transferred-view-owner|' || viewowner FROM pg_views WHERE schemaname = 'uqa_view_ownership_oracle' AND viewname = 'items';
SELECT 'superuser-transferred-matview-owner|' || matviewowner FROM pg_matviews WHERE schemaname = 'uqa_view_ownership_oracle' AND matviewname = 'snapshot';
ALTER VIEW uqa_view_ownership_oracle.items OWNER TO postgres;
ALTER MATERIALIZED VIEW uqa_view_ownership_oracle.snapshot OWNER TO postgres;
REVOKE SELECT ON TABLE uqa_view_ownership_oracle.base FROM uqa_view_owner_target;
REVOKE ALL ON SCHEMA uqa_view_ownership_oracle FROM uqa_view_owner_target;
SELECT pg_temp.view_owner_probe('owner-drop-released', 'postgres', 'DROP ROLE uqa_view_owner_target');

DROP SCHEMA uqa_view_ownership_oracle CASCADE;
DROP ROLE uqa_view_schema_owner;
DROP ROLE uqa_view_owner_member;
DROP ROLE uqa_view_owner_source;
DROP ROLE uqa_view_owner_no_create;
DROP ROLE uqa_view_owner_no_set;
DROP ROLE uqa_view_owner_outsider;
