\set ON_ERROR_STOP on
\o /dev/null
CREATE EXTENSION IF NOT EXISTS dblink;
DROP SCHEMA IF EXISTS uqa_pg18_catalog_visibility_oracle CASCADE;
CREATE SCHEMA uqa_pg18_catalog_visibility_oracle;
SET search_path = uqa_pg18_catalog_visibility_oracle, pg_catalog;
CREATE TABLE catalog_anchor(id integer);
INSERT INTO catalog_anchor VALUES (1);
CREATE TABLE catalog_old_source(id integer);
INSERT INTO catalog_old_source VALUES (10);
CREATE TABLE catalog_new_source(id integer);
INSERT INTO catalog_new_source VALUES (20);
CREATE TABLE catalog_truncate_source(id integer);
INSERT INTO catalog_truncate_source VALUES (30);
CREATE TABLE catalog_evolving_source(id integer);
INSERT INTO catalog_evolving_source VALUES (50);
CREATE VIEW catalog_snapshot_view AS SELECT id FROM catalog_old_source;
CREATE FUNCTION catalog_snapshot_function() RETURNS integer LANGUAGE SQL AS 'SELECT 1';
CREATE FUNCTION try_add_not_null_to_new_table() RETURNS text LANGUAGE plpgsql AS $$
BEGIN
    ALTER TABLE catalog_created_after_snapshot ADD COLUMN marker integer NOT NULL;
    RETURN 'ok';
EXCEPTION WHEN OTHERS THEN
    RETURN SQLSTATE;
END
$$;
SELECT public.dblink_connect('catalog_sibling', 'dbname=' || current_database());
SELECT public.dblink_exec('catalog_sibling', 'SET search_path = uqa_pg18_catalog_visibility_oracle, pg_catalog');
BEGIN ISOLATION LEVEL REPEATABLE READ;
SELECT id FROM catalog_anchor;
SELECT public.dblink_exec('catalog_sibling', $$CREATE OR REPLACE VIEW catalog_snapshot_view AS SELECT id FROM catalog_new_source; CREATE OR REPLACE FUNCTION catalog_snapshot_function() RETURNS integer LANGUAGE SQL AS 'SELECT 2'; CREATE TABLE catalog_created_after_snapshot(id integer); INSERT INTO catalog_created_after_snapshot VALUES (40); TRUNCATE catalog_truncate_source$$);
\o
COPY (
    SELECT label, value
    FROM (
        SELECT 1 AS ordinal, 'view' AS label, id::text AS value FROM catalog_snapshot_view
        UNION ALL
        SELECT 2, 'function', catalog_snapshot_function()::text
        UNION ALL
        SELECT 3, 'new_count', count(*)::text FROM catalog_created_after_snapshot
        UNION ALL
        SELECT 4, 'truncate_count', count(*)::text FROM catalog_truncate_source
    ) AS observations
    ORDER BY ordinal
) TO STDOUT;
COPY (SELECT 'ddl_not_null' AS label, try_add_not_null_to_new_table() AS value) TO STDOUT;
\o /dev/null
SELECT public.dblink_exec('catalog_sibling', 'ALTER TABLE catalog_evolving_source ADD COLUMN marker integer DEFAULT 7; ALTER TABLE catalog_evolving_source RENAME TO catalog_renamed_source');
\o
COPY (
    SELECT label, value
    FROM (
        SELECT 1 AS ordinal, 'altered_row' AS label, id::text || '|' || marker::text AS value FROM catalog_renamed_source
        UNION ALL
        SELECT 2, 'catalog_refresh', id::text FROM catalog_anchor
    ) AS observations
    ORDER BY ordinal
) TO STDOUT;
\o /dev/null
ROLLBACK;
SELECT public.dblink_exec('catalog_sibling', 'DROP TABLE catalog_renamed_source');
\o
COPY (
    SELECT label, value
    FROM (
        SELECT 1 AS ordinal, 'old_regclass' AS label, coalesce(to_regclass('catalog_evolving_source')::text, '<null>') AS value
        UNION ALL
        SELECT 2, 'dropped_regclass', coalesce(to_regclass('catalog_renamed_source')::text, '<null>')
    ) AS observations
    ORDER BY ordinal
) TO STDOUT;
\o /dev/null
SELECT public.dblink_disconnect('catalog_sibling');
DROP SCHEMA uqa_pg18_catalog_visibility_oracle CASCADE;
\o
