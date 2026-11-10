-- Unified Query Algebra
-- Copyright (c) 2023-2026 Cognica, Inc.
-- Captured with PostgreSQL 18.4 (postgres:18.4-alpine).
\set ON_ERROR_STOP on
BEGIN;
CREATE TEMP TABLE backup_parent(a integer, b integer, UNIQUE(a,b));
SAVEPOINT local_repeat;
CREATE TEMP TABLE backup_child(x integer, FOREIGN KEY(x,x) REFERENCES backup_parent(a,b));
\echo local_repeat :SQLSTATE
ROLLBACK TO local_repeat;
SAVEPOINT target_repeat;
\set ON_ERROR_STOP off
CREATE TEMP TABLE backup_child(x integer, FOREIGN KEY(x,x) REFERENCES backup_parent(a,a));
\echo target_repeat :SQLSTATE
\set ON_ERROR_STOP on
ROLLBACK TO target_repeat;
SAVEPOINT action_repeat;
CREATE TEMP TABLE backup_child(x integer, y integer, FOREIGN KEY(x,y) REFERENCES backup_parent(a,b) ON DELETE SET NULL(x,x));
\echo action_repeat :SQLSTATE
ROLLBACK TO action_repeat;
ROLLBACK;
