-- Unified Query Algebra
-- Copyright (c) 2023-2026 Cognica, Inc.
-- Captured with PostgreSQL 18.4 (postgres:18.4-alpine).
\set ON_ERROR_STOP on
BEGIN;
CREATE TEMP TABLE backup_items(id integer, label text);
CREATE FUNCTION pg_temp.backup_identity(x integer) RETURNS integer LANGUAGE SQL IMMUTABLE AS $$ SELECT x $$;
CREATE UNIQUE INDEX backup_saved ON backup_items(pg_temp.backup_identity(id)) INCLUDE(label,label) WHERE id > 0;
INSERT INTO backup_items VALUES(1,'one'),(-1,'negative');
ALTER FUNCTION pg_temp.backup_identity(integer) RENAME TO backup_renamed_identity;
ALTER FUNCTION pg_temp.backup_renamed_identity(integer) VOLATILE;
SELECT 'retained_index', indisunique, indnkeyatts, indnatts, indpred IS NOT NULL FROM pg_index WHERE indexrelid = 'backup_saved'::regclass;
SAVEPOINT duplicate_key;
\set ON_ERROR_STOP off
INSERT INTO backup_items VALUES(1,'duplicate');
\echo duplicate_key :SQLSTATE
\set ON_ERROR_STOP on
ROLLBACK TO duplicate_key;
INSERT INTO backup_items VALUES(2,'two'),(-1,'outside predicate');
SELECT 'later_rows', count(*), count(*) FILTER (WHERE id = -1) FROM backup_items;
CREATE TYPE pg_temp.backup_mood AS ENUM ('low','high');
CREATE TEMP TABLE backup_typed(mood pg_temp.backup_mood);
CREATE INDEX backup_typed_saved ON backup_typed(mood);
CREATE TEMP TABLE backup_type_identity AS SELECT oid, typarray FROM pg_type WHERE oid = 'pg_temp.backup_mood'::regtype;
ALTER TYPE pg_temp.backup_mood RENAME TO backup_renamed_mood;
SELECT 'renamed_type', t.oid = o.oid, t.typarray = o.typarray FROM pg_type t JOIN backup_type_identity o ON t.oid = o.oid;
SAVEPOINT bad_expression;
\set ON_ERROR_STOP off
CREATE INDEX bad ON backup_items ((missing + 1));
\echo missing_column :SQLSTATE
ROLLBACK TO bad_expression;
CREATE INDEX bad ON backup_items(id) WHERE id;
\echo predicate_type :SQLSTATE
ROLLBACK TO bad_expression;
CREATE INDEX bad ON backup_items(id) WHERE true AND id;
\echo predicate_child :SQLSTATE
ROLLBACK TO bad_expression;
CREATE INDEX bad ON backup_items(id) WHERE 'invalid_boolean';
\echo predicate_literal :SQLSTATE
ROLLBACK TO bad_expression;
CREATE INDEX bad ON backup_items((sum(id)));
\echo aggregate_key :SQLSTATE
ROLLBACK TO bad_expression;
CREATE INDEX bad ON backup_items((unnest(ARRAY[id])));
\echo set_key :SQLSTATE
ROLLBACK TO bad_expression;
CREATE INDEX bad ON backup_items((row_number() OVER ()));
\echo window_key :SQLSTATE
ROLLBACK TO bad_expression;
CREATE INDEX bad ON backup_items((CASE WHEN id THEN label ELSE 'x' END));
\echo nested_condition :SQLSTATE
ROLLBACK TO bad_expression;
\set ON_ERROR_STOP on
CREATE SCHEMA backup_owner;
SET LOCAL search_path = backup_owner, public, pg_temp;
CREATE DOMAIN backup_owner.backup_truth AS boolean;
CREATE DOMAIN backup_owner.backup_nested_truth AS backup_owner.backup_truth;
CREATE TABLE backup_flags(id integer, enabled backup_owner.backup_nested_truth);
CREATE TABLE backup_rows(id integer, enabled backup_owner.backup_nested_truth);
INSERT INTO backup_flags VALUES(1,true),(-1,false);
INSERT INTO backup_rows VALUES(1,true),(-1,false);
CREATE FUNCTION backup_owner.backup_row_key(x backup_rows) RETURNS integer LANGUAGE SQL IMMUTABLE AS $$ SELECT (x).id $$;
CREATE FUNCTION backup_owner.backup_row_enabled(x backup_rows) RETURNS backup_owner.backup_nested_truth LANGUAGE SQL IMMUTABLE AS $$ SELECT (x).enabled $$;
CREATE UNIQUE INDEX backup_flag_saved ON backup_flags(id) WHERE enabled;
\echo domain_index_created :SQLSTATE
CREATE UNIQUE INDEX backup_row_saved ON backup_rows(backup_owner.backup_row_key(backup_rows)) WHERE backup_owner.backup_row_enabled(backup_rows);
\echo whole_row_index_created :SQLSTATE
SAVEPOINT rename_rollback;
ALTER TABLE backup_rows RENAME TO backup_rolled_back_rows;
ROLLBACK TO rename_rollback;
ALTER DOMAIN backup_owner.backup_truth RENAME TO backup_renamed_truth;
ALTER DOMAIN backup_owner.backup_nested_truth RENAME TO backup_renamed_nested_truth;
ALTER FUNCTION backup_owner.backup_row_key(backup_rows) RENAME TO backup_renamed_key;
ALTER FUNCTION backup_owner.backup_row_enabled(backup_rows) RENAME TO backup_renamed_enabled;
ALTER TABLE backup_flags RENAME TO backup_renamed_flags;
ALTER TABLE backup_rows RENAME TO backup_renamed_rows;
CREATE TEMP TABLE backup_binding_identity AS SELECT 'backup_renamed_rows'::regclass::oid AS table_oid, 'backup_row_saved'::regclass::oid AS index_oid, 'backup_renamed_rows'::regtype::oid AS row_type_oid;
CREATE SCHEMA backup_moved;
SAVEPOINT move_rollback;
ALTER TABLE backup_renamed_rows SET SCHEMA backup_moved;
ALTER SCHEMA backup_moved RENAME TO backup_rolled_back;
ROLLBACK TO move_rollback;
SELECT 'move_rollback', 'backup_renamed_rows'::regclass::oid = table_oid, 'backup_row_saved'::regclass::oid = index_oid FROM backup_binding_identity;
ALTER TABLE backup_renamed_flags SET SCHEMA backup_moved;
ALTER TABLE backup_renamed_rows SET SCHEMA backup_moved;
ALTER SCHEMA backup_moved RENAME TO backup_retained;
SET LOCAL search_path = backup_retained, backup_owner, public, pg_temp;
SELECT 'moved_bindings', 'backup_renamed_rows'::regclass::oid = table_oid, 'backup_row_saved'::regclass::oid = index_oid, 'backup_renamed_rows'::regtype::oid = row_type_oid FROM backup_binding_identity;
SAVEPOINT typed_duplicate;
\set ON_ERROR_STOP off
INSERT INTO backup_renamed_flags VALUES(1,true);
\echo domain_index_duplicate :SQLSTATE
ROLLBACK TO typed_duplicate;
INSERT INTO backup_renamed_rows VALUES(1,true);
\echo whole_row_index_duplicate :SQLSTATE
ROLLBACK TO typed_duplicate;
\set ON_ERROR_STOP on
INSERT INTO backup_renamed_flags VALUES(1,false),(1,NULL),(2,true);
INSERT INTO backup_renamed_rows VALUES(1,false),(1,NULL),(2,true);
SAVEPOINT typed_update;
\set ON_ERROR_STOP off
UPDATE backup_renamed_rows SET id=1 WHERE id=2;
\echo whole_row_index_update :SQLSTATE
ROLLBACK TO typed_update;
\set ON_ERROR_STOP on
SELECT 'domain_index_rows', count(*), count(*) FILTER (WHERE enabled) FROM backup_renamed_flags;
SELECT 'whole_row_index_rows', count(*), count(*) FILTER (WHERE backup_owner.backup_renamed_enabled(backup_renamed_rows)) FROM backup_renamed_rows;
SELECT 'whole_row_values', backup_owner.backup_renamed_key(backup_renamed_rows) FROM backup_renamed_rows WHERE backup_owner.backup_renamed_enabled(backup_renamed_rows) ORDER BY 2;
ROLLBACK;
