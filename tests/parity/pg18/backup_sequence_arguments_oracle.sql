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
SELECT '-'::regclass::oid AS dash_oid, '010'::regclass::oid AS octal_oid,
       '4294967295'::regclass::oid AS maximum_oid \gset
\echo numeric_regclass_values :dash_oid :octal_oid :maximum_oid
BEGIN;
CREATE TEMP TABLE backup_dash(value bigint DEFAULT nextval('-'));
\echo dash_definition :SQLSTATE
CREATE TEMP VIEW backup_dash_view AS SELECT currval('-'::regclass);
\echo dash_view :SQLSTATE
SAVEPOINT execution;
\set ON_ERROR_STOP off
INSERT INTO backup_dash DEFAULT VALUES;
\echo dash_execution :SQLSTATE
\set ON_ERROR_STOP on
ROLLBACK TO execution;
ROLLBACK;
\set ON_ERROR_STOP off
SELECT '08'::regclass;
\echo invalid_octal_input :SQLSTATE
SELECT '4294967296'::regclass;
\echo overflowing_oid_input :SQLSTATE
SELECT '+5'::regclass;
\echo signed_name_input :SQLSTATE
\set ON_ERROR_STOP on
