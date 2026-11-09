-- Unified Query Algebra
-- Copyright (c) 2023-2026 Cognica, Inc.
-- Captured with PostgreSQL 18.4 (postgres:18.4-alpine).
\set ON_ERROR_STOP on
BEGIN;
CREATE TEMP TABLE backup_numeric(value bigint DEFAULT nextval(2147483647::regclass));
\echo numeric_definition :SQLSTATE
CREATE TEMP VIEW backup_numeric_view AS SELECT currval(2147483647::regclass);
\echo numeric_view :SQLSTATE
CREATE TEMP TABLE backup_wrong_kind(value bigint DEFAULT nextval('backup_numeric'::regclass));
\echo relation_definition :SQLSTATE
CREATE TEMP TABLE backup_dynamic(value bigint DEFAULT nextval('backup_absent'::text));
\echo dynamic_definition :SQLSTATE
SAVEPOINT execution;
\set ON_ERROR_STOP off
INSERT INTO backup_numeric DEFAULT VALUES;
\echo numeric_execution :SQLSTATE
\set ON_ERROR_STOP on
ROLLBACK TO execution;
\set ON_ERROR_STOP off
INSERT INTO backup_wrong_kind DEFAULT VALUES;
\echo relation_execution :SQLSTATE
\set ON_ERROR_STOP on
ROLLBACK TO execution;
\set ON_ERROR_STOP off
INSERT INTO backup_dynamic DEFAULT VALUES;
\echo dynamic_execution :SQLSTATE
\set ON_ERROR_STOP on
ROLLBACK TO execution;
ROLLBACK;
