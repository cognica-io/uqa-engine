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
ROLLBACK;
