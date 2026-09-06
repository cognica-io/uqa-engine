\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned
BEGIN;
CREATE SCHEMA uqa_not_null_only_inheritance;
SET LOCAL search_path = uqa_not_null_only_inheritance, pg_catalog;
CREATE FUNCTION pg_temp.not_null_probe(label text, command text)
RETURNS text LANGUAGE plpgsql AS $oracle$
DECLARE
    state text;
    message text;
BEGIN
    EXECUTE command;
    RETURN label || '|ok';
EXCEPTION WHEN OTHERS THEN
    GET STACKED DIAGNOSTICS state = RETURNED_SQLSTATE, message = MESSAGE_TEXT;
    RETURN label || '|' || state || '|' || message;
END
$oracle$;

CREATE TABLE nn_parent(a integer);
CREATE TABLE nn_child() INHERITS(nn_parent);
SAVEPOINT before_constraint;
ALTER TABLE ONLY nn_parent ALTER COLUMN a SET NOT NULL;
SELECT 'only-parent', conname, connoinherit, convalidated FROM pg_constraint WHERE conrelid = 'nn_parent'::regclass;
ROLLBACK TO before_constraint;
SELECT 'rollback-count', count(*) FROM pg_constraint WHERE conrelid = 'nn_parent'::regclass;
ALTER TABLE ONLY nn_parent ALTER COLUMN a SET NOT NULL;
SELECT pg_temp.not_null_probe('repeat-only', 'ALTER TABLE ONLY nn_parent ALTER COLUMN a SET NOT NULL');
INSERT INTO nn_child VALUES(NULL);
SELECT pg_temp.not_null_probe('recursive-no-inherit', 'ALTER TABLE nn_parent ALTER COLUMN a SET NOT NULL');
SELECT 'preserved-parent', conname, connoinherit, convalidated FROM pg_constraint WHERE conrelid = 'nn_parent'::regclass;
SELECT 'unconstrained-child', count(*) FROM pg_constraint WHERE conrelid = 'nn_child'::regclass;
ALTER TABLE ONLY nn_parent ALTER COLUMN a DROP NOT NULL;
SELECT pg_temp.not_null_probe('child-null-validation', 'ALTER TABLE nn_parent ALTER COLUMN a SET NOT NULL');
SELECT 'failed-recursion-count', count(*) FROM pg_constraint WHERE conrelid IN ('nn_parent'::regclass, 'nn_child'::regclass);
DELETE FROM nn_child;
SELECT pg_temp.not_null_probe('recursive-after-drop', 'ALTER TABLE nn_parent ALTER COLUMN a SET NOT NULL');
SELECT 'inheritable-parent', connoinherit, convalidated FROM pg_constraint WHERE conrelid = 'nn_parent'::regclass;
SELECT pg_temp.not_null_probe('child-enforces', 'INSERT INTO nn_child VALUES(NULL)');

CREATE TABLE nn_leaf(a integer);
ALTER TABLE ONLY nn_leaf ALTER COLUMN a SET NOT NULL;
SELECT 'leaf', connoinherit FROM pg_constraint WHERE conrelid = 'nn_leaf'::regclass;
SELECT pg_temp.not_null_probe('leaf-recursive', 'ALTER TABLE nn_leaf ALTER COLUMN a SET NOT NULL');
CREATE TABLE nn_partitioned(a integer) PARTITION BY RANGE(a);
CREATE TABLE nn_partition PARTITION OF nn_partitioned FOR VALUES FROM(0) TO(10);
SELECT pg_temp.not_null_probe('partition-only', 'ALTER TABLE ONLY nn_partitioned ALTER COLUMN a SET NOT NULL');
SELECT 'partition-failure-count', count(*) FROM pg_constraint WHERE conrelid IN ('nn_partitioned'::regclass, 'nn_partition'::regclass);
ALTER TABLE nn_partitioned ALTER COLUMN a SET NOT NULL;
SELECT pg_temp.not_null_probe('partition-existing-only', 'ALTER TABLE ONLY nn_partitioned ALTER COLUMN a SET NOT NULL');
CREATE TABLE nn_empty_partitioned(a integer) PARTITION BY RANGE(a);
ALTER TABLE ONLY nn_empty_partitioned ALTER COLUMN a SET NOT NULL;
SELECT 'empty-partitioned', connoinherit FROM pg_constraint WHERE conrelid = 'nn_empty_partitioned'::regclass;

CREATE TABLE nn_merge_parent(a integer);
CREATE TABLE nn_merge_child(a integer CONSTRAINT child_nn NOT NULL NO INHERIT) INHERITS(nn_merge_parent);
SELECT pg_temp.not_null_probe('child-no-inherit-set', 'ALTER TABLE nn_merge_parent ALTER COLUMN a SET NOT NULL');
SELECT pg_temp.not_null_probe('child-no-inherit-add', 'ALTER TABLE nn_merge_parent ADD CONSTRAINT parent_nn NOT NULL a');
SELECT 'merge-parent-count', count(*) FROM pg_constraint WHERE conrelid = 'nn_merge_parent'::regclass;
SELECT 'merge-child', conname, connoinherit, convalidated FROM pg_constraint WHERE conrelid = 'nn_merge_child'::regclass;

CREATE TABLE nn_explicit(a integer CONSTRAINT chosen_nn NOT NULL NO INHERIT);
SELECT pg_temp.not_null_probe('explicit-no-inherit', 'ALTER TABLE nn_explicit ALTER COLUMN a SET NOT NULL');
SELECT pg_temp.not_null_probe('explicit-only', 'ALTER TABLE ONLY nn_explicit ALTER COLUMN a SET NOT NULL');
CREATE TABLE nn_unvalidated(a integer);
ALTER TABLE nn_unvalidated ADD CONSTRAINT retained_nn NOT NULL a NOT VALID;
SELECT 'unvalidated-before', conname, convalidated FROM pg_constraint WHERE conrelid = 'nn_unvalidated'::regclass;
ALTER TABLE nn_unvalidated ALTER COLUMN a SET NOT NULL;
SELECT 'validated-after', conname, convalidated FROM pg_constraint WHERE conrelid = 'nn_unvalidated'::regclass;
CREATE TABLE nn_errors(a integer);
SELECT pg_temp.not_null_probe('missing-column', 'ALTER TABLE nn_errors ALTER COLUMN absent SET NOT NULL');
SELECT pg_temp.not_null_probe('system-column', 'ALTER TABLE nn_errors ALTER COLUMN xmin SET NOT NULL');
INSERT INTO nn_errors VALUES(NULL);
SELECT pg_temp.not_null_probe('parent-null-validation', 'ALTER TABLE nn_errors ALTER COLUMN a SET NOT NULL');
SELECT 'validation-failure-count', count(*) FROM pg_constraint WHERE conrelid = 'nn_errors'::regclass;
ROLLBACK;
