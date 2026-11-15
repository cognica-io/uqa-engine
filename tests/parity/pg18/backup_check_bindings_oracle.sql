-- Unified Query Algebra
-- Copyright (c) 2023-2026 Cognica, Inc.
-- Independent PostgreSQL 18.4 CHECK binding and lifecycle outcomes.
\set ON_ERROR_STOP on
BEGIN;
CREATE SCHEMA backup_check_bindings;
SET LOCAL search_path=backup_check_bindings,public;
CREATE DOMAIN truth AS boolean;
CREATE DOMAIN nested_truth AS truth CHECK(VALUE);
CREATE DOMAIN positive AS integer CHECK(VALUE>0);
CREATE DOMAIN later AS integer;
CREATE TABLE old_domain_values(id later);
INSERT INTO old_domain_values VALUES(-1);
ALTER DOMAIN later ADD CONSTRAINT later_positive CHECK(VALUE>0) NOT VALID;
CREATE TABLE items(id integer,flag truth);
CREATE DOMAIN row_domain AS items CHECK((VALUE).id>0);
INSERT INTO items VALUES(-1,false),(1,true);
ALTER TABLE items ADD CONSTRAINT direct_guard CHECK(flag AND id>0) NOT VALID;
CREATE FUNCTION row_guard(x items) RETURNS truth LANGUAGE SQL VOLATILE AS $$ SELECT (x).flag $$;
ALTER TABLE items ADD CONSTRAINT row_guard_check CHECK(row_guard(items)) NOT VALID;
CREATE TABLE latent(id integer CHECK(id/0>0));
CREATE FOREIGN DATA WRAPPER backup_check_wrapper;
CREATE SERVER backup_check_server FOREIGN DATA WRAPPER backup_check_wrapper;
CREATE FOREIGN TABLE remote_items(id integer CHECK(id>0),flag truth,CONSTRAINT flag_guard CHECK(flag)) SERVER backup_check_server;
SELECT 'unvalidated', count(*) FROM pg_constraint WHERE connamespace='backup_check_bindings'::regnamespace AND NOT convalidated;
CREATE TEMP TABLE saved_check_oids AS SELECT oid,conrelid,contypid FROM pg_constraint WHERE connamespace='backup_check_bindings'::regnamespace;
SAVEPOINT names;
ALTER TABLE items RENAME TO rolled_back;
ALTER DOMAIN truth RENAME TO rolled_back_truth;
ROLLBACK TO names;
SELECT 'rollback_names', to_regclass('items') IS NOT NULL, to_regtype('truth') IS NOT NULL;
ALTER DOMAIN truth RENAME TO renamed_truth;
ALTER FUNCTION row_guard(items) RENAME TO renamed_guard;
ALTER TABLE items RENAME TO renamed_items;
CREATE SCHEMA backup_check_moved;
ALTER TABLE renamed_items SET SCHEMA backup_check_moved;
ALTER SCHEMA backup_check_moved RENAME TO backup_check_retained;
ALTER FOREIGN TABLE remote_items RENAME TO renamed_remote;
SELECT 'retained_identities', count(*)=count(c.oid), bool_and(s.conrelid=c.conrelid AND s.contypid=c.contypid) FROM saved_check_oids s LEFT JOIN pg_constraint c ON c.oid=s.oid;
SELECT 'old_rows', string_agg(id::text,',' ORDER BY id) FROM backup_check_retained.renamed_items;
SELECT 'old_domain_rows', id FROM old_domain_values;
SAVEPOINT invalid;
\set ON_ERROR_STOP off
INSERT INTO backup_check_retained.renamed_items VALUES(-2,false);
\echo insert_check :SQLSTATE
ROLLBACK TO invalid;
UPDATE backup_check_retained.renamed_items SET flag=false WHERE id=1;
\echo update_check :SQLSTATE
ROLLBACK TO invalid;
INSERT INTO old_domain_values VALUES(-2);
\echo domain_write :SQLSTATE
ROLLBACK TO invalid;
SELECT (-1)::positive;
\echo domain_cast :SQLSTATE
ROLLBACK TO invalid;
SELECT ROW(-1,true)::row_domain;
\echo composite_domain_cast :SQLSTATE
ROLLBACK TO invalid;
INSERT INTO latent VALUES(1);
\echo delayed_evaluation :SQLSTATE
ROLLBACK TO invalid;
CREATE TABLE bad(id integer CHECK(id));
\echo non_boolean_column :SQLSTATE
ROLLBACK TO invalid;
CREATE TABLE collated_condition(id integer, CHECK('true' COLLATE "C"));
\echo collated_unknown_true :SQLSTATE
ROLLBACK TO invalid;
CREATE TABLE bad(id integer, CHECK('invalid_boolean' COLLATE "C"));
\echo collated_unknown_invalid :SQLSTATE
ROLLBACK TO invalid;
CREATE TABLE bad(id integer, CHECK(true AND id));
\echo non_boolean_operand :SQLSTATE
ROLLBACK TO invalid;
CREATE DOMAIN bad AS integer CHECK(VALUE);
\echo non_boolean_domain :SQLSTATE
ROLLBACK TO invalid;
CREATE FOREIGN TABLE bad(id integer CHECK(id)) SERVER backup_check_server;
\echo non_boolean_foreign :SQLSTATE
ROLLBACK TO invalid;
CREATE TABLE bad(id integer, CHECK(sum(id)>0));
\echo aggregate :SQLSTATE
ROLLBACK TO invalid;
CREATE TABLE bad(id integer, CHECK((SELECT true)));
\echo subquery :SQLSTATE
ROLLBACK TO invalid;
\set ON_ERROR_STOP on
INSERT INTO backup_check_retained.renamed_items VALUES(2,true),(3,NULL);
INSERT INTO old_domain_values VALUES(2);
SELECT 'later_rows', string_agg(id::text,',' ORDER BY id) FROM backup_check_retained.renamed_items;
SELECT 'still_unvalidated', count(*) FROM pg_constraint WHERE conname IN ('direct_guard','row_guard_check','later_positive') AND NOT convalidated;
ROLLBACK;
